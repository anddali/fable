//! Schema tests for embedding spaces (FAB-31, spec §7 M-3, S-8, S-13):
//! `embedding_spaces` + `chunk_embeddings` (untyped vector), the per-space
//! partial expression HNSW index helpers and the exactly-one-active rule.
//!
//! Each test creates and drops its own database. Without `DATABASE_URL` the
//! tests are skipped (CI always sets it).

use std::sync::atomic::{AtomicU32, Ordering};

use fable_db::{
    create_space_index, create_space_index_concurrently, drop_space_index,
    drop_space_index_concurrently, space_index_name, space_predicate, space_vector_expr, DbError,
    SpaceIndexError,
};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{ConnectOptions, Connection};
/// A migrated throwaway database.
struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn create() -> Option<Self> {
        let Ok(base) = std::env::var("DATABASE_URL") else {
            eprintln!("DATABASE_URL not set; skipping Postgres test");
            return None;
        };
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "fable_test_fab31_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let admin: PgConnectOptions = base.parse().expect("DATABASE_URL");
        let mut conn = admin.connect().await.expect("connect admin");
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut conn)
            .await
            .expect("create database");
        conn.close().await.ok();
        let url = admin.clone().database(&name).to_url_lossy().to_string();
        let pool = fable_db::connect(&url).await.expect("connect");
        fable_db::migrate(&pool).await.expect("migrate");
        Some(Self { admin, name, pool })
    }

    async fn drop_db(self) {
        self.pool.close().await;
        let mut conn = self.admin.connect().await.expect("connect admin");
        sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        ))
        .execute(&mut conn)
        .await
        .expect("drop database");
    }
}

/// SQLSTATE and constraint name of a failed statement.
fn db_error(err: sqlx::Error) -> (String, Option<String>) {
    let db = err.as_database_error().expect("database error");
    (
        db.code().expect("sqlstate").into_owned(),
        db.constraint().map(str::to_owned),
    )
}

const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";
const CHECK_VIOLATION: &str = "23514";

async fn exec(pool: &PgPool, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::query(sql).execute(pool).await.map(|_| ())
}

async fn scalar<T>(pool: &PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

/// Collection + source + document + `n` chunks; returns (document, chunk ids).
async fn seed_chunks(pool: &PgPool, n: i32) -> (String, Vec<String>) {
    let coll: String = scalar(
        pool,
        "INSERT INTO collections (name) VALUES ('Runbooks') RETURNING id::text",
    )
    .await;
    let src: String = scalar(
        pool,
        &format!(
            "INSERT INTO sources (collection_id, name, connector) \
             VALUES ('{coll}', 'Uploads', 'upload') RETURNING id::text"
        ),
    )
    .await;
    let doc: String = scalar(
        pool,
        &format!(
            "INSERT INTO documents (source_id, collection_id, external_id, title) \
             VALUES ('{src}', '{coll}', 'file-1', 'Queue service') RETURNING id::text"
        ),
    )
    .await;
    let chunks = sqlx::query_scalar(
        "INSERT INTO chunks (document_id, collection_id, ordinal, text) \
         SELECT $1::uuid, $2::uuid, g, 'chunk ' || g FROM generate_series(0, $3 - 1) g \
         RETURNING id::text",
    )
    .bind(&doc)
    .bind(&coll)
    .bind(n)
    .fetch_all(pool)
    .await
    .unwrap();
    (doc, chunks)
}

async fn new_space(pool: &PgPool, model: &str, dim: i32) -> i64 {
    sqlx::query_scalar("INSERT INTO embedding_spaces (model, dim) VALUES ($1, $2) RETURNING id")
        .bind(model)
        .bind(dim)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn activate(pool: &PgPool, space: i64) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE embedding_spaces SET status = 'retired', retired_at = now() \
         WHERE status = 'active'",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE embedding_spaces SET status = 'active', activated_at = now() WHERE id = $1",
    )
    .bind(space)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

async fn insert_vec(pool: &PgPool, chunk: &str, space: i64, v: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO chunk_embeddings (chunk_id, space_id, embedding) \
         VALUES ($1::uuid, $2, $3::vector)",
    )
    .bind(chunk)
    .bind(space)
    .bind(v)
    .execute(pool)
    .await
    .map(|_| ())
}

