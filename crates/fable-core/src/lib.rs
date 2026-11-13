//! Bootstrap config, shared types, errors, RBAC, secrets and plugin traits.
//!
//! This crate is the only place in the workspace that reads environment
//! variables (see [`config`]).

#![forbid(unsafe_code)]

pub mod block;
pub mod config;
pub mod item;
pub mod plugin;
pub mod role;

pub use block::{Block, BlockSpan, BlockType, ChunkDraft, ImageRef, Metadata, ParsedDocument};
pub use config::{AuthMode, BootstrapConfig, ConfigError, Environment, MasterKey};
pub use item::ItemRef;
pub use role::Role;
