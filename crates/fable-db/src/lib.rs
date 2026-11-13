//! Postgres pool, embedded migrations and embedding-space index helpers.

#![forbid(unsafe_code)]

use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnection, PgPool, PgPoolOptions};
use sqlx::Connection;

/// Migrations embedded from `crates/fable-db/migrations` (spec §7 M-7).
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// First key of the session-level advisory lock held while migrations run
/// (spec §2 D-3). The second key is [`MIGRATION_LOCK_TARGET_SQL`], so the
/// lock covers one migration target (database + schema), not the whole
/// cluster. In `pg_locks` it shows as `locktype = 'advisory'`,
/// `classid = 0x0FAB1E00` (`objsubid = 2`).
pub const MIGRATION_LOCK_CLASS: i32 = 0x0FAB_1E00;

/// SQL expression for the second advisory-lock key: a hash of the current
/// database and schema, where the migrations (and `_sqlx_migrations`) live.
pub const MIGRATION_LOCK_TARGET_SQL: &str =
    "hashtext(current_database() || '.' || coalesce(current_schema(), ''))";

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

/// What a migration run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Versions applied by this run, in order. Empty when the schema was
    /// already up to date (e.g. another replica migrated first).
    pub applied: Vec<i64>,
    /// Whether another session held the lock when this run started.
    pub waited_for_lock: bool,
}

/// Apply all pending embedded migrations under the migration advisory lock.
///
/// Safe to call from any number of replicas at once: one applies the
/// migrations, the others wait for the lock and then find nothing to do.
pub async fn migrate(pool: &PgPool) -> Result<MigrationReport, DbError> {
    migrate_with(pool, &MIGRATOR).await
}

/// [`migrate`] with an explicit migrator (used by tests).
///
/// The lock is taken on a connection detached from the pool and closed
/// afterwards, so it is released even when a migration fails: a session lock
/// is never handed back to the pool or leaked to another task.
pub async fn migrate_with(pool: &PgPool, migrator: &Migrator) -> Result<MigrationReport, DbError> {
    let mut conn = pool.acquire().await?.detach();
    let result = migrate_locked(&mut conn, migrator).await;
    // Closing the session releases the advisory lock whatever happened.
    if let Err(err) = conn.close().await {
        tracing::debug!(error = %err, "closing migration connection");
    }
    result
}

async fn migrate_locked(
    conn: &mut PgConnection,
    migrator: &Migrator,
) -> Result<MigrationReport, DbError> {
    let acquired: bool = sqlx::query_scalar(&format!(
        "SELECT pg_try_advisory_lock($1, {MIGRATION_LOCK_TARGET_SQL})"
    ))
    .bind(MIGRATION_LOCK_CLASS)
    .fetch_one(&mut *conn)
    .await?;
    let waited_for_lock = !acquired;
    if waited_for_lock {
        tracing::info!("another instance is running migrations; waiting for the lock");
        sqlx::query(&format!(
            "SELECT pg_advisory_lock($1, {MIGRATION_LOCK_TARGET_SQL})"
        ))
        .bind(MIGRATION_LOCK_CLASS)
        .execute(&mut *conn)
        .await?;
    }

    let before = applied_versions(conn).await?;
    // `run_direct` rather than `run(&mut conn)`: the latter's higher-ranked
    // `Acquire` bound makes this future non-`Send` (sqlx documents
    // `run_direct` as the workaround).
    migrator.run_direct(conn).await?;
    let after = applied_versions(conn).await?;
    let applied: Vec<i64> = after.into_iter().filter(|v| !before.contains(v)).collect();
    for version in &applied {
        let description = migrator
            .iter()
            .find(|m| m.version == *version)
            .map(|m| m.description.as_ref())
            .unwrap_or_default();
        tracing::info!(version, description, "applied migration");
    }

    sqlx::query(&format!(
        "SELECT pg_advisory_unlock($1, {MIGRATION_LOCK_TARGET_SQL})"
    ))
    .bind(MIGRATION_LOCK_CLASS)
    .execute(&mut *conn)
    .await?;
    Ok(MigrationReport {
        applied,
        waited_for_lock,
    })
}

/// Successfully applied versions, or none when sqlx's bookkeeping table does
/// not exist yet.
async fn applied_versions(conn: &mut PgConnection) -> Result<Vec<i64>, DbError> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&mut *conn)
        .await?;
    if !exists {
        return Ok(Vec::new());
    }
    let versions =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&mut *conn)
            .await?;
    Ok(versions)
}
