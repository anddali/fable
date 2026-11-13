//! Request IDs and per-request log context (spec §16 N-6).
//!
//! Every request gets an ID: an inbound `x-request-id` is kept when it is a
//! short printable token, otherwise a UUID is generated. The ID is echoed in
//! the `x-request-id` response header, stored as a [`RequestId`] request
//! extension and recorded on a `request` span together with the method and
//! path, so every log line emitted while handling the request carries it.
//! The auth extractors add the principal through
//! [`fable_core::logging::record_principal`].

use std::time::Instant;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

/// The request/response header carrying the request ID.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Longest inbound request ID we accept.
const MAX_LEN: usize = 128;

/// The ID of the current request (request extension).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn accept_inbound(value: &HeaderValue) -> Option<String> {
    let s = value.to_str().ok()?;
    let ok = !s.is_empty()
        && s.len() <= MAX_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    ok.then(|| s.to_owned())
}

/// Liveness/readiness probes are polled constantly: log them at debug only.
fn is_probe(path: &str) -> bool {
    matches!(path, "/healthz" | "/readyz")
}

/// Middleware assigning the request ID and wrapping the request in its span.
pub async fn layer(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(accept_inbound)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let header = HeaderValue::from_str(&id).expect("validated or generated ID is a valid header");
    req.headers_mut()
        .insert(X_REQUEST_ID.clone(), header.clone());
    req.extensions_mut().insert(RequestId(id.clone()));

    // Path only: query strings may carry search text.
    let path = req.uri().path().to_owned();
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        method = %req.method(),
        path = %path,
        principal = tracing::field::Empty,
    );

    let started = Instant::now();
    let mut res = next.run(req).instrument(span.clone()).await;
    let status = res.status().as_u16();
    let latency_ms = started.elapsed().as_millis() as u64;
    span.in_scope(|| {
        if res.status().is_server_error() {
            tracing::error!(status, latency_ms, "request failed");
        } else if is_probe(&path) {
            tracing::debug!(status, latency_ms, "request completed");
        } else {
            tracing::info!(status, latency_ms, "request completed");
        }
    });
    res.headers_mut().insert(X_REQUEST_ID.clone(), header);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_ids_are_validated() {
        let ok = |s: &str| accept_inbound(&HeaderValue::from_str(s).unwrap());
        assert_eq!(ok("abc-123_X.y:z").as_deref(), Some("abc-123_X.y:z"));
        assert_eq!(ok(""), None);
        assert_eq!(ok("has space"), None);
        assert_eq!(ok("quote\"inject"), None);
        assert_eq!(ok(&"a".repeat(MAX_LEN)).map(|s| s.len()), Some(MAX_LEN));
        assert_eq!(ok(&"a".repeat(MAX_LEN + 1)), None);
    }
}
