//! Black-box tests of the `fable` binary.

use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;

fn fable() -> Command {
    let mut cmd = Command::cargo_bin("fable").unwrap();
    // Isolate from the developer's environment.
    for (k, _) in std::env::vars() {
        if k.starts_with("RAG_") {
            cmd.env_remove(k);
        }
    }
    cmd
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn help_lists_roles_and_subcommands() {
    fable()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--role"))
        .stdout(predicate::str::contains("migrate"))
        .stdout(predicate::str::contains("healthcheck"))
        .stdout(predicate::str::contains("parse-worker"))
        .stdout(predicate::str::contains("eval"))
        .stdout(predicate::str::contains("rotate-master-key"));
}

#[test]
fn rejects_unknown_role() {
    fable().args(["--role", "both"]).assert().code(2);
}

#[test]
fn rejects_bad_role_env() {
    fable()
        .env("RAG_ROLE", "both")
        .arg("migrate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid bootstrap configuration"));
}

#[test]
fn prod_with_auth_disabled_fails_startup() {
    fable()
        .env("RAG_ENVIRONMENT", "prod")
        .env(
            "RAG_MASTER_KEY",
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
        )
        .env("RAG_AUTH__MODE", "disabled")
        .env("RAG_DATABASE_URL", "postgres://u:p@127.0.0.1:1/none")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "RAG_AUTH__MODE=disabled is not permitted when RAG_ENVIRONMENT=prod",
        ));
}

#[test]
fn missing_master_key_outside_dev_fails_startup() {
    for env in ["staging", "prod"] {
        fable()
            .env("RAG_ENVIRONMENT", env)
            .env("RAG_DATABASE_URL", "postgres://u:p@127.0.0.1:1/none")
            .assert()
            .failure()
            .stderr(predicate::str::contains("RAG_MASTER_KEY is required"));
    }
}

#[test]
fn invalid_master_key_is_not_echoed() {
    fable()
        .env("RAG_MASTER_KEY", "c2VjcmV0LXRvby1zaG9ydA==")
        .arg("migrate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("RAG_MASTER_KEY is invalid"))
        .stderr(predicate::str::contains("c2VjcmV0LXRvby1zaG9ydA==").not());
}

#[test]
fn rejects_bad_log_level() {
    fable()
        .env("RAG_LOG_LEVEL", "fable=[")
        .arg("migrate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid RAG_LOG_LEVEL"));
}

#[test]
fn stub_subcommands_exit_not_implemented() {
    for sub in ["parse-worker", "eval"] {
        fable()
            .arg(sub)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not implemented"));
    }
}

