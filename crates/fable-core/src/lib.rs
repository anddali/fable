//! Bootstrap config, shared types, errors, RBAC, secrets and plugin traits.
//!
//! This crate is the only place in the workspace that reads environment
//! variables (see [`config`]).

#![forbid(unsafe_code)]

pub mod config;
pub mod role;

pub use config::{AuthMode, BootstrapConfig, ConfigError, Environment, MasterKey};
pub use role::Role;
