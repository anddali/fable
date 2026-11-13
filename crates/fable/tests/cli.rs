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
    for sub in ["parse-worker", "eval", "rotate-master-key"] {
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
    let mut server = KillOnDrop(
        StdCommand::new(assert_cmd::cargo::cargo_bin("fable"))
            .env("RAG_DATABASE_URL", &db)
            .env("RAG_HTTP__BIND", &bind)
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
}
