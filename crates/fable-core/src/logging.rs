//! Structured logging context (spec §16 N-6).
//!
//! Logs are JSON (configured by the binary). Context travels in `tracing`
//! spans so every event inside a request, job or document carries the same
//! fields. This module fixes the field names so the API, workers and
//! connectors agree on them:
//!
//! | field         | set by                                              |
//! |---------------|-----------------------------------------------------|
//! | `request_id`  | the HTTP request span (fable-api)                   |
//! | `principal`   | auth extractors, via [`record_principal`]           |
//! | `job_id`, `job_kind` | [`job_span`] around each queue job           |
//! | `collection_id`, `document_id` | [`document_span`] while indexing a document |
//!
//! Never put secrets or document content into span fields.

use std::fmt::Display;

use tracing::Span;

/// Field holding the request ID on HTTP request spans.
pub const REQUEST_ID: &str = "request_id";
/// Field holding the authenticated principal on HTTP request spans.
pub const PRINCIPAL: &str = "principal";

/// Record the authenticated principal on the current span.
///
/// Called by the auth extractors once a request is authenticated. Has no
/// effect when the current span does not declare a `principal` field (the
/// HTTP request span declares it empty).
pub fn record_principal(principal: impl Display) {
    Span::current().record(PRINCIPAL, tracing::field::display(principal));
}

/// Span to enter while running a queue job.
pub fn job_span(job_id: impl Display, job_kind: &str) -> Span {
    tracing::info_span!(
        "job",
        job_id = %job_id,
        job_kind = job_kind,
    )
}

/// Span to enter while syncing or indexing one document.
pub fn document_span(collection_id: impl Display, document_id: impl Display) -> Span {
    tracing::info_span!(
        "document",
        collection_id = %collection_id,
        document_id = %document_id,
    )
}
