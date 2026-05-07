//! GenAI/LLM token usage API endpoints

use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use otelite_core::api::{
    CostSeriesPoint, ErrorRateByModel, ErrorResponse, FinishReasonCount, LatencyStats, RetryStats,
    TokenUsageResponse, ToolUsage, TopSpan,
};
use serde::{Deserialize, Serialize};

/// Query parameters for token usage endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct TokenUsageQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
}

/// Get token usage statistics for GenAI/LLM spans
///
/// Returns aggregated token usage grouped by model and system (provider).
/// Only includes spans with `gen_ai.system` attribute.
#[utoipa::path(
    get,
    path = "/api/genai/usage",
    params(TokenUsageQuery),
    responses(
        (status = 200, description = "Token usage summary", body = TokenUsageResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_token_usage(
    State(state): State<AppState>,
    Query(query): Query<TokenUsageQuery>,
) -> Result<Json<TokenUsageResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (summary, by_model, by_system) = state
        .storage
        .query_token_usage(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query token usage: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(TokenUsageResponse {
        summary,
        by_model,
        by_system,
    }))
}

/// Query parameters for cost-over-time endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct CostSeriesQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
    /// Bucket size in seconds (defaults to 3600 = 1 hour)
    pub bucket: Option<i64>,
}

/// Get time-bucketed token usage (cost-over-time)
///
/// Aggregates input/output/cache tokens and request counts into fixed-size time buckets
/// grouped by model. Use for charting cost trends.
#[utoipa::path(
    get,
    path = "/api/genai/cost_series",
    params(CostSeriesQuery),
    responses(
        (status = 200, description = "Cost series points", body = Vec<CostSeriesPoint>),
        (status = 400, description = "Invalid bucket parameter", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_cost_series(
    State(state): State<AppState>,
    Query(query): Query<CostSeriesQuery>,
) -> Result<Json<Vec<CostSeriesPoint>>, (StatusCode, Json<ErrorResponse>)> {
    let bucket_seconds = query.bucket.unwrap_or(3600);
    if bucket_seconds <= 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::bad_request(
                "bucket must be a positive number of seconds",
            )),
        ));
    }
    let bucket_ns = bucket_seconds.saturating_mul(1_000_000_000);

    let series = state
        .storage
        .query_cost_series(query.start_time, query.end_time, bucket_ns)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query cost series: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(series))
}

/// Query parameters for top-spans endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct TopSpansQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
    /// Maximum number of spans to return (default 20, capped at 100)
    pub limit: Option<usize>,
}

/// Get the top-N most expensive LLM spans by total tokens
#[utoipa::path(
    get,
    path = "/api/genai/top_spans",
    params(TopSpansQuery),
    responses(
        (status = 200, description = "Top expensive spans", body = Vec<TopSpan>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_top_spans(
    State(state): State<AppState>,
    Query(query): Query<TopSpansQuery>,
) -> Result<Json<Vec<TopSpan>>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);

    let spans = state
        .storage
        .query_top_spans(query.start_time, query.end_time, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query top spans: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(spans))
}

/// Query parameters for finish-reason distribution endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct FinishReasonsQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
}

/// Get the distribution of finish / stop reasons across LLM spans
///
/// Combines OTel plural `gen_ai.response.finish_reasons`, singular `gen_ai.response.finish_reason`,
/// and Claude Code `stop_reason` values from `claude_code.api_response_body` log bodies.
#[utoipa::path(
    get,
    path = "/api/genai/finish_reasons",
    params(FinishReasonsQuery),
    responses(
        (status = 200, description = "Finish reason counts", body = Vec<FinishReasonCount>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_finish_reasons(
    State(state): State<AppState>,
    Query(query): Query<FinishReasonsQuery>,
) -> Result<Json<Vec<FinishReasonCount>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_finish_reasons(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query finish reasons: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(rows))
}

/// Query parameters for latency endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct LatencyQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
}

/// Get latency / TTFT percentile statistics per model for LLM spans.
#[utoipa::path(
    get,
    path = "/api/genai/latency",
    params(LatencyQuery),
    responses(
        (status = 200, description = "Latency statistics per model", body = Vec<LatencyStats>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_latency_stats(
    State(state): State<AppState>,
    Query(query): Query<LatencyQuery>,
) -> Result<Json<Vec<LatencyStats>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_latency_stats(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query latency stats: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(rows))
}

/// Query parameters for error-rate endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct ErrorRateQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
}

/// Get error rate per model across LLM spans.
#[utoipa::path(
    get,
    path = "/api/genai/error_rate",
    params(ErrorRateQuery),
    responses(
        (status = 200, description = "Error rate per model", body = Vec<ErrorRateByModel>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_error_rate(
    State(state): State<AppState>,
    Query(query): Query<ErrorRateQuery>,
) -> Result<Json<Vec<ErrorRateByModel>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_error_rate(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query error rate: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(rows))
}

/// Query parameters for tool-usage endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct ToolUsageQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
    /// Maximum number of tools to return (default 20, capped at 100)
    pub limit: Option<usize>,
}

/// Get aggregated per-tool usage for tool-execution spans.
#[utoipa::path(
    get,
    path = "/api/genai/tool_usage",
    params(ToolUsageQuery),
    responses(
        (status = 200, description = "Tool usage aggregates", body = Vec<ToolUsage>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_tool_usage(
    State(state): State<AppState>,
    Query(query): Query<ToolUsageQuery>,
) -> Result<Json<Vec<ToolUsage>>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);

    let rows = state
        .storage
        .query_tool_usage(query.start_time, query.end_time, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query tool usage: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(rows))
}

/// Query parameters for retry-stats endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct RetryStatsQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
}

/// Get retry statistics across LLM spans.
#[utoipa::path(
    get,
    path = "/api/genai/retry_stats",
    params(RetryStatsQuery),
    responses(
        (status = 200, description = "Retry statistics", body = RetryStats),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_retry_stats(
    State(state): State<AppState>,
    Query(query): Query<RetryStatsQuery>,
) -> Result<Json<RetryStats>, (StatusCode, Json<ErrorResponse>)> {
    let stats = state
        .storage
        .query_retry_stats(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query retry stats: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(stats))
}
