//! Shared API response types for Otelite
//!
//! This module defines the canonical API response structures used across
//! otelite-server, otelite-cli, and otelite-tui. All types derive both Serialize
//! and Deserialize to support both server-side serialization and client-side
//! deserialization.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Standard error response for all API endpoints
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ErrorResponse {
    /// Human-readable error message
    pub error: String,
    /// Machine-readable error code
    pub code: String,
    /// Optional additional details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl ErrorResponse {
    /// Create a new error response
    pub fn new(code: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: code.into(),
            details: None,
        }
    }

    /// Create an error response with details
    pub fn with_details(
        code: impl Into<String>,
        error: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            error: error.into(),
            code: code.into(),
            details: Some(details.into()),
        }
    }

    /// Create a bad request error
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("BAD_REQUEST", message)
    }

    /// Create a not found error
    pub fn not_found(resource: impl Into<String>) -> Self {
        Self::new("NOT_FOUND", format!("{} not found", resource.into()))
    }

    /// Create an internal server error
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("INTERNAL_ERROR", message)
    }

    /// Create a storage error
    pub fn storage_error(operation: impl Into<String>) -> Self {
        Self::with_details(
            "STORAGE_ERROR",
            format!("Storage operation failed: {}", operation.into()),
            "Check storage configuration and disk space",
        )
    }
}

/// Response for log listing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogsResponse {
    pub logs: Vec<LogEntry>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// Individual log entry for API response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogEntry {
    pub timestamp: i64,
    pub severity: String,
    pub severity_text: Option<String>,
    pub body: String,
    /// Total byte length of the original body (populated when body may be truncated in list view).
    #[serde(default)]
    pub body_length: usize,
    /// True when the body was truncated to fit list-view limits. Full body available via GET /api/logs/:id.
    #[serde(default)]
    pub body_truncated: bool,
    #[serde(default)]
    pub attributes: HashMap<String, String>,
    pub resource: Option<Resource>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

impl LogEntry {
    /// Truncate body to `max_bytes` and set `body_truncated`/`body_length` accordingly.
    pub fn truncate_body(mut self, max_bytes: usize) -> Self {
        let original_len = self.body.len();
        self.body_length = original_len;
        if original_len > max_bytes {
            // Truncate at a char boundary.
            let cut = self
                .body
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i < max_bytes)
                .last()
                .unwrap_or(0);
            self.body.truncate(cut);
            self.body_truncated = true;
        }
        self
    }
}

/// Resource information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Resource {
    pub attributes: HashMap<String, String>,
}

/// Response for trace listing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TracesResponse {
    pub traces: Vec<TraceEntry>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// Individual trace entry (aggregated from spans)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TraceEntry {
    pub trace_id: String,
    pub root_span_name: String,
    pub start_time: i64,
    pub duration: i64,
    pub span_count: usize,
    pub service_names: Vec<String>,
    pub has_errors: bool,
}

/// Detailed trace with all spans
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TraceDetail {
    pub trace_id: String,
    pub spans: Vec<SpanEntry>,
    pub start_time: i64,
    pub end_time: i64,
    pub duration: i64,
    pub span_count: usize,
    pub service_names: Vec<String>,
}

/// Individual span entry
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SpanEntry {
    pub span_id: String,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub start_time: i64,
    pub end_time: i64,
    pub duration: i64,
    #[serde(default)]
    pub attributes: HashMap<String, String>,
    pub resource: Option<Resource>,
    pub status: SpanStatus,
    pub events: Vec<SpanEvent>,
}

/// Span status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SpanStatus {
    pub code: String,
    pub message: Option<String>,
}

/// Span event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SpanEvent {
    pub name: String,
    pub timestamp: i64,
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

/// Metric response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetricResponse {
    pub name: String,
    pub description: Option<String>,
    pub unit: Option<String>,
    pub metric_type: String,
    pub value: MetricValue,
    pub timestamp: i64,
    #[serde(default)]
    pub attributes: HashMap<String, String>,
    pub resource: Option<Resource>,
}

/// Metric value (can be different types)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetricValue {
    Gauge(f64),
    Counter(i64),
    Histogram(HistogramValue),
    Summary(SummaryValue),
}

/// Histogram value
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramValue {
    pub sum: f64,
    pub count: u64,
    pub buckets: Vec<HistogramBucket>,
}

/// Histogram bucket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramBucket {
    pub upper_bound: f64,
    pub count: u64,
}

/// Summary value
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryValue {
    pub sum: f64,
    pub count: u64,
    pub quantiles: Vec<Quantile>,
}

