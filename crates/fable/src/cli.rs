//! Command-line interface (spec §2 D-1–D-3, D-5).

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fable_core::{Role, SecretString};

/// Exit code of subcommands that are not implemented yet.
pub const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// Fable RAG platform.
///
/// Without a subcommand, runs the selected role(s): migrations on boot, then
/// the HTTP API and/or the background workers.
#[derive(Debug, Parser)]
#[command(name = "fable", version, about, long_about)]
pub struct Cli {
    /// Which parts to run. Overrides `RAG_ROLE`; default `all`.
    #[arg(long, value_enum)]
    pub role: Option<RoleArg>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Apply database migrations and exit.
    Migrate,
    /// Probe the local `/healthz` endpoint; exit 0 when healthy.
    Healthcheck(HealthcheckArgs),
    /// Run a parse worker child process (internal; started by workers).
    ParseWorker,
    /// Run a retrieval evaluation.
    Eval,
    /// Re-encrypt stored secrets with a new master key.
    ///
    /// Reads the current key from RAG_MASTER_KEY (in `dev` without it, the
    /// stored development key), re-encrypts every secret under the new key
    /// in one transaction, then exits. Afterwards set RAG_MASTER_KEY to the
    /// new key and restart every replica.
    RotateMasterKey(RotateMasterKeyArgs),
}

impl Command {
    /// The subcommand name as typed on the command line.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Migrate => "migrate",
            Command::Healthcheck(_) => "healthcheck",
            Command::ParseWorker => "parse-worker",
            Command::Eval => "eval",
            Command::RotateMasterKey(_) => "rotate-master-key",
        }
    }
}

/// Options for `fable healthcheck`.
#[derive(Debug, Args)]
pub struct HealthcheckArgs {
    /// URL to probe. Default: `/healthz` on the local `RAG_HTTP__BIND` port.
    #[arg(long)]
    pub url: Option<String>,

    /// Request timeout in seconds.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub timeout_secs: u64,
}

impl HealthcheckArgs {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// Options for `fable rotate-master-key`.
#[derive(Debug, Args)]
#[group(id = "new_key_source", required = true, multiple = false)]
pub struct RotateMasterKeyArgs {
    /// The new master key: 32 random bytes, base64 (`openssl rand -base64
    /// 32`). Visible in the process list; prefer --new-key-file.
    #[arg(long, value_name = "BASE64", value_parser = parse_secret, group = "new_key_source")]
    pub new_key: Option<SecretString>,

    /// Read the new master key (base64) from this file.
    #[arg(long, value_name = "PATH", group = "new_key_source")]
    pub new_key_file: Option<PathBuf>,
}

/// Wrap a key argument straight away so `Debug` of the parsed CLI redacts it.
fn parse_secret(s: &str) -> Result<SecretString, std::convert::Infallible> {
    Ok(SecretString::from(s))
}

/// CLI mirror of [`Role`] (kept separate so `fable-core` has no clap dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RoleArg {
    Api,
    Worker,
    All,
}

impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Api => Role::Api,
            RoleArg::Worker => Role::Worker,
            RoleArg::All => Role::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_roles_and_subcommands() {
        let cli = Cli::try_parse_from(["fable", "--role", "worker"]).unwrap();
        assert_eq!(cli.role.map(Role::from), Some(Role::Worker));
        assert!(cli.command.is_none());

        assert!(Cli::try_parse_from(["fable", "--role", "both"]).is_err());

        for (arg, name) in [
            ("migrate", "migrate"),
            ("healthcheck", "healthcheck"),
            ("parse-worker", "parse-worker"),
            ("eval", "eval"),
        ] {
            let cli = Cli::try_parse_from(["fable", arg]).unwrap();
            assert_eq!(cli.command.unwrap().name(), name);
        }
    }

    #[test]
    fn rotate_master_key_needs_exactly_one_key_source() {
        assert!(Cli::try_parse_from(["fable", "rotate-master-key"]).is_err());
        assert!(Cli::try_parse_from([
            "fable",
            "rotate-master-key",
            "--new-key",
            "a",
            "--new-key-file",
            "/k"
        ])
        .is_err());

        let cli = Cli::try_parse_from([
            "fable",
            "rotate-master-key",
            "--new-key",
            "c2VjcmV0LWtleQ==",
        ])
        .unwrap();
        let dump = format!("{cli:?}");
        assert!(!dump.contains("c2VjcmV0LWtleQ"), "{dump}");
        let Some(Command::RotateMasterKey(args)) = cli.command else {
            panic!("expected rotate-master-key");
        };
        assert_eq!(
            fable_core::ExposeSecret::expose_secret(args.new_key.as_ref().unwrap()),
            "c2VjcmV0LWtleQ=="
        );

        let cli =
            Cli::try_parse_from(["fable", "rotate-master-key", "--new-key-file", "/k"]).unwrap();
        assert_eq!(cli.command.unwrap().name(), "rotate-master-key");
    }
}
