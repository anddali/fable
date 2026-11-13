//! Shared application state handed to every route module.

use fable_core::SecretCipher;
use sqlx::PgPool;

/// State shared by all handlers. Cheap to clone (everything inside is a
/// handle).
#[derive(Clone, Debug)]
pub struct AppState {
    /// Postgres pool: the only state of the platform.
    pub db: PgPool,
    /// Shared outbound HTTP client (connection pooling for model providers
    /// and connectors).
    pub http: reqwest::Client,
    /// Seals and opens portal-managed secrets with the master key resolved
    /// at startup (S-4, C-7).
    pub secrets: SecretCipher,
}

impl AppState {
    pub fn new(db: PgPool, http: reqwest::Client, secrets: SecretCipher) -> Self {
        Self { db, http, secrets }
    }
}
