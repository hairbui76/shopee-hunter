//! Local health/metrics HTTP service (admin endpoints arrive in Phase 21).
//! Must only ever bind to localhost or a private interface.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::json;
use shopee_hunter_observability::{HealthRegistry, Metrics, ServiceState};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ApiState {
    pub health: HealthRegistry,
    pub metrics: Metrics,
    pub started_at: DateTime<Utc>,
    pub version: &'static str,
    pub metrics_enabled: bool,
}

pub type SharedApiState = Arc<ApiState>;

pub fn router(state: SharedApiState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/health/details", get(details))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn live() -> impl IntoResponse {
    StatusCode::OK
}

async fn ready(State(state): State<SharedApiState>) -> impl IntoResponse {
    match state.health.overall() {
        ServiceState::Failed => (StatusCode::SERVICE_UNAVAILABLE, "failed"),
        _ => (StatusCode::OK, "ready"),
    }
}

async fn details(State(state): State<SharedApiState>) -> impl IntoResponse {
    Json(json!({
        "version": state.version,
        "started_at": state.started_at,
        "uptime_secs": (Utc::now() - state.started_at).num_seconds(),
        "overall": state.health.overall(),
        "services": state.health.snapshot(),
    }))
}

async fn metrics(State(state): State<SharedApiState>) -> impl IntoResponse {
    if !state.metrics_enabled {
        return (StatusCode::NOT_FOUND, String::new());
    }
    (StatusCode::OK, state.metrics.render_prometheus())
}

/// Serve the API until the cancellation token fires.
pub async fn serve(
    bind: SocketAddr,
    state: SharedApiState,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(event = "api_started", addr = %bind);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await?;
    tracing::info!(event = "api_stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt as _;

    fn state() -> SharedApiState {
        Arc::new(ApiState {
            health: HealthRegistry::new(),
            metrics: Metrics::new(),
            started_at: Utc::now(),
            version: "test",
            metrics_enabled: true,
        })
    }

    #[tokio::test]
    async fn live_is_ok_and_ready_reflects_health() {
        let s = state();
        let app = router(Arc::clone(&s));
        let res = app
            .clone()
            .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        s.health.handle("w").mark_failure("db down");
        let res = app
            .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn metrics_endpoint_renders_prometheus_text() {
        let s = state();
        s.metrics.inc("app_test_total", &[]);
        let res = router(Arc::clone(&s))
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("app_test_total 1"));
    }
}
