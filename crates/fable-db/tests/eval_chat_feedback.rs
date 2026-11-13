//! Schema tests for the evaluation and chat feedback tables (FAB-34, spec
//! §7 M-4/M-5, §12.3 E-1..E-6) and the M-5 filter indexes.
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
            "fable_test_fab34_{}_{}",
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
async fn new_collection(pool: &PgPool, name: &str) -> String {
    scalar_text(
        pool,
        &format!("INSERT INTO collections (name) VALUES ('{name}') RETURNING id::text"),
    )
    .await
}

async fn new_query_log(pool: &PgPool, colls: &[&str]) -> String {
    let ids = colls
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    scalar_text(
        pool,
        &format!(
            "INSERT INTO query_logs (principal_oid, query, collection_ids, filters) \
             VALUES ('oid-1', 'how do I deploy?', '[{ids}]', \
                     '{{\"mime_types\": [\"application/pdf\"]}}') \
             RETURNING id::text"
        ),
    )
    .await
}

#[tokio::test]
async fn eval_tables_defaults_and_constraints() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let coll = new_collection(pool, "docs").await;

    let set = scalar_text(
        pool,
        &format!(
            "INSERT INTO eval_sets (name, collection_ids, created_by) \
             VALUES ('golden', ARRAY['{coll}']::uuid[], 'oid-1') RETURNING id::text"
        ),
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_sets (name, collection_ids) VALUES ('golden', ARRAY['{coll}']::uuid[])"
        ),
        UNIQUE_VIOLATION,
        "uq_eval_sets_name",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO eval_sets (name, collection_ids) VALUES ('empty', '{}')",
        CHECK_VIOLATION,
        "ck_eval_sets_collection_ids_not_empty",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO eval_sets (name, collection_ids) VALUES ('nulls', ARRAY[NULL]::uuid[])",
        CHECK_VIOLATION,
        "ck_eval_sets_collection_ids_not_empty",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_sets (name, collection_ids) VALUES (' ', ARRAY['{coll}']::uuid[])"
        ),
        CHECK_VIOLATION,
        "ck_eval_sets_name_not_blank",
    )
    .await;

    // Items: defaults, optional chunk expectations, origin and question checks.
    let doc = "00000000-0000-0000-0000-0000000000d1";
    let (item, origin, chunks_null, notes): (String, String, bool, String) =
        sqlx::query_as(&format!(
            "INSERT INTO eval_items (set_id, question, expected_document_ids) \
             VALUES ('{set}', 'How do I deploy?', ARRAY['{doc}']::uuid[]) \
             RETURNING id::text, origin, expected_chunk_ids IS NULL, notes"
        ))
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        (origin.as_str(), chunks_null, notes.as_str()),
        ("manual", true, "")
    );
    // Expected ids are not foreign keys: a vanished document must keep failing.
    exec(
        pool,
        &format!(
            "INSERT INTO eval_items (set_id, question, expected_document_ids, expected_chunk_ids, origin) \
             VALUES ('{set}', 'Unknown doc?', ARRAY[gen_random_uuid()], ARRAY[gen_random_uuid()], 'feedback')"
        ),
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        &format!("INSERT INTO eval_items (set_id, question) VALUES ('{set}', '  ')"),
        CHECK_VIOLATION,
        "ck_eval_items_question_not_blank",
    )
    .await;
    expect_violation(
        pool,
        &format!("INSERT INTO eval_items (set_id, question) VALUES ('{set}', repeat('x', 4001))"),
        CHECK_VIOLATION,
        "ck_eval_items_question_length",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_items (set_id, question, origin) VALUES ('{set}', 'q', 'robot')"
        ),
        CHECK_VIOLATION,
        "ck_eval_items_origin_valid",
    )
    .await;
    expect_violation(
        pool,
        "INSERT INTO eval_items (set_id, question) VALUES (gen_random_uuid(), 'q')",
        FOREIGN_KEY_VIOLATION,
        "fk_eval_items_set_id_eval_sets",
    )
    .await;

    // Runs: queued -> running -> succeeded, timestamps tied to status.
    let (run, status, snapshot, metrics): (String, String, String, String) =
        sqlx::query_as(&format!(
            "INSERT INTO eval_runs (set_id, requested_by, config_snapshot) \
             VALUES ('{set}', 'oid-1', '{{\"mode\": \"hybrid\", \"k\": [1, 5, 10]}}') \
             RETURNING id::text, status, config_snapshot->>'mode', metrics::text"
        ))
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        (status.as_str(), snapshot.as_str(), metrics.as_str()),
        ("queued", "hybrid", "{}")
    );
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_runs (set_id, status, started_at) VALUES ('{set}', 'done', now())"
        ),
        CHECK_VIOLATION,
        "ck_eval_runs_status_valid",
    )
    .await;
    expect_violation(
        pool,
        &format!("INSERT INTO eval_runs (set_id, status) VALUES ('{set}', 'running')"),
        CHECK_VIOLATION,
        "ck_eval_runs_started_at",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_runs (set_id, status, started_at) VALUES ('{set}', 'succeeded', now())"
        ),
        CHECK_VIOLATION,
        "ck_eval_runs_finished_at",
    )
    .await;
    expect_violation(
        pool,
        &format!("INSERT INTO eval_runs (set_id, metrics) VALUES ('{set}', '[]')"),
        CHECK_VIOLATION,
        "ck_eval_runs_metrics_object",
    )
    .await;
    expect_violation(
        pool,
        &format!("INSERT INTO eval_runs (set_id, config_snapshot) VALUES ('{set}', '1')"),
        CHECK_VIOLATION,
        "ck_eval_runs_config_snapshot_object",
    )
    .await;
    exec(
        pool,
        &format!("UPDATE eval_runs SET status = 'running', started_at = now() WHERE id = '{run}'"),
    )
    .await
    .unwrap();

    // Results: rank is 1-based, within the hit list, arrays aligned.
    exec(
        pool,
        &format!(
            "INSERT INTO eval_results (run_id, item_id, rank_of_first_expected, hit_ids, \
                                       hit_chunk_ids, hit_scores, latency_ms) \
             VALUES ('{run}', '{item}', 2, \
                     ARRAY[gen_random_uuid(), '{doc}'::uuid], \
                     ARRAY[gen_random_uuid(), gen_random_uuid()], \
                     ARRAY[0.9, 0.8]::real[], 12)"
        ),
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        &format!("INSERT INTO eval_results (run_id, item_id) VALUES ('{run}', '{item}')"),
        UNIQUE_VIOLATION,
        "pk_eval_results",
    )
    .await;
    let item2 = scalar_text(
        pool,
        &format!(
            "INSERT INTO eval_items (set_id, question) VALUES ('{set}', 'q2') RETURNING id::text"
        ),
    )
    .await;
    // A miss: no expected document in the hits.
    exec(
        pool,
        &format!("INSERT INTO eval_results (run_id, item_id) VALUES ('{run}', '{item2}')"),
    )
    .await
    .unwrap();
    exec(
        pool,
        &format!("DELETE FROM eval_results WHERE item_id = '{item2}'"),
    )
    .await
    .unwrap();
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_results (run_id, item_id, rank_of_first_expected) \
             VALUES ('{run}', '{item2}', 0)"
        ),
        CHECK_VIOLATION,
        "ck_eval_results_rank_positive",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_results (run_id, item_id, rank_of_first_expected) \
             VALUES ('{run}', '{item2}', 1)"
        ),
        CHECK_VIOLATION,
        "ck_eval_results_rank_within_hits",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO eval_results (run_id, item_id, hit_ids) \
             VALUES ('{run}', '{item2}', ARRAY[gen_random_uuid()])"
        ),
        CHECK_VIOLATION,
        "ck_eval_results_hit_arrays_aligned",
    )
    .await;

    exec(
        pool,
        &format!(
            "UPDATE eval_runs SET status = 'succeeded', finished_at = now(), \
             metrics = '{{\"recall@1\": 0, \"recall@5\": 1, \"mrr\": 0.5}}' WHERE id = '{run}'"
        ),
    )
    .await
    .unwrap();

    // Deleting an item drops its results; deleting the set drops everything.
    exec(pool, &format!("DELETE FROM eval_items WHERE id = '{item}'"))
        .await
        .unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM eval_results").await, 0);
    exec(pool, &format!("DELETE FROM eval_sets WHERE id = '{set}'"))
        .await
        .unwrap();
    for table in ["eval_items", "eval_runs", "eval_results"] {
        assert_eq!(
            count(pool, &format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }

    db.drop_db().await;
}

#[tokio::test]
async fn chat_feedback_constraints_and_upsert() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let coll = new_collection(pool, "docs").await;
    let log = new_query_log(pool, &[&coll]).await;

    let insert = |rating: &str, comment: &str| {
        format!(
            "INSERT INTO chat_feedback (query_log_id, principal_oid, rating, comment, \
                                        collection_ids, retrieved_chunk_ids, answer_text) \
             VALUES ('{log}', 'oid-1', '{rating}', '{comment}', ARRAY['{coll}']::uuid[], \
                     ARRAY[gen_random_uuid()], 'Use the deploy script.') \
             ON CONFLICT (query_log_id, principal_oid) DO UPDATE \
               SET rating = EXCLUDED.rating, comment = EXCLUDED.comment, updated_at = now() \
             RETURNING id::text"
        )
    };
    let first = scalar_text(pool, &insert("up", "")).await;
    // A second vote by the same principal updates the row.
    let second = scalar_text(pool, &insert("down", "outdated")).await;
    assert_eq!(first, second);
    let (rating, comment): (String, String) =
        sqlx::query_as("SELECT rating, comment FROM chat_feedback")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((rating.as_str(), comment.as_str()), ("down", "outdated"));

    expect_violation(
        pool,
        &format!(
            "INSERT INTO chat_feedback (query_log_id, principal_oid, rating, collection_ids) \
             VALUES ('{log}', 'oid-2', 'meh', ARRAY['{coll}']::uuid[])"
        ),
        CHECK_VIOLATION,
        "ck_chat_feedback_rating_valid",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO chat_feedback (query_log_id, principal_oid, rating, comment, collection_ids) \
             VALUES ('{log}', 'oid-2', 'up', repeat('x', 4001), ARRAY['{coll}']::uuid[])"
        ),
        CHECK_VIOLATION,
        "ck_chat_feedback_comment_length",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO chat_feedback (query_log_id, principal_oid, rating, collection_ids) \
             VALUES ('{log}', 'oid-2', 'up', '{{}}')"
        ),
        CHECK_VIOLATION,
        "ck_chat_feedback_collection_ids_not_empty",
    )
    .await;
    expect_violation(
        pool,
        &format!(
            "INSERT INTO chat_feedback (query_log_id, principal_oid, rating, collection_ids) \
             VALUES (gen_random_uuid(), 'oid-2', 'up', ARRAY['{coll}']::uuid[])"
        ),
        FOREIGN_KEY_VIOLATION,
        "fk_chat_feedback_query_log_id_query_logs",
    )
    .await;

    // Per-collection listing uses array containment.
    assert_eq!(
        count(
            pool,
            &format!(
                "SELECT count(*) FROM chat_feedback \
                 WHERE collection_ids @> ARRAY['{coll}']::uuid[] AND rating = 'down'"
            ),
        )
        .await,
        1
    );

    // Feedback goes with its query log.
    exec(pool, &format!("DELETE FROM query_logs WHERE id = '{log}'"))
        .await
        .unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM chat_feedback").await, 0);

    db.drop_db().await;
}

