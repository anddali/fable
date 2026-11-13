//! Encryption at rest for portal-managed secrets (spec §5.2 S-4).
//!
//! Every secret value (connection credentials, API keys for model
//! providers, …) is sealed with AES-256-GCM under the master key
//! (`RAG_MASTER_KEY`), with a fresh random 96-bit nonce per value. The
//! ciphertext, the nonce and the `key_version` of the master key that
//! sealed it are stored side by side, so `fable rotate-master-key` can
//! re-encrypt everything under a new key.
//!
//! Plaintext only ever exists as a [`SecretString`] (zeroized on drop, with a
//! redacting `Debug` and no `Display`). Nothing in this module puts key
//! material or plaintext into an error message.
//!
//! Key lookup goes through the [`MasterKeyProvider`] seam, resolved by
//! [`KeyVersion`]. v1 has one implementation, [`Keyring`], fed from the env
//! var by the bootstrap config (decision FAB-142); a KMS / Key Vault provider
//! can be added later without a schema change.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

pub use secrecy::{ExposeSecret, SecretString};

/// Length of a master key in bytes (AES-256).
pub const MASTER_KEY_LEN: usize = 32;

/// Length of an AES-GCM nonce in bytes.
pub const NONCE_LEN: usize = 12;

/// Domain-separation prefix for the associated data. The key version is
/// appended, so a ciphertext cannot be relabelled with another version.
const AAD_PREFIX: &[u8] = b"fable:secret:v1:";

/// Version of the master key that sealed a value (the `key_version`
/// column, Postgres `INT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyVersion(pub i32);

impl KeyVersion {
    /// The version used for the first master key of an installation.
    pub const INITIAL: KeyVersion = KeyVersion(1);

    /// The version a rotation to a new key should use.
    pub fn next(self) -> KeyVersion {
        KeyVersion(self.0 + 1)
    }
}

impl fmt::Display for KeyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Errors from key parsing, encryption and decryption. Never carries key
/// material or plaintext.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The master key could not be parsed.
    #[error("invalid master key: {0}")]
    InvalidKey(&'static str),
    /// No key with this version is available to the provider.
    #[error("no master key available for key_version {0}")]
    UnknownKeyVersion(KeyVersion),
    /// A stored nonce has the wrong length.
    #[error("stored secret has an invalid nonce length {0} (expected {NONCE_LEN})")]
    InvalidNonce(usize),
    /// Authentication failed: wrong key or tampered/corrupt ciphertext.
    #[error(
        "failed to decrypt secret sealed with key_version {0}: wrong master key or corrupt data"
    )]
    Decrypt(KeyVersion),
    /// Encryption failed (only possible for absurdly large inputs).
    #[error("failed to encrypt secret")]
    Encrypt,
    /// Decrypted bytes are not valid UTF-8.
    #[error("decrypted secret is not valid UTF-8")]
    NotUtf8,
}

/// A 256-bit master key. Zeroized on drop; `Debug` is redacted.
#[derive(Clone)]
pub struct MasterKey(Zeroizing<[u8; MASTER_KEY_LEN]>);

impl MasterKey {
    /// Wrap raw key bytes.
    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Parse a base64-encoded 32-byte key (standard or URL-safe alphabet,
    /// padded or not; surrounding whitespace is ignored).
    pub fn from_base64(encoded: &str) -> Result<Self, CryptoError> {
        let encoded = encoded.trim();
        if encoded.is_empty() {
            return Err(CryptoError::InvalidKey("it is empty"));
        }
        let decoded = [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
            .iter()
            .find_map(|engine| engine.decode(encoded).ok())
            .map(Zeroizing::new)
            .ok_or(CryptoError::InvalidKey("it is not valid base64"))?;
        if decoded.len() != MASTER_KEY_LEN {
            return Err(CryptoError::InvalidKey(
                "it must decode to exactly 32 bytes",
            ));
        }
        let mut bytes = [0u8; MASTER_KEY_LEN];
        bytes.copy_from_slice(&decoded);
        let key = Self::from_bytes(bytes);
        bytes.zeroize();
        Ok(key)
    }

    /// Generate a fresh random key from the OS CSPRNG (used for the `dev`
    /// auto-generated key, C-7, and by key rotation tooling).
    pub fn generate() -> Self {
        let key = Aes256Gcm::generate_key(OsRng);
        let mut bytes = [0u8; MASTER_KEY_LEN];
        bytes.copy_from_slice(&key);
        let out = Self::from_bytes(bytes);
        bytes.zeroize();
        out
    }

    /// The raw key bytes. Callers must not log or persist them.
    pub fn expose_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.0
    }

    /// The key as standard padded base64, the `RAG_MASTER_KEY` format.
    pub fn to_base64(&self) -> SecretString {
        SecretString::from(STANDARD.encode(self.0.as_slice()))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(self.0.as_slice()))
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

/// Resolves master keys by version. Implementations must be cheap to call;
/// a remote (KMS) provider should cache unwrapped keys.
pub trait MasterKeyProvider: Send + Sync {
    /// The version new values are sealed with.
    fn current_version(&self) -> KeyVersion;

    /// The key for `version`, if this provider has it.
    fn key(&self, version: KeyVersion) -> Option<&MasterKey>;
}

/// In-memory set of master keys: one current key plus, during a rotation,
/// the previous keys needed to open older values.
#[derive(Debug, Clone)]
pub struct Keyring {
    current: KeyVersion,
    keys: BTreeMap<KeyVersion, MasterKey>,
}

impl Keyring {
    /// A keyring with a single key, which is also the current one.
    pub fn new(version: KeyVersion, key: MasterKey) -> Self {
        Self {
            current: version,
            keys: BTreeMap::from([(version, key)]),
        }
    }

    /// Add a key that can still decrypt but is not used for new values.
    /// Replaces an existing non-current key with the same version.
    pub fn with_decrypt_only(mut self, version: KeyVersion, key: MasterKey) -> Self {
        if version != self.current {
            self.keys.insert(version, key);
        }
        self
    }

    /// Versions this keyring can decrypt, ascending.
    pub fn versions(&self) -> impl Iterator<Item = KeyVersion> + '_ {
        self.keys.keys().copied()
    }
}

impl MasterKeyProvider for Keyring {
    fn current_version(&self) -> KeyVersion {
        self.current
    }

