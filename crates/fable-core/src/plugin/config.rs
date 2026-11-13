//! Typed plugin configs validated at onboarding time (spec §8 P-2).

use std::fmt;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Config for plugins that take no options; rejects any key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyConfig {}

/// One field-level validation problem. `path` is a dotted JSON path
/// (`chunker.max_chars`, `targets[0].space`); empty for the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    /// Where the problem is.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

/// A config failed validation. The API maps this to a 422 with field errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ConfigValidationError {
    /// The problems found (serde stops at the first one).
    pub errors: Vec<FieldError>,
}

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for e in &self.errors {
            if !first {
                f.write_str("; ")?;
            }
            first = false;
            if e.path.is_empty() {
                write!(f, "{}", e.message)?;
            } else {
                write!(f, "{}: {}", e.path, e.message)?;
            }
        }
        Ok(())
    }
}

/// Deserialize a config into its typed struct, reporting the failing path.
/// `null` is treated as `{}` so plugins with all-default configs need none.
pub fn parse_config<C: DeserializeOwned>(raw: &Value) -> Result<C, ConfigValidationError> {
    let empty = Value::Object(Default::default());
    let raw = if raw.is_null() { &empty } else { raw };
    serde_path_to_error::deserialize::<_, C>(raw).map_err(|e| ConfigValidationError {
        errors: vec![FieldError {
            path: path_string(e.path()),
            message: e.inner().to_string(),
        }],
    })
}

/// Like [`parse_config`] but for secrets: the message never echoes the
/// offending value, only where the problem is.
pub fn parse_secret<C: DeserializeOwned>(raw: &Value) -> Result<C, ConfigValidationError> {
    let empty = Value::Object(Default::default());
    let raw = if raw.is_null() { &empty } else { raw };
    serde_path_to_error::deserialize::<_, C>(raw).map_err(|e| {
        let inner = e.inner().to_string();
        // serde's "missing field `x`" / "unknown field `x`" name keys, not
        // values, so they are safe; anything else is replaced.
        let message = if inner.starts_with("missing field") || inner.starts_with("unknown field") {
            inner
        } else {
            "invalid value".to_owned()
        };
        ConfigValidationError {
            errors: vec![FieldError {
                path: path_string(e.path()),
                message,
            }],
        }
    })
}

fn path_string(path: &serde_path_to_error::Path) -> String {
    let s = path.to_string();
    if s == "." {
        String::new()
    } else {
        s
    }
}

/// The JSON Schema of a config type, as a JSON value.
pub fn schema_of<C: JsonSchema>() -> Value {
    schemars::schema_for!(C).to_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Cfg {
        #[serde(default = "d")]
        max_chars: u32,
        inner: Option<Inner>,
    }
    fn d() -> u32 {
        1000
    }
    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Inner {
        size: u32,
    }

    #[test]
    fn null_is_empty_object() {
        let c: Cfg = parse_config(&Value::Null).unwrap();
        assert_eq!(c.max_chars, 1000);
        parse_config::<EmptyConfig>(&Value::Null).unwrap();
    }

    #[test]
    fn reports_path_of_bad_field() {
        let e = parse_config::<Cfg>(&json!({"inner": {"size": "big"}})).unwrap_err();
        assert_eq!(e.errors[0].path, "inner.size");
        assert!(e.to_string().starts_with("inner.size: invalid type"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let e = parse_config::<Cfg>(&json!({"max_char": 5})).unwrap_err();
        assert!(e.errors[0].message.contains("unknown field `max_char`"));
        assert!(parse_config::<EmptyConfig>(&json!({"x": 1})).is_err());
    }

    #[test]
    fn secret_errors_do_not_echo_values() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct S {
            token: u32,
        }
        let e = parse_secret::<S>(&json!({"token": "hunter2"})).unwrap_err();
        assert_eq!(e.errors[0].path, "token");
        assert!(!e.to_string().contains("hunter2"));
        let e = parse_secret::<S>(&json!({})).unwrap_err();
        assert!(e.to_string().contains("missing field `token`"));
    }

    #[test]
    fn schema_denies_additional_properties() {
        let s = schema_of::<Cfg>();
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(
            schema_of::<EmptyConfig>()["additionalProperties"],
            json!(false)
        );
    }
}