/// Quantile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quantile {
    pub quantile: f64,
    pub value: f64,
}

// Conversion implementations from telemetry types

impl From<crate::telemetry::LogRecord> for LogEntry {
    fn from(log: crate::telemetry::LogRecord) -> Self {
        let body_length = log.body.len();
        Self {
            timestamp: log.timestamp,
            severity: log.severity.as_str().to_string(),
            severity_text: Some(log.severity.as_str().to_string()),
            body: log.body,
            body_length,
            body_truncated: false,
            attributes: log.attributes,
            resource: log.resource.map(Resource::from),
            trace_id: log.trace_id,
            span_id: log.span_id,
        }
    }
}

impl From<crate::telemetry::Resource> for Resource {
    fn from(resource: crate::telemetry::Resource) -> Self {
        Self {
            attributes: resource.attributes,
        }
    }
}

impl From<crate::telemetry::Span> for SpanEntry {
    fn from(span: crate::telemetry::Span) -> Self {
        use crate::telemetry::trace::{SpanKind, StatusCode};

        let kind_str = match span.kind {
            SpanKind::Internal => "Internal",
            SpanKind::Server => "Server",
            SpanKind::Client => "Client",
            SpanKind::Producer => "Producer",
            SpanKind::Consumer => "Consumer",
        };

        let status_code_str = match span.status.code {
            StatusCode::Unset => "Unset",
            StatusCode::Ok => "Ok",
            StatusCode::Error => "Error",
        };

        Self {
            span_id: span.span_id,
            trace_id: span.trace_id,
            parent_span_id: span.parent_span_id,
            name: span.name,
            kind: kind_str.to_string(),
            start_time: span.start_time,
            end_time: span.end_time,
            duration: span.end_time - span.start_time,
            attributes: span.attributes,
            resource: span.resource.map(Resource::from),
            status: SpanStatus {
                code: status_code_str.to_string(),
                message: span.status.message,
            },
            events: span
                .events
                .into_iter()
                .map(|e| SpanEvent {
                    name: e.name,
                    timestamp: e.timestamp,
                    attributes: e.attributes,
                })
                .collect(),
        }
    }
}