    fn key(&self, version: KeyVersion) -> Option<&MasterKey> {
        self.keys.get(&version)
    }
}

/// A sealed secret as stored in a row: `secret_ciphertext`
/// (ciphertext ‖ 16-byte GCM tag), `secret_nonce` and `key_version`.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedSecret {
    /// Ciphertext followed by the authentication tag.
    pub ciphertext: Vec<u8>,
    /// The 12-byte nonce, unique per value.
    pub nonce: Vec<u8>,
    /// Version of the master key that sealed this value.
    pub key_version: KeyVersion,
}

impl fmt::Debug for EncryptedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptedSecret")
            .field("ciphertext_len", &self.ciphertext.len())
            .field("key_version", &self.key_version)
            .finish_non_exhaustive()
    }
}

/// Seals and opens secrets with keys from a [`MasterKeyProvider`].
/// Cheap to clone; share one per process.
#[derive(Clone)]
pub struct SecretCipher {
    provider: Arc<dyn MasterKeyProvider>,
}

impl fmt::Debug for SecretCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretCipher")
            .field("current_version", &self.provider.current_version())
            .finish_non_exhaustive()
    }
}

impl SecretCipher {
    /// Build a cipher over any key provider.
    pub fn new(provider: Arc<dyn MasterKeyProvider>) -> Self {
        Self { provider }
    }

    /// Convenience: a cipher over a single-key [`Keyring`].
    pub fn single(version: KeyVersion, key: MasterKey) -> Self {
        Self::new(Arc::new(Keyring::new(version, key)))
    }

    /// The version new values are sealed with.
    pub fn current_version(&self) -> KeyVersion {
        self.provider.current_version()
    }

    /// Seal `plaintext` under the current key with a fresh random nonce.
    pub fn encrypt(&self, plaintext: &SecretString) -> Result<EncryptedSecret, CryptoError> {
        let version = self.provider.current_version();
        let key = self
            .provider
            .key(version)
            .ok_or(CryptoError::UnknownKeyVersion(version))?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let aad = aad(version);
        let ciphertext = key
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.expose_secret().as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Encrypt)?;
        Ok(EncryptedSecret {
            ciphertext,
            nonce: nonce.to_vec(),
            key_version: version,
        })
    }

    /// Open a sealed value with the key of its `key_version`.
    pub fn decrypt(&self, sealed: &EncryptedSecret) -> Result<SecretString, CryptoError> {
        let version = sealed.key_version;
        let key = self
            .provider
            .key(version)
            .ok_or(CryptoError::UnknownKeyVersion(version))?;
        if sealed.nonce.len() != NONCE_LEN {
            return Err(CryptoError::InvalidNonce(sealed.nonce.len()));
        }
        let aad = aad(version);
        let plaintext = key
            .cipher()
            .decrypt(
                Nonce::from_slice(&sealed.nonce),
                Payload {
                    msg: &sealed.ciphertext,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| CryptoError::Decrypt(version))?;
        let text = std::str::from_utf8(&plaintext).map_err(|_| CryptoError::NotUtf8)?;
        Ok(SecretString::from(text))
    }

    /// Re-seal a value under the current key (used by key rotation).
    /// Returns the input unchanged if it already uses the current version.
    pub fn reencrypt(&self, sealed: &EncryptedSecret) -> Result<EncryptedSecret, CryptoError> {
        if sealed.key_version == self.provider.current_version() {
            // Still authenticate it, so rotation surfaces corrupt rows.
            self.decrypt(sealed)?;
            return Ok(sealed.clone());
        }
        let plaintext = self.decrypt(sealed)?;
        self.encrypt(&plaintext)
    }
}

