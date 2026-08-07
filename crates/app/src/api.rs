//! Local health/metrics/admin HTTP service (ROADMAP Phases 21-22). Must only
//! ever bind to localhost or a private interface; mutating admin endpoints
//! additionally require a shared-secret token.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::json;
use shopee_hunter_observability::{HealthRegistry, Metrics, ServiceState};
use shopee_hunter_storage::{ClaimRepository, Database, ScheduleRepository};
use tokio_util::sync::CancellationToken;

use crate::control::ControlPlane;

#[derive(Clone)]
pub struct ApiState {
    pub health: HealthRegistry,
    pub metrics: Metrics,
    pub started_at: DateTime<Utc>,
    pub version: &'static str,
    pub metrics_enabled: bool,
    /// Present when the DB and controls are wired (admin endpoints active).
    pub admin: Option<AdminState>,
}

#[derive(Clone)]
pub struct AdminState {
    pub db: Database,
    pub control: ControlPlane,
    /// Empty disables mutating admin endpoints.
    pub token: String,
}

pub type SharedApiState = Arc<ApiState>;

pub fn router(state: SharedApiState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/health/details", get(details))
        .route("/metrics", get(metrics))
        .route("/admin/session", get(admin_session))
        .route("/admin/jobs", get(admin_jobs))
        .route("/admin/claims/recent", get(admin_recent_claims))
        .route("/admin/claims/pause", post(admin_pause))
        .route("/admin/claims/resume", post(admin_resume))
        .route("/admin/session/refresh", post(admin_refresh))
        .with_state(state)
}

/// Guard a mutating admin request: requires a configured token that matches
/// the `x-admin-token` header. Returns None when authorized.
fn authorize(state: &ApiState, headers: &HeaderMap) -> Option<(StatusCode, &'static str)> {
    let admin = match &state.admin {
        Some(a) if !a.token.is_empty() => a,
        _ => return Some((StatusCode::NOT_FOUND, "admin disabled")),
    };
    let provided = headers.get("x-admin-token").and_then(|v| v.to_str().ok());
    // Constant-length compare is unnecessary for a localhost-bound token, but
    // require an exact match.
    if provided == Some(admin.token.as_str()) {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "invalid admin token"))
    }
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

// --- Admin endpoints (Phases 20-21) ---

async fn admin_session(State(state): State<SharedApiState>) -> impl IntoResponse {
    match &state.admin {
        Some(a) => Json(json!({
            "session": a.control.session().snapshot(),
            "claims_paused": a.control.claims_paused(),
            "claims_allowed": a.control.claims_allowed(),
        }))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "admin disabled").into_response(),
    }
}

async fn admin_jobs(State(state): State<SharedApiState>) -> impl IntoResponse {
    let admin = match &state.admin {
        Some(a) => a,
        None => return (StatusCode::NOT_FOUND, "admin disabled").into_response(),
    };
    match ScheduleRepository::new(&admin.db).open_jobs().await {
        Ok(jobs) => Json(json!({
            "count": jobs.len(),
            "jobs": jobs.iter().map(|j| json!({
                "id": j.id,
                "voucher_id": j.voucher_id,
                "action": j.action.as_str(),
                "execute_at": j.execute_at,
                "status": j.status.as_str(),
                "attempt_count": j.attempt_count,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn admin_recent_claims(State(state): State<SharedApiState>) -> impl IntoResponse {
    let admin = match &state.admin {
        Some(a) => a,
        None => return (StatusCode::NOT_FOUND, "admin disabled").into_response(),
    };
    match ClaimRepository::new(&admin.db).recent(20).await {
        Ok(rows) => Json(json!({
            "count": rows.len(),
            "attempts": rows.iter().map(|a| json!({
                "voucher_id": a.voucher_id,
                "result_class": a.result_class.map(|c| c.as_str()),
                "latency_ms": a.latency_ms,
                "started_at": a.started_at,
                "retry_index": a.retry_index,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn admin_pause(State(state): State<SharedApiState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(rejection) = authorize(&state, &headers) {
        return rejection.into_response();
    }
    state.admin.as_ref().unwrap().control.pause_claims();
    (StatusCode::OK, "claims paused").into_response()
}

async fn admin_resume(
    State(state): State<SharedApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(rejection) = authorize(&state, &headers) {
        return rejection.into_response();
    }
    state.admin.as_ref().unwrap().control.resume_claims();
    (StatusCode::OK, "claims resumed").into_response()
}

async fn admin_refresh(
    State(state): State<SharedApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(rejection) = authorize(&state, &headers) {
        return rejection.into_response();
    }
    state
        .admin
        .as_ref()
        .unwrap()
        .control
        .request_session_refresh();
    (StatusCode::OK, "session refresh requested").into_response()
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
            admin: None,
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

    async fn admin_state(token: &str) -> SharedApiState {
        use shopee_hunter_session::SessionManager;
        let dir = tempfile::tempdir().unwrap();
        std::mem::forget(dir); // keep DB file alive for the test
        let url = format!(
            "sqlite:///tmp/claude-1001/admin-{}.db?mode=rwc",
            token.len()
        );
        let db = Database::connect(&url, 2).await.unwrap();
        let session = SessionManager::new();
        Arc::new(ApiState {
            health: HealthRegistry::new(),
            metrics: Metrics::new(),
            started_at: Utc::now(),
            version: "test",
            metrics_enabled: true,
            admin: Some(AdminState {
                db,
                control: ControlPlane::new(session),
                token: token.to_string(),
            }),
        })
    }

    #[tokio::test]
    async fn admin_pause_requires_token() {
        let s = admin_state("secrettoken").await;
        let app = router(Arc::clone(&s));

        // No token → 401.
        let res = app
            .clone()
            .oneshot(
                Request::post("/admin/claims/pause")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // Correct token → 200 and the control flips.
        let res = app
            .oneshot(
                Request::post("/admin/claims/pause")
                    .header("x-admin-token", "secrettoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(s.admin.as_ref().unwrap().control.claims_paused());
    }

    #[tokio::test]
    async fn admin_jobs_is_readable_without_token() {
        let s = admin_state("t").await;
        let res = router(Arc::clone(&s))
            .oneshot(Request::get("/admin/jobs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
