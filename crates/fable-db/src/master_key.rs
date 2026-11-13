//! Master key resolution at startup and `fable rotate-master-key`
//! (spec §4 C-7, §5.2 S-4).
//!
//! The `master_key_state` row records the `key_version` of the key that
//! currently seals every secret plus a key-check value (a fixed plaintext
//! sealed under that key). At startup [`resolve`] matches `RAG_MASTER_KEY`
//! against it:
//!
//! * the right key → a [`SecretCipher`] for the platform;
//! * a missing or wrong key while encrypted secrets exist → an error naming
//!   the problem (never silent data loss);
//! * no key in `dev` → an auto-generated key stored in the database (the
//!   caller logs a loud warning).
//!
//! [`rotate`] re-encrypts every secret under a new key in one transaction.
//! Nothing here logs or formats key material or plaintext.

use std::collections::BTreeMap;
use std::sync::Arc;

use fable_core::secrets::{
    CryptoError, EncryptedSecret, ExposeSecret, KeyVersion, Keyring, MasterKey, SecretCipher,
    SecretString, MASTER_KEY_LEN,
};
use sqlx::{PgConnection, PgPool};
use zeroize::Zeroizing;

/// First key of the transaction-level advisory lock serialising key
/// resolution and rotation across replicas (second key as for migrations:
/// [`crate::MIGRATION_LOCK_TARGET_SQL`]).
pub const MASTER_KEY_LOCK_CLASS: i32 = 0x0FAB_1E41;

/// Fixed plaintext of the key-check value. Not secret: it only proves which
/// key sealed it (AES-GCM authentication fails under any other key).
const KEY_CHECK_PLAINTEXT: &str = "fable master key check v1";

/// A table column set holding sealed secrets.
#[derive(Debug, Clone, Copy)]
pub struct SecretColumns {
    pub table: &'static str,
    /// Primary key column (`uuid`).
    pub id: &'static str,
    pub ciphertext: &'static str,
    pub nonce: &'static str,
    pub key_version: &'static str,
}

/// Every place an encrypted secret is stored. A new table with sealed
/// secrets must be listed here so startup checks and rotation cover it.
pub const SECRET_COLUMNS: &[SecretColumns] = &[SecretColumns {
    table: "connections",
    id: "id",
    ciphertext: "secret_ciphertext",
    nonce: "secret_nonce",
    key_version: "key_version",
}];

/// Where the master key in use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// `RAG_MASTER_KEY`.
    Env,
    /// The auto-generated development key stored in the database (C-7).
    DevGenerated,
}

/// The outcome of [`resolve`].
#[derive(Debug, Clone)]
pub struct ResolvedMasterKey {
    /// Seals and opens platform secrets.
    pub cipher: SecretCipher,
    pub source: KeySource,
    pub key_version: KeyVersion,
    /// Number of stored secrets (all sealed with `key_version`).
    pub secrets: i64,
    /// Set when the recorded key was replaced because no secrets were
    /// stored (e.g. `RAG_MASTER_KEY` changed on an empty install).
    pub rekeyed_from: Option<KeyVersion>,
}

/// The outcome of [`rotate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationReport {
    pub old_version: KeyVersion,
    pub new_version: KeyVersion,
    /// Secrets re-encrypted under the new key.
    pub reencrypted: u64,
}

/// Master key problems. Messages name the problem and the fix; they never
/// contain key material or secret values.
#[derive(Debug, thiserror::Error)]
pub enum MasterKeyError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("RAG_MASTER_KEY is required when RAG_ENVIRONMENT is not `dev`")]
    MasterKeyRequired,
    #[error(
        "RAG_MASTER_KEY is not set, but {secrets} stored secret(s) are sealed with master key \
         version {key_version}; set RAG_MASTER_KEY to the key they were sealed with"
    )]
    MissingMasterKey {
        key_version: KeyVersion,
        secrets: i64,
    },
    #[error(
        "RAG_MASTER_KEY does not match the master key (version {key_version}) that sealed the \
         {secrets} stored secret(s); set RAG_MASTER_KEY to that key (after \
         `fable rotate-master-key`, the new key)"
    )]
    WrongMasterKey {
        key_version: KeyVersion,
        secrets: i64,
    },
    #[error(
        "{secrets} stored secret(s) are sealed with the auto-generated development key, not \
         RAG_MASTER_KEY; to move them to RAG_MASTER_KEY, run `fable rotate-master-key \
         --new-key-file <file>` with RAG_ENVIRONMENT=dev and RAG_MASTER_KEY unset"
    )]
    DevKeyInUse { secrets: i64 },
    #[error(
        "{secrets} secret(s) in {table} are sealed with master key version {found}, but the \
         current master key is version {current}; they cannot be decrypted"
    )]
    StaleKeyVersion {
        table: &'static str,
        found: KeyVersion,
        current: KeyVersion,
        secrets: i64,
    },
    #[error("the stored master key check value is corrupt: {0}")]
    CorruptState(CryptoError),
    #[error("cannot re-encrypt the secret of {table} row {id}: {source}")]
    UndecryptableSecret {
        table: &'static str,
        id: String,
        source: CryptoError,
    },
    #[error("the new master key is the same as the current one")]
    SameKey,
    #[error("{0}")]
    Crypto(#[from] CryptoError),
}

