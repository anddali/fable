//! Postgres pool, embedded migrations and embedding-space index helpers.

#![forbid(unsafe_code)]

use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Migrations embedded from `crates/fable-db/migrations`.
///
/// sqlx takes a Postgres advisory lock while applying them, so several
/// replicas can boot at once (spec §2 D-3).
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Database errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// Could not connect or a query failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Applying migrations failed.
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Open a connection pool.
pub async fn connect(database_url: &str) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Apply all pending embedded migrations (advisory-locked).
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}
