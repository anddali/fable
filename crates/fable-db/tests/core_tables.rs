//! Schema tests for the core tables (FAB-29, spec §7 M-1/M-2/M-5/M-6):
//! collections, ACLs, sources, documents, upload items, sync runs, API keys
//! and query logs, against a real Postgres.
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
            "fable_test_fab29_{}_{}",
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

/// Collection + upload source + one document; returns (collection, source, document).
async fn seed(pool: &PgPool) -> (String, String, String) {
    let coll = id(
        pool,
        "INSERT INTO collections (name) VALUES ('Handbook') RETURNING id::text",
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
            "INSERT INTO documents (source_id, collection_id, external_id, source_version) \
             VALUES ('{src}', '{coll}', 'file-1', 'sha256:abc') RETURNING id::text"
        ),
    )
    .await;
    (coll, src, doc)
}

#[tokio::test]
async fn defaults_match_python_schema_and_no_blob_columns() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, _, doc) = seed(pool).await;

    let (visibility, cron, enabled, pipeline): (String, String, bool, String) = sqlx::query_as(
        "SELECT visibility, sync_cron, sync_enabled, pipeline::text \
             FROM collections WHERE id = $1::uuid",
    )
    .bind(&coll)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(visibility, "private");
    assert_eq!(cron, "0 2 * * *");
    assert!(enabled);
    assert_eq!(pipeline, "{}");

    let (status, hash, version, last_error_at): (String, String, String, Option<String>) =
        sqlx::query_as(
            "SELECT status, content_hash, source_version, last_error_at::text \
             FROM documents WHERE id = $1::uuid",
        )
        .bind(&doc)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(hash, "");
    assert_eq!(version, "sha256:abc");
    assert_eq!(last_error_at, None);

    // M-2 / ST-1: no blob columns anywhere.
    let blobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = current_schema() AND column_name = 'blob_key'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(blobs, 0);

    // M-5 filter indexes exist.
    for index in [
        "ix_documents_source_updated_at",
        "ix_documents_mime_type",
        "ix_documents_source_id",
    ] {
        let found: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_indexes WHERE indexname = $1")
            .bind(index)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(found, 1, "{index}");
    }

    // M-6: no Postgres enum types.
    let enums: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_type WHERE typtype = 'e'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(enums, 0);
    db.drop_db().await;
}