#[tokio::test]
async fn deleting_a_collection_detaches_eval_sets_and_feedback() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let a = new_collection(pool, "a").await;
    let b = new_collection(pool, "b").await;

    // One set and one feedback row over a only, one each over a and b.
    for (name, colls) in [
        ("only-a", format!("'{a}'")),
        ("a-and-b", format!("'{a}', '{b}'")),
    ] {
        let set = scalar_text(
            pool,
            &format!(
                "INSERT INTO eval_sets (name, collection_ids) \
                 VALUES ('{name}', ARRAY[{colls}]::uuid[]) RETURNING id::text"
            ),
        )
        .await;
        let item = scalar_text(
            pool,
            &format!("INSERT INTO eval_items (set_id, question) VALUES ('{set}', 'q') RETURNING id::text"),
        )
        .await;
        let run = scalar_text(
            pool,
            &format!("INSERT INTO eval_runs (set_id) VALUES ('{set}') RETURNING id::text"),
        )
        .await;
        exec(
            pool,
            &format!("INSERT INTO eval_results (run_id, item_id) VALUES ('{run}', '{item}')"),
        )
        .await
        .unwrap();
        let log = new_query_log(pool, &[&a]).await;
        exec(
            pool,
            &format!(
                "INSERT INTO chat_feedback (query_log_id, principal_oid, rating, collection_ids) \
                 VALUES ('{log}', 'oid-1', 'down', ARRAY[{colls}]::uuid[])"
            ),
        )
        .await
        .unwrap();
    }
    // A duplicated id must not leave an empty array behind.
    exec(
        pool,
        &format!(
            "INSERT INTO eval_sets (name, collection_ids) VALUES ('dup-a', ARRAY['{a}', '{a}']::uuid[])"
        ),
    )
    .await
    .unwrap();

    exec(pool, &format!("DELETE FROM collections WHERE id = '{a}'"))
        .await
        .unwrap();

    assert_eq!(
        scalar_text(pool, "SELECT string_agg(name, ',') FROM eval_sets").await,
        "a-and-b"
    );
    assert_eq!(
        scalar_text(pool, "SELECT collection_ids::text FROM eval_sets").await,
        format!("{{{b}}}")
    );
    assert_eq!(
        scalar_text(pool, "SELECT collection_ids::text FROM chat_feedback").await,
        format!("{{{b}}}")
    );
    for (table, n) in [
        ("eval_items", 1),
        ("eval_runs", 1),
        ("eval_results", 1),
        ("chat_feedback", 1),
    ] {
        assert_eq!(
            count(pool, &format!("SELECT count(*) FROM {table}")).await,
            n,
            "{table}"
        );
    }

    exec(pool, &format!("DELETE FROM collections WHERE id = '{b}'"))
        .await
        .unwrap();
    for table in [
        "eval_sets",
        "eval_items",
        "eval_runs",
        "eval_results",
        "chat_feedback",
    ] {
        assert_eq!(
            count(pool, &format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }

    db.drop_db().await;
}

#[tokio::test]
async fn filter_indexes_and_query_log_filters_exist() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;

    // M-5: single-column filter indexes on documents.
    for (index, column) in [
        ("ix_documents_source_updated_at", "source_updated_at"),
        ("ix_documents_mime_type", "mime_type"),
        ("ix_documents_source_id", "source_id"),
    ] {
        let def = scalar_text(
            pool,
            &format!("SELECT indexdef FROM pg_indexes WHERE indexname = '{index}'"),
        )
        .await;
        assert!(
            def.contains(&format!("(({column}))")) || def.contains(&format!("({column})")),
            "{def}"
        );
    }
    // Array-containment lookups are indexed.
    for index in [
        "ix_eval_sets_collection_ids",
        "ix_chat_feedback_collection_ids",
    ] {
        let def = scalar_text(
            pool,
            &format!("SELECT indexdef FROM pg_indexes WHERE indexname = '{index}'"),
        )
        .await;
        assert!(def.contains("USING gin"), "{def}");
    }

    // F-6: query_logs.filters is a jsonb object.
    let coll = new_collection(pool, "docs").await;
    let log = new_query_log(pool, &[&coll]).await;
    assert_eq!(
        scalar_text(
            pool,
            &format!("SELECT filters->'mime_types'->>0 FROM query_logs WHERE id = '{log}'"),
        )
        .await,
        "application/pdf"
    );
    expect_violation(
        pool,
        "INSERT INTO query_logs (principal_oid, query, filters) VALUES ('o', 'q', '[]')",
        CHECK_VIOLATION,
        "ck_query_logs_filters_object",
    )
    .await;

    db.drop_db().await;
}
