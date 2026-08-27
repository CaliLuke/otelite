//! GenAI/LLM token usage API endpoints

use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use otelite_core::api::{
    AgentRolesResponse, CallsSeriesPoint, ContextTypeSplit, ConversationCostRow,
    ConversationDepthStats, CostSeriesPoint, ErrorRateByModel, ErrorResponse, ErrorTypeBreakdown,
    FinishReasonCount, GenAiCapabilityResponse, HourOfDayBucket, LatencyByContextBin,
    LatencySeriesPoint, LatencyStats, ModelDriftPair, ProviderMixResponse, ReasoningShareResponse,
    RequestParamProfile, RetrievalStats, RetryStats, SessionCostRow, StopReasonCount,
    TokenUsageResponse, ToolApprovalStats, ToolErrorEntry, ToolUsage, TopSpan, TopSpanSort,
    TruncationRateByModel,
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
    /// Sort dimension: total_tokens (default), duration, output_input_ratio
    /// (output divided by all input context), cache_efficiency
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

/// Get native GenAI telemetry capability coverage and provenance.
#[utoipa::path(
    get,
    path = "/api/genai/capabilities",
    params(ModelAnalyticsQuery),
    responses(
        (status = 200, description = "GenAI telemetry capability report", body = GenAiCapabilityResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_genai_capabilities(
    State(state): State<AppState>,
    Query(query): Query<ModelAnalyticsQuery>,
) -> Result<Json<GenAiCapabilityResponse>, (StatusCode, Json<ErrorResponse>)> {
    let report = state
        .storage
        .query_genai_capabilities(query.start_time, query.end_time, query.model.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query GenAI capabilities: {e}"
                ))),
            )
        })?;
    Ok(Json(report))
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

/// Query parameters for the cache hit rate / cache economics endpoint.
#[derive(Debug, Deserialize, Serialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct CacheQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    /// Model filter (only used without `by_model`; the economics payload is
    /// always per-model).
    pub model: Option<String>,
    /// Pass `1` (or `true`) to return the cache-economics payload
    /// (per-model read/write split, hit rate, read:write ratio, estimated
    /// savings, plus a time-bucketed series). Without it, the original
    /// per-model hit-rate list is returned unchanged.
    pub by_model: Option<String>,
    /// Bucket size in seconds for the economics series (default 3600).
    /// Only used with `by_model=1`.
    pub bucket_secs: Option<u64>,
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

/// `by_model=1` (or `true`) enables the cache-economics payload.
fn by_model_enabled(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true"))
}

/// Cache token hit rate per model; with `by_model=1` returns the full cache
/// economics payload instead (`CacheEconomicsResponse`).
#[utoipa::path(
    get,
    path = "/api/genai/cache_hit_rate",
    params(CacheQuery),
    responses(
        (status = 200, description = "Per-model cache hit rate (default) or cache economics with by_model=1", body = serde_json::Value),
        (status = 400, description = "Invalid bucket_secs", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_cache_hit_rate(
    State(state): State<AppState>,
    Query(query): Query<CacheQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    if by_model_enabled(query.by_model.as_deref()) {
        let bucket_secs = query.bucket_secs.unwrap_or(3600);
        if bucket_secs == 0 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::bad_request(
                    "bucket_secs must be a positive number of seconds",
                )),
            ));
        }
        let mut response = state
            .storage
            .query_cache_economics(
                query.start_time,
                query.end_time,
                (bucket_secs as i64) * 1_000_000_000,
            )
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::storage_error(format!(
                        "query cache economics: {}",
                        e
                    ))),
                )
            })?;
        // Enrich per-model estimated savings. The cache-read price is
        // unknown when the pricing table has no entry or no cache-read rate
        // for the model — savings stay null and savings_known is false.
        let pricing = state.pricing.snapshot().await;
        for m in &mut response.models {
            let r = pricing
                .db
                .compute_cache_savings(Some(&m.model), m.cache_read_tokens, None);
            m.est_savings_usd = r.cost;
            m.savings_known = r.cost.is_some();
        }
        return Ok(Json(serde_json::to_value(response).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "serialize cache economics: {e}"
                ))),
            )
        })?));
    }

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
    Ok(Json(serde_json::to_value(rows).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::storage_error(format!(
                "serialize cache hit rate: {e}"
            ))),
        )
    })?))
}

/// Reasoning ("thinking") token share per model, plus a global
/// per-effort breakdown. Reasoning tokens are priced at the model's output
/// rate — that is what thinking costs.
#[utoipa::path(
    get,
    path = "/api/genai/reasoning_share",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Reasoning token share by model and effort", body = ReasoningShareResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_reasoning_share(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<ReasoningShareResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut response = state
        .storage
        .query_reasoning_share(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query reasoning share: {e}"
                ))),
            )
        })?;

    // Enrich per-model cost: reasoning tokens billed at the output rate.
    let pricing = state.pricing.snapshot().await;
    for m in &mut response.models {
        let usage = TokenUsage {
            input: 0,
            output: m.reasoning_tokens,
            cache_creation: 0,
            cache_read: 0,
        };
        m.cost_usd = pricing
            .db
            .compute_cost(Some(m.model.as_str()), usage, None)
            .cost;
    }
    Ok(Json(response))
}