impl From<crate::telemetry::Metric> for MetricResponse {
    fn from(metric: crate::telemetry::Metric) -> Self {
        use crate::telemetry::metric::MetricType;

        let (metric_type_str, value) = match metric.metric_type {
            MetricType::Gauge(v) => ("gauge", MetricValue::Gauge(v)),
            MetricType::Counter(v) => ("counter", MetricValue::Counter(v as i64)),
            MetricType::Histogram {
                count,
                sum,
                buckets,
            } => (
                "histogram",
                MetricValue::Histogram(HistogramValue {
                    sum,
                    count,
                    buckets: buckets
                        .into_iter()
                        .map(|b| HistogramBucket {
                            upper_bound: b.upper_bound,
                            count: b.count,
                        })
                        .collect(),
                }),
            ),
            MetricType::Summary {
                count,
                sum,
                quantiles,
            } => (
                "summary",
                MetricValue::Summary(SummaryValue {
                    sum,
                    count,
                    quantiles: quantiles
                        .into_iter()
                        .map(|q| Quantile {
                            quantile: q.quantile,
                            value: q.value,
                        })
                        .collect(),
                }),
            ),
        };

        Self {
            name: metric.name,
            description: metric.description,
            unit: metric.unit,
            metric_type: metric_type_str.to_string(),
            value,
            timestamp: metric.timestamp,
            attributes: metric.attributes,
            resource: metric.resource.map(Resource::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_response_new() {
        let err = ErrorResponse::new("TEST_ERROR", "Test error message");
        assert_eq!(err.code, "TEST_ERROR");
        assert_eq!(err.error, "Test error message");
        assert!(err.details.is_none());
    }

    #[test]
    fn test_error_response_with_details() {
        let err =
            ErrorResponse::with_details("TEST_ERROR", "Test error message", "Additional details");
        assert_eq!(err.code, "TEST_ERROR");
        assert_eq!(err.error, "Test error message");
        assert_eq!(err.details, Some("Additional details".to_string()));
    }

    #[test]
    fn test_error_response_bad_request() {
        let err = ErrorResponse::bad_request("Invalid parameter");
        assert_eq!(err.code, "BAD_REQUEST");
        assert_eq!(err.error, "Invalid parameter");
    }

    #[test]
    fn test_error_response_not_found() {
        let err = ErrorResponse::not_found("Log entry");
        assert_eq!(err.code, "NOT_FOUND");
        assert_eq!(err.error, "Log entry not found");
    }

    #[test]
    fn test_error_response_internal_error() {
        let err = ErrorResponse::internal_error("Database connection failed");
        assert_eq!(err.code, "INTERNAL_ERROR");
        assert_eq!(err.error, "Database connection failed");
    }

    #[test]
    fn test_error_response_storage_error() {
        let err = ErrorResponse::storage_error("write");
        assert_eq!(err.code, "STORAGE_ERROR");
        assert!(err.error.contains("write"));
        assert!(err.details.is_some());
    }

    #[test]
    fn test_error_response_serialization() {
        let err = ErrorResponse::with_details("TEST", "message", "details");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"code\":\"TEST\""));
        assert!(json.contains("\"error\":\"message\""));
        assert!(json.contains("\"details\":\"details\""));
    }

    #[test]
    fn test_error_response_deserialization() {
        let json = r#"{"error":"test message","code":"TEST_CODE","details":"test details"}"#;
        let err: ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(err.code, "TEST_CODE");
        assert_eq!(err.error, "test message");
        assert_eq!(err.details, Some("test details".to_string()));
    }
}

/// Token usage summary response for GenAI/LLM spans
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenUsageResponse {
    /// Overall token usage summary
    pub summary: TokenUsageSummary,
    /// Token usage grouped by model
    pub by_model: Vec<ModelUsage>,
    /// Token usage grouped by system (provider)
    pub by_system: Vec<SystemUsage>,
}

/// Overall token usage summary
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenUsageSummary {
    /// Total input tokens across all requests
    pub total_input_tokens: u64,
    /// Total output tokens across all requests
    pub total_output_tokens: u64,
    /// Total number of GenAI requests
    pub total_requests: usize,
    /// Total cache creation input tokens (Anthropic prompt caching)
    #[serde(default)]
    pub total_cache_creation_tokens: u64,
    /// Total cache read input tokens (Anthropic prompt caching)
    #[serde(default)]
    pub total_cache_read_tokens: u64,
}

/// Token usage for a specific model
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ModelUsage {
    /// Model name (e.g., "gpt-4", "claude-sonnet-4-20250514")
    pub model: String,
    /// Input tokens for this model
    pub input_tokens: u64,
    /// Output tokens for this model
    pub output_tokens: u64,
    /// Number of requests for this model
    pub requests: usize,
}

/// Token usage for a specific system (provider)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SystemUsage {
    /// System name (e.g., "openai", "anthropic")
    pub system: String,
    /// Input tokens for this system
    pub input_tokens: u64,
    /// Output tokens for this system
    pub output_tokens: u64,
    /// Number of requests for this system
    pub requests: usize,
}

/// A single time-bucketed cost/usage data point
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CostSeriesPoint {
    /// Bucket start timestamp in nanoseconds since Unix epoch
    pub timestamp: i64,
    /// Model name (nullable — spans without a model attribute are grouped under null)
    pub model: Option<String>,
    /// Input tokens in this bucket
    pub input_tokens: u64,
    /// Output tokens in this bucket
    pub output_tokens: u64,
    /// Cache creation input tokens (Anthropic prompt caching)
    pub cache_creation_tokens: u64,
    /// Cache read input tokens (Anthropic prompt caching)
    pub cache_read_tokens: u64,
    /// Number of requests in this bucket
    pub requests: usize,
    /// Estimated cost in USD for this bucket, computed server-side. `None` when
    /// no pricing data matched the bucket's model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Origin of the cost figure: "litellm", "fallback", or "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<String>,
}

/// Sort dimension for top-N span queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum TopSpanSort {
    /// By total token count (default — same as before).
    #[default]
    TotalTokens,
    /// By span duration (slowest first).
    Duration,
    /// By output/input token ratio (most verbose first).
    OutputInputRatio,
    /// By cache efficiency: worst cache-read rate (ascending) first.
    CacheEfficiency,
}

/// A single top-N expensive LLM span
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TopSpan {
    pub trace_id: String,
    pub span_id: String,
    /// Span start time (nanoseconds since Unix epoch)
    pub start_time: i64,
    /// Span duration in nanoseconds
    pub duration: i64,
    pub model: Option<String>,
    pub system: Option<String>,
    pub session_id: Option<String>,
    pub prompt_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    /// First finish/stop reason for this span (e.g. "max_tokens", "end_turn").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// `gen_ai.conversation.id` attribute if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Estimated cost in USD, computed server-side from the pricing database.
    /// `None` when no pricing data matched this row's (model, system).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Origin of the cost figure: "litellm", "fallback", or "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<String>,
    /// Human-readable tooltip explaining why cost is None (e.g.
    /// "no pricing data for claude-foo on bedrock").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_reason: Option<String>,
    /// Derived output token throughput (output_tokens / span_duration_sec).
    /// Span duration includes network + queue time — not pure generation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_output_tokens_per_sec: Option<f64>,
}

