//! HTTP daemon for local sandbox clients.

use axum::{routing::get, Json, Router};
use serde::Serialize;

/// Build the daemon router.
pub fn router() -> Router {
    Router::new().route("/health", get(health))
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}
