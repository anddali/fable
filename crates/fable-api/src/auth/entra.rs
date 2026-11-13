//! Entra ID v2 access-token validation (spec §14 A-1).
//!
//! The API is a pure OAuth2 resource server: it never issues tokens, it
//! only checks bearer tokens minted by Entra for its app registration:
//!
//! - signature: RS256 against the tenant's JWKS ([`JwksCache`], cached
//!   1 h, refetched on an unknown `kid`);
//! - `iss` = `https://login.microsoftonline.com/{tenant}/v2.0`;
//! - `aud` = the configured audience (also accepted as `api://{audience}`
//!   when it is a bare client id, since Entra uses either form depending on
//!   how the scope was requested);
//! - `exp` / `nbf` with [`LEEWAY`] of clock skew.
//!
//! The result is [`EntraClaims`]; turning it into a platform principal
//! (role mapping, default role) is the caller's job (A-2).

use std::time::Duration;

use fable_core::config::AuthConfig;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, Validation};
use serde::Deserialize;

use super::jwks::{JwksCache, JwksError};

/// Allowed clock skew for `exp` and `nbf`.
pub const LEEWAY: Duration = Duration::from_secs(60);

/// Entra's public authority host.
const AUTHORITY: &str = "https://login.microsoftonline.com";

/// Where to validate tokens against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntraSettings {
    /// Exact expected `iss`.
    pub issuer: String,
    /// Accepted `aud` values.
    pub audiences: Vec<String>,
    /// JWKS endpoint for the tenant.
    pub jwks_uri: String,
}

impl EntraSettings {
    /// Settings for the configured tenant and audience. Both must be set.
    pub fn from_config(auth: &AuthConfig) -> Result<Self, TokenError> {
        let tenant = auth.tenant_id.trim();
        let audience = auth.audience.trim();
        if tenant.is_empty() || audience.is_empty() {
            return Err(TokenError::NotConfigured);
        }
        let mut audiences = vec![audience.to_owned()];
        if !audience.starts_with("api://") {
            audiences.push(format!("api://{audience}"));
        }
        Ok(Self {
            issuer: format!("{AUTHORITY}/{tenant}/v2.0"),
            audiences,
            jwks_uri: format!("{AUTHORITY}/{tenant}/discovery/v2.0/keys"),
        })
    }
}

/// Why a bearer token was rejected. Messages never contain the token.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// `RAG_AUTH__TENANT_ID` or `RAG_AUTH__AUDIENCE` is empty.
    #[error("entra auth is not configured (tenant id and audience are required)")]
    NotConfigured,
    /// Not a structurally valid JWT.
    #[error("malformed token")]
    Malformed,
    /// Header `alg` is not RS256.
    #[error("unsupported token algorithm")]
    UnsupportedAlgorithm,
    /// Header has no `kid`.
    #[error("token has no key id")]
    MissingKid,
    /// No JWKS key matches the token's `kid`.
    #[error("token signed with an unknown key")]
    UnknownKey,
    /// Signing keys could not be loaded; not the caller's fault.
    #[error("signing keys unavailable")]
    KeysUnavailable,
    /// `exp` is in the past (beyond leeway).
    #[error("token expired")]
    Expired,
    /// `nbf` is in the future (beyond leeway).
    #[error("token not yet valid")]
    NotYetValid,
    /// Bad signature.
    #[error("invalid token signature")]
    InvalidSignature,
    /// `iss` mismatch.
    #[error("invalid token issuer")]
    InvalidIssuer,
    /// `aud` mismatch.
    #[error("invalid token audience")]
    InvalidAudience,
    /// A required claim is missing or has the wrong type.
    #[error("invalid token claims: {0}")]
    InvalidClaims(String),
}

impl TokenError {
    /// Whether the failure is on our side (keys unavailable or auth not
    /// configured) rather than a bad token: callers answer 503, not 401.
    pub fn is_server_error(&self) -> bool {
        matches!(self, Self::KeysUnavailable | Self::NotConfigured)
    }
}

impl From<jsonwebtoken::errors::Error> for TokenError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        match err.kind() {
            ErrorKind::ExpiredSignature => Self::Expired,
            ErrorKind::ImmatureSignature => Self::NotYetValid,
            ErrorKind::InvalidSignature => Self::InvalidSignature,
            ErrorKind::InvalidIssuer => Self::InvalidIssuer,
            ErrorKind::InvalidAudience => Self::InvalidAudience,
            ErrorKind::InvalidAlgorithm => Self::UnsupportedAlgorithm,
            ErrorKind::MissingRequiredClaim(c) => Self::InvalidClaims(format!("missing {c}")),
            ErrorKind::Json(e) => Self::InvalidClaims(e.to_string()),
            _ => Self::Malformed,
        }
    }
}

