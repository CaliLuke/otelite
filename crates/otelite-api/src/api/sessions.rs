//! Session-level API endpoints.

use crate::server::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use chrono::{DateTime, Local, Utc};
use otelite_core::api::{
    ErrorResponse, SessionContextGrowth, SessionDiagnoseResponse, SessionInteraction,
};
use otelite_core::query::{Operator, QueryPredicate, QueryValue};
use otelite_core::storage::QueryParams;
use otelite_core::telemetry::trace::StatusCode as SpanStatusCode;
use otelite_core::telemetry::{GenAiSpanInfo, Span};
use std::collections::{HashMap, HashSet};

fn extract_ttft(attrs: &HashMap<String, String>) -> Option<f64> {
    attrs
        .get("gen_ai.server.time_to_first_token")
        .or_else(|| attrs.get("llm.time_to_first_token"))
        .or_else(|| attrs.get("ttft_ms"))
        .and_then(|v| v.parse::<f64>().ok())
}

fn root_llm_span_index(spans: &[Span]) -> Option<usize> {
    spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.parent_span_id.is_none())
        .find(|(_, s)| s.attributes.keys().any(|k| k.starts_with("gen_ai.")))
        .map(|(i, _)| i)
        .or_else(|| {
            spans
                .iter()
                .enumerate()
                .find(|(_, s)| s.attributes.keys().any(|k| k.starts_with("gen_ai.")))
                .map(|(i, _)| i)
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
    let trace_list_query = QueryParams {
        predicates: vec![QueryPredicate {
            field: "session.id".to_string(),
            operator: Operator::Equal,
            value: QueryValue::String(session_id.clone()),
        }],
        ..Default::default()
    };

    let trace_entries = state
        .storage
        .query_spans_for_trace_list(&trace_list_query, 500)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal_error(e.to_string())),
            )
        })?;

    if trace_entries.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!("session {}", session_id))),
        ));
    }

    let mut sorted = trace_entries;
    sorted.sort_by_key(|t| t.start_time);

    let mut interactions: Vec<SessionInteraction> = Vec::new();

    for (idx, trace_entry) in sorted.iter().enumerate() {
        let span_query = QueryParams {
            trace_id: Some(trace_entry.trace_id.clone()),
            limit: Some(1000),
            ..Default::default()
        };

        let spans = match state.storage.query_spans(&span_query).await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let root_idx = match root_llm_span_index(&spans) {
            Some(i) => i,
            None => continue,
        };
        let root = &spans[root_idx];

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
            trace_id: trace_entry.trace_id.clone(),
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

    let input_series: Vec<u64> = interactions
        .iter()
        .filter_map(|i| i.input_tokens)
        .collect();
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