async fn index_def(pool: &PgPool, space: i64) -> Option<String> {
    sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE indexname = $1")
        .bind(space_index_name(space))
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn per_space_partial_hnsw_index() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (_, chunks) = seed_chunks(pool, 3).await;
    let small = new_space(pool, "fake-3", 3).await;
    let large = new_space(pool, "fake-4", 4).await;

    // Created in the caller's transaction together with the space row.
    let mut tx = pool.begin().await.unwrap();
    create_space_index(&mut *tx, small, 3).await.unwrap();
    tx.commit().await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    create_space_index_concurrently(&mut conn, large, 4)
        .await
        .unwrap();
    // Idempotent.
    create_space_index(pool, small, 3).await.unwrap();
    create_space_index_concurrently(&mut conn, large, 4)
        .await
        .unwrap();

    let def = index_def(pool, small).await.expect("index exists");
    assert!(def.contains("USING hnsw"), "{def}");
    assert!(
        def.contains("((embedding)::vector(3)) vector_cosine_ops"),
        "{def}"
    );
    assert!(
        def.contains(&format!("WHERE (space_id = {small})")),
        "{def}"
    );
    let def = index_def(pool, large).await.expect("index exists");
    assert!(def.contains("vector(4)"), "{def}");

    // Spaces of different dimensions share the untyped column.
    for (i, c) in chunks.iter().enumerate() {
        insert_vec(pool, c, small, &format!("[{}, 1, 0]", i + 1))
            .await
            .unwrap();
        insert_vec(pool, c, large, &format!("[0, {}, 1, 0]", i + 1))
            .await
            .unwrap();
    }
    // The index expression's cast rejects a vector of the wrong dimension.
    let more = chunks[0].clone();
    exec(
        pool,
        &format!("DELETE FROM chunk_embeddings WHERE chunk_id = '{more}' AND space_id = {small}"),
    )
    .await
    .unwrap();
    let err = insert_vec(pool, &more, small, "[1, 2, 3, 4]")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expected 3 dimensions"), "{err}");

    // A nearest-neighbour query written with the helpers uses the space's
    // index, and returns only that space's vectors.
    let knn = format!(
        "SELECT chunk_id::text FROM chunk_embeddings WHERE {} \
         ORDER BY {} <=> '[0, 1, 1, 0]'::vector(4) LIMIT 2",
        space_predicate(large),
        space_vector_expr(4),
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN {knn}"))
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    let plan = plan.join("\n");
    assert!(plan.contains(&space_index_name(large)), "{plan}");
    let hits: Vec<String> = sqlx::query_scalar(&knn).fetch_all(&mut *tx).await.unwrap();
    assert_eq!(hits.first(), Some(&chunks[0]));
    tx.commit().await.unwrap();

    // Drop: idempotent, in a transaction or concurrently.
    drop_space_index(pool, small).await.unwrap();
    drop_space_index(pool, small).await.unwrap();
    assert_eq!(index_def(pool, small).await, None);
    drop_space_index_concurrently(&mut conn, large)
        .await
        .unwrap();
    assert_eq!(index_def(pool, large).await, None);

    // Invalid arguments never reach SQL.
    assert!(matches!(
        create_space_index(pool, small, 2001).await,
        Err(DbError::SpaceIndex(SpaceIndexError::InvalidDim(2001)))
    ));
    assert!(matches!(
        drop_space_index(pool, 0).await,
        Err(DbError::SpaceIndex(SpaceIndexError::InvalidSpaceId(0)))
    ));
    drop(conn);
    db.drop_db().await;
}

#[tokio::test]
async fn concurrent_build_replaces_an_invalid_index() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let space = new_space(pool, "fake", 3).await;
    // Simulate an interrupted CREATE INDEX CONCURRENTLY: an index of the
    // right name marked invalid.
    let name = space_index_name(space);
    exec(
        pool,
        &format!("CREATE INDEX {name} ON chunk_embeddings (space_id)"),
    )
    .await
    .unwrap();
    exec(
        pool,
        &format!("UPDATE pg_index SET indisvalid = false WHERE indexrelid = '{name}'::regclass"),
    )
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    create_space_index_concurrently(&mut conn, space, 3)
        .await
        .unwrap();
    drop(conn);
    let def = index_def(pool, space).await.expect("rebuilt");
    assert!(def.contains("USING hnsw"), "{def}");
    let valid: bool = scalar(
        pool,
        &format!("SELECT indisvalid FROM pg_index WHERE indexrelid = '{name}'::regclass"),
    )
    .await;
    assert!(valid);
    db.drop_db().await;
}

