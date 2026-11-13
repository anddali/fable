//! Bootstrap configuration from `RAG_`-prefixed environment variables
//! (spec §4 C-1, C-2, C-5–C-7).
//!
//! This is the only module in the workspace that reads the environment.
//! Only the bootstrap set below is read; every other `RAG_` variable
//! (including the legacy ones removed by C-3) is ignored here. Everything
//! else lives in the database and is edited in the portal.
//!
//! | Variable | Default |
//! |---|---|
//! | `RAG_DATABASE_URL` | — (required by anything that touches the DB) |
//! | `RAG_MASTER_KEY` | — (required unless `RAG_ENVIRONMENT=dev`) |
//! | `RAG_ENVIRONMENT` | `dev` (`dev`, `staging`, `prod`) |
//! | `RAG_LOG_LEVEL` | `info` (a `tracing` filter directive) |
//! | `RAG_ROLE` | `all` |
//! | `RAG_HTTP__BIND` | `0.0.0.0:8000` |
//! | `RAG_SCRATCH_DIR` | `/tmp/fable` |
//! | `RAG_SCRATCH_MAX_BYTES` | 10 GiB |
//! | `RAG_WORKER__SYNC_CONCURRENCY` | 2 |
//! | `RAG_WORKER__INDEX_CONCURRENCY` | 4 |
//! | `RAG_WORKER__PARSE_PROCESSES` | CPU count |
//! | `RAG_WORKER__SHUTDOWN_TIMEOUT_SECS` | 30 |
//! | `RAG_AUTH__MODE` | `entra` (`entra`, `disabled`) |
//! | `RAG_AUTH__TENANT_ID`, `__AUDIENCE`, `__SPA_CLIENT_ID`, `__API_SCOPE` | empty |
//! | `RAG_AUTH__ROLE_MAP` | `{"Rag.Admin":"admin","Rag.User":"user"}` (JSON object) |
//! | `RAG_AUTH__DEFAULT_ROLE` | `user` (empty = none) |

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;
use figment::providers::{Env, Serialized};
use figment::Figment;
use secrecy::{ExposeSecret, SecretBox, SecretString};
use serde::{Deserialize, Deserializer, Serialize};

use crate::role::Role;

/// Default HTTP bind address (`RAG_HTTP__BIND`).
pub const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8000";
/// Default scratch directory (`RAG_SCRATCH_DIR`, spec D-8).
pub const DEFAULT_SCRATCH_DIR: &str = "/tmp/fable";
/// Default scratch cap (`RAG_SCRATCH_MAX_BYTES`, spec D-8): 10 GiB.
pub const DEFAULT_SCRATCH_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Default log filter (`RAG_LOG_LEVEL`).
pub const DEFAULT_LOG_LEVEL: &str = "info";
/// Length of the decoded master key in bytes (AES-256).
pub const MASTER_KEY_LEN: usize = 32;

/// Env keys read by [`BootstrapConfig::from_env`], after stripping `RAG_`
/// and replacing `__` with `.` (matched case-insensitively).
const BOOTSTRAP_KEYS: &[&str] = &[
    "database_url",
    "master_key",
    "environment",
    "log_level",
    "role",
    "http.bind",
    "scratch_dir",
    "scratch_max_bytes",
    "worker.sync_concurrency",
    "worker.index_concurrency",
    "worker.parse_processes",
    "worker.shutdown_timeout_secs",
    "auth.mode",
    "auth.tenant_id",
    "auth.audience",
    "auth.spa_client_id",
    "auth.api_scope",
    "auth.role_map",
    "auth.default_role",
];

/// Keys whose values are taken verbatim as strings. Figment otherwise
/// parses env values (`123` → integer, `true` → bool, `[a]` → array), which
/// would break e.g. a numeric-looking tenant id or a base64 key.
const STRING_KEYS: &[&str] = &[
    "database_url",
    "master_key",
    "environment",
    "log_level",
    "role",
    "scratch_dir",
    "auth.mode",
    "auth.tenant_id",
    "auth.audience",
    "auth.spa_client_id",
    "auth.api_scope",
    "auth.role_map",
    "auth.default_role",
];

