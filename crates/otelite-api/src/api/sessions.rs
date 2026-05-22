//! Session-level API endpoints.

use crate::server::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use chrono::{DateTime, Local, Utc};
use otelite_core::api::{
    ErrorResponse, SessionContextGrowth, SessionDiagnoseResponse, SessionInteraction,
    SessionListResponse, SessionSummary,
};
use otelite_core::query::{Operator, QueryPredicate, QueryValue};
use otelite_core::storage::QueryParams;
use otelite_core::telemetry::trace::StatusCode as SpanStatusCode;
use otelite_core::telemetry::{GenAiSpanInfo, Span};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};

fn extract_ttft(attrs: &HashMap<String, String>) -> Option<f64> {
    attrs
        .get("gen_ai.server.time_to_first_token")
        .or_else(|| attrs.get("llm.time_to_first_token"))
        .or_else(|| attrs.get("ttft_ms"))
        .and_then(|v| v.parse::<f64>().ok())
}

fn root_llm_span(spans: &[Span]) -> Option<&Span> {
    spans
        .iter()
        .filter(|s| s.parent_span_id.is_none())
        .find(|s| s.attributes.keys().any(|k| k.starts_with("gen_ai.")))
        .or_else(|| {
            spans
                .iter()
                .find(|s| s.attributes.keys().any(|k| k.starts_with("gen_ai.")))
        })
}

