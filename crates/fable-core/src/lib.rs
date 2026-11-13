//! Bootstrap config, shared types, errors, RBAC, secrets and plugin traits.
//!
//! This crate is the only place in the workspace that reads environment
//! variables (see [`config`]). Secrets are encrypted at rest with
//! [`secrets::SecretCipher`].

#![forbid(unsafe_code)]

pub mod block;
pub mod config;
pub mod item;
pub mod logging;
pub mod plugin;
pub mod principal;
pub mod rbac;
pub mod role;
pub mod secrets;

pub use block::{Block, BlockSpan, BlockType, ChunkDraft, ImageRef, Metadata, ParsedDocument};
pub use config::{AuthMode, BootstrapConfig, ConfigError, Environment};
pub use item::ItemRef;
pub use principal::{AuthMethod, GlobalRole, Principal, RoleMap};
pub use rbac::{
    can_create_collections, can_use_connection, effective_collection_role, require_collection_role,
    AclEntry, CollectionAccess, CollectionRole, PrincipalType, RbacError, UsableBy, UsableByError,
    Visibility,
};
pub use role::Role;
pub use secrets::{
    CryptoError, EncryptedSecret, ExposeSecret, KeyVersion, Keyring, MasterKey, MasterKeyProvider,
    SecretCipher, SecretString,
};
