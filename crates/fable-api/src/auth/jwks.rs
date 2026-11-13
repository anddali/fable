//! JSON Web Key Set cache for token signature validation (spec §14 A-1).
//!
//! Keys are cached for [`DEFAULT_TTL`] (1 h). A token whose `kid` is not in
//! the cached set triggers an immediate refetch, so Entra key rotation is
//! picked up without waiting for the TTL. Refetches are single-flight and
//! rate-limited by a cooldown so a flood of tokens with made-up `kid`s
//! cannot turn into a flood of requests to the identity provider. When a
//! refetch fails, keys from the previous (expired) set are still served.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

/// How long a fetched key set is trusted before it is refetched.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
/// Minimum time between two fetch attempts (unknown `kid` or failures).
pub const DEFAULT_REFETCH_COOLDOWN: Duration = Duration::from_secs(60);
/// Timeout for one JWKS request.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a signing key could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum JwksError {
    /// The key set was fetched but has no usable key with this `kid`.
    #[error("no signing key with the token's kid")]
    UnknownKid,
    /// The key set could not be fetched and nothing usable is cached.
    #[error("signing keys are unavailable: {0}")]
    Unavailable(String),
}

/// A cached, self-refreshing JWKS.
pub struct JwksCache {
    http: reqwest::Client,
    uri: String,
    ttl: Duration,
    refetch_cooldown: Duration,
    state: RwLock<State>,
    /// Serialises fetches (single flight).
    refresh: Mutex<()>,
}

#[derive(Default)]
struct State {
    keys: Arc<HashMap<String, Arc<DecodingKey>>>,
    /// When the current `keys` were fetched; `None` before the first success.
    fetched_at: Option<Instant>,
    /// When the last fetch was attempted, successful or not.
    last_attempt: Option<Instant>,
}

impl State {
    fn is_fresh(&self, ttl: Duration) -> bool {
        self.fetched_at.is_some_and(|t| t.elapsed() < ttl)
    }

    fn may_fetch(&self, cooldown: Duration) -> bool {
        self.last_attempt.is_none_or(|t| t.elapsed() >= cooldown)
    }
}

impl JwksCache {
    /// A cache for `uri` with the default TTL and cooldown.
    pub fn new(http: reqwest::Client, uri: impl Into<String>) -> Self {
        Self::with_timing(http, uri, DEFAULT_TTL, DEFAULT_REFETCH_COOLDOWN)
    }

    /// A cache with explicit timing (used by tests).
    pub fn with_timing(
        http: reqwest::Client,
        uri: impl Into<String>,
        ttl: Duration,
        refetch_cooldown: Duration,
    ) -> Self {
        Self {
            http,
            uri: uri.into(),
            ttl,
            refetch_cooldown,
            state: RwLock::new(State::default()),
            refresh: Mutex::new(()),
        }
    }

    /// The JWKS endpoint this cache reads.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The decoding key for `kid`, fetching the key set when it is missing,
    /// expired, or does not contain `kid`.
    pub async fn key(&self, kid: &str) -> Result<Arc<DecodingKey>, JwksError> {
        {
            let state = self.state.read().await;
            if state.is_fresh(self.ttl) {
                if let Some(key) = state.keys.get(kid) {
                    return Ok(key.clone());
                }
            }
        }

        let _flight = self.refresh.lock().await;

        // Another task may have refreshed while we waited for the lock.
        let (cached, may_fetch, ever_fetched) = {
            let state = self.state.read().await;
            let cached = state.keys.get(kid).cloned();
            if state.is_fresh(self.ttl) {
                if let Some(key) = cached {
                    return Ok(key);
                }
            }
            (
                cached,
                state.may_fetch(self.refetch_cooldown),
                state.fetched_at.is_some(),
            )
        };

        if !may_fetch {
            return match cached {
                Some(key) => Ok(key),
                None if ever_fetched => Err(JwksError::UnknownKid),
                None => Err(JwksError::Unavailable(
                    "recent fetch failed; retrying after cooldown".to_owned(),
                )),
            };
        }

        let result = self.fetch().await;
        let mut state = self.state.write().await;
        let now = Instant::now();
        state.last_attempt = Some(now);
        match result {
            Ok(keys) => {
                tracing::debug!(uri = %self.uri, keys = keys.len(), "jwks.fetched");
                state.keys = Arc::new(keys);
                state.fetched_at = Some(now);
                state.keys.get(kid).cloned().ok_or(JwksError::UnknownKid)
            }
            Err(reason) => match cached {
                Some(key) => {
                    tracing::warn!(uri = %self.uri, %reason, "jwks.refresh_failed_serving_stale");
                    Ok(key)
                }
                None => {
                    tracing::warn!(uri = %self.uri, %reason, "jwks.fetch_failed");
                    Err(JwksError::Unavailable(reason))
                }
            },
        }
    }

    async fn fetch(&self) -> Result<HashMap<String, Arc<DecodingKey>>, String> {
        let response = self
            .http
            .get(&self.uri)
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("bad status: {e}"))?;
        let set: JwkSet = response
            .json()
            .await
            .map_err(|e| format!("invalid JWKS body: {e}"))?;
        Ok(parse_keys(set))
    }
}

/// The subset of a JWKS we need. Unknown members (`x5c`, `issuer`, …) are
/// ignored.
#[derive(Deserialize)]
struct JwkSet {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

/// Usable RSA signing keys by `kid`. Keys that are not RSA, are marked for
/// encryption, lack a `kid`, or are malformed are skipped individually.
fn parse_keys(set: JwkSet) -> HashMap<String, Arc<DecodingKey>> {
    let mut keys = HashMap::new();
    for jwk in set.keys {
        if jwk.kty != "RSA" || jwk.use_.as_deref().is_some_and(|u| u != "sig") {
            continue;
        }
        let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e) else {
            continue;
        };
        match DecodingKey::from_rsa_components(&n, &e) {
            Ok(key) => {
                keys.insert(kid, Arc::new(key));
            }
            Err(err) => tracing::warn!(%kid, error = %err, "jwks.key_skipped"),
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keys_keeps_only_rsa_signing_keys_with_kid() {
        let set: JwkSet = serde_json::from_value(serde_json::json!({
            "keys": [
                {"kty": "RSA", "kid": "a", "use": "sig", "n": "AQAB", "e": "AQAB", "x5c": ["..."]},
                {"kty": "RSA", "kid": "b", "n": "AQAB", "e": "AQAB"},
                {"kty": "RSA", "kid": "enc", "use": "enc", "n": "AQAB", "e": "AQAB"},
                {"kty": "RSA", "n": "AQAB", "e": "AQAB"},
                {"kty": "EC", "kid": "ec", "crv": "P-256", "x": "AA", "y": "AA"},
                {"kty": "RSA", "kid": "no-n", "e": "AQAB"}
            ]
        }))
        .unwrap();
        let mut kids: Vec<_> = parse_keys(set).into_keys().collect();
        kids.sort();
        assert_eq!(kids, ["a", "b"]);
    }

    #[test]
    fn empty_body_is_an_empty_set() {
        let set: JwkSet = serde_json::from_str("{}").unwrap();
        assert!(parse_keys(set).is_empty());
    }
}