/// `key_version, check_ciphertext, check_nonce, dev_key`.
type StateRow = (i32, Vec<u8>, Vec<u8>, Option<Vec<u8>>);

/// The `master_key_state` row.
struct State {
    version: KeyVersion,
    check: EncryptedSecret,
    dev_key: Option<MasterKey>,
}

impl State {
    /// Whether `key` is the key the check value was sealed with.
    fn matches(&self, key: &MasterKey) -> bool {
        SecretCipher::single(self.version, key.clone())
            .decrypt(&self.check)
            .is_ok_and(|p| p.expose_secret() == KEY_CHECK_PLAINTEXT)
    }
}

/// Resolve the master key at startup.
///
/// `env_key` is `RAG_MASTER_KEY`; `allow_dev_key` is true only in `dev`,
/// where a missing env key falls back to (or creates) the stored dev key.
/// Safe to call from many replicas at once.
pub async fn resolve(
    pool: &PgPool,
    env_key: Option<&MasterKey>,
    allow_dev_key: bool,
) -> Result<ResolvedMasterKey, MasterKeyError> {
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    let (resolved, _) = resolve_in(&mut tx, env_key, allow_dev_key).await?;
    tx.commit().await?;
    Ok(resolved)
}

/// Re-encrypt every stored secret under `new_key` in one transaction.
///
/// `current_key` is `RAG_MASTER_KEY` (the key in use now); in `dev` without
/// it, the stored dev key is used and removed afterwards. Running replicas
/// keep the old key in memory: restart them with the new `RAG_MASTER_KEY`.
pub async fn rotate(
    pool: &PgPool,
    current_key: Option<&MasterKey>,
    allow_dev_key: bool,
    new_key: &MasterKey,
) -> Result<RotationReport, MasterKeyError> {
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    let (current, old_key) = resolve_in(&mut tx, current_key, allow_dev_key).await?;
    if old_key.expose_bytes() == new_key.expose_bytes() {
        return Err(MasterKeyError::SameKey);
    }
    let old_version = current.key_version;
    let new_version = old_version.next();
    let cipher = SecretCipher::new(Arc::new(
        Keyring::new(new_version, new_key.clone()).with_decrypt_only(old_version, old_key),
    ));

    let mut reencrypted = 0u64;
    for cols in SECRET_COLUMNS {
        let rows: Vec<(String, Vec<u8>, Vec<u8>, i32)> = sqlx::query_as(&format!(
            "SELECT {id}::text, {ct}, {nonce}, {kv} FROM {table} \
             WHERE {ct} IS NOT NULL ORDER BY {id} FOR UPDATE",
            id = cols.id,
            ct = cols.ciphertext,
            nonce = cols.nonce,
            kv = cols.key_version,
            table = cols.table,
        ))
        .fetch_all(&mut *tx)
        .await?;
        for (id, ciphertext, nonce, version) in rows {
            let sealed = EncryptedSecret {
                ciphertext,
                nonce,
                key_version: KeyVersion(version),
            };
            let resealed = cipher.reencrypt(&sealed).map_err(|source| {
                MasterKeyError::UndecryptableSecret {
                    table: cols.table,
                    id: id.clone(),
                    source,
                }
            })?;
            sqlx::query(&format!(
                "UPDATE {table} SET {ct} = $1, {nonce} = $2, {kv} = $3 WHERE {id} = $4::uuid",
                table = cols.table,
                ct = cols.ciphertext,
                nonce = cols.nonce,
                kv = cols.key_version,
                id = cols.id,
            ))
            .bind(&resealed.ciphertext)
            .bind(&resealed.nonce)
            .bind(resealed.key_version.0)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            reencrypted += 1;
        }
    }

    write_state(&mut tx, new_version, new_key, None, true).await?;
    let diff = serde_json::json!({
        "key_version": { "from": old_version.0, "to": new_version.0 },
        "secrets_reencrypted": reencrypted,
        "previous_key_source": match current.source {
            KeySource::Env => "env",
            KeySource::DevGenerated => "dev_generated",
        },
    });
    sqlx::query(
        "INSERT INTO audit_log (actor_oid, actor_name, action, entity, entity_id, diff) \
         VALUES (NULL, 'fable rotate-master-key', 'rotate_key', 'master_key', $1, $2::jsonb)",
    )
    .bind(new_version.0.to_string())
    .bind(diff.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(RotationReport {
        old_version,
        new_version,
        reencrypted,
    })
}

async fn lock(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(&format!(
        "SELECT pg_advisory_xact_lock($1, {})",
        crate::MIGRATION_LOCK_TARGET_SQL
    ))
    .bind(MASTER_KEY_LOCK_CLASS)
    .execute(conn)
    .await?;
    Ok(())
}

/// Core of [`resolve`], inside the caller's locked transaction. Also
/// returns the key itself (for rotation).
async fn resolve_in(
    conn: &mut PgConnection,
    env_key: Option<&MasterKey>,
    allow_dev_key: bool,
) -> Result<(ResolvedMasterKey, MasterKey), MasterKeyError> {
    if env_key.is_none() && !allow_dev_key {
        return Err(MasterKeyError::MasterKeyRequired);
    }
    let state = load_state(conn).await?;
    let by_version = secrets_by_version(conn).await?;
    let secrets: i64 = by_version.values().map(|(_, n)| n).sum();

    let (key, source, version, rekeyed_from) = match (state, env_key) {
        (Some(state), Some(env)) => {
            if state.matches(env) {
                if state.dev_key.is_some() {
                    // RAG_MASTER_KEY was set to the dev key: env now owns it.
                    write_state(conn, state.version, env, None, false).await?;
                }
                (env.clone(), KeySource::Env, state.version, None)
            } else if secrets == 0 {
                let version = state.version.next();
                write_state(conn, version, env, None, true).await?;
                (env.clone(), KeySource::Env, version, Some(state.version))
            } else if state.dev_key.is_some() {
                return Err(MasterKeyError::DevKeyInUse { secrets });
            } else {
                return Err(MasterKeyError::WrongMasterKey {
                    key_version: state.version,
                    secrets,
                });
            }
        }
        (Some(state), None) => match state.dev_key {
            Some(ref dev) => {
                if !state.matches(dev) {
                    return Err(MasterKeyError::CorruptState(CryptoError::Decrypt(
                        state.version,
                    )));
                }
                (dev.clone(), KeySource::DevGenerated, state.version, None)
            }
            None if secrets == 0 => {
                let version = state.version.next();
                let dev = MasterKey::generate();
                write_state(conn, version, &dev, Some(&dev), true).await?;
                (dev, KeySource::DevGenerated, version, Some(state.version))
            }
            None => {
                return Err(MasterKeyError::MissingMasterKey {
                    key_version: state.version,
                    secrets,
                })
            }
        },
        (None, Some(env)) => {
            // First boot with this table. Secrets written before it existed
            // are adopted if they all open under RAG_MASTER_KEY.
            let version = adopt_version(conn, &by_version, env).await?;
            write_state(conn, version, env, None, false).await?;
            (env.clone(), KeySource::Env, version, None)
        }
        (None, None) => {
            if let Some((&version, _)) = by_version.iter().next() {
                return Err(MasterKeyError::MissingMasterKey {
                    key_version: version,
                    secrets,
                });
            }
            let dev = MasterKey::generate();
            write_state(conn, KeyVersion::INITIAL, &dev, Some(&dev), false).await?;
            (dev, KeySource::DevGenerated, KeyVersion::INITIAL, None)
        }
    };

    // Every secret must be sealed with the current version; anything else
    // could not be decrypted later.
    for (&found, &(table, n)) in &by_version {
        if found != version {
            return Err(MasterKeyError::StaleKeyVersion {
                table,
                found,
                current: version,
                secrets: n,
            });
        }
    }

    let cipher = SecretCipher::new(Arc::new(Keyring::new(version, key.clone())));
    Ok((
        ResolvedMasterKey {
            cipher,
            source,
            key_version: version,
            secrets,
            rekeyed_from,
        },
        key,
    ))
}

/// The version to record for existing secrets on first boot: they must all
/// share one version and open under `key`.
async fn adopt_version(
    conn: &mut PgConnection,
    by_version: &BTreeMap<KeyVersion, (&'static str, i64)>,
    key: &MasterKey,
) -> Result<KeyVersion, MasterKeyError> {
    let secrets: i64 = by_version.values().map(|(_, n)| n).sum();
    let mut versions = by_version.keys().copied();
    let Some(version) = versions.next() else {
        return Ok(KeyVersion::INITIAL);
    };
    if versions.next().is_some() {
        // Mixed versions: the check below fails for at least one of them.
        let newest = *by_version.keys().next_back().expect("non-empty");
        return Err(MasterKeyError::WrongMasterKey {
            key_version: newest,
            secrets,
        });
    }
    let cipher = SecretCipher::single(version, key.clone());
    for cols in SECRET_COLUMNS {
        let sample: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(&format!(
            "SELECT {ct}, {nonce} FROM {table} WHERE {ct} IS NOT NULL LIMIT 1",
            ct = cols.ciphertext,
            nonce = cols.nonce,
            table = cols.table,
        ))
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((ciphertext, nonce)) = sample {
            let sealed = EncryptedSecret {
                ciphertext,
                nonce,
                key_version: version,
            };
            if cipher.decrypt(&sealed).is_err() {
                return Err(MasterKeyError::WrongMasterKey {
                    key_version: version,
                    secrets,
                });
            }
        }
    }
    Ok(version)
}

async fn load_state(conn: &mut PgConnection) -> Result<Option<State>, MasterKeyError> {
    let row: Option<StateRow> = sqlx::query_as(
        "SELECT key_version, check_ciphertext, check_nonce, dev_key \
         FROM master_key_state FOR UPDATE",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some((version, ciphertext, nonce, dev_key)) = row else {
        return Ok(None);
    };
    let dev_key = dev_key
        .map(Zeroizing::new)
        .map(|raw| {
            let bytes: [u8; MASTER_KEY_LEN] = raw
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::InvalidKey("stored dev key must be 32 bytes"))?;
            Ok::<_, CryptoError>(MasterKey::from_bytes(bytes))
        })
        .transpose()
        .map_err(MasterKeyError::CorruptState)?;
    let version = KeyVersion(version);
    Ok(Some(State {
        version,
        check: EncryptedSecret {
            ciphertext,
            nonce,
            key_version: version,
        },
        dev_key,
    }))
}

/// Stored secret counts per key version (with one table name for messages).
async fn secrets_by_version(
    conn: &mut PgConnection,
) -> Result<BTreeMap<KeyVersion, (&'static str, i64)>, MasterKeyError> {
    let mut out: BTreeMap<KeyVersion, (&'static str, i64)> = BTreeMap::new();
    for cols in SECRET_COLUMNS {
        let rows: Vec<(i32, i64)> = sqlx::query_as(&format!(
            "SELECT {kv}, count(*) FROM {table} WHERE {ct} IS NOT NULL GROUP BY {kv}",
            kv = cols.key_version,
            table = cols.table,
            ct = cols.ciphertext,
        ))
        .fetch_all(&mut *conn)
        .await?;
        for (version, n) in rows {
            out.entry(KeyVersion(version)).or_insert((cols.table, 0)).1 += n;
        }
    }
    Ok(out)
}

/// Upsert the state row for `key` at `version`.
async fn write_state(
    conn: &mut PgConnection,
    version: KeyVersion,
    key: &MasterKey,
    dev_key: Option<&MasterKey>,
    rotated: bool,
) -> Result<(), MasterKeyError> {
    let check = SecretCipher::single(version, key.clone())
        .encrypt(&SecretString::from(KEY_CHECK_PLAINTEXT))?;
    sqlx::query(
        "INSERT INTO master_key_state (singleton, key_version, check_ciphertext, check_nonce, dev_key) \
         VALUES (true, $1, $2, $3, $4) \
         ON CONFLICT (singleton) DO UPDATE SET \
             key_version = EXCLUDED.key_version, \
             check_ciphertext = EXCLUDED.check_ciphertext, \
             check_nonce = EXCLUDED.check_nonce, \
             dev_key = EXCLUDED.dev_key, \
             rotated_at = CASE WHEN $5 THEN now() ELSE master_key_state.rotated_at END",
    )
    .bind(version.0)
    .bind(&check.ciphertext)
    .bind(&check.nonce)
    .bind(dev_key.map(|k| k.expose_bytes().as_slice()))
    .bind(rotated)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
