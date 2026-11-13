//! Bootstrap config, shared types, errors, RBAC, secrets and plugin traits.
//!
//! This crate is the only place in the workspace that reads environment
//! variables (see [`config`]). Secrets are encrypted at rest with
//! [`secrets::SecretCipher`].

#![forbid(unsafe_code)]

pub mod config;
pub mod role;
pub mod secrets;

pub use config::{AuthMode, BootstrapConfig, ConfigError, Environment};
pub use role::Role;
pub use secrets::{
    CryptoError, EncryptedSecret, ExposeSecret, KeyVersion, Keyring, MasterKey, MasterKeyProvider,
    SecretCipher, SecretString,
};