/// Deployment environment (`RAG_ENVIRONMENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// Local development: relaxed guards (auto-generated master key,
    /// `auth.mode=disabled` allowed).
    #[default]
    Dev,
    Staging,
    Prod,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Dev => "dev",
            Environment::Staging => "staging",
            Environment::Prod => "prod",
        }
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Sign-in mode (`RAG_AUTH__MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Validate Microsoft Entra ID bearer tokens.
    #[default]
    Entra,
    /// Inject a local dev principal. Never allowed in `prod` (C-6).
    Disabled,
}

/// The 32-byte master key that encrypts stored secrets (`RAG_MASTER_KEY`).
///
/// Never printed: `Debug` is redacted and there is no `Display`.
pub struct MasterKey(SecretBox<[u8; MASTER_KEY_LEN]>);

impl MasterKey {
    /// Decode a base64 (standard or URL-safe, padding optional) key that
    /// must decode to exactly 32 bytes. Errors never include the value.
    pub fn from_base64(encoded: &str) -> Result<Self, ConfigError> {
        let encoded = encoded.trim();
        if encoded.is_empty() {
            return Err(ConfigError::InvalidMasterKey("it is empty"));
        }
        let cfg =
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
        let decoded = GeneralPurpose::new(&base64::alphabet::STANDARD, cfg)
            .decode(encoded)
            .or_else(|_| GeneralPurpose::new(&base64::alphabet::URL_SAFE, cfg).decode(encoded))
            .map_err(|_| ConfigError::InvalidMasterKey("it is not valid base64"))?;
        let bytes: [u8; MASTER_KEY_LEN] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| ConfigError::InvalidMasterKey("it must decode to exactly 32 bytes"))?;
        Ok(Self(SecretBox::new(Box::new(bytes))))
    }

    /// The raw key bytes. Callers must not log or persist them.
    pub fn expose_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        self.0.expose_secret()
    }
}

impl Clone for MasterKey {
    fn clone(&self) -> Self {
        Self(SecretBox::new(Box::new(*self.0.expose_secret())))
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

/// Settings needed before a database and a signed-in admin exist.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// `RAG_DATABASE_URL`, normalised to `postgres://` (C-5). Optional here
    /// so subcommands that do not touch the database (e.g. `healthcheck`)
    /// work without it; see [`Self::require_database_url`].
    pub database_url: Option<SecretString>,
    /// `RAG_MASTER_KEY`. Always `Some` outside `dev` (C-7).
    pub master_key: Option<MasterKey>,
    /// `RAG_ENVIRONMENT`.
    pub environment: Environment,
    /// `RAG_LOG_LEVEL`: a `tracing` env-filter directive.
    pub log_level: String,
    /// `RAG_ROLE`; overridden by `--role` on the command line.
    pub role: Role,
    /// `RAG_HTTP__*`.
    pub http: HttpConfig,
    /// `RAG_SCRATCH_*`.
    pub scratch: ScratchConfig,
    /// `RAG_WORKER__*`.
    pub worker: WorkerConfig,
    /// `RAG_AUTH__*` (C-2).
    pub auth: AuthConfig,
}

/// HTTP listener settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// `RAG_HTTP__BIND`, default `0.0.0.0:8000`.
    pub bind: SocketAddr,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_HTTP_BIND
                .parse()
                .expect("DEFAULT_HTTP_BIND is a valid socket address"),
        }
    }
}

/// Ephemeral scratch space (spec D-8).
#[derive(Debug, Clone)]
pub struct ScratchConfig {
    /// `RAG_SCRATCH_DIR`, default `/tmp/fable`.
    pub dir: PathBuf,
    /// `RAG_SCRATCH_MAX_BYTES`, default 10 GiB.
    pub max_bytes: u64,
}