/// Aggregated cost/token row for a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SessionCostRow {
    pub session_id: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<String>,
}

/// Aggregated cost/token row for a single conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConversationCostRow {
    pub conversation_id: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<String>,
}

/// Distribution entry for a single finish reason
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FinishReasonCount {
    pub reason: String,
    pub count: usize,
}

/// Latency / TTFT percentile statistics for LLM spans, grouped by model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LatencyStats {
    pub model: Option<String>,
    pub count: usize,
    pub avg_ms: f64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    /// TTFT is reported only when any span in the group carried a ttft attribute.
    pub ttft_count: usize,
    pub ttft_p50_ms: Option<i64>,
    pub ttft_p95_ms: Option<i64>,
    pub ttft_p99_ms: Option<i64>,
    /// Derived output token throughput (output_tokens / span_duration_sec).
    /// Span duration includes network + queue time, NOT pure generation time.
    /// Only set for spans where both output_tokens > 0 and duration > 0.
    pub derived_tokens_per_sec_p50: Option<f64>,
    pub derived_tokens_per_sec_p95: Option<f64>,
    pub derived_tokens_per_sec_p99: Option<f64>,
    /// Distribution of input token counts (context / prompt size).
    pub input_tokens_p50: Option<i64>,
    pub input_tokens_p95: Option<i64>,
    pub input_tokens_p99: Option<i64>,
    /// Distribution of output/input token ratio (generation verbosity).
    pub output_input_ratio_p50: Option<f64>,
    pub output_input_ratio_p95: Option<f64>,
    pub output_input_ratio_p99: Option<f64>,
}

/// Error-rate summary for LLM spans grouped by model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ErrorRateByModel {
    pub model: Option<String>,
    pub total: usize,
    pub errors: usize,
    /// Fraction in the range 0.0..1.0.
    pub error_rate: f64,
}

/// Aggregated per-tool usage for tool-execution spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ToolUsage {
    pub tool_name: String,
    pub count: usize,
    pub success_count: usize,
    pub error_count: usize,
    pub avg_duration_ms: f64,
    pub total_duration_ms: i64,
}

/// Retry statistics across LLM spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RetryStats {
    pub total_llm_calls: usize,
    /// Calls with attempt > 1 (Claude Code) or comparable retry markers.
    pub retried_calls: usize,
    /// Sum of (attempt - 1) across all calls — total extra attempts.
    pub extra_attempts: usize,
    /// Fraction in the range 0.0..1.0.
    pub retry_rate: f64,
}

/// Retrieval / RAG statistics aggregated across retriever spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RetrievalStats {
    pub total_retrievals: usize,
    pub avg_documents_per_query: f64,
    /// None when no retrieval span emitted a document score.
    pub avg_top_document_score: Option<f64>,
    pub top_queries: Vec<TopRetrievalQuery>,
}

/// A single grouped retrieval query with aggregate stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TopRetrievalQuery {
    pub query: String,
    pub count: usize,
    pub avg_documents: f64,
    pub avg_top_score: Option<f64>,
}

/// Truncation rate (finish_reason = max_tokens/length) per model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TruncationRateByModel {
    pub model: Option<String>,
    pub total: usize,
    pub truncated: usize,
    /// Fraction in the range 0.0..1.0.
    pub rate: f64,
}

/// Cache token efficiency per model.
/// `hit_rate` = cache_read_tokens / (cache_read_tokens + input_tokens).
/// Only set when at least one of the token counts is non-zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CacheHitRateByModel {
    pub model: Option<String>,
    pub total_input_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_rate: Option<f64>,
}

/// Distribution of `gen_ai.request.temperature` values across LLM calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TemperatureBucket {
    /// Rounded to 2 decimal places. None = attribute not set.
    pub temperature: Option<f64>,
    pub count: usize,
}

/// Distribution of `gen_ai.request.max_tokens` values across LLM calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MaxTokensBucket {
    /// None = attribute not set.
    pub max_tokens: Option<i64>,
    pub count: usize,
}

