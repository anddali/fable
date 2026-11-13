//! Schema tests for `connections`, `platform_settings` and `audit_log`
//! (FAB-32, spec §5.2 S-1/S-7/S-8/S-10/S-12/S-16) against a real Postgres.
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
            "fable_test_fab32_{}_{}",
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
const RESTRICT_VIOLATION: &str = "23001";

async fn insert_connection(pool: &PgPool, kind: &str, name: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO connections (kind, name, config) VALUES ($1, $2, '{\"base_url\":\"x\"}') \
         RETURNING id::text",
    )
    .bind(kind)
    .bind(name)
    .fetch_one(pool)
    .await
}

#[tokio::test]
async fn connection_defaults_and_secret_columns() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let id = insert_connection(pool, "confluence", "Confluence – service account")
        .await
        .unwrap();

    let (status, usable_by, groups, has_secret): (String, String, Vec<String>, bool) =
        sqlx::query_as(
            "SELECT status, usable_by, usable_by_groups, secret_ciphertext IS NOT NULL \
             FROM connections WHERE id = $1::uuid",
        )
        .bind(&id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(status, "enabled");
    assert_eq!(usable_by, "admins", "least privilege by default (S-7)");
    assert!(groups.is_empty());
    assert!(!has_secret);

    // A full sealed secret plus expiry (S-4, S-16) is accepted.
    sqlx::query(
        "UPDATE connections SET secret_ciphertext = $2, secret_nonce = $3, key_version = 1, \
         credential_expires_at = now() + interval '30 days' WHERE id = $1::uuid",
    )
    .bind(&id)
    .bind(vec![7u8; 32])
    .bind(vec![1u8; 12])
    .execute(pool)
    .await
    .unwrap();

    // Partial secrets, short nonces and bad key versions are rejected.
    for (sql, constraint) in [
        (
            "UPDATE connections SET secret_nonce = NULL WHERE id = $1::uuid",
            "connections_secret_complete",
        ),
        (
            "UPDATE connections SET secret_nonce = '\\x00'::bytea WHERE id = $1::uuid",
            "connections_secret_nonce_len",
        ),
        (
            "UPDATE connections SET key_version = 0 WHERE id = $1::uuid",
            "connections_key_version_positive",
        ),
        (
            "UPDATE connections SET config = '[]' WHERE id = $1::uuid",
            "connections_config_object",
        ),
        (
            "UPDATE connections SET last_test_ok = true WHERE id = $1::uuid",
            "connections_last_test_consistent",
        ),
    ] {
        let err = sqlx::query(sql).bind(&id).execute(pool).await.unwrap_err();
        assert_eq!(
            db_error(err),
            (CHECK_VIOLATION.into(), Some(constraint.into())),
            "{sql}"
        );
    }

    // A recorded test result is accepted.
    sqlx::query(
        "UPDATE connections SET last_test_at = now(), last_test_ok = false, \
         last_test_error = '403 on page list' WHERE id = $1::uuid",
    )
    .bind(&id)
    .execute(pool)
    .await
    .unwrap();

    db.drop_db().await;
}

#[tokio::test]
async fn connection_names_are_unique_ignoring_case() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    insert_connection(pool, "openai", "OpenAI").await.unwrap();
    let err = insert_connection(pool, "anthropic", " openai ")
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (UNIQUE_VIOLATION.into(), Some("connections_name_key".into()))
    );
    for (kind, name, constraint) in [
        ("openai", "  ", "connections_name_not_blank"),
        ("OpenAI", "x", "connections_kind_format"),
    ] {
        let err = insert_connection(pool, kind, name).await.unwrap_err();
        assert_eq!(
            db_error(err),
            (CHECK_VIOLATION.into(), Some(constraint.into()))
        );
    }
    db.drop_db().await;
}

#[tokio::test]
async fn usable_by_and_status_are_constrained() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let id = insert_connection(pool, "sharepoint", "SharePoint – HR")
        .await
        .unwrap();

    sqlx::query(
        "UPDATE connections SET usable_by = 'groups', usable_by_groups = ARRAY['g-hr', 'g-it'] \
         WHERE id = $1::uuid",
    )
    .bind(&id)
    .execute(pool)
    .await
    .unwrap();
    // Group lookup used by "connections this user may use".
    let visible: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM connections \
         WHERE usable_by = 'all_users' OR usable_by_groups && $1",
    )
    .bind(vec!["g-hr".to_owned()])
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(visible, 1);

    for sql in [
        "UPDATE connections SET usable_by_groups = '{}' WHERE id = $1::uuid",
        "UPDATE connections SET usable_by = 'all_users' WHERE id = $1::uuid",
    ] {
        let err = sqlx::query(sql).bind(&id).execute(pool).await.unwrap_err();
        assert_eq!(
            db_error(err),
            (
                CHECK_VIOLATION.into(),
                Some("connections_usable_by_groups_consistent".into())
            ),
            "{sql}"
        );
    }
    let err = sqlx::query("UPDATE connections SET usable_by = 'everyone', usable_by_groups = '{}' WHERE id = $1::uuid")
        .bind(&id)
        .execute(pool)
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err).1.as_deref(),
        Some("connections_usable_by_valid")
    );

    // CF-10: imported connection without credential, flagged for attention.
    for status in ["disabled", "needs_attention", "enabled"] {
        sqlx::query(
            "UPDATE connections SET status = $2, status_reason = 'credentials required' \
             WHERE id = $1::uuid",
        )
        .bind(&id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }
    let err = sqlx::query("UPDATE connections SET status = 'broken' WHERE id = $1::uuid")
        .bind(&id)
        .execute(pool)
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            CHECK_VIOLATION.into(),
            Some("connections_status_valid".into())
        )
    );
    db.drop_db().await;
}