/// Background worker sizing.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkerConfig {
    /// `RAG_WORKER__SYNC_CONCURRENCY`, default 2.
    pub sync_concurrency: NonZeroUsize,
    /// `RAG_WORKER__INDEX_CONCURRENCY`, default 4.
    pub index_concurrency: NonZeroUsize,
    /// `RAG_WORKER__PARSE_PROCESSES`, default the CPU count.
    pub parse_processes: NonZeroUsize,
    /// `RAG_WORKER__SHUTDOWN_TIMEOUT_SECS`, default 30.
    pub shutdown_timeout_secs: u64,
}

impl WorkerConfig {
    /// Grace period for in-flight jobs on shutdown.
    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.shutdown_timeout_secs)
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            sync_concurrency: NonZeroUsize::new(2).expect("non-zero"),
            index_concurrency: NonZeroUsize::new(4).expect("non-zero"),
            parse_processes: std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
            shutdown_timeout_secs: 30,
        }
    }
}

/// Sign-in configuration (C-2). Stays in env so an admin can always sign in.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// `RAG_AUTH__MODE`, default `entra`.
    pub mode: AuthMode,
    /// `RAG_AUTH__TENANT_ID`: Entra tenant.
    pub tenant_id: String,
    /// `RAG_AUTH__AUDIENCE`: expected token audience (API app client id or
    /// Application ID URI).
    pub audience: String,
    /// `RAG_AUTH__SPA_CLIENT_ID`: the portal's SPA app registration.
    pub spa_client_id: String,
    /// `RAG_AUTH__API_SCOPE`: scope the SPA requests; empty means
    /// `<audience>/.default` (see [`Self::effective_api_scope`]).
    pub api_scope: String,
    /// `RAG_AUTH__ROLE_MAP`: Entra app role → platform global role, as a
    /// JSON object.
    #[serde(deserialize_with = "deserialize_role_map")]
    pub role_map: BTreeMap<String, String>,
    /// `RAG_AUTH__DEFAULT_ROLE`: role for users with no mapped app role;
    /// empty means none (see [`Self::default_role`]).
    pub default_role: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::default(),
            tenant_id: String::new(),
            audience: String::new(),
            spa_client_id: String::new(),
            api_scope: String::new(),
            role_map: default_role_map(),
            default_role: "user".to_owned(),
        }
    }
}

impl AuthConfig {
    /// The role granted when no app role maps, if any.
    pub fn default_role(&self) -> Option<&str> {
        Some(self.default_role.trim()).filter(|r| !r.is_empty())
    }

    /// The scope the SPA requests: `api_scope`, else `<audience>/.default`,
    /// else empty.
    pub fn effective_api_scope(&self) -> String {
        if !self.api_scope.trim().is_empty() {
            self.api_scope.trim().to_owned()
        } else if !self.audience.trim().is_empty() {
            format!("{}/.default", self.audience.trim())
        } else {
            String::new()
        }
    }
}

fn default_role_map() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("Rag.Admin".to_owned(), "admin".to_owned()),
        ("Rag.User".to_owned(), "user".to_owned()),
    ])
}

/// Accept the role map as a JSON object string (the env form, compatible
/// with the Python settings) or as an already-structured map.
fn deserialize_role_map<'de, D>(d: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Map(BTreeMap<String, String>),
        Json(String),
    }
    match Raw::deserialize(d)? {
        Raw::Map(m) => Ok(m),
        Raw::Json(s) if s.trim().is_empty() => Ok(BTreeMap::new()),
        Raw::Json(s) => serde_json::from_str(&s).map_err(|e| {
            serde::de::Error::custom(format!(
                "expected a JSON object of app role to platform role, e.g. \
                 {{\"Rag.Admin\":\"admin\"}}: {e}"
            ))
        }),
    }
}

