//! Liveness endpoint used by `fable healthcheck` (spec §2 D-5, §13).

use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// Liveness response.
#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    /// Always `"ok"` while the process serves HTTP.
    pub status: &'static str,
}

/// Process liveness; does not touch the database.
#[utoipa::path(get, path = "/healthz", tag = "health",
    responses((status = 200, description = "Process is alive", body = Health)))]
pub async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Health routes.
pub fn router() -> OpenApiRouter {
    OpenApiRouter::new().routes(routes!(healthz))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let (app, openapi) = crate::router();
        let res = app
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!({"status": "ok"}));
        assert!(openapi.paths.paths.contains_key("/healthz"));
    }
}