#[tokio::test]
async fn exactly_one_active_space() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    // Fresh install: a building space and no active one is fine (S-15).
    let blue = new_space(pool, "model-a", 3).await;
    let status: String = scalar(
        pool,
        &format!("SELECT status FROM embedding_spaces WHERE id = {blue}"),
    )
    .await;
    assert_eq!(status, "building");

    activate(pool, blue).await.unwrap();
    let green = new_space(pool, "model-b", 4).await;

    // A second active space is rejected immediately.
    let err = exec(
        pool,
        &format!(
            "UPDATE embedding_spaces SET status = 'active', activated_at = now() WHERE id = {green}"
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            UNIQUE_VIOLATION.into(),
            Some("uq_embedding_spaces_one_active".into())
        )
    );

    // Leaving no active space is rejected at commit.
    let err = exec(
        pool,
        &format!(
            "UPDATE embedding_spaces SET status = 'retired', retired_at = now() WHERE id = {blue}"
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            CHECK_VIOLATION.into(),
            Some("ck_embedding_spaces_exactly_one_active".into())
        )
    );
    let err = exec(
        pool,
        &format!("DELETE FROM embedding_spaces WHERE id = {blue}"),
    )
    .await
    .unwrap_err();
    assert_eq!(db_error(err).0, CHECK_VIOLATION);

    // The blue/green flip is atomic.
    activate(pool, green).await.unwrap();
    let states: Vec<(i64, String, bool)> = sqlx::query_as(
        "SELECT id, status, retired_at IS NOT NULL FROM embedding_spaces ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        states,
        vec![
            (blue, "retired".into(), true),
            (green, "active".into(), false)
        ]
    );
    // Rolling back to the retired space during its grace period works too.
    activate(pool, blue).await.unwrap();
    // A retired space can be dropped.
    activate(pool, green).await.unwrap();
    exec(
        pool,
        &format!("DELETE FROM embedding_spaces WHERE id = {blue}"),
    )
    .await
    .unwrap();
    db.drop_db().await;
}