/// Distribution of request parameter settings (temperature, max_tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestParamProfile {
    pub temperature_buckets: Vec<TemperatureBucket>,
    pub max_tokens_buckets: Vec<MaxTokensBucket>,
}

/// Turn-count distribution across all observed conversations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConversationDepthStats {
    pub total_conversations: usize,
    pub avg_turns: f64,
    pub p50_turns: i64,
    pub p95_turns: i64,
    pub p99_turns: i64,
}

/// Single time-bucket latency point, grouped by model (LLM mode) or span name (all-spans mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LatencySeriesPoint {
    /// Bucket start timestamp in nanoseconds since Unix epoch.
    pub timestamp: i64,
    /// Model name — set when `span_filter=llm` (default); null for `span_filter=all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Span name — set when `span_filter=all`; null for `span_filter=llm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Number of spans in this bucket.
    pub count: usize,
    /// Number of error spans (status_code = 2) in this bucket.
    pub error_count: usize,
    /// Minimum span duration in milliseconds.
    pub min_ms: i64,
    /// Average span duration in milliseconds.
    pub avg_ms: f64,
    /// 95th-percentile span duration in milliseconds.
    pub p95_ms: i64,
    /// Maximum span duration in milliseconds.
    pub max_ms: i64,
    /// Average time-to-first-token in milliseconds (None when no spans carry TTFT data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_ttft_ms: Option<f64>,
    /// 95th-percentile time-to-first-token in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_ttft_ms: Option<i64>,
}

/// LLM latency broken down by input-token context size bin.
/// Useful for understanding whether larger prompts are slower.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LatencyByContextBin {
    /// Human-readable bin label, e.g. "0–1K", "1K–10K", "100K+".
    pub bin: String,
    /// Inclusive lower bound of the bin in tokens.
    pub min_tokens: u64,
    /// Exclusive upper bound (u64::MAX for the open-ended last bin).
    pub max_tokens: u64,
    /// Model (None = spans without a model attribute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub count: usize,
    pub avg_ms: f64,
    pub p95_ms: i64,
    pub max_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_ttft_ms: Option<f64>,
}

/// Single time-bucket point for calls-over-time series.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CallsSeriesPoint {
    /// Bucket start timestamp in nanoseconds since Unix epoch.
    pub timestamp: i64,
    /// Model name — set when `span_filter=llm` (default); null for `span_filter=all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Span name — set when `span_filter=all`; null for `span_filter=llm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Number of spans in this bucket.
    pub requests: usize,
}

/// Per-(model, error_type) breakdown of error spans, bucketed into actionable categories.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ErrorTypeBreakdown {
    pub model: Option<String>,
    /// Raw `error.type` value as observed (or exception.type / HTTP status code).
    pub error_type: String,
    /// Coarse actionable bucket: "rate_limit" | "timeout" | "context_length" |
    /// "content_filter" | "auth" | "server_error" | "unknown"
    pub bucket: String,
    pub count: usize,
}

/// A (request_model → response_model) pair that providers actually served.
/// `differs == true` means the provider silently rerouted to a different model snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ModelDriftPair {
    pub request_model: Option<String>,
    pub response_model: Option<String>,
    pub count: usize,
    /// True when both fields are non-null and differ from each other.
    pub differs: bool,
}

/// One LLM interaction within a session (derived from a single trace's root GenAI span).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SessionInteraction {
    pub index: usize,
    /// Wall-clock time of the interaction start (HH:MM:SS, server local time).
    pub time: String,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    /// Time-to-first-token in seconds (None when not instrumented or non-streaming).
    pub ttft_secs: Option<f64>,
    pub duration_ms: i64,
    pub is_error: bool,
    /// True when a stream started (TTFT present) but the span ended in error after >30 s.
    pub is_stall: bool,
    pub response_id: Option<String>,
    pub trace_id: String,
    pub start_time_ns: i64,
}

/// Context-growth summary for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SessionContextGrowth {
    pub first_tokens: u64,
    pub last_tokens: u64,
    pub peak_tokens: u64,
    pub interaction_count: usize,
}

/// Full session diagnose report returned by GET /api/sessions/:id/diagnose.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SessionDiagnoseResponse {
    pub session_id: String,
    pub models: Vec<String>,
    /// Formatted start time of the first interaction (RFC 3339).
    pub start_time: String,
    /// Formatted end time of the last interaction (RFC 3339).
    pub end_time: String,
    pub total_interactions: usize,
    pub error_count: usize,
    pub stall_count: usize,
    pub interactions: Vec<SessionInteraction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_growth: Option<SessionContextGrowth>,
}
