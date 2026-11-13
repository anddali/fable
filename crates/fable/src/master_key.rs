//! Master key at startup and `fable rotate-master-key` (spec §4 C-7, §5.2 S-4).

use anyhow::Context;
use fable_core::{BootstrapConfig, Environment, ExposeSecret, MasterKey, SecretCipher};
use fable_db::master_key::{self, KeySource};
use sqlx::PgPool;
use zeroize::Zeroizing;

use crate::cli::RotateMasterKeyArgs;

/// Match `RAG_MASTER_KEY` against the database and return the platform's
/// secret cipher. Fails with a message naming the problem when the key is
/// missing or wrong while encrypted secrets exist.
pub async fn resolve(config: &BootstrapConfig, pool: &PgPool) -> anyhow::Result<SecretCipher> {
    let resolved = master_key::resolve(
        pool,
        config.master_key.as_ref(),
        config.environment == Environment::Dev,
    )
    .await
    .context("checking the master key")?;
    if let Some(previous) = resolved.rekeyed_from {
        tracing::warn!(
            previous_key_version = %previous,
            key_version = %resolved.key_version,
            "the master key differs from the one recorded in the database; no secrets are stored, so the new key was adopted"
        );
    }
    match resolved.source {
        KeySource::Env => tracing::info!(
            key_version = %resolved.key_version,
            secrets = resolved.secrets,
            "master key verified"
        ),
        KeySource::DevGenerated => tracing::warn!(
            environment = %config.environment,
            key_version = %resolved.key_version,
            secrets = resolved.secrets,
            "RAG_MASTER_KEY is not set: secrets are encrypted with an auto-generated DEVELOPMENT key stored in the database itself; never run like this outside development (set RAG_MASTER_KEY, and move existing secrets with `fable rotate-master-key`)"
        ),
    }
    Ok(resolved.cipher)
}

/// `fable rotate-master-key`.
pub async fn rotate(config: &BootstrapConfig, args: &RotateMasterKeyArgs) -> anyhow::Result<()> {
    let new_key = read_new_key(args)?;
    let pool = fable_db::connect(config.require_database_url()?).await?;
    // The table may not exist yet on a fresh database.
    fable_db::migrate(&pool)
        .await
        .context("running database migrations")?;
    let report = master_key::rotate(
        &pool,
        config.master_key.as_ref(),
        config.environment == Environment::Dev,
        &new_key,
    )
    .await
    .context("rotating the master key")?;
    pool.close().await;
    tracing::info!(
        old_key_version = %report.old_version,
        new_key_version = %report.new_version,
        reencrypted = report.reencrypted,
        "master key rotated"
    );
    println!(
        "Re-encrypted {} secret(s); master key version {} -> {}.\n\
         Next: set RAG_MASTER_KEY to the new key and restart every replica \
         (running instances still hold the old key and cannot read secrets).",
        report.reencrypted, report.old_version, report.new_version
    );
    Ok(())
}

fn read_new_key(args: &RotateMasterKeyArgs) -> anyhow::Result<MasterKey> {
    let parsed = match (&args.new_key, &args.new_key_file) {
        (Some(key), _) => MasterKey::from_base64(key.expose_secret()),
        (None, Some(path)) => {
            let text = Zeroizing::new(
                std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?,
            );
            MasterKey::from_base64(&text)
        }
        (None, None) => anyhow::bail!("--new-key or --new-key-file is required"),
    };
    parsed.map_err(|err| {
        anyhow::anyhow!(
            "new master key: {err} (expected 32 random bytes, base64-encoded, e.g. `openssl rand -base64 32`)"
        )
    })
}
