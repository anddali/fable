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
use fable_core::BootstrapConfig;

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();
    match real_main(cli) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "fatal");
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn real_main(cli: Cli) -> anyhow::Result<ExitCode> {
    let config = BootstrapConfig::from_env()?;
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

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    // The log level becomes a bootstrap setting (RAG_LOG_LEVEL) with the
    // bootstrap-config work; until then default to `info`.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::new("info"))
        .with_writer(std::io::stderr)
        .init();
}