fn aad(version: KeyVersion) -> [u8; AAD_PREFIX.len() + 4] {
    let mut out = [0u8; AAD_PREFIX.len() + 4];
    out[..AAD_PREFIX.len()].copy_from_slice(AAD_PREFIX);
    out[AAD_PREFIX.len()..].copy_from_slice(&version.0.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAINTEXT: &str = "s3cr3t-token-value";

    fn secret(s: &str) -> SecretString {
        SecretString::from(s)
    }

    fn cipher_v1() -> (SecretCipher, MasterKey) {
        let key = MasterKey::generate();
        (SecretCipher::single(KeyVersion(1), key.clone()), key)
    }

    #[test]
    fn round_trip() {
        let (cipher, _) = cipher_v1();
        let sealed = cipher.encrypt(&secret(PLAINTEXT)).unwrap();
        assert_eq!(sealed.key_version, KeyVersion(1));
        assert_eq!(sealed.nonce.len(), NONCE_LEN);
        assert_eq!(sealed.ciphertext.len(), PLAINTEXT.len() + 16);
        assert!(!sealed
            .ciphertext
            .windows(PLAINTEXT.len())
            .any(|w| w == PLAINTEXT.as_bytes()));
        assert_eq!(cipher.decrypt(&sealed).unwrap().expose_secret(), PLAINTEXT);
    }

    #[test]
    fn empty_and_unicode_round_trip() {
        let (cipher, _) = cipher_v1();
        for s in ["", "pässwörd 🔑", &"x".repeat(10_000)] {
            let sealed = cipher.encrypt(&secret(s)).unwrap();
            assert_eq!(cipher.decrypt(&sealed).unwrap().expose_secret(), s);
        }
    }

    #[test]
    fn fresh_nonce_per_value() {
        let (cipher, _) = cipher_v1();
        let a = cipher.encrypt(&secret(PLAINTEXT)).unwrap();
        let b = cipher.encrypt(&secret(PLAINTEXT)).unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn wrong_key_fails_without_leaking() {
        let (cipher, _) = cipher_v1();
        let sealed = cipher.encrypt(&secret(PLAINTEXT)).unwrap();
        let (other, _) = cipher_v1();
        let err = other.decrypt(&sealed).unwrap_err();
        assert_eq!(err, CryptoError::Decrypt(KeyVersion(1)));
        assert!(!err.to_string().contains(PLAINTEXT));
    }

    #[test]
    fn tampering_is_detected() {
        let (cipher, _) = cipher_v1();
        let sealed = cipher.encrypt(&secret(PLAINTEXT)).unwrap();

        let mut flipped = sealed.clone();
        flipped.ciphertext[0] ^= 1;
        assert!(matches!(
            cipher.decrypt(&flipped),
            Err(CryptoError::Decrypt(_))
        ));

        let mut bad_nonce = sealed.clone();
        bad_nonce.nonce[0] ^= 1;
        assert!(matches!(
            cipher.decrypt(&bad_nonce),
            Err(CryptoError::Decrypt(_))
        ));

        let mut short_nonce = sealed.clone();
        short_nonce.nonce.truncate(8);
        assert_eq!(
            cipher.decrypt(&short_nonce).unwrap_err(),
            (CryptoError::InvalidNonce(8))
        );

        let mut truncated = sealed;
        truncated.ciphertext.truncate(4);
        assert!(matches!(
            cipher.decrypt(&truncated),
            Err(CryptoError::Decrypt(_))
        ));
    }

    #[test]
    fn key_version_is_authenticated() {
        // Same key registered under two versions: relabelling the version
        // must still fail because it is bound into the associated data.
        let key = MasterKey::generate();
        let ring = Keyring::new(KeyVersion(2), key.clone()).with_decrypt_only(KeyVersion(1), key);
        let cipher = SecretCipher::new(Arc::new(ring));
        let mut sealed = cipher.encrypt(&secret(PLAINTEXT)).unwrap();
        sealed.key_version = KeyVersion(1);
        assert_eq!(
            cipher.decrypt(&sealed).unwrap_err(),
            (CryptoError::Decrypt(KeyVersion(1)))
        );
    }

    #[test]
    fn unknown_version() {
        let (cipher, _) = cipher_v1();
        let mut sealed = cipher.encrypt(&secret(PLAINTEXT)).unwrap();
        sealed.key_version = KeyVersion(7);
        assert_eq!(
            cipher.decrypt(&sealed).unwrap_err(),
            (CryptoError::UnknownKeyVersion(KeyVersion(7)))
        );
    }

    #[test]
    fn rotation_reencrypts_under_new_version() {
        let (old_cipher, old_key) = cipher_v1();
        let sealed = old_cipher.encrypt(&secret(PLAINTEXT)).unwrap();

        let ring = Keyring::new(KeyVersion(1).next(), MasterKey::generate())
            .with_decrypt_only(KeyVersion(1), old_key);
        assert_eq!(
            ring.versions().collect::<Vec<_>>(),
            [KeyVersion(1), KeyVersion(2)]
        );
        let rotating = SecretCipher::new(Arc::new(ring.clone()));
        let rotated = rotating.reencrypt(&sealed).unwrap();
        assert_eq!(rotated.key_version, KeyVersion(2));
        assert_eq!(
            rotating.decrypt(&rotated).unwrap().expose_secret(),
            PLAINTEXT
        );

        // Already-current values are kept as is (after authenticating them).
        assert_eq!(rotating.reencrypt(&rotated).unwrap(), rotated);
        let mut corrupt = rotated.clone();
        corrupt.ciphertext[0] ^= 1;
        assert!(rotating.reencrypt(&corrupt).is_err());

        // After rotation only the new key is needed.
        let new_only = SecretCipher::new(Arc::new(Keyring::new(
            KeyVersion(2),
            ring.key(KeyVersion(2)).unwrap().clone(),
        )));
        assert_eq!(
            new_only.decrypt(&rotated).unwrap().expose_secret(),
            PLAINTEXT
        );
        assert_eq!(
            new_only.decrypt(&sealed).unwrap_err(),
            (CryptoError::UnknownKeyVersion(KeyVersion(1)))
        );
    }

    #[test]
    fn decrypt_only_cannot_replace_current() {
        let current = MasterKey::generate();
        let ring = Keyring::new(KeyVersion(1), current.clone())
            .with_decrypt_only(KeyVersion(1), MasterKey::generate());
        let sealed = SecretCipher::single(KeyVersion(1), current)
            .encrypt(&secret(PLAINTEXT))
            .unwrap();
        assert!(SecretCipher::new(Arc::new(ring)).decrypt(&sealed).is_ok());
    }

    #[test]
    fn base64_key_parsing() {
        let key = MasterKey::generate();
        let encoded = key.to_base64();
        let parsed = MasterKey::from_base64(encoded.expose_secret()).unwrap();
        let sealed = SecretCipher::single(KeyVersion(1), key)
            .encrypt(&secret(PLAINTEXT))
            .unwrap();
        assert_eq!(
            SecretCipher::single(KeyVersion(1), parsed)
                .decrypt(&sealed)
                .unwrap()
                .expose_secret(),
            PLAINTEXT
        );

        // Other alphabets / padding and surrounding whitespace.
        let raw = [0xfbu8; 32];
        for encoded in [
            STANDARD.encode(raw),
            STANDARD_NO_PAD.encode(raw),
            URL_SAFE.encode(raw),
            format!("  {}\n", URL_SAFE_NO_PAD.encode(raw)),
        ] {
            MasterKey::from_base64(&encoded).unwrap();
        }

        assert_eq!(
            MasterKey::from_base64("").unwrap_err(),
            CryptoError::InvalidKey("it is empty")
        );
        assert_eq!(
            MasterKey::from_base64("not base64!!").unwrap_err(),
            CryptoError::InvalidKey("it is not valid base64")
        );
        let short = STANDARD.encode([1u8; 16]);
        let err = MasterKey::from_base64(&short).unwrap_err();
        assert_eq!(
            err,
            CryptoError::InvalidKey("it must decode to exactly 32 bytes")
        );
        assert!(!err.to_string().contains(&short));
    }

    #[test]
    fn nothing_secret_in_debug() {
        let key = MasterKey::from_bytes([0x41; 32]);
        let encoded = key.to_base64();
        let cipher = SecretCipher::single(KeyVersion(3), key.clone());
        let plaintext = secret(PLAINTEXT);
        let sealed = cipher.encrypt(&plaintext).unwrap();
        let ring = Keyring::new(KeyVersion(3), key.clone());

        let dumps = [
            format!("{key:?}"),
            format!("{cipher:?}"),
            format!("{plaintext:?}"),
            format!("{sealed:?}"),
            format!("{ring:?}"),
            format!("{encoded:?}"),
        ];
        for dump in &dumps {
            assert!(!dump.contains(PLAINTEXT), "{dump}");
            assert!(!dump.contains(encoded.expose_secret()), "{dump}");
            assert!(!dump.contains("AAAA"), "{dump}");
            assert!(!dump.contains("65, 65"), "{dump}");
        }
        assert!(dumps[3].contains("key_version"));
    }
}