/// GET /api/sessions/:session_id/diagnose
///
/// Returns a forensic report for an LLM session: per-interaction token counts,
/// latency, errors, streaming stalls, and context growth.
pub async fn get_session_diagnose(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionDiagnoseResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Single query: all spans where session.id = <session_id>.
    // query_spans applies predicates via json_extract — no per-trace round-trips needed.
    let query = QueryParams {
        predicates: vec![QueryPredicate {
            field: "session.id".to_string(),
            operator: Operator::Equal,
            value: QueryValue::String(session_id.clone()),
        }],
        limit: Some(10_000),
        ..Default::default()
    };

    let all_spans = state.storage.query_spans(&query).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal_error(e.to_string())),
        )
    })?;

    if all_spans.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!("session {}", session_id))),
        ));
    }

    // Group spans by trace_id.
    let mut by_trace: HashMap<String, Vec<Span>> = HashMap::new();
    for span in all_spans {
        by_trace
            .entry(span.trace_id.clone())
            .or_default()
            .push(span);
    }

    // Sort trace groups by the earliest span start_time (chronological order).
    let mut trace_groups: Vec<(String, Vec<Span>)> = by_trace.into_iter().collect();
    trace_groups.sort_by_key(|(_, spans)| spans.iter().map(|s| s.start_time).min().unwrap_or(0));

    let mut interactions: Vec<SessionInteraction> = Vec::new();

    for (idx, (trace_id, spans)) in trace_groups.iter().enumerate() {
        let root = match root_llm_span(spans) {
            Some(s) => s,
            None => continue,
        };

        let genai = GenAiSpanInfo::from_attributes(&root.attributes);
        let ttft = extract_ttft(&root.attributes);
        let duration_ms = (root.end_time - root.start_time) / 1_000_000;
        let is_error = root.status.code == SpanStatusCode::Error;
        let is_stall = is_error && ttft.is_some() && duration_ms > 30_000;

        let dt = DateTime::<Utc>::from_timestamp_nanos(root.start_time);
        let time_str = dt.with_timezone(&Local).format("%H:%M:%S").to_string();

        interactions.push(SessionInteraction {
            index: idx + 1,
            time: time_str,
            model: genai
                .model
                .clone()
                .or_else(|| root.attributes.get("gen_ai.request.model").cloned()),
            input_tokens: genai.input_tokens,
            output_tokens: genai.output_tokens,
            cache_read_tokens: genai.cache_read_tokens,
            ttft_secs: ttft,
            duration_ms,
            is_error,
            is_stall,
            response_id: genai.response_id.clone(),
            trace_id: trace_id.clone(),
            start_time_ns: root.start_time,
        });
    }

    if interactions.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "GenAI spans for session {}",
                session_id
            ))),
        ));
    }

    let models: Vec<String> = interactions
        .iter()
        .filter_map(|i| i.model.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let first_ts = interactions.first().map(|i| i.start_time_ns).unwrap_or(0);
    let last_ts = interactions.last().map(|i| i.start_time_ns).unwrap_or(0);
    let start_time = DateTime::<Utc>::from_timestamp_nanos(first_ts)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let end_time = DateTime::<Utc>::from_timestamp_nanos(last_ts)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    let error_count = interactions.iter().filter(|i| i.is_error).count();
    let stall_count = interactions.iter().filter(|i| i.is_stall).count();

    let input_series: Vec<u64> = interactions.iter().filter_map(|i| i.input_tokens).collect();
    let context_growth = if input_series.len() >= 2 {
        Some(SessionContextGrowth {
            first_tokens: *input_series.first().unwrap(),
            last_tokens: *input_series.last().unwrap(),
            peak_tokens: *input_series.iter().max().unwrap(),
            interaction_count: interactions.len(),
        })
    } else {
        None
    };

    Ok(Json(SessionDiagnoseResponse {
        session_id,
        models,
        start_time,
        end_time,
        total_interactions: interactions.len(),
        error_count,
        stall_count,
        interactions,
        context_growth,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct SessionListQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub limit: Option<usize>,
}

/// GET /api/sessions
///
/// Lists distinct GenAI sessions seen in the time window with summary stats:
/// model(s), interaction count, total tokens, error count, first/last seen.
///
/// Strategy: single `query_spans` over the window for any span carrying
/// `session.id`, group by session.id in memory, aggregate.
pub async fn list_sessions(
    State(state): State<AppState>,
    Query(params): Query<SessionListQuery>,
) -> Result<Json<SessionListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let limit = params.limit.unwrap_or(200);

    // No "exists" predicate available; scan all spans in the window and
    // filter in memory. Limit caps the worst case at 20k spans (typically
    // many fewer once a time window is applied).
    let query = QueryParams {
        start_time: params.start_time,
        end_time: params.end_time,
        limit: Some(20_000),
        ..Default::default()
    };

    let all_spans = state.storage.query_spans(&query).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal_error(e.to_string())),
        )
    })?;

    // Group by session.id, then by trace_id (one interaction = one trace).
    let mut by_session: HashMap<String, HashMap<String, Vec<Span>>> = HashMap::new();
    for span in all_spans {
        let sid = match span.attributes.get("session.id") {
            Some(s) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        by_session
            .entry(sid)
            .or_default()
            .entry(span.trace_id.clone())
            .or_default()
            .push(span);
    }

    let mut summaries: Vec<SessionSummary> = Vec::with_capacity(by_session.len());

    for (session_id, by_trace) in by_session {
        let mut models: BTreeSet<String> = BTreeSet::new();
        let mut interaction_count = 0usize;
        let mut total_input: u64 = 0;
        let mut total_output: u64 = 0;
        let mut error_count = 0usize;
        let mut first_seen_ns = i64::MAX;
        let mut last_seen_ns = i64::MIN;

        for (_trace_id, spans) in by_trace {
            let root = match root_llm_span(&spans) {
                Some(s) => s,
                None => continue,
            };
            interaction_count += 1;
            let genai = GenAiSpanInfo::from_attributes(&root.attributes);
            if let Some(m) = genai.model.as_deref().or_else(|| {
                root.attributes
                    .get("gen_ai.request.model")
                    .map(|s| s.as_str())
            }) {
                models.insert(m.to_string());
            }
            if let Some(v) = genai.input_tokens {
                total_input += v;
            }
            if let Some(v) = genai.output_tokens {
                total_output += v;
            }
            if root.status.code == SpanStatusCode::Error {
                error_count += 1;
            }
            if root.start_time < first_seen_ns {
                first_seen_ns = root.start_time;
            }
            if root.start_time > last_seen_ns {
                last_seen_ns = root.start_time;
            }
        }

        if interaction_count == 0 {
            continue;
        }

        summaries.push(SessionSummary {
            session_id,
            models: models.into_iter().collect(),
            interaction_count,
            total_input_tokens: total_input,
            total_output_tokens: total_output,
            error_count,
            first_seen_ns,
            last_seen_ns,
        });
    }

    // Sort newest-first by last_seen, then truncate.
    summaries.sort_by_key(|s| std::cmp::Reverse(s.last_seen_ns));
    let total = summaries.len();
    summaries.truncate(limit);

    Ok(Json(SessionListResponse {
        sessions: summaries,
        total,
    }))
}
