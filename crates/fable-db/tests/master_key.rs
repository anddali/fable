//! Master key resolution and rotation (FAB-41, spec §4 C-7, §5.2 S-4)
//! against a real Postgres.
//!
//! Each test creates and drops its own database. Without `DATABASE_URL` the
//! tests are skipped (CI always sets it).

use std::sync::atomic::{AtomicU32, Ordering};

use fable_core::{ExposeSecret, KeyVersion, MasterKey, SecretCipher, SecretString};
use fable_db::master_key::{self, KeySource, MasterKeyError};
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
            "fable_test_fab41_{}_{}",
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

/// Insert a connection whose secret is sealed by `cipher`; returns its id.
async fn insert_secret(
    pool: &PgPool,
    cipher: &SecretCipher,
    name: &str,
    plaintext: &str,
) -> String {
    let sealed = cipher.encrypt(&SecretString::from(plaintext)).unwrap();
    sqlx::query_scalar(
        "INSERT INTO connections (kind, name, secret_ciphertext, secret_nonce, key_version) \
         VALUES ('confluence', $1, $2, $3, $4) RETURNING id::text",
    )
    .bind(name)
    .bind(&sealed.ciphertext)
    .bind(&sealed.nonce)
    .bind(sealed.key_version.0)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Decrypt every stored secret with `cipher`, by connection name.
async fn open_all(pool: &PgPool, cipher: &SecretCipher) -> Vec<(String, String)> {
    let rows: Vec<(String, Vec<u8>, Vec<u8>, i32)> = sqlx::query_as(
        "SELECT name, secret_ciphertext, secret_nonce, key_version FROM connections \
         WHERE secret_ciphertext IS NOT NULL ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(name, ciphertext, nonce, v)| {
            let plain = cipher
                .decrypt(&fable_core::EncryptedSecret {
                    ciphertext,
                    nonce,
                    key_version: KeyVersion(v),
                })
                .unwrap();
            (name, plain.expose_secret().to_owned())
        })
        .collect()
}

async fn key_versions(pool: &PgPool) -> Vec<i32> {
    sqlx::query_scalar(
        "SELECT key_version FROM connections WHERE key_version IS NOT NULL ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn stored_dev_key(pool: &PgPool) -> Option<Vec<u8>> {
    sqlx::query_scalar("SELECT dev_key FROM master_key_state")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn env_key_is_recorded_and_verified() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let key = MasterKey::generate();

    // First boot: recorded as version 1, no dev key.
    let r = master_key::resolve(pool, Some(&key), false).await.unwrap();
    assert_eq!(r.source, KeySource::Env);
    assert_eq!(r.key_version, KeyVersion(1));
    assert_eq!(r.secrets, 0);
    assert_eq!(stored_dev_key(pool).await, None);
    insert_secret(pool, &r.cipher, "a", "token-a").await;

    // Next boot with the same key: verified.
    let r = master_key::resolve(pool, Some(&key), false).await.unwrap();
    assert_eq!(r.key_version, KeyVersion(1));
    assert_eq!(r.secrets, 1);
    assert!(r.rekeyed_from.is_none());
    assert_eq!(
        open_all(pool, &r.cipher).await,
        [("a".into(), "token-a".into())]
    );

    // Wrong key while secrets exist: startup failure naming the problem.
    let err = master_key::resolve(pool, Some(&MasterKey::generate()), false)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        MasterKeyError::WrongMasterKey {
            key_version: KeyVersion(1),
            secrets: 1
        }
    ));
    assert!(err.to_string().contains("RAG_MASTER_KEY does not match"));

    // Missing key while secrets exist: never falls back to a dev key.
    let err = master_key::resolve(pool, None, true).await.unwrap_err();
    assert!(matches!(
        err,
        MasterKeyError::MissingMasterKey { secrets: 1, .. }
    ));
    assert!(err.to_string().contains("RAG_MASTER_KEY is not set"));
    let err = master_key::resolve(pool, None, false).await.unwrap_err();
    assert!(matches!(err, MasterKeyError::MasterKeyRequired));

    // Nothing was changed by the failed attempts.
    let r = master_key::resolve(pool, Some(&key), false).await.unwrap();
    assert_eq!(r.key_version, KeyVersion(1));
    db.drop_db().await;
}

#[tokio::test]
async fn changed_key_without_secrets_is_adopted() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    master_key::resolve(pool, Some(&MasterKey::generate()), false)
        .await
        .unwrap();
    let new = MasterKey::generate();
    let r = master_key::resolve(pool, Some(&new), false).await.unwrap();
    assert_eq!(r.rekeyed_from, Some(KeyVersion(1)));
    assert_eq!(r.key_version, KeyVersion(2));
    let r = master_key::resolve(pool, Some(&new), false).await.unwrap();
    assert_eq!(r.rekeyed_from, None);
    assert_eq!(r.key_version, KeyVersion(2));
    db.drop_db().await;
}

