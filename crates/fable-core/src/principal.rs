//! The authenticated caller and global role mapping (spec §14 A-2, A-6).
//!
//! A [`Principal`] is what every handler sees after authentication: the
//! Entra object id, display name, sign-in name, group object ids and the
//! platform-global roles. Global roles come from the token's `roles` claim
//! (Entra app roles) mapped through [`RoleMap`], with the configured
//! default role when nothing maps.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::AuthConfig;

/// Platform-global role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalRole {
    /// Full platform control; implicit owner of every collection.
    Admin,
    /// Regular user: may create collections and use what they are granted.
    User,
}

impl GlobalRole {
    /// All roles.
    pub const ALL: [GlobalRole; 2] = [GlobalRole::Admin, GlobalRole::User];

    /// Stable lowercase name, as used in `RAG_AUTH__ROLE_MAP` values.
    pub fn as_str(self) -> &'static str {
        match self {
            GlobalRole::Admin => "admin",
            GlobalRole::User => "user",
        }
    }
}

impl fmt::Display for GlobalRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for an unknown global role name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown global role {0:?}; expected one of: admin, user")]
pub struct ParseGlobalRoleError(String);

impl FromStr for GlobalRole {
    type Err = ParseGlobalRoleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GlobalRole::ALL
            .into_iter()
            .find(|r| r.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| ParseGlobalRoleError(s.to_owned()))
    }
}

/// How the caller authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// Interactive Entra sign-in (or the dev principal in `disabled` mode).
    Sso,
    /// A platform-issued `ragk_…` API key (A-3): search-only, limited to
    /// `scopes` (collection ids).
    ApiKey {
        /// The `api_keys.id` row.
        key_id: Uuid,
        /// Collection ids the key may search.
        scopes: BTreeSet<Uuid>,
    },
}

/// Object id of the dev principal injected in `disabled` mode (A-6).
pub const DEV_PRINCIPAL_OID: &str = "00000000-0000-0000-0000-000000000001";

/// An authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Entra object id of the user or service principal.
    pub oid: String,
    /// Display name; may be empty.
    pub name: String,
    /// Sign-in name / email; may be empty.
    pub email: String,
    /// Entra group object ids (`groups` claim). Empty for API keys.
    pub group_ids: BTreeSet<String>,
    /// Platform-global roles.
    pub global_roles: BTreeSet<GlobalRole>,
    /// How the caller authenticated.
    pub auth: AuthMethod,
}

impl Principal {
    /// The local dev admin injected when `RAG_AUTH__MODE=disabled` (A-6).
    /// Callers must only use it outside `prod`; see
    /// [`crate::config::BootstrapConfig`] for the startup guard.
    pub fn dev() -> Self {
        Self {
            oid: DEV_PRINCIPAL_OID.to_owned(),
            name: "Local Developer".to_owned(),
            email: "dev@localhost".to_owned(),
            group_ids: BTreeSet::new(),
            global_roles: BTreeSet::from([GlobalRole::Admin, GlobalRole::User]),
            auth: AuthMethod::Sso,
        }
    }

    /// Whether the caller holds the global `admin` role.
    pub fn is_admin(&self) -> bool {
        self.global_roles.contains(&GlobalRole::Admin)
    }

    /// Whether the caller holds `role`; `admin` implies every role.
    pub fn has_global_role(&self, role: GlobalRole) -> bool {
        self.is_admin() || self.global_roles.contains(&role)
    }

    /// Whether the caller signed in interactively (not an API key).
    pub fn is_sso(&self) -> bool {
        matches!(self.auth, AuthMethod::Sso)
    }

    /// The key's collection scopes, for API-key callers.
    pub fn key_scopes(&self) -> Option<&BTreeSet<Uuid>> {
        match &self.auth {
            AuthMethod::Sso => None,
            AuthMethod::ApiKey { scopes, .. } => Some(scopes),
        }
    }
}

/// Error building a [`RoleMap`] from configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleMapError {
    /// A `RAG_AUTH__ROLE_MAP` value is not a platform role.
    #[error("RAG_AUTH__ROLE_MAP maps {app_role:?} to {source}")]
    Mapping {
        app_role: String,
        source: ParseGlobalRoleError,
    },
    /// `RAG_AUTH__DEFAULT_ROLE` is not a platform role.
    #[error("RAG_AUTH__DEFAULT_ROLE: {0}")]
    Default(ParseGlobalRoleError),
}

/// Entra app role → platform global role, plus the fallback role (A-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleMap {
    map: BTreeMap<String, GlobalRole>,
    default_role: Option<GlobalRole>,
}

impl RoleMap {
    /// A map from explicit entries.
    pub fn new(map: BTreeMap<String, GlobalRole>, default_role: Option<GlobalRole>) -> Self {
        Self { map, default_role }
    }

    /// The configured map (`RAG_AUTH__ROLE_MAP`, `RAG_AUTH__DEFAULT_ROLE`).
    /// App role names are matched exactly (Entra claim values are
    /// case-sensitive); platform role names are case-insensitive.
    pub fn from_config(auth: &AuthConfig) -> Result<Self, RoleMapError> {
        let map = auth
            .role_map
            .iter()
            .map(|(app_role, role)| {
                role.parse()
                    .map(|r| (app_role.clone(), r))
                    .map_err(|source| RoleMapError::Mapping {
                        app_role: app_role.clone(),
                        source,
                    })
            })
            .collect::<Result<_, _>>()?;
        let default_role = auth
            .default_role()
            .map(str::parse)
            .transpose()
            .map_err(RoleMapError::Default)?;
        Ok(Self { map, default_role })
    }