/// The shape read from the environment, before validation.
#[derive(Deserialize)]
#[serde(default)]
struct RawConfig {
    database_url: Option<String>,
    master_key: Option<String>,
    environment: Environment,
    log_level: String,
    role: Role,
    http: HttpConfig,
    scratch_dir: PathBuf,
    scratch_max_bytes: u64,
    worker: WorkerConfig,
    auth: AuthConfig,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            database_url: None,
            master_key: None,
            environment: Environment::default(),
            log_level: DEFAULT_LOG_LEVEL.to_owned(),
            role: Role::default(),
            http: HttpConfig::default(),
            scratch_dir: PathBuf::from(DEFAULT_SCRATCH_DIR),
            scratch_max_bytes: DEFAULT_SCRATCH_MAX_BYTES,
            worker: WorkerConfig::default(),
            auth: AuthConfig::default(),
        }
    }
}

/// Invalid or missing bootstrap configuration. Messages name the variable
/// and never include secret values.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A variable could not be parsed.
    #[error("invalid bootstrap configuration: {0}")]
    Invalid(String),
    /// A variable required by the requested operation is not set.
    #[error("{0} is not set")]
    Missing(&'static str),
    /// `RAG_MASTER_KEY` is set but unusable.
    #[error("RAG_MASTER_KEY is invalid: {0} (expected 32 random bytes, base64-encoded, e.g. `openssl rand -base64 32`)")]
    InvalidMasterKey(&'static str),
    /// `RAG_MASTER_KEY` is missing outside `dev` (C-7).
    #[error("RAG_MASTER_KEY is required when RAG_ENVIRONMENT={0} (only `dev` may run without it); generate one with `openssl rand -base64 32`")]
    MasterKeyRequired(Environment),
    /// `RAG_AUTH__MODE=disabled` in `prod` (C-6).
    #[error("RAG_AUTH__MODE=disabled is not permitted when RAG_ENVIRONMENT=prod")]
    AuthDisabledInProd,
}

impl BootstrapConfig {
    /// Load from the process environment (`RAG_` prefix, `__` nesting) and
    /// apply the startup guards (C-5–C-7).
    pub fn from_env() -> Result<Self, ConfigError> {
        let env = || Env::prefixed("RAG_").split("__");
        let typed: Vec<&str> = BOOTSTRAP_KEYS
            .iter()
            .copied()
            .filter(|k| !STRING_KEYS.contains(k))
            .collect();
        let mut figment = Figment::new().merge(env().only(&typed));
        for (key, value) in env().only(STRING_KEYS).iter() {
            figment = figment.merge(Serialized::default(key.as_str(), value));
        }
        let raw: RawConfig = figment
            .extract()
            .map_err(|e| ConfigError::Invalid(e.to_string()))?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let database_url = raw
            .database_url
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
            .map(|u| SecretString::from(normalize_database_url(&u)));

        let master_key = raw
            .master_key
            .filter(|k| !k.trim().is_empty())
            .map(|k| MasterKey::from_base64(&k))
            .transpose()?;
        if master_key.is_none() && raw.environment != Environment::Dev {
            return Err(ConfigError::MasterKeyRequired(raw.environment));
        }

        if raw.auth.mode == AuthMode::Disabled && raw.environment == Environment::Prod {
            return Err(ConfigError::AuthDisabledInProd);
        }

        let log_level = match raw.log_level.trim() {
            "" => DEFAULT_LOG_LEVEL.to_owned(),
            l => l.to_owned(),
        };
        if raw.scratch_dir.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("RAG_SCRATCH_DIR is empty".into()));
        }
        if raw.scratch_max_bytes == 0 {
            return Err(ConfigError::Invalid(
                "RAG_SCRATCH_MAX_BYTES must be greater than 0".into(),
            ));
        }

        Ok(Self {
            database_url,
            master_key,
            environment: raw.environment,
            log_level,
            role: raw.role,
            http: raw.http,
            scratch: ScratchConfig {
                dir: raw.scratch_dir,
                max_bytes: raw.scratch_max_bytes,
            },
            worker: raw.worker,
            auth: raw.auth,
        })
    }

    /// The database URL, or an error naming the missing variable. The
    /// returned string contains credentials: never log it.
    pub fn require_database_url(&self) -> Result<&str, ConfigError> {
        self.database_url
            .as_ref()
            .map(|u| u.expose_secret())
            .ok_or(ConfigError::Missing("RAG_DATABASE_URL"))
    }
}

