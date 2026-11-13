//! HTTP API: axum router, auth extractors, paging, MCP and embedded UI.

#![forbid(unsafe_code)]

use std::future::Future;
use std::io;

use axum::Router;
use tokio::net::TcpListener;
use utoipa::openapi::OpenApi;
use utoipa_axum::router::OpenApiRouter;

pub mod request_id;
pub mod routes;
pub mod state;

pub use request_id::RequestId;
pub use state::AppState;

/// Build the application router and its OpenAPI document.
///
/// Route modules return `OpenApiRouter<AppState>` and are merged here; the
/// request-ID/log-context layer wraps all of them.
pub fn router(state: AppState) -> (Router, OpenApi) {
    let (router, openapi) = OpenApiRouter::new()
        .merge(routes::health::router())
        .split_for_parts();
    let router = router
        .layer(axum::middleware::from_fn(request_id::layer))
        .with_state(state);
    (router, openapi)
}

/// Serve the API on `listener` until `shutdown` resolves.
pub async fn serve<F>(listener: TcpListener, state: AppState, shutdown: F) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (app, _openapi) = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}
