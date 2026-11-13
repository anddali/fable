//! Schema tests for the chunks table (FAB-30, spec §7 M-2/M-5, I-6, CR-3):
//! source text only, block map, context, embed_text_sha256 and the
//! indexer-maintained weighted `fts` column with its GIN index.
//!
//! Each test creates and drops its own database. Without `DATABASE_URL` the
//! tests are skipped (CI always sets it).

use std::sync::atomic::{AtomicU32, Ordering};

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
            "fable_test_fab30_{}_{}",
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

async fn id(pool: &PgPool, sql: &str) -> String {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Collection + upload source + one document; returns (collection, document).
async fn seed(pool: &PgPool) -> (String, String) {
    let coll = id(
        pool,
        "INSERT INTO collections (name) VALUES ('Runbooks') RETURNING id::text",
    )
    .await;
    let src = id(
        pool,
        &format!(
            "INSERT INTO sources (collection_id, name, connector) \
             VALUES ('{coll}', 'Uploads', 'upload') RETURNING id::text"
        ),
    )
    .await;
    let doc = id(
        pool,
        &format!(
            "INSERT INTO documents (source_id, collection_id, external_id, title) \
             VALUES ('{src}', '{coll}', 'file-1', 'Queue service') RETURNING id::text"
        ),
    )
    .await;
    (coll, doc)
}

/// Insert a chunk the way the indexer does: fts built by `chunk_fts`.
async fn insert_chunk(
    pool: &PgPool,
    coll: &str,
    doc: &str,
    ordinal: i32,
    text: &str,
    context: Option<&str>,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO chunks (document_id, collection_id, ordinal, text, blocks, context, \
                             embed_text_sha256, chunk_metadata, fts) \
         SELECT d.id, $2::uuid, $3, $4, \
                '[{\"offset\": 0, \"len\": 5, \"type\": \"heading\", \"level\": 2}]'::jsonb, \
                $5, encode(sha256(convert_to($4, 'UTF8')), 'hex'), \
                '{\"heading_path\": [\"Ops\", \"Scaling\"]}'::jsonb, \
                chunk_fts('english', d.title, $5, $4) \
         FROM documents d WHERE d.id = $1::uuid \
         RETURNING id::text",
    )
    .bind(doc)
    .bind(coll)
    .bind(ordinal)
    .bind(text)
    .bind(context)
    .fetch_one(pool)
    .await
}

#[tokio::test]
async fn chunks_have_m2_columns_and_no_embedding() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;

    let columns: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT column_name::text, data_type::text, is_nullable::text \
         FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'chunks' \
         ORDER BY column_name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let col = |name: &str| {
        columns
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, t, null)| (t.as_str(), null.as_str()))
    };
    assert_eq!(col("text"), Some(("text", "NO")));
    assert_eq!(col("blocks"), Some(("jsonb", "NO")));
    assert_eq!(col("context"), Some(("text", "YES")));
    assert_eq!(col("embed_text_sha256"), Some(("character varying", "NO")));
    assert_eq!(col("fts"), Some(("tsvector", "NO")));
    assert_eq!(col("chunk_metadata"), Some(("jsonb", "NO")));
    // Vectors live in chunk_embeddings (M-3); the generated tsv is gone (M-5).
    assert_eq!(col("embedding"), None);
    assert_eq!(col("tsv"), None);

    // fts is indexer-maintained, not a generated column.
    let generated: String = sqlx::query_scalar(
        "SELECT is_generated::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'chunks' \
           AND column_name = 'fts'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(generated, "NEVER");

    // GIN index on fts.
    let indexdef: String =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE indexname = 'ix_chunks_fts'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(indexdef.contains("USING gin (fts)"), "{indexdef}");
    db.drop_db().await;
}

#[tokio::test]
async fn fts_weights_title_a_context_b_text_c() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, doc) = seed(pool).await;
    let chunk = insert_chunk(
        pool,
        &coll,
        &doc,
        0,
        "Scale to 5 replicas if depth exceeds 10k.",
        Some("Describes autoscaling for the billing worker."),
    )
    .await
    .unwrap();

    // Text stays the source text only: no title, breadcrumb or context.
    let (text, context, blocks): (String, Option<String>, String) =
        sqlx::query_as("SELECT text, context, blocks::text FROM chunks WHERE id = $1::uuid")
            .bind(&chunk)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(text, "Scale to 5 replicas if depth exceeds 10k.");
    assert_eq!(
        context.as_deref(),
        Some("Describes autoscaling for the billing worker.")
    );
    assert!(blocks.contains("\"type\": \"heading\""), "{blocks}");

    // Each part lands in the fts column with its weight.
    for (query, weight) in [("queue", "a"), ("billing", "b"), ("replicas", "c")] {
        let hit: bool = sqlx::query_scalar(&format!(
            "SELECT fts @@ to_tsquery('english', '{query}:{weight}') \
             FROM chunks WHERE id = $1::uuid"
        ))
        .bind(&chunk)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(hit, "{query} should carry weight {weight}");
        let wrong: bool = sqlx::query_scalar(&format!(
            "SELECT fts @@ to_tsquery('english', '{query}:{}') FROM chunks WHERE id = $1::uuid",
            if weight == "a" { "bc" } else { "a" }
        ))
        .bind(&chunk)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(!wrong, "{query} has the wrong weight");
    }

    // Hybrid search's lexical leg (R-5) matches through websearch_to_tsquery,
    // and a title hit outranks a body-only hit.
    let other = insert_chunk(pool, &coll, &doc, 1, "The queue drains nightly.", None)
        .await
        .unwrap();
    let ranked: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM chunks \
         WHERE fts @@ websearch_to_tsquery('english', 'service') \
         ORDER BY ts_rank(fts, websearch_to_tsquery('english', 'service')) DESC",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(ranked.len(), 2, "title words are searchable on every chunk");
    let body_hit: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM chunks WHERE fts @@ websearch_to_tsquery('english', 'drains')",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(body_hit, vec![other]);

    // The language is per collection (M-5): `simple` keeps stop words.
    let simple: bool = sqlx::query_scalar(
        "SELECT chunk_fts('simple', NULL, NULL, 'the queue') @@ to_tsquery('simple', 'the')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(simple);
    db.drop_db().await;
}

