//! Log-output tests. They live in their own test binary, run from a single
//! test function: `tracing` caches callsite interest process-wide, so
//! capturing output while other tests run concurrently without a subscriber
//! is flaky.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

#[test]
fn structured_log_context() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(logs_are_json_with_request_context());
    job_and_document_spans_add_context();
}

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every log line emitted while handling a request carries its request ID,
/// method, path and (once authenticated) principal, as JSON.
async fn logs_are_json_with_request_context() {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let router = axum::Router::new()
        .route(
            "/work",
            axum::routing::get(|| async {
                fable_core::logging::record_principal("user:alice");
                tracing::info!("handler ran");
                "done"
            }),
        )
        .layer(axum::middleware::from_fn(fable_api::request_id::layer));
    router
        .oneshot(
            Request::get("/work?q=secret-query")
                .header("x-request-id", "req-7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!out.contains("secret-query"), "query string leaked: {out}");
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).expect("JSON log line"))
        .collect();
    let handler = lines
        .iter()
        .find(|l| l["fields"]["message"] == "handler ran")
        .expect("handler event");
    assert_eq!(handler["span"]["request_id"], "req-7");
    assert_eq!(handler["span"]["method"], "GET");
    assert_eq!(handler["span"]["path"], "/work");
    assert_eq!(handler["span"]["principal"], "user:alice");
    let done = lines
        .iter()
        .find(|l| l["fields"]["message"] == "request completed")
        .expect("completion event");
    assert_eq!(done["fields"]["status"], 200);
    assert_eq!(done["span"]["request_id"], "req-7");
}

/// Job and document spans nest, so worker log lines carry both contexts.
fn job_and_document_spans_add_context() {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let _job = fable_core::logging::job_span(17, "index").entered();
        let _doc = fable_core::logging::document_span("c-1", "d-2").entered();
        tracing::info!("indexed");
    });
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let line: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    let spans = line["spans"].as_array().unwrap();
    assert_eq!(spans[0]["job_id"], "17");
    assert_eq!(spans[0]["job_kind"], "index");
    assert_eq!(spans[1]["collection_id"], "c-1");
    assert_eq!(spans[1]["document_id"], "d-2");
}
