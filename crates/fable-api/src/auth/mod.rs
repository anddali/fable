//! Authentication (spec §14).
//!
//! - [`entra`]: Entra ID v2 access-token validation (A-1).
//! - [`jwks`]: the signing-key cache it uses.
//! - [`principal`]: claims → [`fable_core::Principal`] with global role
//!   mapping, and the `disabled`-mode dev principal (A-2, A-6).

pub mod entra;
pub mod jwks;
pub mod principal;

pub use entra::{EntraClaims, EntraSettings, EntraValidator, TokenError};
pub use jwks::{JwksCache, JwksError};
pub use principal::{principal_from_claims, AuthError, AuthSetupError, Authenticator};