#[tokio::test]
async fn chunk_constraints() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, doc) = seed(pool).await;
    insert_chunk(pool, &coll, &doc, 0, "a", None).await.unwrap();

    // One chunk per (document, ordinal).
    let err = insert_chunk(pool, &coll, &doc, 0, "b", None)
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            UNIQUE_VIOLATION.into(),
            Some("uq_chunks_document_id".into())
        )
    );

    let base = "INSERT INTO chunks (document_id, collection_id, ordinal, text";
    let cases = [
        (
            format!("{base}) VALUES ('{doc}', '{coll}', -1, 'x')"),
            "ck_chunks_ordinal_non_negative",
        ),
        (
            format!("{base}, blocks) VALUES ('{doc}', '{coll}', 5, 'x', '{{}}')"),
            "ck_chunks_blocks_array",
        ),
        (
            format!("{base}, chunk_metadata) VALUES ('{doc}', '{coll}', 5, 'x', '[]')"),
            "ck_chunks_chunk_metadata_object",
        ),
        (
            format!("{base}, context) VALUES ('{doc}', '{coll}', 5, 'x', '')"),
            "ck_chunks_context_not_empty",
        ),
        (
            format!("{base}, embed_text_sha256) VALUES ('{doc}', '{coll}', 5, 'x', 'ABC')"),
            "ck_chunks_embed_text_sha256_hex",
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

    // Chunks must belong to an existing document and collection.
    let err = exec(
        pool,
        &format!("{base}) VALUES ('00000000-0000-0000-0000-000000000001', '{coll}', 9, 'x')"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            FOREIGN_KEY_VIOLATION.into(),
            Some("fk_chunks_document_id_documents".into())
        )
    );

    // Defaults for a minimal row (e.g. converted from the Python schema).
    exec(pool, &format!("{base}) VALUES ('{doc}', '{coll}', 7, 'x')"))
        .await
        .unwrap();
    let (blocks, meta, sha, fts_empty): (String, String, String, bool) = sqlx::query_as(
        "SELECT blocks::text, chunk_metadata::text, embed_text_sha256, fts = ''::tsvector \
         FROM chunks WHERE ordinal = 7",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        (blocks.as_str(), meta.as_str(), sha.as_str()),
        ("[]", "{}", "")
    );
    assert!(fts_empty);
    db.drop_db().await;
}

#[tokio::test]
async fn chunks_cascade_from_documents_and_collections() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, doc) = seed(pool).await;
    insert_chunk(pool, &coll, &doc, 0, "a", None).await.unwrap();
    insert_chunk(pool, &coll, &doc, 1, "b", None).await.unwrap();

    // I-6: replacement in one transaction (delete old, insert new with the
    // same ordinals) works with the unique constraint.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM chunks WHERE document_id = $1::uuid")
        .bind(&doc)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO chunks (document_id, collection_id, ordinal, text) \
         VALUES ($1::uuid, $2::uuid, 0, 'a2'), ($1::uuid, $2::uuid, 1, 'b2')",
    )
    .bind(&doc)
    .bind(&coll)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count(pool, "chunks").await, 2);

    // ST-2: deleting the document removes its chunks.
    exec(pool, &format!("DELETE FROM documents WHERE id = '{doc}'"))
        .await
        .unwrap();
    assert_eq!(count(pool, "chunks").await, 0);

    // ...and so does deleting the collection.
    let (coll, doc) = {
        let src = id(
            pool,
            &format!("SELECT id::text FROM sources WHERE collection_id = '{coll}'"),
        )
        .await;
        let doc = id(
            pool,
            &format!(
                "INSERT INTO documents (source_id, collection_id, external_id) \
                 VALUES ('{src}', '{coll}', 'file-2') RETURNING id::text"
            ),
        )
        .await;
        (coll, doc)
    };
    insert_chunk(pool, &coll, &doc, 0, "c", None).await.unwrap();
    assert_eq!(count(pool, "chunks").await, 1);
    exec(
        pool,
        &format!("DELETE FROM collections WHERE id = '{coll}'"),
    )
    .await
    .unwrap();
    assert_eq!(count(pool, "chunks").await, 0);
    db.drop_db().await;
}
