//! Turning a request's bearer token into a [`Principal`] (spec §14 A-2, A-6).
//!
//! - `entra` mode: the token is validated ([`EntraValidator`], A-1) and its
//!   claims become a principal: `oid`, `name`, `email`, `groups` →
//!   `group_ids`, and `roles` mapped through the configured [`RoleMap`]
//!   with the default role as fallback. A caller whose roles map to nothing
//!   and with no default role has no platform access ([`AuthError::NoPlatformRole`]).
//! - `disabled` mode: every request gets [`Principal::dev`], an admin.
//!   Refused in `prod` here as well as in the bootstrap config.
//!
//! `ragk_…` API keys (A-3) are resolved separately, before this step.

use std::collections::BTreeSet;

use fable_core::config::{AuthMode, BootstrapConfig, Environment};
use fable_core::principal::{AuthMethod, Principal, RoleMap, RoleMapError};

use super::entra::{EntraClaims, EntraValidator, TokenError};

/// Why a request could not be authenticated.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No bearer token on a request that needs one (401).
    #[error("missing bearer token")]
    MissingToken,
    /// The token was rejected (401) or could not be checked (503, see
    /// [`TokenError::is_server_error`]).
    #[error(transparent)]
    Token(#[from] TokenError),
    /// Valid token, but no app role maps to a platform role and there is
    /// no default role (403).
    #[error("no platform role assigned; ask an admin for an Entra app role")]
    NoPlatformRole,
}

/// Build the principal for validated Entra claims.
pub fn principal_from_claims(
    claims: EntraClaims,
    role_map: &RoleMap,
) -> Result<Principal, AuthError> {
    let global_roles = role_map.resolve(claims.roles.iter().map(String::as_str));
    if global_roles.is_empty() {
        return Err(AuthError::NoPlatformRole);
    }
    Ok(Principal {
        oid: claims.oid,
        name: claims.name,
        email: claims.email,
        group_ids: claims
            .groups
            .into_iter()
            .filter(|g| !g.is_empty())
            .collect::<BTreeSet<_>>(),
        global_roles,
        auth: AuthMethod::Sso,
    })
}

/// Why an [`Authenticator`] could not be built.
#[derive(Debug, thiserror::Error)]
pub enum AuthSetupError {
    /// `disabled` mode requested in `prod` (A-6).
    #[error("RAG_AUTH__MODE=disabled is not permitted when RAG_ENVIRONMENT=prod")]
    DisabledInProd,
    /// Invalid role map.
    #[error(transparent)]
    RoleMap(#[from] RoleMapError),
    /// Entra settings missing.
    #[error(transparent)]
    Token(#[from] TokenError),
}

enum Mode {
    Entra {
        validator: Box<EntraValidator>,
        role_map: RoleMap,
    },
    Disabled,
}

/// Resolves SSO bearer tokens to principals for the configured auth mode.
pub struct Authenticator {
    mode: Mode,
}

impl Authenticator {
    /// An authenticator for the bootstrap config, fetching JWKS with `http`.
    pub fn from_config(
        config: &BootstrapConfig,
        http: reqwest::Client,
    ) -> Result<Self, AuthSetupError> {
        match config.auth.mode {
            AuthMode::Disabled => Self::disabled(config.environment),
            AuthMode::Entra => Ok(Self::entra(
                EntraValidator::from_config(&config.auth, http)?,
                RoleMap::from_config(&config.auth)?,
            )),
        }
    }

    /// Entra mode with an explicit validator and role map.
    pub fn entra(validator: EntraValidator, role_map: RoleMap) -> Self {
        Self {
            mode: Mode::Entra {
                validator: Box::new(validator),
                role_map,
            },
        }
    }

    /// `disabled` mode: inject the dev admin. Refused in `prod` (A-6).
    pub fn disabled(environment: Environment) -> Result<Self, AuthSetupError> {
        if environment == Environment::Prod {
            return Err(AuthSetupError::DisabledInProd);
        }
        tracing::warn!(
            %environment,
            "authentication is DISABLED: every request runs as the local dev admin"
        );
        Ok(Self {
            mode: Mode::Disabled,
        })
    }

    /// Whether this is `disabled` mode.
    pub fn is_disabled(&self) -> bool {
        matches!(self.mode, Mode::Disabled)
    }

    /// The principal for a request's bearer token (without the `Bearer `
    /// prefix), if any.
    pub async fn authenticate(&self, bearer: Option<&str>) -> Result<Principal, AuthError> {
        match &self.mode {
            Mode::Disabled => Ok(Principal::dev()),
            Mode::Entra {
                validator,
                role_map,
            } => {
                let token = bearer
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or(AuthError::MissingToken)?;
                let claims = validator.validate(token).await?;
                principal_from_claims(claims, role_map)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fable_core::principal::GlobalRole;

    use super::*;

    fn claims(roles: &[&str], groups: &[&str]) -> EntraClaims {
        EntraClaims {
            oid: "oid-1".to_owned(),
            tenant_id: Some("tid".to_owned()),
            name: "Ada".to_owned(),
            email: "ada@example.com".to_owned(),
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    fn default_map() -> RoleMap {
        RoleMap::from_config(&Default::default()).unwrap()
    }

    #[test]
    fn claims_become_a_principal() {
        let p = principal_from_claims(
            claims(&["Rag.Admin"], &["g1", "g2", "g1", ""]),
            &default_map(),
        )
        .unwrap();
        assert_eq!(p.oid, "oid-1");
        assert_eq!(p.name, "Ada");
        assert_eq!(p.email, "ada@example.com");
        assert_eq!(
            p.group_ids,
            BTreeSet::from(["g1".to_owned(), "g2".to_owned()])
        );
        assert_eq!(p.global_roles, BTreeSet::from([GlobalRole::Admin]));
        assert!(p.is_sso());
    }

    #[test]
    fn unmapped_roles_fall_back_to_default_role() {
        let p = principal_from_claims(claims(&["Something.Else"], &[]), &default_map()).unwrap();
        assert_eq!(p.global_roles, BTreeSet::from([GlobalRole::User]));
        let p = principal_from_claims(claims(&[], &[]), &default_map()).unwrap();
        assert_eq!(p.global_roles, BTreeSet::from([GlobalRole::User]));
    }

    #[test]
    fn no_mapped_role_and_no_default_is_rejected() {
        let map = RoleMap::new(
            BTreeMap::from([("Rag.Admin".to_owned(), GlobalRole::Admin)]),
            None,
        );
        assert!(matches!(
            principal_from_claims(claims(&["Rag.User"], &[]), &map),
            Err(AuthError::NoPlatformRole)
        ));
    }

    #[tokio::test]
    async fn disabled_mode_injects_dev_admin_outside_prod() {
        for env in [Environment::Dev, Environment::Staging] {
            let auth = Authenticator::disabled(env).unwrap();
            assert!(auth.is_disabled());
            let p = auth.authenticate(None).await.unwrap();
            assert_eq!(p, Principal::dev());
            assert!(p.is_admin());
            // A token is ignored, not validated.
            assert_eq!(auth.authenticate(Some("x")).await.unwrap(), p);
        }
    }

    #[test]
    fn disabled_mode_is_refused_in_prod() {
        assert!(matches!(
            Authenticator::disabled(Environment::Prod),
            Err(AuthSetupError::DisabledInProd)
        ));
    }
}