/// Rewrite SQLAlchemy-style URLs (`postgresql+psycopg://…`, or any
/// `postgresql+<driver>://`) to `postgres://…` (C-5). Other URLs are
/// returned unchanged.
pub fn normalize_database_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let scheme_lc = scheme.to_ascii_lowercase();
        if let Some(driver) = scheme_lc.strip_prefix("postgresql+") {
            if !driver.is_empty() {
                return format!("postgres://{rest}");
            }
        }
    }
    url.to_owned()
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Jail closures return figment::Error
mod tests {
    use super::*;

    /// base64 of the bytes 0, 1, …, 31.
    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    fn key_bytes() -> [u8; 32] {
        std::array::from_fn(|i| i as u8)
    }

    #[test]
    fn defaults_without_env() {
        figment::Jail::expect_with(|_jail| {
            let cfg = BootstrapConfig::from_env().unwrap();
            assert_eq!(cfg.environment, Environment::Dev);
            assert_eq!(cfg.role, Role::All);
            assert_eq!(cfg.log_level, "info");
            assert_eq!(cfg.http.bind.to_string(), DEFAULT_HTTP_BIND);
            assert!(cfg.database_url.is_none());
            assert!(cfg.master_key.is_none());
            assert!(matches!(
                cfg.require_database_url(),
                Err(ConfigError::Missing("RAG_DATABASE_URL"))
            ));
            assert_eq!(cfg.scratch.dir, PathBuf::from("/tmp/fable"));
            assert_eq!(cfg.scratch.max_bytes, 10 * 1024 * 1024 * 1024);
            assert_eq!(cfg.worker.sync_concurrency.get(), 2);
            assert_eq!(cfg.worker.index_concurrency.get(), 4);
            assert_eq!(
                cfg.worker.parse_processes,
                std::thread::available_parallelism().unwrap()
            );
            assert_eq!(cfg.worker.shutdown_timeout(), Duration::from_secs(30));
            assert_eq!(cfg.auth.mode, AuthMode::Entra);
            assert_eq!(cfg.auth.role_map, default_role_map());
            assert_eq!(cfg.auth.default_role(), Some("user"));
            assert_eq!(cfg.auth.effective_api_scope(), "");
            Ok(())
        });
    }

