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
    /// An embedding-space index helper got invalid arguments.
    #[error(transparent)]
    SpaceIndex(SpaceIndexError),
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

// ---------------------------------------------------------------------------
// Embedding-space indexes (spec §7 M-3)
// ---------------------------------------------------------------------------

/// Largest dimension pgvector can HNSW-index for the `vector` type. Matches
/// the `embedding_spaces.dim` CHECK.
pub const MAX_SPACE_DIM: i32 = 2000;

/// Invalid arguments to an embedding-space index helper.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpaceIndexError {
    /// Space ids come from an identity column and are always positive.
    #[error("invalid embedding space id {0}")]
    InvalidSpaceId(i64),
    /// Outside `1..=MAX_SPACE_DIM`.
    #[error("embedding dimension {0} is outside 1..={MAX_SPACE_DIM}")]
    InvalidDim(i32),
}

impl From<SpaceIndexError> for DbError {
    fn from(err: SpaceIndexError) -> Self {
        DbError::SpaceIndex(err)
    }
}

/// Name of the partial HNSW index of one embedding space.
pub fn space_index_name(space_id: i64) -> String {
    format!("ix_chunk_embeddings_hnsw_space_{space_id}")
}

/// The indexed expression of a space's HNSW index. Nearest-neighbour queries
/// must order by `<expr> <=> $1::vector(<dim>)` with exactly this text for
/// the planner to use the index.
pub fn space_vector_expr(dim: i32) -> String {
    format!("(embedding::vector({dim}))")
}

/// The partial-index predicate of a space, with the id inlined as a literal.
///
/// Use this rather than `space_id = $n`: with a bind parameter Postgres may
/// switch to a generic plan that cannot prove the partial predicate, and
/// silently fall back to a sequential scan.
pub fn space_predicate(space_id: i64) -> String {
    format!("space_id = {space_id}")
}

fn check_space(space_id: i64, dim: Option<i32>) -> Result<(), SpaceIndexError> {
    if space_id <= 0 {
        return Err(SpaceIndexError::InvalidSpaceId(space_id));
    }
    if let Some(dim) = dim {
        if !(1..=MAX_SPACE_DIM).contains(&dim) {
            return Err(SpaceIndexError::InvalidDim(dim));
        }
    }
    Ok(())
}

fn create_space_index_sql(space_id: i64, dim: i32, concurrently: bool) -> String {
    format!(
        "CREATE INDEX {}IF NOT EXISTS {} ON chunk_embeddings \
         USING hnsw ({} vector_cosine_ops) WHERE {}",
        if concurrently { "CONCURRENTLY " } else { "" },
        space_index_name(space_id),
        space_vector_expr(dim),
        space_predicate(space_id),
    )
}

fn drop_space_index_sql(space_id: i64, concurrently: bool) -> String {
    format!(
        "DROP INDEX {}IF EXISTS {}",
        if concurrently { "CONCURRENTLY " } else { "" },
        space_index_name(space_id),
    )
}

/// Create the partial expression HNSW index of an embedding space:
/// `USING hnsw ((embedding::vector(dim)) vector_cosine_ops) WHERE space_id = id`.
///
/// Runs in the caller's transaction (pass `&mut *tx`), so it can be committed
/// together with the `embedding_spaces` row; it blocks writes to
/// `chunk_embeddings` while it builds. Idempotent. For a space that already
/// holds many vectors outside a transaction, prefer
/// [`create_space_index_concurrently`].
pub async fn create_space_index<'e, E>(executor: E, space_id: i64, dim: i32) -> Result<(), DbError>
where
    E: sqlx::PgExecutor<'e>,
{
    check_space(space_id, Some(dim))?;
    sqlx::query(&create_space_index_sql(space_id, dim, false))
        .execute(executor)
        .await?;
    Ok(())
}

/// [`create_space_index`] with `CREATE INDEX CONCURRENTLY`: does not block
/// writers, but cannot run inside a transaction. An invalid index left by an
/// earlier interrupted concurrent build is dropped and rebuilt.
pub async fn create_space_index_concurrently(
    conn: &mut PgConnection,
    space_id: i64,
    dim: i32,
) -> Result<(), DbError> {
    check_space(space_id, Some(dim))?;
    let invalid: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_index i \
         WHERE i.indexrelid = to_regclass($1) AND NOT i.indisvalid)",
    )
    .bind(space_index_name(space_id))
    .fetch_one(&mut *conn)
    .await?;
    if invalid {
        tracing::warn!(space_id, "dropping invalid embedding-space index");
        sqlx::query(&drop_space_index_sql(space_id, true))
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query(&create_space_index_sql(space_id, dim, true))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Drop the HNSW index of an embedding space (e.g. before deleting a retired
/// space). Runs in the caller's transaction. Idempotent.
pub async fn drop_space_index<'e, E>(executor: E, space_id: i64) -> Result<(), DbError>
where
    E: sqlx::PgExecutor<'e>,
{
    check_space(space_id, None)?;
    sqlx::query(&drop_space_index_sql(space_id, false))
        .execute(executor)
        .await?;
    Ok(())
}

/// [`drop_space_index`] with `DROP INDEX CONCURRENTLY` (outside a transaction).
pub async fn drop_space_index_concurrently(
    conn: &mut PgConnection,
    space_id: i64,
) -> Result<(), DbError> {
    check_space(space_id, None)?;
    sqlx::query(&drop_space_index_sql(space_id, true))
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod space_index_tests {
    use super::*;

    #[test]
    fn renders_partial_expression_index() {
        assert_eq!(
            create_space_index_sql(7, 1536, false),
            "CREATE INDEX IF NOT EXISTS ix_chunk_embeddings_hnsw_space_7 ON chunk_embeddings \
             USING hnsw ((embedding::vector(1536)) vector_cosine_ops) WHERE space_id = 7"
        );
        assert_eq!(
            drop_space_index_sql(7, true),
            "DROP INDEX CONCURRENTLY IF EXISTS ix_chunk_embeddings_hnsw_space_7"
        );
    }

    #[test]
    fn validates_arguments() {
        assert_eq!(check_space(1, Some(1)), Ok(()));
        assert_eq!(check_space(1, Some(MAX_SPACE_DIM)), Ok(()));
        assert_eq!(
            check_space(0, Some(3)),
            Err(SpaceIndexError::InvalidSpaceId(0))
        );
        assert_eq!(
            check_space(-4, None),
            Err(SpaceIndexError::InvalidSpaceId(-4))
        );
        assert_eq!(check_space(1, Some(0)), Err(SpaceIndexError::InvalidDim(0)));
        assert_eq!(
            check_space(1, Some(MAX_SPACE_DIM + 1)),
            Err(SpaceIndexError::InvalidDim(MAX_SPACE_DIM + 1))
        );
    }
}
