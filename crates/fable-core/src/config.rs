//! Bootstrap configuration from `RAG_`-prefixed environment variables
//! (spec §4).
//!
//! This is the only module in the workspace that reads the environment.
//! It currently holds the minimum needed by the `fable` binary's roles and
//! subcommands; the full C-1/C-2 set, legacy URL normalisation and the prod
//! guards are added by the bootstrap-config ticket.

use std::net::SocketAddr;

use figment::providers::Env;
use figment::Figment;
use serde::Deserialize;

use crate::role::Role;

/// Default HTTP bind address (`RAG_HTTP__BIND`).
pub const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8000";

/// Settings needed before a database and a signed-in admin exist.
#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapConfig {
    /// `RAG_DATABASE_URL`. Optional here so subcommands that do not touch
    /// the database (e.g. `healthcheck`) work without it.
    #[serde(default)]
    pub database_url: Option<String>,
    /// `RAG_ROLE`; overridden by `--role` on the command line.
    #[serde(default)]
    pub role: Role,
    /// `RAG_HTTP__*`.
    #[serde(default)]
    pub http: HttpConfig,
}

/// HTTP listener settings.
#[derive(Debug, Clone, Deserialize)]
pub struct HttpConfig {
    /// `RAG_HTTP__BIND`, default `0.0.0.0:8000`.
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

fn default_bind() -> SocketAddr {
    DEFAULT_HTTP_BIND
        .parse()
        .expect("DEFAULT_HTTP_BIND is a valid socket address")
}

/// Invalid or missing bootstrap configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A variable could not be parsed.
    #[error("invalid bootstrap configuration: {0}")]
    Invalid(String),
    /// A variable required by the requested operation is not set.
    #[error("{0} is not set")]
    Missing(&'static str),
}

impl BootstrapConfig {
    /// Load from the process environment (`RAG_` prefix, `__` nesting).
    /// Only the keys modelled above are read; other `RAG_` vars are ignored.
    pub fn from_env() -> Result<Self, ConfigError> {
        Figment::new()
            .merge(Env::prefixed("RAG_").split("__"))
            .extract()
            .map_err(|e| ConfigError::Invalid(e.to_string()))
    }

    /// The database URL, or an error naming the missing variable.
    pub fn require_database_url(&self) -> Result<&str, ConfigError> {
        self.database_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or(ConfigError::Missing("RAG_DATABASE_URL"))
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Jail closures return figment::Error
mod tests {
    use super::*;

    #[test]
    fn defaults_without_env() {
        figment::Jail::expect_with(|_jail| {
            let cfg = BootstrapConfig::from_env().unwrap();
            assert_eq!(cfg.role, Role::All);
            assert_eq!(cfg.http.bind.to_string(), DEFAULT_HTTP_BIND);
            assert!(cfg.database_url.is_none());
            assert!(matches!(
                cfg.require_database_url(),
                Err(ConfigError::Missing("RAG_DATABASE_URL"))
            ));
            Ok(())
        });
    }

    #[test]
    fn reads_prefixed_nested_vars() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RAG_ROLE", "worker");
            jail.set_env("RAG_HTTP__BIND", "127.0.0.1:9000");
            jail.set_env("RAG_DATABASE_URL", "postgres://u:p@db/rag");
            let cfg = BootstrapConfig::from_env().unwrap();
            assert_eq!(cfg.role, Role::Worker);
            assert_eq!(cfg.http.bind.port(), 9000);
            assert_eq!(cfg.require_database_url().unwrap(), "postgres://u:p@db/rag");
            Ok(())
        });
    }

    #[test]
    fn rejects_bad_role() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RAG_ROLE", "both");
            assert!(matches!(
                BootstrapConfig::from_env(),
                Err(ConfigError::Invalid(_))
            ));
            Ok(())
        });
    }
}
