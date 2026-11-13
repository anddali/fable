//! Router-level tests: health probes, request IDs and log context.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use fable_api::{AppState, RequestId};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

/// State whose pool points at a port nothing listens on.
fn unreachable_db_state() -> AppState {
    let db = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy("postgres://u:p@127.0.0.1:1/none")
        .unwrap();
    AppState::new(db, reqwest::Client::new())
}

fn app(state: AppState) -> Router {
    fable_api::router(state).0
}

async fn get(
    app: Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, headers, json)
}

#[tokio::test]
async fn healthz_returns_ok_without_db() {
    let (status, _, json) = get(
        app(unreachable_db_state()),
        Request::get("/healthz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!({"status": "ok"}));
}

#[tokio::test]
async fn readyz_is_503_when_db_is_down() {
    let (status, _, json) = get(
        app(unreachable_db_state()),
        Request::get("/readyz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json,
        serde_json::json!({"status": "unavailable", "database": "unavailable"})
    );
}

/// Needs a Postgres in `DATABASE_URL` (CI service container); skipped otherwise.
#[tokio::test]
async fn readyz_is_ok_when_db_answers() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set; skipping");
        return;
    };
    let db = PgPoolOptions::new().connect_lazy(&url).unwrap();
    let (status, _, json) = get(
        app(AppState::new(db, reqwest::Client::new())),
        Request::get("/readyz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!({"status": "ok", "database": "ok"}));
}

#[test]
fn openapi_lists_health_paths() {
    let (_, openapi) = fable_api::router(unreachable_db_state_blocking());
    assert!(openapi.paths.paths.contains_key("/healthz"));
    assert!(openapi.paths.paths.contains_key("/readyz"));
}

fn unreachable_db_state_blocking() -> AppState {
    // connect_lazy needs a runtime context for the pool's reaper.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { unreachable_db_state() })
}

#[tokio::test]
async fn generates_request_id_when_absent() {
    let (_, headers, _) = get(
        app(unreachable_db_state()),
        Request::get("/healthz").body(Body::empty()).unwrap(),
    )
    .await;
    let id = headers.get("x-request-id").unwrap().to_str().unwrap();
    assert!(uuid::Uuid::parse_str(id).is_ok(), "not a uuid: {id}");
}

#[tokio::test]
async fn keeps_valid_inbound_request_id_and_replaces_invalid_one() {
    let (_, headers, _) = get(
        app(unreachable_db_state()),
        Request::get("/healthz")
            .header("x-request-id", "edge-abc.123")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(headers.get("x-request-id").unwrap(), "edge-abc.123");

    let (_, headers, _) = get(
        app(unreachable_db_state()),
        Request::get("/healthz")
            .header("x-request-id", "bad id with spaces")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let id = headers.get("x-request-id").unwrap().to_str().unwrap();
    assert!(uuid::Uuid::parse_str(id).is_ok(), "not replaced: {id}");
}

#[tokio::test]
async fn request_id_is_available_to_handlers() {
    let router = axum::Router::new()
        .route(
            "/echo",
            axum::routing::get(
                |axum::Extension(id): axum::Extension<RequestId>| async move { id.0 },
            ),
        )
        .layer(axum::middleware::from_fn(fable_api::request_id::layer));
    let res = router
        .oneshot(
            Request::get("/echo")
                .header("x-request-id", "req-42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"req-42");
}
