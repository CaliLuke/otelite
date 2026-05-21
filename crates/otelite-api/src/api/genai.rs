//! GenAI/LLM token usage API endpoints

use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use otelite_core::api::{
    CacheHitRateByModel, CallsSeriesPoint, ConversationCostRow, ConversationDepthStats,
    CostSeriesPoint, ErrorRateByModel, ErrorResponse, ErrorTypeBreakdown, FinishReasonCount,
    LatencyByContextBin, LatencySeriesPoint, LatencyStats, ModelDriftPair, RequestParamProfile,
    RetrievalStats, RetryStats, SessionCostRow, TokenUsageResponse, ToolUsage, TopSpan,
    TopSpanSort, TruncationRateByModel,
};
use otelite_core::pricing::{PricingDatabase, TokenUsage};
use serde::{Deserialize, Serialize};

/// Enrich a batch of TopSpan rows with computed cost fields.
fn enrich_top_spans(rows: &mut [TopSpan], db: &PricingDatabase) {
    for row in rows {
        let usage = TokenUsage {
            input: row.input_tokens,
            output: row.output_tokens,
            cache_creation: row.cache_creation_tokens,
            cache_read: row.cache_read_tokens,
        };
        let result = db.compute_cost(row.model.as_deref(), usage, row.system.as_deref());
        row.cost = result.cost;
        row.cost_source = Some(result.source.as_str().to_string());
        row.cost_reason = result.reason;
        let duration_ms = row.duration / 1_000_000;
        if row.output_tokens > 0 && duration_ms > 0 {
            row.derived_output_tokens_per_sec =
                Some(row.output_tokens as f64 / (duration_ms as f64 / 1000.0));
        }
    }
}

/// Enrich cost-series bucket rows. Cost is computed per-bucket using the
/// bucket's aggregate token counts and the model that dominates the bucket.
/// Provider isn't carried at the bucket level so we pass `None` for system —
/// the fallback table matches on model name alone.
fn enrich_cost_series(rows: &mut [CostSeriesPoint], db: &PricingDatabase) {
    for row in rows {
        let usage = TokenUsage {
            input: row.input_tokens,
            output: row.output_tokens,
            cache_creation: row.cache_creation_tokens,
            cache_read: row.cache_read_tokens,
        };
        let result = db.compute_cost(row.model.as_deref(), usage, None);
        row.cost = result.cost;
        row.cost_source = Some(result.source.as_str().to_string());
    }
}

/// Query parameters for token usage endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct TokenUsageQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
    /// Filter to a specific model name
    pub model: Option<String>,
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
        .query_token_usage(query.start_time, query.end_time, query.model.as_deref())
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
    /// Filter to a specific model name
    pub model: Option<String>,
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

    let mut series = state
        .storage
        .query_cost_series(
            query.start_time,
            query.end_time,
            bucket_ns,
            query.model.as_deref(),
        )
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

    let pricing = state.pricing.snapshot().await;
    enrich_cost_series(&mut series, &pricing.db);

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
    /// Sort dimension: total_tokens (default), duration, output_input_ratio, cache_efficiency
    #[serde(default)]
    pub sort_by: TopSpanSort,
    /// When true, return only spans with finish_reason max_tokens or length
    #[serde(default)]
    pub truncated_only: bool,
}

/// Query parameters for top-sessions endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct TopGroupQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub limit: Option<usize>,
}

/// Get the top-N LLM spans by the requested sort dimension
#[utoipa::path(
    get,
    path = "/api/genai/top_spans",
    params(TopSpansQuery),
    responses(
        (status = 200, description = "Top spans", body = Vec<TopSpan>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_top_spans(
    State(state): State<AppState>,
    Query(query): Query<TopSpansQuery>,
) -> Result<Json<Vec<TopSpan>>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);

    let mut spans = state
        .storage
        .query_top_spans(
            query.start_time,
            query.end_time,
            limit,
            query.sort_by,
            query.truncated_only,
        )
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

    let pricing = state.pricing.snapshot().await;
    enrich_top_spans(&mut spans, &pricing.db);

    Ok(Json(spans))
}

fn enrich_session_rows(rows: &mut [SessionCostRow], db: &PricingDatabase) {
    for row in rows {
        let usage = TokenUsage {
            input: row.input_tokens,
            output: row.output_tokens,
            ..Default::default()
        };
        let result = db.compute_cost(None, usage, None);
        row.cost = result.cost;
        row.cost_source = Some(result.source.as_str().to_string());
    }
}

