//! Shared application state handed to every route module.

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
}

impl AppState {
    pub fn new(db: PgPool, http: reqwest::Client) -> Self {
        Self { db, http }
    }
}