    #[test]
    fn reads_full_bootstrap_set() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RAG_ENVIRONMENT", "staging");
            jail.set_env("RAG_MASTER_KEY", KEY_B64);
            jail.set_env("RAG_LOG_LEVEL", "debug,sqlx=warn");
            jail.set_env("RAG_ROLE", "worker");
            jail.set_env("RAG_HTTP__BIND", "127.0.0.1:9000");
            jail.set_env("RAG_DATABASE_URL", "postgres://u:p@db/rag");
            jail.set_env("RAG_SCRATCH_DIR", "/var/scratch");
            jail.set_env("RAG_SCRATCH_MAX_BYTES", "1048576");
            jail.set_env("RAG_WORKER__SYNC_CONCURRENCY", "3");
            jail.set_env("RAG_WORKER__INDEX_CONCURRENCY", "8");
            jail.set_env("RAG_WORKER__PARSE_PROCESSES", "5");
            jail.set_env("RAG_WORKER__SHUTDOWN_TIMEOUT_SECS", "45");
            jail.set_env("RAG_AUTH__MODE", "disabled");
            // Numeric-looking and GUID values must stay strings.
            jail.set_env("RAG_AUTH__TENANT_ID", "12345");
            jail.set_env(
                "RAG_AUTH__AUDIENCE",
                "api://00000000-0000-0000-0000-000000000001",
            );
            jail.set_env("RAG_AUTH__SPA_CLIENT_ID", "1e3");
            jail.set_env(
                "RAG_AUTH__ROLE_MAP",
                r#"{"Rag.Admin":"admin","Rag.Reader":"user"}"#,
            );
            jail.set_env("RAG_AUTH__DEFAULT_ROLE", "");
            let cfg = BootstrapConfig::from_env().unwrap();
            assert_eq!(cfg.environment, Environment::Staging);
            assert_eq!(cfg.master_key.unwrap().expose_bytes(), &key_bytes());
            assert_eq!(cfg.log_level, "debug,sqlx=warn");
            assert_eq!(cfg.role, Role::Worker);
            assert_eq!(cfg.http.bind.port(), 9000);
            assert_eq!(cfg.scratch.dir, PathBuf::from("/var/scratch"));
            assert_eq!(cfg.scratch.max_bytes, 1_048_576);
            assert_eq!(cfg.worker.sync_concurrency.get(), 3);
            assert_eq!(cfg.worker.index_concurrency.get(), 8);
            assert_eq!(cfg.worker.parse_processes.get(), 5);
            assert_eq!(cfg.worker.shutdown_timeout_secs, 45);
            assert_eq!(cfg.auth.mode, AuthMode::Disabled);
            assert_eq!(cfg.auth.tenant_id, "12345");
            assert_eq!(cfg.auth.spa_client_id, "1e3");
            assert_eq!(
                cfg.auth.effective_api_scope(),
                "api://00000000-0000-0000-0000-000000000001/.default"
            );
            assert_eq!(cfg.auth.role_map.len(), 2);
            assert_eq!(cfg.auth.role_map["Rag.Reader"], "user");
            assert_eq!(cfg.auth.default_role(), None);
            Ok(())
        });
    }

    #[test]
    fn ignores_non_bootstrap_vars() {
        figment::Jail::expect_with(|jail| {
            // Removed by C-3 / unknown: must not affect or break loading.
            jail.set_env("RAG_EMBEDDING__MODEL", "text-embedding-3-small");
            jail.set_env("RAG_BLOB_DIR", "var/blobs");
            jail.set_env("RAG_MAX_UPLOAD_BYTES", "not-a-number");
            jail.set_env("RAG_HTTP__OTHER", "x");
            assert!(BootstrapConfig::from_env().is_ok());
            Ok(())
        });
    }

    #[test]
    fn normalises_legacy_database_url() {
        figment::Jail::expect_with(|jail| {
            jail.set_env(
                "RAG_DATABASE_URL",
                "postgresql+psycopg://rag:rag@db:5432/rag",
            );
            let cfg = BootstrapConfig::from_env().unwrap();
            assert_eq!(
                cfg.require_database_url().unwrap(),
                "postgres://rag:rag@db:5432/rag"
            );
            Ok(())
        });
        assert_eq!(
            normalize_database_url("postgresql+asyncpg://h/db"),
            "postgres://h/db"
        );
        assert_eq!(
            normalize_database_url("postgresql://h/db"),
            "postgresql://h/db"
        );
        assert_eq!(normalize_database_url("postgres://h/db"), "postgres://h/db");
    }

    #[test]
    fn database_url_is_redacted_in_debug() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RAG_DATABASE_URL", "postgres://u:hunter2@db/rag");
            jail.set_env("RAG_MASTER_KEY", KEY_B64);
            let cfg = BootstrapConfig::from_env().unwrap();
            let dbg = format!("{cfg:?}");
            assert!(!dbg.contains("hunter2"), "{dbg}");
            assert!(!dbg.contains(KEY_B64), "{dbg}");
            Ok(())
        });
    }

    #[test]
    fn auth_disabled_in_prod_fails() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RAG_ENVIRONMENT", "prod");
            jail.set_env("RAG_MASTER_KEY", KEY_B64);
            jail.set_env("RAG_AUTH__MODE", "disabled");
            let err = BootstrapConfig::from_env().unwrap_err();
            assert!(matches!(err, ConfigError::AuthDisabledInProd));
            assert!(err.to_string().contains("RAG_AUTH__MODE=disabled"));

            jail.set_env("RAG_AUTH__MODE", "entra");
            assert!(BootstrapConfig::from_env().is_ok());
            Ok(())
        });
    }

    #[test]
    fn auth_disabled_allowed_outside_prod() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RAG_AUTH__MODE", "disabled");
            assert!(BootstrapConfig::from_env().is_ok());
            jail.set_env("RAG_ENVIRONMENT", "staging");
            jail.set_env("RAG_MASTER_KEY", KEY_B64);
            assert!(BootstrapConfig::from_env().is_ok());
            Ok(())
        });
    }

    #[test]
    fn master_key_required_outside_dev() {
        for env in ["staging", "prod"] {
            figment::Jail::expect_with(|jail| {
                jail.set_env("RAG_ENVIRONMENT", env);
                let err = BootstrapConfig::from_env().unwrap_err();
                assert!(matches!(err, ConfigError::MasterKeyRequired(_)));
                assert!(err.to_string().contains("RAG_MASTER_KEY is required"));
                assert!(err.to_string().contains(env));

                jail.set_env("RAG_MASTER_KEY", "   ");
                assert!(matches!(
                    BootstrapConfig::from_env(),
                    Err(ConfigError::MasterKeyRequired(_))
                ));
                Ok(())
            });
        }
    }

    #[test]
    fn invalid_master_key_fails_in_every_environment() {
        for bad in ["not base64!!", "AAECAwQ=", "c2hvcnQ"] {
            figment::Jail::expect_with(|jail| {
                jail.set_env("RAG_MASTER_KEY", bad);
                let err = BootstrapConfig::from_env().unwrap_err();
                assert!(matches!(err, ConfigError::InvalidMasterKey(_)), "{err}");
                assert!(!err.to_string().contains(bad));
                Ok(())
            });
        }
    }

    #[test]
    fn master_key_accepts_url_safe_and_unpadded() {
        let bytes: [u8; 32] = [0xfb; 32];
        use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
        for enc in [STANDARD.encode(bytes), URL_SAFE_NO_PAD.encode(bytes)] {
            let key = MasterKey::from_base64(&enc).unwrap();
            assert_eq!(key.expose_bytes(), &bytes);
            assert_eq!(format!("{key:?}"), "MasterKey([REDACTED])");
        }
    }

    #[test]
    fn rejects_invalid_values() {
        for (var, value) in [
            ("RAG_ROLE", "both"),
            ("RAG_ENVIRONMENT", "production"),
            ("RAG_AUTH__MODE", "none"),
            ("RAG_HTTP__BIND", "nope"),
            ("RAG_WORKER__INDEX_CONCURRENCY", "0"),
            ("RAG_WORKER__PARSE_PROCESSES", "-1"),
            ("RAG_SCRATCH_MAX_BYTES", "0"),
            ("RAG_SCRATCH_MAX_BYTES", "10GB"),
            ("RAG_AUTH__ROLE_MAP", "Rag.Admin=admin"),
        ] {
            figment::Jail::expect_with(|jail| {
                jail.set_env(var, value);
                assert!(
                    matches!(BootstrapConfig::from_env(), Err(ConfigError::Invalid(_))),
                    "{var}={value} should be rejected"
                );
                Ok(())
            });
        }
    }
}