#[tokio::test]
async fn platform_settings_protect_referenced_connections() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let conn_id = insert_connection(pool, "openai", "OpenAI").await.unwrap();

    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by) VALUES \
         ('embedding', jsonb_build_object('connection_id', $1::text, 'model', 'text-embedding-3-small', 'dim', 1536), 'oid-1'), \
         ('limits', '{\"max_upload_bytes\": 104857600, \"search_max_top_k\": 50}', 'oid-1')",
    )
    .bind(&conn_id)
    .execute(pool)
    .await
    .unwrap();

    let referenced: Option<String> = sqlx::query_scalar(
        "SELECT connection_id::text FROM platform_settings WHERE key = 'embedding'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(referenced.as_deref(), Some(conn_id.as_str()));
    let limits: Option<String> = sqlx::query_scalar(
        "SELECT connection_id::text FROM platform_settings WHERE key = 'limits'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(limits, None);

    // S-8: a connection used by a model setting cannot be deleted.
    let err = sqlx::query("DELETE FROM connections WHERE id = $1::uuid")
        .bind(&conn_id)
        .execute(pool)
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            FOREIGN_KEY_VIOLATION.into(),
            Some("platform_settings_connection_id_fkey".into())
        )
    );

    // Pointing a setting at an unknown connection is rejected too.
    let err = sqlx::query(
        "UPDATE platform_settings SET value = jsonb_set(value, '{connection_id}', \
         to_jsonb(gen_random_uuid()::text)) WHERE key = 'embedding'",
    )
    .execute(pool)
    .await
    .unwrap_err();
    assert_eq!(db_error(err).0, FOREIGN_KEY_VIOLATION);

    // Unknown keys and non-object values are rejected.
    for (sql, constraint) in [
        (
            "INSERT INTO platform_settings (key, value) VALUES ('theme', '{}')",
            "platform_settings_key_valid",
        ),
        (
            "INSERT INTO platform_settings (key, value) VALUES ('chat', '[]')",
            "platform_settings_value_object",
        ),
    ] {
        let err = sqlx::query(sql).execute(pool).await.unwrap_err();
        assert_eq!(
            db_error(err),
            (CHECK_VIOLATION.into(), Some(constraint.into())),
            "{sql}"
        );
    }

    // Once the setting moves away, the connection can go.
    sqlx::query(
        "UPDATE platform_settings SET value = value - 'connection_id' WHERE key = 'embedding'",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM connections WHERE id = $1::uuid")
        .bind(&conn_id)
        .execute(pool)
        .await
        .unwrap();
    db.drop_db().await;
}

#[tokio::test]
async fn audit_log_is_append_only() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let conn_id = insert_connection(pool, "slack", "Slack – eng")
        .await
        .unwrap();

    // Written in the same transaction as the change it records.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE connections SET name = 'Slack – engineering' WHERE id = $1::uuid")
        .bind(&conn_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    for action in ["update", "test"] {
        sqlx::query(
            "INSERT INTO audit_log (actor_oid, actor_name, action, entity, entity_id, diff) \
             VALUES ('oid-1', 'Ada', $1, 'connection', $2, \
             '{\"name\": {\"old\": \"Slack – eng\", \"new\": \"Slack – engineering\"}, \"secret\": \"secret changed\"}')",
        )
        .bind(action)
        .bind(&conn_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // System actions have no actor.
    sqlx::query("INSERT INTO audit_log (action, entity) VALUES ('rotate_key', 'master_key')")
        .execute(pool)
        .await
        .unwrap();

    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log ORDER BY at DESC, id DESC")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(actions, ["rotate_key", "test", "update"]);

    let err = sqlx::query("UPDATE audit_log SET diff = '{}'")
        .execute(pool)
        .await
        .unwrap_err();
    assert_eq!(db_error(err).0, RESTRICT_VIOLATION);

    let err = sqlx::query("INSERT INTO audit_log (action, entity) VALUES ('', 'connection')")
        .execute(pool)
        .await
        .unwrap_err();
    assert_eq!(
        db_error(err),
        (
            CHECK_VIOLATION.into(),
            Some("audit_log_action_not_blank".into())
        )
    );

    // The trail outlives the entity; retention may delete old rows.
    sqlx::query("DELETE FROM connections WHERE id = $1::uuid")
        .bind(&conn_id)
        .execute(pool)
        .await
        .unwrap();
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE entity_id = $1")
        .bind(&conn_id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(kept, 2);
    sqlx::query("DELETE FROM audit_log WHERE at < now() + interval '1 second'")
        .execute(pool)
        .await
        .unwrap();
    db.drop_db().await;
}