    /// The fallback role, if any.
    pub fn default_role(&self) -> Option<GlobalRole> {
        self.default_role
    }

    /// Map token app roles to platform roles, ignoring unmapped ones.
    pub fn map<'a>(&self, app_roles: impl IntoIterator<Item = &'a str>) -> BTreeSet<GlobalRole> {
        app_roles
            .into_iter()
            .filter_map(|r| self.map.get(r).copied())
            .collect()
    }

    /// The caller's global roles: the mapped roles, or the default role
    /// when nothing maps. Empty means the caller has no platform access.
    pub fn resolve<'a>(
        &self,
        app_roles: impl IntoIterator<Item = &'a str>,
    ) -> BTreeSet<GlobalRole> {
        let roles = self.map(app_roles);
        if roles.is_empty() {
            self.default_role.into_iter().collect()
        } else {
            roles
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(map: &[(&str, &str)], default_role: &str) -> AuthConfig {
        AuthConfig {
            role_map: map
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            default_role: default_role.to_owned(),
            ..AuthConfig::default()
        }
    }

    #[test]
    fn global_role_parses_and_displays() {
        for r in GlobalRole::ALL {
            assert_eq!(r.as_str().parse::<GlobalRole>().unwrap(), r);
            assert_eq!(r.to_string(), r.as_str());
        }
        assert_eq!(" Admin ".parse::<GlobalRole>().unwrap(), GlobalRole::Admin);
        assert!("owner".parse::<GlobalRole>().is_err());
        assert_eq!(
            serde_json::to_string(&GlobalRole::Admin).unwrap(),
            "\"admin\""
        );
    }

    #[test]
    fn default_config_maps_rag_admin_and_rag_user() {
        let m = RoleMap::from_config(&AuthConfig::default()).unwrap();
        assert_eq!(m.map(["Rag.Admin"]), BTreeSet::from([GlobalRole::Admin]));
        assert_eq!(m.map(["Rag.User"]), BTreeSet::from([GlobalRole::User]));
        assert_eq!(
            m.map(["Rag.Admin", "Rag.User", "Other"]),
            BTreeSet::from([GlobalRole::Admin, GlobalRole::User])
        );
        assert_eq!(m.default_role(), Some(GlobalRole::User));
    }

    #[test]
    fn app_roles_match_exactly() {
        let m = RoleMap::from_config(&AuthConfig::default()).unwrap();
        assert!(m.map(["rag.admin", "Rag.Admin "]).is_empty());
    }

    #[test]
    fn default_role_applies_only_when_nothing_maps() {
        let m = RoleMap::from_config(&cfg(&[("Rag.Admin", "admin")], "user")).unwrap();
        assert_eq!(m.resolve([]), BTreeSet::from([GlobalRole::User]));
        assert_eq!(m.resolve(["Unknown"]), BTreeSet::from([GlobalRole::User]));
        assert_eq!(
            m.resolve(["Rag.Admin"]),
            BTreeSet::from([GlobalRole::Admin])
        );
    }

    #[test]
    fn empty_default_role_means_no_access() {
        let m = RoleMap::from_config(&cfg(&[("Rag.Admin", "admin")], " ")).unwrap();
        assert_eq!(m.default_role(), None);
        assert!(m.resolve(["Rag.User"]).is_empty());
    }

    #[test]
    fn custom_map_with_case_insensitive_platform_roles() {
        let m = RoleMap::from_config(&cfg(&[("Readers", "USER"), ("Ops", "Admin")], "")).unwrap();
        assert_eq!(m.map(["Readers"]), BTreeSet::from([GlobalRole::User]));
        assert_eq!(m.map(["Ops"]), BTreeSet::from([GlobalRole::Admin]));
    }

    #[test]
    fn unknown_platform_roles_are_rejected() {
        let err = RoleMap::from_config(&cfg(&[("Rag.Owner", "owner")], "user")).unwrap_err();
        assert!(
            matches!(err, RoleMapError::Mapping { ref app_role, .. } if app_role == "Rag.Owner")
        );
        assert!(err.to_string().contains("Rag.Owner"));
        let err = RoleMap::from_config(&cfg(&[], "superuser")).unwrap_err();
        assert!(matches!(err, RoleMapError::Default(_)));
    }

    #[test]
    fn dev_principal_is_an_sso_admin() {
        let p = Principal::dev();
        assert_eq!(p.oid, DEV_PRINCIPAL_OID);
        assert!(p.is_admin() && p.is_sso());
        assert!(p.has_global_role(GlobalRole::User));
        assert!(p.key_scopes().is_none());
    }

    #[test]
    fn admin_implies_user_but_not_vice_versa() {
        let mut p = Principal::dev();
        p.global_roles = BTreeSet::from([GlobalRole::User]);
        assert!(!p.is_admin());
        assert!(p.has_global_role(GlobalRole::User));
        assert!(!p.has_global_role(GlobalRole::Admin));
    }

    #[test]
    fn api_key_principal_exposes_scopes() {
        let scope = Uuid::new_v4();
        let mut p = Principal::dev();
        p.auth = AuthMethod::ApiKey {
            key_id: Uuid::new_v4(),
            scopes: BTreeSet::from([scope]),
        };
        assert!(!p.is_sso());
        assert_eq!(p.key_scopes(), Some(&BTreeSet::from([scope])));
    }
}
