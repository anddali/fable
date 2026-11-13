//! Migration runner tests against a real Postgres (`DATABASE_URL`).
//!
//! Each test creates and drops its own database. Without `DATABASE_URL` the
//! tests are skipped (CI always sets it).

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fable_db::{migrate_with, MIGRATION_LOCK_CLASS, MIGRATION_LOCK_TARGET_SQL};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool};
use sqlx::{ConnectOptions, Connection};

static OK: Migrator = sqlx::migrate!("./tests/migrations/ok");
static FAILING: Migrator = sqlx::migrate!("./tests/migrations/failing");

/// A throwaway database, dropped on [`TestDb::drop_db`].
struct TestDb {
    admin: PgConnectOptions,
    name: String,
    url: String,
}

impl TestDb {
    async fn create() -> Option<Self> {
        let Ok(base) = std::env::var("DATABASE_URL") else {
            eprintln!("DATABASE_URL not set; skipping Postgres test");
            return None;
        };
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "fable_test_migrate_{}_{}",
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
        Some(Self { admin, name, url })
    }

    async fn pool(&self) -> PgPool {
        fable_db::connect(&self.url).await.expect("connect")
    }

    async fn conn(&self) -> PgConnection {
        PgConnection::connect(&self.url).await.expect("connect")
    }

    async fn drop_db(self) {
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

async fn try_lock(conn: &mut PgConnection) -> bool {
    sqlx::query_scalar(&format!(
        "SELECT pg_try_advisory_lock($1, {MIGRATION_LOCK_TARGET_SQL})"
    ))
    .bind(MIGRATION_LOCK_CLASS)
    .fetch_one(conn)
    .await
    .unwrap()
}

async fn unlock(conn: &mut PgConnection) {
    let released: bool = sqlx::query_scalar(&format!(
        "SELECT pg_advisory_unlock($1, {MIGRATION_LOCK_TARGET_SQL})"
    ))
    .bind(MIGRATION_LOCK_CLASS)
    .fetch_one(conn)
    .await
    .unwrap();
    assert!(released);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_replicas_apply_each_migration_once() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    const REPLICAS: usize = 8;
    let mut pools = Vec::new();
    for _ in 0..REPLICAS {
        pools.push(db.pool().await);
    }
    let runs = pools
        .iter()
        .cloned()
        .map(|pool| async move { migrate_with(&pool, &OK).await });
    let reports = futures_join_all(runs).await;

    let mut applied_by = 0;
    for report in reports {
        let report = report.expect("every replica boots");
        if !report.applied.is_empty() {
            applied_by += 1;
            assert_eq!(report.applied, vec![20240101000000, 20240101000001]);
        }
    }
    assert_eq!(applied_by, 1, "exactly one replica applies the migrations");

    let pool = &pools[0];
    let widgets: i64 = sqlx::query_scalar("SELECT count(*) FROM widgets")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(widgets, 1);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(rows, 2);

    // No pooled session is left holding the lock.
    let mut conn = db.conn().await;
    assert!(try_lock(&mut conn).await);
    unlock(&mut conn).await;
    conn.close().await.ok();

    for pool in pools {
        pool.close().await;
    }
    db.drop_db().await;
}

#[tokio::test]
async fn second_run_is_a_no_op() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool().await;
    let first = migrate_with(&pool, &OK).await.unwrap();
    assert_eq!(first.applied.len(), 2);
    assert!(!first.waited_for_lock);
    let second = migrate_with(&pool, &OK).await.unwrap();
    assert!(second.applied.is_empty());
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn waits_while_another_session_holds_the_lock() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let mut holder = db.conn().await;
    assert!(try_lock(&mut holder).await);

    let pool = db.pool().await;
    let task = tokio::spawn({
        let pool = pool.clone();
        async move { migrate_with(&pool, &OK).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!task.is_finished(), "migrations must wait for the lock");
    let applied: bool = sqlx::query_scalar("SELECT to_regclass('widgets') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!applied);

    unlock(&mut holder).await;
    let report = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("migration finishes once the lock is free")
        .unwrap()
        .unwrap();
    assert!(report.waited_for_lock);
    assert_eq!(report.applied.len(), 2);

    holder.close().await.ok();
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn failed_migration_releases_the_lock() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    // One connection: if the lock leaked back into the pool, the next
    // checkout would still hold it.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&db.url)
        .await
        .unwrap();
    let err = migrate_with(&pool, &FAILING).await.unwrap_err();
    assert!(err.to_string().contains("migration failed"), "{err}");

    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND classid::bigint = $1 \
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .bind(i64::from(MIGRATION_LOCK_CLASS))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held, 0);

    let mut other = db.conn().await;
    assert!(try_lock(&mut other).await);
    unlock(&mut other).await;
    other.close().await.ok();
    pool.close().await;
    db.drop_db().await;
}

#[tokio::test]
async fn embedded_migrations_apply_cleanly() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool().await;
    fable_db::migrate(&pool).await.expect("embedded migrations");
    let again = fable_db::migrate(&pool).await.unwrap();
    assert!(again.applied.is_empty());
    pool.close().await;
    db.drop_db().await;
}

/// Minimal `join_all` so the test needs no extra dependency.
async fn futures_join_all<F, T>(futs: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handles: Vec<_> = futs.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}
