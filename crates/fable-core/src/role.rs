//! Process roles (spec §2 D-1, D-2).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Which parts of the platform this process runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// HTTP only: REST API, `/mcp`, portal UI, health endpoints.
    Api,
    /// Background only: beat, sync and index workers.
    Worker,
    /// Everything in one process (the default).
    #[default]
    All,
}

impl Role {
    /// All roles, in CLI display order.
    pub const ALL: [Role; 3] = [Role::Api, Role::Worker, Role::All];

    /// Stable lowercase name, as accepted on the CLI and in `RAG_ROLE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Api => "api",
            Role::Worker => "worker",
            Role::All => "all",
        }
    }

    /// Whether this role serves HTTP.
    pub fn runs_api(self) -> bool {
        matches!(self, Role::Api | Role::All)
    }

    /// Whether this role runs background workers.
    pub fn runs_worker(self) -> bool {
        matches!(self, Role::Worker | Role::All)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for an unknown role name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role {0:?}; expected one of: api, worker, all")]
pub struct ParseRoleError(String);

impl FromStr for Role {
    type Err = ParseRoleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Role::ALL
            .into_iter()
            .find(|r| r.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| ParseRoleError(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_roles() {
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
            assert_eq!(role.to_string(), role.as_str());
        }
        assert_eq!("API".parse::<Role>().unwrap(), Role::Api);
        assert!("both".parse::<Role>().is_err());
    }

    #[test]
    fn role_capabilities() {
        assert!(Role::Api.runs_api() && !Role::Api.runs_worker());
        assert!(!Role::Worker.runs_api() && Role::Worker.runs_worker());
        assert!(Role::All.runs_api() && Role::All.runs_worker());
        assert_eq!(Role::default(), Role::All);
    }
}