fn enrich_conversation_rows(rows: &mut [ConversationCostRow], db: &PricingDatabase) {
    for row in rows {
        let usage = TokenUsage {
            input: row.input_tokens,
            output: row.output_tokens,
            ..Default::default()
        };
        let result = db.compute_cost(None, usage, None);
        row.cost = result.cost;
        row.cost_source = Some(result.source.as_str().to_string());
    }
}

/// Get the top-N sessions by total token usage
#[utoipa::path(
    get,
    path = "/api/genai/top_sessions",
    params(TopGroupQuery),
    responses(
        (status = 200, description = "Top sessions", body = Vec<SessionCostRow>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_top_sessions(
    State(state): State<AppState>,
    Query(query): Query<TopGroupQuery>,
) -> Result<Json<Vec<SessionCostRow>>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);

    let mut rows = state
        .storage
        .query_top_sessions(query.start_time, query.end_time, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query top sessions: {}",
                    e
                ))),
            )
        })?;

    let pricing = state.pricing.snapshot().await;
    enrich_session_rows(&mut rows, &pricing.db);

    Ok(Json(rows))
}

/// Get the top-N conversations (gen_ai.conversation.id) by total token usage
#[utoipa::path(
    get,
    path = "/api/genai/top_conversations",
    params(TopGroupQuery),
    responses(
        (status = 200, description = "Top conversations", body = Vec<ConversationCostRow>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_top_conversations(
    State(state): State<AppState>,
    Query(query): Query<TopGroupQuery>,
) -> Result<Json<Vec<ConversationCostRow>>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);

    let mut rows = state
        .storage
        .query_top_conversations(query.start_time, query.end_time, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query top conversations: {}",
                    e
                ))),
            )
        })?;

    let pricing = state.pricing.snapshot().await;
    enrich_conversation_rows(&mut rows, &pricing.db);

    Ok(Json(rows))
}

/// Query parameters for finish-reason distribution endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct FinishReasonsQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
    /// Filter to a specific model name
    pub model: Option<String>,
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
        .query_finish_reasons(query.start_time, query.end_time, query.model.as_deref())
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
    /// Filter to a specific model name
    pub model: Option<String>,
}

/// Get latency / TTFT percentile statistics per model for LLM spans.
#[utoipa::path(
    get,
    path = "/api/genai/latency_stats",
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
        .query_latency_stats(query.start_time, query.end_time, query.model.as_deref())
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
    /// Filter to a specific model name
    pub model: Option<String>,
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
        .query_error_rate(query.start_time, query.end_time, query.model.as_deref())
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

/// Query parameters for retrieval-stats endpoint
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct RetrievalStatsQuery {
    /// Start time (nanoseconds since Unix epoch)
    pub start_time: Option<i64>,
    /// End time (nanoseconds since Unix epoch)
    pub end_time: Option<i64>,
    /// Maximum number of top queries to return (default 5, capped at 20)
    pub limit: Option<usize>,
}

/// Get aggregated retrieval / RAG statistics across retriever spans.
///
/// Retriever spans are identified by `openinference.span.kind = 'RETRIEVER'` or
/// the presence of a `retrieval.query` attribute. Returns total counts, average
/// documents per query, average top-1 document score, and the top-N most-frequent
/// queries.
#[utoipa::path(
    get,
    path = "/api/genai/retrieval_stats",
    params(RetrievalStatsQuery),
    responses(
        (status = 200, description = "Retrieval statistics", body = RetrievalStats),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_retrieval_stats(
    State(state): State<AppState>,
    Query(query): Query<RetrievalStatsQuery>,
) -> Result<Json<RetrievalStats>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(5).clamp(1, 20);

    let stats = state
        .storage
        .query_retrieval_stats(query.start_time, query.end_time, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query retrieval stats: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(stats))
}

