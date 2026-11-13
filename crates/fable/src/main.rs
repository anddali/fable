//! The `fable` binary: one process for the REST API, `/mcp`, the portal and
//! the background workers, plus ops subcommands (spec §2).

#![forbid(unsafe_code)]

mod cli;
mod healthcheck;
mod run;

// Link the connector crates enabled by Cargo features so their plugin
// registrations are part of the binary.
#[cfg(feature = "connector-confluence")]
use connector_confluence as _;
#[cfg(feature = "connector-git")]
use connector_git as _;
#[cfg(feature = "connector-sharepoint")]
use connector_sharepoint as _;
#[cfg(feature = "connector-slack")]
use connector_slack as _;
#[cfg(feature = "connector-upload")]
use connector_upload as _;
// Library crates not yet wired into a code path.
use fable_models as _;
use fable_parsers as _;
use fable_pipeline as _;
use fable_queue as _;
use fable_search as _;
use fable_worker as _;

use std::process::ExitCode;

use clap::Parser;
use fable_core::{AuthMode, BootstrapConfig};

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Configuration errors (including the prod guards, spec §4 C-6/C-7) are
    // startup failures. Tracing is not set up yet: report on stderr.
    let config = match BootstrapConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = init_tracing(&config.log_level) {
        eprintln!("error: invalid RAG_LOG_LEVEL: {err}");
        return ExitCode::FAILURE;
    }
    // The container healthcheck runs every few seconds; keep its output to
    // the probe result.
    if !matches!(cli.command, Some(Command::Healthcheck(_))) {
        log_config_warnings(&config);
    }
    match real_main(cli, config) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "fatal");
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn real_main(cli: Cli, config: BootstrapConfig) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match cli.command {
        None => runtime.block_on(run::run(config, cli.role))?,
        Some(Command::Migrate) => runtime.block_on(run::migrate(&config))?,
        Some(Command::Healthcheck(args)) => {
            return Ok(runtime.block_on(healthcheck::healthcheck(&config, &args)));
        }
        Some(cmd @ (Command::ParseWorker | Command::Eval | Command::RotateMasterKey)) => {
            eprintln!("fable {}: not implemented yet", cmd.name());
            return Ok(ExitCode::from(cli::EXIT_NOT_IMPLEMENTED));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn init_tracing(log_level: &str) -> anyhow::Result<()> {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(log_level)?;
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    Ok(())
}

/// Loud warnings for configurations that are allowed but unsafe.
fn log_config_warnings(config: &BootstrapConfig) {
    if config.auth.mode == AuthMode::Disabled {
        tracing::warn!(
            environment = %config.environment,
            "RAG_AUTH__MODE=disabled: every request runs as the local dev principal; never use this outside development"
        );
    }
    if config.master_key.is_none() {
        // Only reachable in `dev` (C-7); the DB-stored dev key is managed by
        // the secrets module.
        tracing::warn!(
            environment = %config.environment,
            "RAG_MASTER_KEY is not set: a generated development key stored in the database will encrypt secrets; set RAG_MASTER_KEY for any non-dev deployment"
        );
    }
}