/// Identity claims of a validated Entra token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntraClaims {
    /// Object id of the user or service principal (`oid`, else `sub`).
    pub oid: String,
    /// Tenant id (`tid`), if present.
    pub tenant_id: Option<String>,
    /// Display name (`name`), empty if absent.
    pub name: String,
    /// Sign-in name: `preferred_username`, else `upn`, else `email`; empty
    /// if none.
    pub email: String,
    /// Group object ids (`groups`).
    pub groups: Vec<String>,
    /// Entra app roles (`roles`), unmapped.
    pub roles: Vec<String>,
}

#[derive(Deserialize)]
struct RawClaims {
    oid: Option<String>,
    sub: Option<String>,
    tid: Option<String>,
    name: Option<String>,
    preferred_username: Option<String>,
    upn: Option<String>,
    email: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    roles: Vec<String>,
}

impl TryFrom<RawClaims> for EntraClaims {
    type Error = TokenError;

    fn try_from(raw: RawClaims) -> Result<Self, TokenError> {
        let oid = raw
            .oid
            .or(raw.sub)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| TokenError::InvalidClaims("missing oid".to_owned()))?;
        let email = [raw.preferred_username, raw.upn, raw.email]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        Ok(Self {
            oid,
            tenant_id: raw.tid,
            name: raw.name.unwrap_or_default(),
            email,
            groups: raw.groups,
            roles: raw.roles,
        })
    }
}

/// Validates Entra access tokens.
pub struct EntraValidator {
    settings: EntraSettings,
    validation: Validation,
    jwks: JwksCache,
}

impl EntraValidator {
    /// A validator for the configured tenant, fetching keys with `http`.
    pub fn from_config(auth: &AuthConfig, http: reqwest::Client) -> Result<Self, TokenError> {
        let settings = EntraSettings::from_config(auth)?;
        let jwks = JwksCache::new(http, settings.jwks_uri.clone());
        Ok(Self::new(settings, jwks))
    }

    /// A validator with explicit settings and key cache.
    pub fn new(settings: EntraSettings, jwks: JwksCache) -> Self {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = LEEWAY.as_secs();
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.set_issuer(&[&settings.issuer]);
        validation.set_audience(&settings.audiences);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        Self {
            settings,
            validation,
            jwks,
        }
    }

    /// The settings in use.
    pub fn settings(&self) -> &EntraSettings {
        &self.settings
    }

    /// Validate `token` (without the `Bearer ` prefix) and return its
    /// identity claims.
    pub async fn validate(&self, token: &str) -> Result<EntraClaims, TokenError> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| TokenError::Malformed)?;
        if header.alg != Algorithm::RS256 {
            return Err(TokenError::UnsupportedAlgorithm);
        }
        let kid = header.kid.ok_or(TokenError::MissingKid)?;
        let key = self.jwks.key(&kid).await.map_err(|e| match e {
            JwksError::UnknownKid => TokenError::UnknownKey,
            JwksError::Unavailable(_) => TokenError::KeysUnavailable,
        })?;
        let data = jsonwebtoken::decode::<RawClaims>(token, &key, &self.validation)?;
        data.claims.try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(tenant: &str, audience: &str) -> AuthConfig {
        AuthConfig {
            tenant_id: tenant.to_owned(),
            audience: audience.to_owned(),
            ..AuthConfig::default()
        }
    }

    #[test]
    fn settings_derive_issuer_jwks_and_audiences() {
        let s = EntraSettings::from_config(&auth("tid-1", "client-1")).unwrap();
        assert_eq!(s.issuer, "https://login.microsoftonline.com/tid-1/v2.0");
        assert_eq!(
            s.jwks_uri,
            "https://login.microsoftonline.com/tid-1/discovery/v2.0/keys"
        );
        assert_eq!(s.audiences, ["client-1", "api://client-1"]);
    }

    #[test]
    fn app_id_uri_audience_is_not_doubled() {
        let s = EntraSettings::from_config(&auth("t", "api://rag")).unwrap();
        assert_eq!(s.audiences, ["api://rag"]);
    }

    #[test]
    fn missing_tenant_or_audience_is_not_configured() {
        assert!(matches!(
            EntraSettings::from_config(&auth("", "a")),
            Err(TokenError::NotConfigured)
        ));
        assert!(matches!(
            EntraSettings::from_config(&auth("t", "  ")),
            Err(TokenError::NotConfigured)
        ));
    }

    fn raw(v: serde_json::Value) -> RawClaims {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn claims_fall_back_to_sub_and_upn() {
        let c = EntraClaims::try_from(raw(serde_json::json!({
            "sub": "s", "upn": "u@x", "roles": ["Rag.User"]
        })))
        .unwrap();
        assert_eq!(c.oid, "s");
        assert_eq!(c.email, "u@x");
        assert_eq!(c.name, "");
        assert!(c.groups.is_empty());
        assert_eq!(c.roles, ["Rag.User"]);
    }

    #[test]
    fn claims_without_oid_or_sub_are_rejected() {
        assert!(matches!(
            EntraClaims::try_from(raw(serde_json::json!({"name": "n"}))),
            Err(TokenError::InvalidClaims(_))
        ));
    }
}