/// Metadata about the pricing database currently in use by the server.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PricingMetadata {
    /// "litellm" when the upstream LiteLLM fetch has succeeded at least once;
    /// "fallback" when only the hardcoded Claude 4.x table is available.
    pub source: &'static str,
    /// Number of entries in the active pricing database (0 for fallback-only).
    pub entry_count: usize,
    /// Unix milliseconds of the last successful LiteLLM fetch, if any.
    pub last_fetched_unix_ms: Option<i64>,
    /// Unix milliseconds of the last failed LiteLLM fetch, if any.
    pub last_failed_unix_ms: Option<i64>,
    /// Date the hardcoded Claude 4.x fallback table was last verified against
    /// Anthropic's list rates.
    pub fallback_last_verified: &'static str,
    /// URL to the LiteLLM source file for attribution / deep-linking.
    pub source_url: &'static str,
    /// MIT-license acknowledgement for the LiteLLM data.
    pub license: &'static str,
    /// User-facing disclaimer text — safe to render inline.
    pub disclaimer: &'static str,
}

/// Return the list of agent-framework recognizers (CrewAI, AutoGen, LangGraph).
/// The web UI and any other client consumes this to know which attributes to
/// group under each framework section — keeps the vocabulary in one place.
#[utoipa::path(
    get,
    path = "/api/genai/agent_framework_defs",
    responses(
        (status = 200, description = "Agent framework recognizers"),
    ),
    tag = "genai"
)]
pub async fn get_agent_framework_defs(
) -> Json<&'static [otelite_core::agent_frameworks::AgentFrameworkRecognizer]> {
    Json(otelite_core::agent_frameworks::AGENT_FRAMEWORKS)
}

const PRICING_DISCLAIMER: &str =
    "Cost figures are best-effort estimates. Per-token rates sourced from the LiteLLM \
     community pricing database (MIT-licensed, © 2023 Berri AI). When the upstream \
     fetch is unavailable, a small hand-curated Claude 4.x fallback table is used.";

/// Return metadata describing which pricing database the server is currently
/// using. The frontend reads this once to render the disclaimer banner and a
/// source/freshness badge.
#[utoipa::path(
    get,
    path = "/api/genai/pricing_metadata",
    responses(
        (status = 200, description = "Pricing metadata", body = PricingMetadata),
    ),
    tag = "genai"
)]
pub async fn get_pricing_metadata(State(state): State<AppState>) -> Json<PricingMetadata> {
    let snapshot = state.pricing.snapshot().await;
    Json(PricingMetadata {
        source: if snapshot.db.is_litellm() {
            "litellm"
        } else {
            "fallback"
        },
        entry_count: snapshot.db.len(),
        last_fetched_unix_ms: snapshot.last_fetched_unix_ms,
        last_failed_unix_ms: snapshot.last_failed_unix_ms,
        fallback_last_verified: otelite_core::pricing::FALLBACK_LAST_VERIFIED,
        source_url: otelite_core::pricing::LITELLM_SOURCE_URL,
        license: otelite_core::pricing::LITELLM_LICENSE,
        disclaimer: PRICING_DISCLAIMER,
    })
}

/// Query parameters shared by the new per-model analytics endpoints.
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct ModelAnalyticsQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub model: Option<String>,
}

/// Query parameters for time-series endpoints.
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct TimeSeriesQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    /// Bucket size in seconds (default 3600 = 1 hour).
    pub bucket_secs: Option<u64>,
    /// Span filter: "llm" (default) = LLM spans only; "all" = all OTel spans grouped by name.
    pub span_filter: Option<String>,
}

/// Query parameters for time-series endpoints that also accept a model filter.
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct ModelTimeSeriesQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    /// Bucket size in seconds (default 3600 = 1 hour).
    pub bucket_secs: Option<u64>,
    /// Optional model filter (e.g. "claude-opus-4-7").
    pub model: Option<String>,
    /// Span filter: "llm" (default) = LLM spans only; "all" = all OTel spans grouped by name.
    pub span_filter: Option<String>,
}

/// Query parameters for endpoints that only filter by time.
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct TimeRangeQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

