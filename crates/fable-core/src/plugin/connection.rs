//! Resolved connections handed to plugins, and connection-kind definitions
//! (spec §5.1, §5.2 S-2, §8 P-5, P-8).

use std::fmt;

use schemars::JsonSchema;
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::plugin::config::{parse_config, parse_secret, schema_of, ConfigValidationError};

/// A connection as the platform hands it to a plugin: non-secret config
/// plus the decrypted secret, which exists only in memory (S-4).
///
/// `Debug` never prints the secret.
#[derive(Clone)]
pub struct ResolvedConnection {
    /// Connection id (stringified UUID).
    pub id: String,
    /// Connection kind, e.g. `confluence`, `openai`.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Non-secret config (`connections.config`), shaped by the kind's
    /// config schema.
    pub config: Value,
    /// Decrypted secret payload: the JSON text of the kind's secret object.
    /// `None` when the kind has no secret or none is set.
    pub secret: Option<SecretString>,
    /// Whether the connection is enabled.
    pub enabled: bool,
    /// Result of the last connection test; `None` if never tested.
    pub last_test_ok: Option<bool>,
}

impl ResolvedConnection {
    /// P-5: usable when enabled and the last test passed or never ran.
    pub fn is_ready(&self) -> bool {
        self.enabled && self.last_test_ok != Some(false)
    }
}

impl fmt::Debug for ResolvedConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedConnection")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("config", &self.config)
            .field("secret_set", &self.secret.is_some())
            .field("enabled", &self.enabled)
            .field("last_test_ok", &self.last_test_ok)
            .finish()
    }
}

/// Defines a connection kind (S-2): its typed config and secret, whose JSON
/// Schemas drive the portal form. Both types should use
/// `#[serde(deny_unknown_fields)]`. Secret fields are usually `String` in
/// the schema type; the platform keeps the payload as a `SecretString`.
pub trait ConnectionKindDef: 'static {
    /// Stable kind identifier (`confluence`, `openai`, …).
    const KIND: &'static str;
    /// Human-readable label for the portal.
    const LABEL: &'static str;
    /// Non-secret config.
    type Config: DeserializeOwned + JsonSchema;
    /// Secret fields.
    type Secret: DeserializeOwned + JsonSchema;
}

/// A registered connection kind. Create with [`ConnectionKindRegistration::of`]
/// and submit with [`register_connection_kind!`](crate::register_connection_kind).
pub struct ConnectionKindRegistration {
    /// Stable kind identifier.
    pub kind: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// JSON Schema of the config.
    pub config_schema: fn() -> Value,
    /// JSON Schema of the secret.
    pub secret_schema: fn() -> Value,
    /// Validate a config value.
    pub validate_config: fn(&Value) -> Result<(), ConfigValidationError>,
    /// Validate a secret value; messages never echo values.
    pub validate_secret: fn(&Value) -> Result<(), ConfigValidationError>,
}

impl ConnectionKindRegistration {
    /// Registration for `K`.
    pub const fn of<K: ConnectionKindDef>() -> Self {
        Self {
            kind: K::KIND,
            label: K::LABEL,
            config_schema: schema_of::<K::Config>,
            secret_schema: schema_of::<K::Secret>,
            validate_config: validate::<K::Config>,
            validate_secret: validate_secret::<K::Secret>,
        }
    }
}

fn validate<C: DeserializeOwned>(v: &Value) -> Result<(), ConfigValidationError> {
    parse_config::<C>(v).map(drop)
}

fn validate_secret<C: DeserializeOwned>(v: &Value) -> Result<(), ConfigValidationError> {
    parse_secret::<C>(v).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn debug_hides_secret() {
        let c = ResolvedConnection {
            id: "1".into(),
            kind: "git".into(),
            name: "GitHub".into(),
            config: json!({"host": "github.com"}),
            secret: Some(SecretString::from("ghp_supersecret")),
            enabled: true,
            last_test_ok: None,
        };
        let s = format!("{c:?}");
        assert!(!s.contains("supersecret"));
        assert!(s.contains("secret_set: true"));
    }

    #[test]
    fn readiness() {
        let mut c = ResolvedConnection {
            id: "1".into(),
            kind: "git".into(),
            name: "x".into(),
            config: Value::Null,
            secret: None,
            enabled: true,
            last_test_ok: None,
        };
        assert!(c.is_ready());
        c.last_test_ok = Some(true);
        assert!(c.is_ready());
        c.last_test_ok = Some(false);
        assert!(!c.is_ready());
        c.last_test_ok = None;
        c.enabled = false;
        assert!(!c.is_ready());
    }
}
