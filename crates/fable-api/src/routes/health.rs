//! Liveness and readiness probes (spec §2 D-5, §13).
//!
//! `/healthz` answers as long as the process serves HTTP; `fable healthcheck`
//! (the container healthcheck) probes it. `/readyz` additionally checks that
//! Postgres answers, for load balancers and orchestrators.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use sqlx::Connection;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

/// Upper bound for the readiness DB probe.
const DB_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Liveness response.
#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    /// Always `"ok"` while the process serves HTTP.
    pub status: &'static str,
}

/// Readiness response.
#[derive(Debug, Serialize, ToSchema)]
pub struct Readiness {
    /// `"ok"` when every check passed, otherwise `"unavailable"`.
    pub status: &'static str,
    /// Database probe result: `"ok"` or `"unavailable"`.
    pub database: &'static str,
}

/// Process liveness; does not touch the database.
#[utoipa::path(get, path = "/healthz", tag = "health",
    responses((status = 200, description = "Process is alive", body = Health)))]
pub async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Readiness: the process is up and Postgres answers.
#[utoipa::path(get, path = "/readyz", tag = "health",
    responses(
        (status = 200, description = "Ready to serve traffic", body = Readiness),
        (status = 503, description = "A dependency is unavailable", body = Readiness),
    ))]
pub async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let probe = async {
        let mut conn = state.db.acquire().await?;
        conn.ping().await
    };
    let db_ok = match tokio::time::timeout(DB_PROBE_TIMEOUT, probe).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness: database probe failed");
            false
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = DB_PROBE_TIMEOUT.as_millis() as u64,
                "readiness: database probe timed out"
            );
            false
        }
    };
    if db_ok {
        (
            StatusCode::OK,
            Json(Readiness {
                status: "ok",
                database: "ok",
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Readiness {
                status: "unavailable",
                database: "unavailable",
            }),
        )
    }
}

/// Health routes.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(healthz))
        .routes(routes!(readyz))
}