#[tokio::test]
async fn dev_key_is_generated_once_and_reused() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;

    // Replicas booting together agree on one generated key.
    let results = futures_join_all(pool, 6).await;
    let first = &results[0];
    assert_eq!(first.source, KeySource::DevGenerated);
    let sealed = first.cipher.encrypt(&SecretString::from("x")).unwrap();
    for r in &results {
        assert_eq!(r.key_version, KeyVersion(1));
        assert_eq!(r.cipher.decrypt(&sealed).unwrap().expose_secret(), "x");
    }
    let dev = stored_dev_key(pool).await.expect("dev key stored");
    assert_eq!(dev.len(), 32);

    insert_secret(pool, &first.cipher, "a", "token-a").await;
    let again = master_key::resolve(pool, None, true).await.unwrap();
    assert_eq!(again.source, KeySource::DevGenerated);
    assert_eq!(open_all(pool, &again.cipher).await.len(), 1);

    // Setting some other RAG_MASTER_KEY while dev-sealed secrets exist fails
    // with a pointer to rotation.
    let err = master_key::resolve(pool, Some(&MasterKey::generate()), false)
        .await
        .unwrap_err();
    assert!(matches!(err, MasterKeyError::DevKeyInUse { secrets: 1 }));
    assert!(err.to_string().contains("rotate-master-key"));

    // Setting RAG_MASTER_KEY to the dev key itself hands it over to env.
    let same = MasterKey::from_bytes(dev.try_into().unwrap());
    let r = master_key::resolve(pool, Some(&same), false).await.unwrap();
    assert_eq!(r.source, KeySource::Env);
    assert_eq!(stored_dev_key(pool).await, None);
    db.drop_db().await;
}

async fn futures_join_all(pool: &PgPool, n: usize) -> Vec<master_key::ResolvedMasterKey> {
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..n {
        let pool = pool.clone();
        set.spawn(async move { master_key::resolve(&pool, None, true).await.unwrap() });
    }
    let mut out = Vec::new();
    while let Some(r) = set.join_next().await {
        out.push(r.unwrap());
    }
    out
}

#[tokio::test]
async fn rotation_reencrypts_every_secret_in_one_transaction() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let old = MasterKey::generate();
    let r = master_key::resolve(pool, Some(&old), false).await.unwrap();
    for (name, token) in [("a", "token-a"), ("b", "token-b"), ("c", "pässwörd 🔑")] {
        insert_secret(pool, &r.cipher, name, token).await;
    }
    // A connection without a secret is left alone.
    sqlx::query("INSERT INTO connections (kind, name) VALUES ('git', 'public')")
        .execute(pool)
        .await
        .unwrap();
    let before = open_all(pool, &r.cipher).await;

    // Same key and wrong current key are refused without changes.
    assert!(matches!(
        master_key::rotate(pool, Some(&old), false, &old).await,
        Err(MasterKeyError::SameKey)
    ));
    let new = MasterKey::generate();
    assert!(matches!(
        master_key::rotate(pool, Some(&MasterKey::generate()), false, &new).await,
        Err(MasterKeyError::WrongMasterKey { .. })
    ));
    assert_eq!(key_versions(pool).await, [1, 1, 1]);

    let report = master_key::rotate(pool, Some(&old), false, &new)
        .await
        .unwrap();
    assert_eq!(report.old_version, KeyVersion(1));
    assert_eq!(report.new_version, KeyVersion(2));
    assert_eq!(report.reencrypted, 3);
    assert_eq!(key_versions(pool).await, [2, 2, 2]);

    // The old key no longer starts the platform; the new one does and opens
    // every secret.
    assert!(matches!(
        master_key::resolve(pool, Some(&old), false).await,
        Err(MasterKeyError::WrongMasterKey {
            key_version: KeyVersion(2),
            secrets: 3
        })
    ));
    let r = master_key::resolve(pool, Some(&new), false).await.unwrap();
    assert_eq!(r.key_version, KeyVersion(2));
    assert_eq!(open_all(pool, &r.cipher).await, before);
    let rotated_at: Option<String> =
        sqlx::query_scalar("SELECT rotated_at::text FROM master_key_state")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(rotated_at.is_some());

    // Audited as a system action, without key material.
    let (actor, action, entity, entity_id, diff): (Option<String>, String, String, String, String) =
        sqlx::query_as("SELECT actor_oid, action, entity, entity_id, diff::text FROM audit_log")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(actor, None);
    assert_eq!(
        (action.as_str(), entity.as_str(), entity_id.as_str()),
        ("rotate_key", "master_key", "2")
    );
    let diff: serde_json::Value = serde_json::from_str(&diff).unwrap();
    assert_eq!(
        diff,
        serde_json::json!({
            "key_version": {"from": 1, "to": 2},
            "secrets_reencrypted": 3,
            "previous_key_source": "env",
        })
    );
    for key in [&old, &new] {
        assert!(!diff.to_string().contains(key.to_base64().expose_secret()));
    }
    db.drop_db().await;
}

