//! Schema tests for the jobs queue table, the content-addressed caches and
//! figures (FAB-33, spec §6 ST-2/ST-3, §7 M-4, §9 Q-1/Q-3/Q-6/Q-8), against a
//! real Postgres with pgvector.
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
            "fable_test_fab33_{}_{}",
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
const CHECK_VIOLATION: &str = "23514";
const FOREIGN_KEY_VIOLATION: &str = "23503";

async fn exec(pool: &PgPool, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::query(sql).execute(pool).await.map(|_| ())
}

async fn expect_violation(pool: &PgPool, sql: &str, code: &str, constraint: &str) {
    let err = exec(pool, sql).await.expect_err(sql);
    assert_eq!(
        db_error(err),
        (code.to_owned(), Some(constraint.to_owned())),
        "{sql}"
    );
}

async fn scalar_text(pool: &PgPool, sql: &str) -> String {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

fn sha(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

/// Collection + upload source + one document; returns (collection, document).
async fn seed(pool: &PgPool, name: &str) -> (String, String) {
    let coll = scalar_text(
        pool,
        &format!("INSERT INTO collections (name) VALUES ('{name}') RETURNING id::text"),
    )
    .await;
    let src = scalar_text(
        pool,
        &format!(
            "INSERT INTO sources (collection_id, name, connector) \
             VALUES ('{coll}', 'Uploads', 'upload') RETURNING id::text"
        ),
    )
    .await;
    let doc = scalar_text(
        pool,
        &format!(
            "INSERT INTO documents (source_id, collection_id, external_id) \
             VALUES ('{src}', '{coll}', 'file-1') RETURNING id::text"
        ),
    )
    .await;
    (coll, doc)
}

#[tokio::test]
async fn jobs_defaults_and_constraints() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;

    let (queue, status, priority, attempts, max_attempts, args, run_at_set): (
        String,
        String,
        i32,
        i32,
        i32,
        String,
        bool,
    ) = sqlx::query_as(
        "INSERT INTO jobs (task) VALUES ('sync') \
         RETURNING queue, status, priority, attempts, max_attempts, args::text, \
                   run_at <= now()",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(queue, "default");
    assert_eq!(status, "todo");
    assert_eq!(priority, 100);
    assert_eq!((attempts, max_attempts), (0, 1));
    assert_eq!(args, "{}");
    assert!(run_at_set);

    expect_violation(
        pool,
        "INSERT INTO jobs (task, status) VALUES ('sync', 'queued')",
        CHECK_VIOLATION,
        "ck_jobs_status_valid",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO jobs (task, args) VALUES ('sync', '[]')",
        CHECK_VIOLATION,
        "ck_jobs_args_object",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO jobs (task, max_attempts) VALUES ('sync', 0)",
        CHECK_VIOLATION,
        "ck_jobs_max_attempts_positive",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO jobs (task) VALUES ('  ')",
        CHECK_VIOLATION,
        "ck_jobs_task_not_blank",
    )
    .await;
    // A running job has an owner.
    expect_violation(
        pool,
        "INSERT INTO jobs (task, status) VALUES ('sync', 'doing')",
        CHECK_VIOLATION,
        "ck_jobs_doing_locked",
    )
    .await;
    // finished_at exactly for terminal statuses.
    expect_violation(
        pool,
        "INSERT INTO jobs (task, status) VALUES ('sync', 'succeeded')",
        CHECK_VIOLATION,
        "ck_jobs_finished_at_terminal",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO jobs (task, finished_at) VALUES ('sync', now())",
        CHECK_VIOLATION,
        "ck_jobs_finished_at_terminal",
    )
    .await;

    // Full lifecycle: todo -> doing -> succeeded.
    exec(
        pool,
        "UPDATE jobs SET status = 'doing', locked_by = 'node-a/1', locked_at = now(), \
         heartbeat_at = now(), attempts = attempts + 1",
    )
    .await
    .unwrap();
    exec(
        pool,
        "UPDATE jobs SET status = 'succeeded', finished_at = now()",
    )
    .await
    .unwrap();

    db.drop_db().await;
}

#[tokio::test]
async fn queueing_lock_allows_one_live_job_per_key() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;

    let first: i64 = sqlx::query_scalar(
        "INSERT INTO jobs (task, lock_key) VALUES ('sync', 'sync:c1') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();

    // Duplicate while todo -> AlreadyQueued.
    expect_violation(
        pool,
        "INSERT INTO jobs (task, lock_key) VALUES ('sync', 'sync:c1')",
        UNIQUE_VIOLATION,
        "uq_jobs_lock_key",
    )
    .await;

    // ON CONFLICT on the partial index is how the queue detects it.
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO jobs (task, lock_key) VALUES ('sync', 'sync:c1') \
         ON CONFLICT (lock_key) WHERE lock_key IS NOT NULL AND status IN ('todo', 'doing') \
         DO NOTHING RETURNING id",
    )
    .fetch_optional(pool)
    .await
    .unwrap();
    assert_eq!(inserted, None);

    // Still held while doing.
    exec(
        pool,
        &format!(
            "UPDATE jobs SET status = 'doing', locked_by = 'w', locked_at = now() \
             WHERE id = {first}"
        ),
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        "INSERT INTO jobs (task, lock_key) VALUES ('sync', 'sync:c1')",
        UNIQUE_VIOLATION,
        "uq_jobs_lock_key",
    )
    .await;

    // Other keys and keyless jobs are independent.
    exec(
        pool,
        "INSERT INTO jobs (task, lock_key) VALUES ('sync', 'sync:c2')",
    )
    .await
    .unwrap();
    exec(pool, "INSERT INTO jobs (task) VALUES ('index'), ('index')")
        .await
        .unwrap();

    // Each terminal status releases the key.
    for terminal in ["succeeded", "failed", "cancelled"] {
        exec(
            pool,
            &format!(
                "UPDATE jobs SET status = '{terminal}', finished_at = now() \
                 WHERE lock_key = 'sync:c1' AND status IN ('todo', 'doing')"
            ),
        )
        .await
        .unwrap();
        exec(
            pool,
            "INSERT INTO jobs (task, lock_key) VALUES ('sync', 'sync:c1')",
        )
        .await
        .unwrap();
    }
    assert_eq!(
        count(pool, "SELECT count(*) FROM jobs WHERE lock_key = 'sync:c1'").await,
        4
    );

    db.drop_db().await;
}

#[tokio::test]
async fn claim_respects_priority_run_at_and_pinning() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;

    exec(
        pool,
        "INSERT INTO jobs (task, priority, run_at, pinned_node) VALUES \
         ('index', 100, now() - interval '3 minutes', NULL), \
         ('index', 100, now() - interval '5 minutes', NULL), \
         ('index',  10, now() - interval '1 minute',  'node-b'), \
         ('index',   0, now() + interval '1 hour',    NULL), \
         ('index',  10, now() - interval '1 minute',  'node-a')",
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        "INSERT INTO jobs (task, pinned_node) VALUES ('index', '')",
        CHECK_VIOLATION,
        "ck_jobs_pinned_node_not_blank",
    )
    .await;

    // The Q-2 claim query as node-a: pinned-to-me first (priority), never
    // node-b's job, never a future run_at; then oldest run_at.
    let claim = "UPDATE jobs SET status = 'doing', locked_by = 'node-a/1', \
                 locked_at = now(), heartbeat_at = now(), attempts = attempts + 1 \
                 WHERE id = (SELECT id FROM jobs \
                   WHERE status = 'todo' AND queue = 'default' AND run_at <= now() \
                     AND (pinned_node IS NULL OR pinned_node = 'node-a') \
                   ORDER BY priority, run_at, id LIMIT 1 FOR UPDATE SKIP LOCKED) \
                 RETURNING coalesce(pinned_node, '') || ':' || priority::text";
    let mut order = Vec::new();
    while let Some(got) = sqlx::query_scalar::<_, String>(claim)
        .fetch_optional(pool)
        .await
        .unwrap()
    {
        order.push(got);
    }
    assert_eq!(order, vec!["node-a:10", ":100", ":100"]);

    // Stale-lock recovery and dead-node pinned jobs (Q-6) use these columns.
    exec(
        pool,
        "UPDATE jobs SET heartbeat_at = now() - interval '10 minutes' \
         WHERE status = 'doing'",
    )
    .await
    .unwrap();
    let reset = sqlx::query(
        "UPDATE jobs SET status = 'todo', locked_by = NULL, locked_at = NULL, \
         heartbeat_at = NULL \
         WHERE status = 'doing' AND heartbeat_at < now() - interval '1 minute'",
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(reset.rows_affected(), 3);
    let failed = sqlx::query(
        "UPDATE jobs SET status = 'failed', finished_at = now(), last_error = 'node gone' \
         WHERE pinned_node = 'node-b' AND status IN ('todo', 'doing')",
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(failed.rows_affected(), 1);

    db.drop_db().await;
}

#[tokio::test]
async fn caches_are_content_addressed_and_prunable() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let space_a = "00000000-0000-0000-0000-00000000000a";
    let space_b = "00000000-0000-0000-0000-00000000000b";
    let (h1, h2) = (sha('1'), sha('2'));

    // One table, any dimension (one per embedding space).
    exec(
        pool,
        &format!(
            "INSERT INTO embedding_cache (space_id, text_sha256, embedding) VALUES \
             ('{space_a}', '{h1}', '[0.1, 0.2, 0.3]'), \
             ('{space_b}', '{h1}', '[1, 0, 0, 0, 0]')"
        ),
    )
    .await
    .unwrap();
    let dims: Vec<i32> =
        sqlx::query_scalar("SELECT vector_dims(embedding) FROM embedding_cache ORDER BY space_id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(dims, vec![3, 5]);
    expect_violation(
        pool,
        &format!(
            "INSERT INTO embedding_cache (space_id, text_sha256, embedding) \
             VALUES ('{space_a}', '{h1}', '[0.4, 0.5, 0.6]')"
        ),
        UNIQUE_VIOLATION,
        "pk_embedding_cache",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO embedding_cache (space_id, text_sha256, embedding) \
             VALUES ('{space_a}', 'ABC', '[1]')"
        ),
        CHECK_VIOLATION,
        "ck_embedding_cache_text_sha256_hex",
    )
    .await;

    exec(
        pool,
        &format!(
            "INSERT INTO vision_cache (image_sha256, model, prompt_sha256, description) VALUES \
             ('{h1}', 'openai/gpt-4o', '{h2}', 'A bar chart.'), \
             ('{h1}', 'anthropic/claude', '{h2}', ''), \
             ('{h2}', 'openai/gpt-4o', '{h2}', 'A diagram.')"
        ),
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        &format!(
            "INSERT INTO vision_cache (image_sha256, model, prompt_sha256, description) \
             VALUES ('{h1}', 'openai/gpt-4o', '{h2}', 'dup')"
        ),
        UNIQUE_VIOLATION,
        "pk_vision_cache",
    )
    .await;

    exec(
        pool,
        &format!(
            "INSERT INTO context_cache (key, model, prompt_sha256, sentence) VALUES \
             ('{h1}', 'haiku', '{h2}', 'From the ops runbook, scaling section.')"
        ),
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        &format!(
            "INSERT INTO context_cache (key, model, prompt_sha256, sentence) \
             VALUES ('{h2}', 'haiku', '{h2}', ' ')"
        ),
        CHECK_VIOLATION,
        "ck_context_cache_sentence_not_empty",
    )
    .await;

    // Hit path: bump last_used_at. Prune: entries unused for 90 days.
    for table in ["embedding_cache", "vision_cache", "context_cache"] {
        exec(
            pool,
            &format!("UPDATE {table} SET last_used_at = now() - interval '91 days'"),
        )
        .await
        .unwrap();
    }
    exec(
        pool,
        &format!(
            "UPDATE embedding_cache SET last_used_at = now() \
             WHERE space_id = '{space_a}' AND text_sha256 = '{h1}'"
        ),
    )
    .await
    .unwrap();
    let mut pruned = 0;
    for table in ["embedding_cache", "vision_cache", "context_cache"] {
        pruned += sqlx::query(&format!(
            "DELETE FROM {table} WHERE last_used_at < now() - interval '90 days'"
        ))
        .execute(pool)
        .await
        .unwrap()
        .rows_affected();
    }
    assert_eq!(pruned, 5);
    assert_eq!(count(pool, "SELECT count(*) FROM embedding_cache").await, 1);

    // ST-3: caches are keyed by hash only, never tied to a document or
    // collection.
    let refs = count(
        pool,
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = current_schema() \
           AND table_name IN ('embedding_cache', 'vision_cache', 'context_cache') \
           AND column_name IN ('document_id', 'collection_id', 'chunk_id', 'source_id')",
    )
    .await;
    assert_eq!(refs, 0);

    db.drop_db().await;
}

#[tokio::test]
async fn figures_are_constrained_and_cascade() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, doc) = seed(pool, "Handbook").await;
    let (coll2, doc2) = seed(pool, "Runbooks").await;

    let insert = |doc: &str, coll: &str, n: i32, mime: &str, w: i32| {
        format!(
            "INSERT INTO figures (document_id, collection_id, n, mime, width, height, data) \
             VALUES ('{doc}', '{coll}', {n}, '{mime}', {w}, 600, '\\x89504e47')"
        )
    };
    exec(pool, &insert(&doc, &coll, 0, "image/png", 1024))
        .await
        .unwrap();
    exec(pool, &insert(&doc, &coll, 1, "image/webp", 800))
        .await
        .unwrap();
    exec(pool, &insert(&doc2, &coll2, 0, "image/png", 10))
        .await
        .unwrap();

    expect_violation(
        pool,
        &insert(&doc, &coll, 1, "image/png", 10),
        UNIQUE_VIOLATION,
        "uq_figures_document_id",
    )
    .await;
    expect_violation(
        pool,
        &insert(&doc, &coll, 2, "image/jpeg", 10),
        CHECK_VIOLATION,
        "ck_figures_mime_valid",
    )
    .await;
    expect_violation(
        pool,
        &insert(&doc, &coll, 2, "image/png", 2048),
        CHECK_VIOLATION,
        "ck_figures_width_valid",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO figures (document_id, collection_id, n, mime, width, height, data) \
             VALUES ('{doc}', '{coll}', 3, 'image/png', 1, 1, '')"
        ),
        CHECK_VIOLATION,
        "ck_figures_data_not_empty",
    )
    .await;
    expect_violation(
        pool,
        &insert(
            "00000000-0000-0000-0000-000000000000",
            &coll,
            0,
            "image/png",
            10,
        ),
        FOREIGN_KEY_VIOLATION,
        "fk_figures_document_id_documents",
    )
    .await;

    // ST-2: deleting a document removes its figures ...
    exec(pool, &format!("DELETE FROM documents WHERE id = '{doc}'"))
        .await
        .unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM figures").await, 1);
    // ... and deleting a collection removes the rest.
    exec(
        pool,
        &format!("DELETE FROM collections WHERE id = '{coll2}'"),
    )
    .await
    .unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM figures").await, 0);

    db.drop_db().await;
}

#[tokio::test]
async fn bytea_only_where_the_spec_allows_it() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    // §6: no source bytes are persisted. The only binary columns are the
    // optional figure images and sealed connection secrets.
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND data_type = 'bytea' \
           AND table_name <> '_sqlx_migrations' \
         ORDER BY 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        cols,
        vec![
            "connections.secret_ciphertext",
            "connections.secret_nonce",
            "figures.data"
        ]
    );
    db.drop_db().await;
}
