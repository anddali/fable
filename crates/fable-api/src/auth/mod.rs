//! Authentication (spec §14).
//!
//! - [`entra`]: Entra ID v2 access-token validation (A-1).
//! - [`jwks`]: the signing-key cache it uses.

pub mod entra;
pub mod jwks;

pub use entra::{EntraClaims, EntraSettings, EntraValidator, TokenError};
pub use jwks::{JwksCache, JwksError};