/// Truncation rate (finish_reason = max_tokens / length) per model.
#[utoipa::path(
    get,
    path = "/api/genai/truncation_rate",
    params(ModelAnalyticsQuery),
    responses(
        (status = 200, description = "Truncation rate by model", body = Vec<TruncationRateByModel>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_truncation_rate(
    State(state): State<AppState>,
    Query(query): Query<ModelAnalyticsQuery>,
) -> Result<Json<Vec<TruncationRateByModel>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_truncation_rate(query.start_time, query.end_time, query.model.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query truncation rate: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// Cache token hit rate per model.
#[utoipa::path(
    get,
    path = "/api/genai/cache_hit_rate",
    params(ModelAnalyticsQuery),
    responses(
        (status = 200, description = "Cache hit rate by model", body = Vec<CacheHitRateByModel>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_cache_hit_rate(
    State(state): State<AppState>,
    Query(query): Query<ModelAnalyticsQuery>,
) -> Result<Json<Vec<CacheHitRateByModel>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_cache_hit_rate(query.start_time, query.end_time, query.model.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query cache hit rate: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// Distribution of request parameter settings (temperature, max_tokens).
#[utoipa::path(
    get,
    path = "/api/genai/request_param_profile",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Request parameter profile", body = RequestParamProfile),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_request_param_profile(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<RequestParamProfile>, (StatusCode, Json<ErrorResponse>)> {
    let profile = state
        .storage
        .query_request_param_profile(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query request param profile: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(profile))
}

/// Turn-count distribution across conversations.
#[utoipa::path(
    get,
    path = "/api/genai/conversation_depth",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Conversation depth statistics", body = ConversationDepthStats),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_conversation_depth(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<ConversationDepthStats>, (StatusCode, Json<ErrorResponse>)> {
    let stats = state
        .storage
        .query_conversation_depth(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query conversation depth: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(stats))
}

/// LLM span latency (min/avg/p95/max + TTFT) per time bucket, grouped by model.
#[utoipa::path(
    get,
    path = "/api/genai/latency_series",
    params(ModelTimeSeriesQuery),
    responses(
        (status = 200, description = "Latency stats per time bucket", body = Vec<LatencySeriesPoint>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_latency_series(
    State(state): State<AppState>,
    Query(query): Query<ModelTimeSeriesQuery>,
) -> Result<Json<Vec<LatencySeriesPoint>>, (StatusCode, Json<ErrorResponse>)> {
    let bucket_secs = query.bucket_secs.unwrap_or(3600).clamp(60, 86400);
    let all_spans = query.span_filter.as_deref() == Some("all");
    let rows = state
        .storage
        .query_latency_series(
            query.start_time,
            query.end_time,
            bucket_secs,
            query.model.as_deref(),
            all_spans,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query latency series: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// LLM call volume over time (parallel to cost_series).
#[utoipa::path(
    get,
    path = "/api/genai/calls_series",
    params(TimeSeriesQuery),
    responses(
        (status = 200, description = "Calls per time bucket", body = Vec<CallsSeriesPoint>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_calls_series(
    State(state): State<AppState>,
    Query(query): Query<TimeSeriesQuery>,
) -> Result<Json<Vec<CallsSeriesPoint>>, (StatusCode, Json<ErrorResponse>)> {
    let bucket_secs = query.bucket_secs.unwrap_or(3600).clamp(60, 86400);
    let all_spans = query.span_filter.as_deref() == Some("all");
    let rows = state
        .storage
        .query_calls_series(query.start_time, query.end_time, bucket_secs, all_spans)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query calls series: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// LLM latency broken down by input-token context size bin × model.
/// Useful for answering "do larger prompts cause slower responses?"
#[utoipa::path(
    get,
    path = "/api/genai/latency_by_context",
    params(ModelAnalyticsQuery),
    responses(
        (status = 200, description = "Latency per context size bin", body = Vec<LatencyByContextBin>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_latency_by_context(
    State(state): State<AppState>,
    Query(query): Query<ModelAnalyticsQuery>,
) -> Result<Json<Vec<LatencyByContextBin>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_latency_by_context(query.start_time, query.end_time, query.model.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query latency by context: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// Per-(model, error_type) breakdown of error spans, bucketed into actionable categories.
#[utoipa::path(
    get,
    path = "/api/genai/error_types",
    params(ModelAnalyticsQuery),
    responses(
        (status = 200, description = "Error type breakdown per model", body = Vec<ErrorTypeBreakdown>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_error_types(
    State(state): State<AppState>,
    Query(query): Query<ModelAnalyticsQuery>,
) -> Result<Json<Vec<ErrorTypeBreakdown>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_error_types(query.start_time, query.end_time, query.model.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query error types: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// All observed (request_model → response_model) pairs with a `differs` flag.
/// `differs == true` indicates silent provider rerouting.
#[utoipa::path(
    get,
    path = "/api/genai/model_drift",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Request→response model pairs", body = Vec<ModelDriftPair>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_model_drift(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<ModelDriftPair>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_model_drift(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query model drift: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}