#[tokio::test]
async fn status_and_role_columns_are_constrained() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, src, doc) = seed(pool).await;

    let cases = [
        (
            format!("UPDATE collections SET visibility = 'secret' WHERE id = '{coll}'"),
            "ck_collections_visibility_valid",
        ),
        (
            format!(
                "INSERT INTO collection_principals (collection_id, principal_type, principal_id, role) \
                 VALUES ('{coll}', 'user', 'oid-1', 'admin')"
            ),
            "ck_collection_principals_role_valid",
        ),
        (
            format!(
                "INSERT INTO collection_principals (collection_id, principal_type, principal_id, role) \
                 VALUES ('{coll}', 'robot', 'oid-1', 'viewer')"
            ),
            "ck_collection_principals_principal_type_valid",
        ),
        (
            format!("UPDATE documents SET status = 'indexing' WHERE id = '{doc}'"),
            "ck_documents_status_valid",
        ),
        (
            format!(
                "INSERT INTO upload_items (source_id, filename, mime_type, size_bytes, content_hash, \
                 status, scratch_node) VALUES ('{src}', 'a.pdf', 'application/pdf', 1, 'h', \
                 'uploading', 'node-a')"
            ),
            "ck_upload_items_status_valid",
        ),
        (
            format!(
                "INSERT INTO sync_runs (collection_id, status, started_at) \
                 VALUES ('{coll}', 'cancelled', now())"
            ),
            "ck_sync_runs_status_valid",
        ),
        (
            "INSERT INTO api_keys (name, prefix, key_hash, owner_oid, scopes) \
             VALUES ('k', 'ragk_ab', 'h1', 'oid', '{}')"
                .to_owned(),
            "ck_api_keys_scopes_array",
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

    // Valid ACL entry; duplicates conflict.
    let acl = format!(
        "INSERT INTO collection_principals (collection_id, principal_type, principal_id, role) \
         VALUES ('{coll}', 'group', 'g-1', 'editor')"
    );
    exec(pool, &acl).await.unwrap();
    assert_eq!(
        db_error(exec(pool, &acl).await.unwrap_err()),
        (
            UNIQUE_VIOLATION.into(),
            Some("uq_collection_principals_collection_id".into())
        )
    );

    // Collection names and (collection, source name) are unique.
    assert_eq!(
        db_error(
            exec(pool, "INSERT INTO collections (name) VALUES ('Handbook')")
                .await
                .unwrap_err()
        ),
        (UNIQUE_VIOLATION.into(), Some("uq_collections_name".into()))
    );
    assert_eq!(
        db_error(
            exec(
                pool,
                &format!(
                    "INSERT INTO sources (collection_id, name, connector) \
                     VALUES ('{coll}', 'Uploads', 'upload')"
                )
            )
            .await
            .unwrap_err()
        ),
        (
            UNIQUE_VIOLATION.into(),
            Some("uq_sources_collection_id".into())
        )
    );
    db.drop_db().await;
}

#[tokio::test]
async fn upload_items_track_status_and_scratch_node() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (_, src, _) = seed(pool).await;

    // Staged uploads must name the node holding the scratch file (U-1).
    let err = exec(
        pool,
        &format!(
            "INSERT INTO upload_items (source_id, filename, mime_type, size_bytes, content_hash) \
             VALUES ('{src}', 'a.pdf', 'application/pdf', 10, 'h')"
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            CHECK_VIOLATION.into(),
            Some("ck_upload_items_staged_has_node".into())
        )
    );

    let item = id(
        pool,
        &format!(
            "INSERT INTO upload_items (source_id, filename, mime_type, size_bytes, content_hash, \
             scratch_node) VALUES ('{src}', 'a.pdf', 'application/pdf', 10, 'h', 'node-a') \
             RETURNING id::text"
        ),
    )
    .await;
    let status: String = sqlx::query_scalar("SELECT status FROM upload_items WHERE id = $1::uuid")
        .bind(&item)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(status, "staged");

    // Node restart expires its staged items (U-3).
    let expired = sqlx::query(
        "UPDATE upload_items SET status = 'expired' \
         WHERE scratch_node = 'node-a' AND status = 'staged'",
    )
    .execute(pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(expired, 1);
    db.drop_db().await;
}

#[tokio::test]
async fn sources_reference_connections_with_restrict() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let coll = id(
        pool,
        "INSERT INTO collections (name) VALUES ('Wiki') RETURNING id::text",
    )
    .await;
    let conn = id(
        pool,
        "INSERT INTO connections (kind, name) VALUES ('confluence', 'Confluence') RETURNING id::text",
    )
    .await;

    // Unknown connection ids are rejected.
    let err = exec(
        pool,
        &format!(
            "INSERT INTO sources (collection_id, name, connector, config) VALUES ('{coll}', 'x', \
             'confluence', '{{\"connection_id\": \"00000000-0000-0000-0000-000000000001\"}}')"
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            FOREIGN_KEY_VIOLATION.into(),
            Some("fk_sources_connection_id_connections".into())
        )
    );

    let src = id(
        pool,
        &format!(
            "INSERT INTO sources (collection_id, name, connector, config) VALUES ('{coll}', \
             'Space ENG', 'confluence', '{{\"connection_id\": \"{conn}\", \"targets\": []}}') \
             RETURNING id::text"
        ),
    )
    .await;
    let linked: String =
        sqlx::query_scalar("SELECT connection_id::text FROM sources WHERE id = $1::uuid")
            .bind(&src)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(linked, conn);

    // A connection in use cannot be deleted (S-8).
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
            Some("fk_sources_connection_id_connections".into())
        )
    );

    // Once the source is gone (here via its collection), deletion works.
    exec(
        pool,
        &format!("DELETE FROM collections WHERE id = '{coll}'"),
    )
    .await
    .unwrap();
    exec(
        pool,
        &format!("DELETE FROM connections WHERE id = '{conn}'"),
    )
    .await
    .unwrap();
    db.drop_db().await;
}