#[tokio::test]
async fn space_constraints() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let base = "INSERT INTO embedding_spaces (model, dim, status";
    let cases = [
        (
            format!("{base}) VALUES ('m', 0, 'building')"),
            "ck_embedding_spaces_dim_range",
        ),
        (
            format!("{base}) VALUES ('m', 2001, 'building')"),
            "ck_embedding_spaces_dim_range",
        ),
        (
            format!("{base}) VALUES (' ', 3, 'building')"),
            "ck_embedding_spaces_model_not_blank",
        ),
        (
            format!("{base}) VALUES ('m', 3, 'ready')"),
            "ck_embedding_spaces_status_valid",
        ),
        (
            format!("{base}) VALUES ('m', 3, 'active')"),
            "ck_embedding_spaces_active_has_activated_at",
        ),
        (
            format!("{base}) VALUES ('m', 3, 'retired')"),
            "ck_embedding_spaces_retired_has_retired_at",
        ),
    ];
    for (sql, constraint) in cases {
        let err = exec(pool, &sql).await.unwrap_err();
        assert_eq!(
            db_error(err),
            (CHECK_VIOLATION.into(), Some(constraint.into())),
            "{sql}"
        );
    }

    // Model and dim never change: that is a new space.
    let space = new_space(pool, "m", 3).await;
    for set in ["dim = 4", "model = 'other'"] {
        let err = exec(
            pool,
            &format!("UPDATE embedding_spaces SET {set} WHERE id = {space}"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            db_error(err),
            (
                CHECK_VIOLATION.into(),
                Some("ck_embedding_spaces_identity_immutable".into())
            ),
            "{set}"
        );
    }

    // S-8: a connection a space uses cannot be deleted.
    let conn: String = scalar(
        pool,
        "INSERT INTO connections (kind, name) VALUES ('azure-openai', 'Embeddings') \
         RETURNING id::text",
    )
    .await;
    exec(
        pool,
        &format!(
            "INSERT INTO embedding_spaces (connection_id, model, dim) VALUES ('{conn}', 'e', 3)"
        ),
    )
    .await
    .unwrap();
    let err = exec(
        pool,
        &format!("DELETE FROM connections WHERE id = '{conn}'"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            FOREIGN_KEY_VIOLATION.into(),
            Some("fk_embedding_spaces_connection_id_connections".into())
        )
    );
    db.drop_db().await;
}

#[tokio::test]
async fn embeddings_cascade_from_chunks_documents_and_spaces() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (doc, chunks) = seed_chunks(pool, 2).await;
    let a = new_space(pool, "a", 3).await;
    let b = new_space(pool, "b", 3).await;
    create_space_index(pool, a, 3).await.unwrap();
    create_space_index(pool, b, 3).await.unwrap();
    for c in &chunks {
        insert_vec(pool, c, a, "[1, 0, 0]").await.unwrap();
        insert_vec(pool, c, b, "[0, 1, 0]").await.unwrap();
    }
    let count = |sql: &'static str| async move { scalar::<i64>(pool, sql).await };
    assert_eq!(count("SELECT count(*) FROM chunk_embeddings").await, 4);

    // One vector per (chunk, space).
    let err = insert_vec(pool, &chunks[0], a, "[1, 1, 0]")
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (UNIQUE_VIOLATION.into(), Some("pk_chunk_embeddings".into()))
    );
    // Vectors need an existing chunk and space.
    let err = insert_vec(pool, "00000000-0000-0000-0000-000000000001", a, "[1, 0, 0]")
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            FOREIGN_KEY_VIOLATION.into(),
            Some("fk_chunk_embeddings_chunk_id_chunks".into())
        )
    );

    // Dropping a space removes its vectors only.
    drop_space_index(pool, b).await.unwrap();
    exec(
        pool,
        &format!("DELETE FROM embedding_spaces WHERE id = {b}"),
    )
    .await
    .unwrap();
    assert_eq!(count("SELECT count(*) FROM chunk_embeddings").await, 2);

    // Chunk replacement / document deletion removes the vectors.
    exec(
        pool,
        &format!("DELETE FROM chunks WHERE id = '{}'", chunks[0]),
    )
    .await
    .unwrap();
    assert_eq!(count("SELECT count(*) FROM chunk_embeddings").await, 1);
    exec(pool, &format!("DELETE FROM documents WHERE id = '{doc}'"))
        .await
        .unwrap();
    assert_eq!(count("SELECT count(*) FROM chunk_embeddings").await, 0);

    // `embedding` is the untyped pgvector type.
    let typ: String = scalar(
        pool,
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute \
         WHERE attrelid = 'chunk_embeddings'::regclass AND attname = 'embedding'",
    )
    .await;
    assert_eq!(typ, "vector");
    db.drop_db().await;
}