#[test]
fn migrate_requires_database_url() {
    fable()
        .arg("migrate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("RAG_DATABASE_URL is not set"));
}

#[test]
fn migrate_fails_when_database_is_unreachable() {
    fable()
        .env("RAG_DATABASE_URL", "postgres://u:p@127.0.0.1:1/none")
        .arg("migrate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("database error"));
}

/// Several `fable migrate` processes at once (replicas booting together) all
/// succeed. Needs `DATABASE_URL`; skipped otherwise.
#[test]
fn concurrent_migrate_processes_all_succeed() {
    let Ok(db) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set; skipping");
        return;
    };
    let children: Vec<_> = (0..4)
        .map(|_| {
            KillOnDrop(
                StdCommand::new(assert_cmd::cargo::cargo_bin("fable"))
                    .env("RAG_DATABASE_URL", &db)
                    .arg("migrate")
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            )
        })
        .collect();
    for mut child in children {
        let status = child.0.wait().unwrap();
        let mut stderr = String::new();
        std::io::Read::read_to_string(child.0.stderr.as_mut().unwrap(), &mut stderr).unwrap();
        assert!(status.success(), "fable migrate failed: {stderr}");
    }
}

#[test]
fn healthcheck_fails_when_nothing_listens() {
    let port = free_port();
    fable()
        .env("RAG_HTTP__BIND", format!("127.0.0.1:{port}"))
        .args(["healthcheck", "--timeout-secs", "2"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("unhealthy"));
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Needs a Postgres in `DATABASE_URL` (CI service container); skipped otherwise.
#[test]
fn api_role_migrates_serves_healthz_and_stops_on_sigterm() {
    let Ok(db) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set; skipping");
        return;
    };

    fable()
        .env("RAG_DATABASE_URL", &db)
        .arg("migrate")
        .assert()
        .success();

    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let scratch = std::env::temp_dir().join(format!("fable-cli-test-{port}"));
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("leftover-from-crash"), b"stale").unwrap();
    std::fs::write(scratch.join(".fable-scratch"), b"").unwrap();
    let mut server = KillOnDrop(
        StdCommand::new(assert_cmd::cargo::cargo_bin("fable"))
            .env("RAG_DATABASE_URL", &db)
            .env("RAG_HTTP__BIND", &bind)
            .env("RAG_SCRATCH_DIR", &scratch)
            .args(["--role", "api"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ok = fable()
            .env("RAG_HTTP__BIND", &bind)
            .args(["healthcheck", "--timeout-secs", "1"])
            .output()
            .unwrap()
            .status
            .success();
        if ok {
            break;
        }
        assert!(Instant::now() < deadline, "server never became healthy");
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited early"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    #[cfg(unix)]
    {
        let pid = server.0.id().to_string();
        assert!(StdCommand::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(s) = server.0.try_wait().unwrap() {
                break s;
            }
            assert!(Instant::now() < deadline, "server ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(status.success(), "unclean exit: {status}");
    }

    // The startup sweep removed the leftover scratch file (I-2).
    assert!(!scratch.join("leftover-from-crash").exists());
    let _ = std::fs::remove_dir_all(&scratch);
}

/// A fresh database for one test (dropped by the caller); `None` without
/// `DATABASE_URL`.
fn fresh_database(tag: &str) -> Option<(String, String)> {
    use sqlx::ConnectOptions;
    let base = std::env::var("DATABASE_URL").ok()?;
    let name = format!("fable_cli_{tag}_{}", std::process::id());
    let admin: sqlx::postgres::PgConnectOptions = base.parse().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut conn = admin.connect().await.unwrap();
        sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .execute(&mut conn)
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut conn)
            .await
            .unwrap();
    });
    let url = admin.database(&name).to_url_lossy().to_string();
    Some((name, url))
}

fn drop_database(name: &str) {
    use sqlx::{ConnectOptions, Connection};
    let admin: sqlx::postgres::PgConnectOptions =
        std::env::var("DATABASE_URL").unwrap().parse().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut conn = admin.connect().await.unwrap();
        sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.ok();
    });
}

const OLD_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const NEW_KEY: &str = "Hx4dHBsaGRgXFhUUExIREA8ODQwLCgkIBwYFBAMCAQA=";

/// Rotation through the binary, then startup with the old key fails with a
/// clear message. Needs `DATABASE_URL`; skipped otherwise.
#[test]
fn rotate_master_key_then_old_key_fails_startup() {
    let Some((name, db)) = fresh_database("rotate") else {
        eprintln!("DATABASE_URL not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("fable-rotate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key_file = dir.join("new.key");
    std::fs::write(&key_file, format!("{NEW_KEY}\n")).unwrap();

    let env = |cmd: &mut Command, key: &str| {
        cmd.env("RAG_ENVIRONMENT", "staging")
            .env("RAG_DATABASE_URL", &db)
            .env("RAG_MASTER_KEY", key)
            .env("RAG_SCRATCH_DIR", &dir)
            .env("RAG_HTTP__BIND", format!("127.0.0.1:{}", free_port()));
    };

    fable()
        .env("RAG_DATABASE_URL", &db)
        .arg("migrate")
        .assert()
        .success();
    // A secret sealed with the old key (version 1), as the portal would store.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = sqlx::PgPool::connect(&db).await.unwrap();
        let cipher = fable_core::SecretCipher::single(
            fable_core::KeyVersion(1),
            fable_core::MasterKey::from_base64(OLD_KEY).unwrap(),
        );
        let sealed = cipher
            .encrypt(&fable_core::SecretString::from("token"))
            .unwrap();
        sqlx::query(
            "INSERT INTO connections (kind, name, secret_ciphertext, secret_nonce, key_version) \
             VALUES ('confluence', 'c', $1, $2, 1)",
        )
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    });

    // An invalid new key is rejected without echoing it.
    let mut cmd = fable();
    env(&mut cmd, OLD_KEY);
    cmd.args(["rotate-master-key", "--new-key", "c2hvcnQ="])
        .assert()
        .failure()
        .stderr(predicate::str::contains("new master key"))
        .stderr(predicate::str::contains("c2hvcnQ=").not());

    let mut cmd = fable();
    env(&mut cmd, OLD_KEY);
    cmd.arg("rotate-master-key")
        .arg("--new-key-file")
        .arg(&key_file)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Re-encrypted 1 secret(s); master key version 1 -> 2",
        ))
        .stdout(predicate::str::contains(NEW_KEY).not())
        .stderr(predicate::str::contains(NEW_KEY).not())
        .stderr(predicate::str::contains(OLD_KEY).not());

    // Rotating again from the old key is refused: it no longer matches.
    let mut cmd = fable();
    env(&mut cmd, OLD_KEY);
    cmd.args(["rotate-master-key", "--new-key", NEW_KEY])
        .assert()
        .failure()
        .stderr(predicate::str::contains("RAG_MASTER_KEY does not match"));

    let mut cmd = fable();
    env(&mut cmd, OLD_KEY);
    cmd.args(["--role", "api"])
        .timeout(Duration::from_secs(60))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "RAG_MASTER_KEY does not match the master key (version 2)",
        ))
        .stderr(predicate::str::contains(OLD_KEY).not());

    let _ = std::fs::remove_dir_all(&dir);
    drop_database(&name);
}