#[tokio::test]
async fn deleting_a_collection_cascades_to_everything_derived() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let (coll, src, _) = seed(pool).await;
    exec(
        pool,
        &format!(
            "INSERT INTO collection_principals (collection_id, principal_type, principal_id, role) \
             VALUES ('{coll}', 'user', 'oid-1', 'owner')"
        ),
    )
    .await
    .unwrap();
    exec(
        pool,
        &format!(
            "INSERT INTO upload_items (source_id, filename, mime_type, size_bytes, content_hash, \
             status) VALUES ('{src}', 'a.pdf', 'application/pdf', 10, 'h', 'indexed')"
        ),
    )
    .await
    .unwrap();
    exec(
        pool,
        &format!(
            "INSERT INTO sync_runs (collection_id, trigger, started_at) \
             VALUES ('{coll}', 'manual', now())"
        ),
    )
    .await
    .unwrap();

    // Deleting a source removes its documents and upload items.
    let other_src = id(
        pool,
        &format!(
            "INSERT INTO sources (collection_id, name, connector) \
             VALUES ('{coll}', 'Other', 'upload') RETURNING id::text"
        ),
    )
    .await;
    exec(
        pool,
        &format!(
            "INSERT INTO documents (source_id, collection_id, external_id) \
             VALUES ('{other_src}', '{coll}', 'x')"
        ),
    )
    .await
    .unwrap();
    assert_eq!(count(pool, "documents").await, 2);
    exec(
        pool,
        &format!("DELETE FROM sources WHERE id = '{other_src}'"),
    )
    .await
    .unwrap();
    assert_eq!(count(pool, "documents").await, 1);

    exec(
        pool,
        &format!("DELETE FROM collections WHERE id = '{coll}'"),
    )
    .await
    .unwrap();
    for table in [
        "collection_principals",
        "sources",
        "documents",
        "upload_items",
        "sync_runs",
    ] {
        assert_eq!(count(pool, table).await, 0, "{table}");
    }
    db.drop_db().await;
}

#[tokio::test]
async fn api_keys_and_query_logs() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let key = id(
        pool,
        "INSERT INTO api_keys (name, prefix, key_hash, owner_oid, scopes) \
         VALUES ('ci', 'ragk_ab', 'hash-1', 'oid-1', '[\"c1\"]') RETURNING id::text",
    )
    .await;
    assert_eq!(
        db_error(
            exec(
                pool,
                "INSERT INTO api_keys (name, prefix, key_hash, owner_oid) \
                 VALUES ('dup', 'ragk_cd', 'hash-1', 'oid-2')"
            )
            .await
            .unwrap_err()
        ),
        (UNIQUE_VIOLATION.into(), Some("uq_api_keys_key_hash".into()))
    );

    exec(
        pool,
        &format!(
            "INSERT INTO query_logs (principal_oid, auth_method, api_key_id, query, filters) \
             VALUES ('oid-1', 'api_key', '{key}', 'how to deploy', '{{\"mime_type\": [\"application/pdf\"]}}')"
        ),
    )
    .await
    .unwrap();
    // Logs outlive the key.
    exec(pool, &format!("DELETE FROM api_keys WHERE id = '{key}'"))
        .await
        .unwrap();
    let (mode, filters): (String, String) =
        sqlx::query_as("SELECT mode, filters::text FROM query_logs")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(mode, "hybrid");
    assert!(filters.contains("application/pdf"));
    db.drop_db().await;
}
