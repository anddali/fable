//! HTTP API: axum router, auth extractors, paging, MCP and embedded UI.

#![forbid(unsafe_code)]

use std::future::Future;
use std::io;

use axum::Router;
use tokio::net::TcpListener;
use utoipa::openapi::OpenApi;
use utoipa_axum::router::OpenApiRouter;

pub mod auth;
pub mod routes;

/// Build the application router and its OpenAPI document.
pub fn router() -> (Router, OpenApi) {
    OpenApiRouter::new()
        .merge(routes::health::router())
        .split_for_parts()
}

/// Serve the API on `listener` until `shutdown` resolves.
pub async fn serve<F>(listener: TcpListener, shutdown: F) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (app, _openapi) = router();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}