/// Sub-agent role attribution: cost and tokens per opencode `agent` label.
#[utoipa::path(
    get,
    path = "/api/genai/agent_roles",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Cost and token attribution per sub-agent role", body = AgentRolesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_agent_roles(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<AgentRolesResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut response = state
        .storage
        .query_agent_roles(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query agent roles: {}",
                    e
                ))),
            )
        })?;

    // Enrich per-model cost from the pricing table. opencode's own cost
    // counter is zero-valued in the wire data, so tokens x price is the only
    // source. Reasoning tokens are not priced (no separate rate in the
    // pricing table); role.cost covers the top-5 models and is None when any
    // of them lacks pricing (e.g. local models).
    let pricing = state.pricing.snapshot().await;
    for role in &mut response.roles {
        let mut total: f64 = 0.0;
        let mut all_priced = true;
        for m in &mut role.top_models {
            let usage = TokenUsage {
                input: m.tokens.input,
                output: m.tokens.output,
                cache_creation: m.tokens.cache_write,
                cache_read: m.tokens.cache_read,
            };
            let result = pricing.db.compute_cost(Some(m.model.as_str()), usage, None);
            m.cost = result.cost;
            m.cost_source = Some(result.source.as_str().to_string());
            m.cost_reason = result.reason;
            match result.cost {
                Some(c) => total += c,
                None => all_priced = false,
            }
        }
        role.cost = if all_priced && !role.top_models.is_empty() {
            Some(total)
        } else {
            None
        };
    }
    Ok(Json(response))
}

/// Provider × model mix: tokens, sessions and estimated cost per provider
/// and model, across opencode, codex and claude_code.
#[utoipa::path(
    get,
    path = "/api/genai/provider_mix",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Provider x model token and cost mix", body = ProviderMixResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_provider_mix(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<ProviderMixResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut response = state
        .storage
        .query_provider_mix(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query provider mix: {}",
                    e
                ))),
            )
        })?;

    // Enrich per-model cost from the pricing table (opencode's own cost
    // counter is zero-valued in the wire data, so tokens x price is the
    // source; reasoning tokens are not priced). Provider cost covers its
    // priced models; None when none of them has known pricing.
    let pricing = state.pricing.snapshot().await;
    for provider in &mut response.providers {
        let mut total: f64 = 0.0;
        let mut any_priced = false;
        for m in &mut provider.models {
            let usage = TokenUsage {
                input: m.tokens.input,
                output: m.tokens.output,
                cache_creation: m.tokens.cache_write,
                cache_read: m.tokens.cache_read,
            };
            let result = pricing.db.compute_cost(Some(m.model.as_str()), usage, None);
            if let Some(c) = result.cost {
                total += c;
                any_priced = true;
            }
            m.cost_usd = result.cost;
            m.cost_source = Some(result.source.as_str().to_string());
        }
        provider.cost_usd = if any_priced { Some(total) } else { None };
    }
    Ok(Json(response))
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

/// Tool approval/rejection summary (claude_code.tool.blocked_on_user spans).
#[utoipa::path(
    get,
    path = "/api/genai/tool_approvals",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Tool approval statistics", body = ToolApprovalStats),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_tool_approvals(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<ToolApprovalStats>, (StatusCode, Json<ErrorResponse>)> {
    let stats = state
        .storage
        .query_tool_approvals(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query tool approvals: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(stats))
}

/// Distribution of stop_reason values across LLM spans.
#[utoipa::path(
    get,
    path = "/api/genai/stop_reasons",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Stop reason distribution", body = Vec<StopReasonCount>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_stop_reasons(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<StopReasonCount>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_stop_reasons(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query stop reasons: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// Token usage broken down by llm_request.context type.
#[utoipa::path(
    get,
    path = "/api/genai/context_type_split",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Context type token split", body = Vec<ContextTypeSplit>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_context_type_split(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<ContextTypeSplit>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_context_type_split(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query context type split: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// Top error messages from failed tool executions.
#[utoipa::path(
    get,
    path = "/api/genai/tool_errors",
    params(ToolUsageQuery),
    responses(
        (status = 200, description = "Tool error messages", body = Vec<ToolErrorEntry>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_tool_errors(
    State(state): State<AppState>,
    Query(query): Query<ToolUsageQuery>,
) -> Result<Json<Vec<ToolErrorEntry>>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.unwrap_or(30).clamp(1, 100);
    let rows = state
        .storage
        .query_tool_errors(query.start_time, query.end_time, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query tool errors: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

/// Hour-of-day activity distribution (UTC).
#[utoipa::path(
    get,
    path = "/api/genai/hour_of_day",
    params(TimeRangeQuery),
    responses(
        (status = 200, description = "Hour-of-day buckets", body = Vec<HourOfDayBucket>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    tag = "genai"
)]
pub async fn get_hour_of_day(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<HourOfDayBucket>>, (StatusCode, Json<ErrorResponse>)> {
    let rows = state
        .storage
        .query_hour_of_day(query.start_time, query.end_time)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::storage_error(format!(
                    "query hour of day: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_model_flag_accepts_one_and_true_only() {
        assert!(by_model_enabled(Some("1")));
        assert!(by_model_enabled(Some("true")));
        assert!(!by_model_enabled(Some("0")));
        assert!(!by_model_enabled(Some("yes")));
        assert!(!by_model_enabled(Some("2")));
        assert!(!by_model_enabled(None));
    }
}
