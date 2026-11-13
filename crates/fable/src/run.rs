//! Running roles (`fable [--role api|worker|all]`) and `fable migrate`.

use anyhow::Context;
use fable_core::{BootstrapConfig, Role};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::cli::RoleArg;

/// `fable migrate`: apply migrations and exit.
pub async fn migrate(config: &BootstrapConfig) -> anyhow::Result<()> {
    let url = config.require_database_url()?;
    let pool = fable_db::connect(url).await?;
    let report = fable_db::migrate(&pool)
        .await
        .context("running database migrations")?;
    if report.applied.is_empty() {
        tracing::info!(
            waited_for_lock = report.waited_for_lock,
            "database schema is up to date"
        );
    } else {
        tracing::info!(count = report.applied.len(), "migrations applied");
    }
    pool.close().await;
    Ok(())
}

/// Default command: migrate on boot, then run the selected role(s) until
/// SIGINT/SIGTERM.
pub async fn run(config: BootstrapConfig, role: Option<RoleArg>) -> anyhow::Result<()> {
    let role: Role = role.map(Role::from).unwrap_or(config.role);
    tracing::info!(%role, "starting fable");

    // Startup migrations are advisory-locked, so replicas may race safely.
    let pool = fable_db::connect(config.require_database_url()?).await?;
    fable_db::migrate(&pool).await?;
    tracing::info!("migrations applied");
    let state = fable_api::AppState::new(pool, http_client()?);

    // Every role uses scratch (uploads on api nodes, fetches on workers):
    // sweep leftovers from a previous run before accepting any work (I-2).
    // Held for the lifetime of `run`; role tasks get clones as they're wired.
    let scratch = fable_worker::ScratchManager::open(&config.scratch)
        .await
        .with_context(|| format!("opening scratch dir {}", config.scratch.dir.display()))?
        .0;
    tracing::info!(
        dir = %scratch.root().display(),
        max_bytes = scratch.max_bytes(),
        "scratch dir ready"
    );

    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();

    if role.runs_api() {
        let bind = config.http.bind;
        let listener = TcpListener::bind(bind)
            .await
            .with_context(|| format!("binding HTTP listener on {bind}"))?;
        tracing::info!(%bind, "HTTP listening");
        let mut rx = stop_rx.clone();
        tasks.spawn(async move {
            fable_api::serve(listener, state, async move {
                let _ = rx.wait_for(|stop| *stop).await;
            })
            .await
            .context("HTTP server")
        });
    }

    if role.runs_worker() {
        // The beat and the sync/index workers are wired in by the queue and
        // worker tickets; until then the worker role idles until shutdown.
        let mut rx = stop_rx.clone();
        tasks.spawn(async move {
            let _ = rx.wait_for(|stop| *stop).await;
            Ok(())
        });
    }

    tokio::select! {
        () = shutdown_signal() => tracing::info!("shutdown requested"),
        Some(res) = tasks.join_next() => {
            // A role exited on its own: treat as fatal and stop the rest.
            let _ = stop_tx.send(true);
            res??;
            anyhow::bail!("a role stopped unexpectedly");
        }
    }

    let _ = stop_tx.send(true);
    while let Some(res) = tasks.join_next().await {
        res??;
    }
    tracing::info!("stopped");
    Ok(())
}

/// The process-wide outbound HTTP client, shared through `AppState`.
fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("fable/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .context("building HTTP client")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}
