// Health check endpoint

use crate::server::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

/// Health check response
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub storage: String,
    pub uptime_seconds: u64,
    /// OTLP gRPC receiver port
    pub otlp_grpc_port: u16,
    /// OTLP HTTP receiver port
    pub otlp_http_port: u16,
}

/// Health check handler
#[utoipa::path(
    get,
    path = "/api/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse)
    ),
    tag = "health"
)]
pub async fn health_check(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, StatusCode> {
    let uptime = state.start_time.elapsed().as_secs();

    let response = HealthResponse {
        status: "healthy".to_string(),
        version: crate::VERSION.to_string(),
        storage: "connected".to_string(),
        uptime_seconds: uptime,
        otlp_grpc_port: state.otlp_grpc_port,
        otlp_http_port: state.otlp_http_port,
    };

    Ok(Json(response))
}