#[tokio::test]
async fn failed_rotation_changes_nothing() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let old = MasterKey::generate();
    let r = master_key::resolve(pool, Some(&old), false).await.unwrap();
    insert_secret(pool, &r.cipher, "a", "token-a").await;
    let bad = insert_secret(pool, &r.cipher, "b", "token-b").await;
    insert_secret(pool, &r.cipher, "c", "token-c").await;
    // Corrupt one row: rotation must fail as a whole.
    sqlx::query(
        "UPDATE connections SET secret_ciphertext = set_byte(secret_ciphertext, 0, \
         get_byte(secret_ciphertext, 0) # 1) WHERE id = $1::uuid",
    )
    .bind(&bad)
    .execute(pool)
    .await
    .unwrap();

    let err = master_key::rotate(pool, Some(&old), false, &MasterKey::generate())
        .await
        .unwrap_err();
    match &err {
        MasterKeyError::UndecryptableSecret { table, id, .. } => {
            assert_eq!(*table, "connections");
            assert_eq!(id, &bad);
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(key_versions(pool).await, [1, 1, 1]);
    let r = master_key::resolve(pool, Some(&old), false).await.unwrap();
    assert_eq!(r.key_version, KeyVersion(1));
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(audit, 0);
    db.drop_db().await;
}

#[tokio::test]
async fn dev_key_rotates_to_env_key() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let dev = master_key::resolve(pool, None, true).await.unwrap();
    insert_secret(pool, &dev.cipher, "a", "token-a").await;

    // Outside dev the stored dev key is never used.
    let new = MasterKey::generate();
    assert!(matches!(
        master_key::rotate(pool, None, false, &new).await,
        Err(MasterKeyError::MasterKeyRequired)
    ));

    let report = master_key::rotate(pool, None, true, &new).await.unwrap();
    assert_eq!(report.reencrypted, 1);
    assert_eq!(stored_dev_key(pool).await, None);

    let r = master_key::resolve(pool, Some(&new), false).await.unwrap();
    assert_eq!(r.source, KeySource::Env);
    assert_eq!(
        open_all(pool, &r.cipher).await,
        [("a".into(), "token-a".into())]
    );
    // No dev key to fall back to any more.
    assert!(matches!(
        master_key::resolve(pool, None, true).await,
        Err(MasterKeyError::MissingMasterKey { .. })
    ));
    db.drop_db().await;
}

#[tokio::test]
async fn pre_existing_secrets_are_adopted_or_rejected() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    // Secrets written before any key was recorded (e.g. state row lost).
    let key = MasterKey::generate();
    let cipher = SecretCipher::single(KeyVersion(3), key.clone());
    insert_secret(pool, &cipher, "a", "token-a").await;

    assert!(matches!(
        master_key::resolve(pool, None, true).await,
        Err(MasterKeyError::MissingMasterKey {
            key_version: KeyVersion(3),
            secrets: 1
        })
    ));
    assert!(matches!(
        master_key::resolve(pool, Some(&MasterKey::generate()), false).await,
        Err(MasterKeyError::WrongMasterKey {
            key_version: KeyVersion(3),
            ..
        })
    ));
    let r = master_key::resolve(pool, Some(&key), false).await.unwrap();
    assert_eq!(r.key_version, KeyVersion(3));
    assert_eq!(open_all(pool, &r.cipher).await.len(), 1);
    db.drop_db().await;
}

#[tokio::test]
async fn secrets_with_another_key_version_fail_startup() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = &db.pool;
    let key = MasterKey::generate();
    let r = master_key::resolve(pool, Some(&key), false).await.unwrap();
    insert_secret(pool, &r.cipher, "a", "token-a").await;
    let other = SecretCipher::single(KeyVersion(5), MasterKey::generate());
    insert_secret(pool, &other, "b", "token-b").await;

    let err = master_key::resolve(pool, Some(&key), false)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        MasterKeyError::StaleKeyVersion {
            table: "connections",
            found: KeyVersion(5),
            current: KeyVersion(1),
            secrets: 1
        }
    ));
    db.drop_db().await;
}
