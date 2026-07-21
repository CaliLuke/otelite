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
use otelite_core::telemetry::{extract_ttft_secs, GenAiSpanInfo, Span};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};

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

        let mut genai = GenAiSpanInfo::from_attributes(&root.attributes);
        let mut ttft = extract_ttft_secs(&root.attributes);
        let duration_ms = (root.end_time - root.start_time) / 1_000_000;
        let is_error = root.status.code == SpanStatusCode::Error;
        let is_stall = is_error && ttft.is_some() && duration_ms > 30_000;

        // Claude Code traces use claude_code.interaction as the root span with
        // claude_code.llm_request children. When the root span carries no token
        // counts, aggregate across all LLM child spans so the modal displays
        // real numbers instead of blanks.
        if genai.input_tokens.is_none() && genai.output_tokens.is_none() {
            let llm_spans: Vec<&Span> = spans
                .iter()
                .filter(|s| {
                    s.name.contains("llm_request")
                        || s.attributes.keys().any(|k| k.starts_with("gen_ai."))
                })
                .collect();

            if !llm_spans.is_empty() {
                // Use the first LLM span for scalar fields (model, TTFT, response_id).
                let first = llm_spans[0];
                let first_genai = GenAiSpanInfo::from_attributes(&first.attributes);
                if genai.model.is_none() {
                    genai.model = first_genai.model;
                }
                if genai.response_id.is_none() {
                    genai.response_id = first_genai.response_id;
                }
                if ttft.is_none() {
                    ttft = extract_ttft_secs(&first.attributes);
                }

                // Sum token counts across all LLM spans in the trace (one
                // trace can contain multiple tool-call round-trips).
                let mut total_input: u64 = 0;
                let mut total_output: u64 = 0;
                let mut total_cache_read: u64 = 0;
                let mut total_cache_creation: u64 = 0;
                for s in &llm_spans {
                    let sg = GenAiSpanInfo::from_attributes(&s.attributes);
                    total_input += sg.input_tokens.unwrap_or(0);
                    total_output += sg.output_tokens.unwrap_or(0);
                    total_cache_read += sg.cache_read_tokens.unwrap_or(0);
                    total_cache_creation += sg.cache_creation_tokens.unwrap_or(0);
                }
                if total_input > 0 || total_output > 0 {
                    genai.input_tokens = Some(total_input);
                    genai.output_tokens = Some(total_output);
                }
                if total_cache_read > 0 {
                    genai.cache_read_tokens = Some(total_cache_read);
                }
                if total_cache_creation > 0 {
                    genai.cache_creation_tokens = Some(total_cache_creation);
                }
            }
        }

        let dt = DateTime::<Utc>::from_timestamp_nanos(root.start_time);
        let time_str = dt.with_timezone(&Local).format("%H:%M:%S").to_string();

        // For errored interactions, fetch the api_request_body log to get body_length.
        // prompt.id is available directly on the span attributes.
        let (body_length, prompt_id) = if is_error {
            let log_params = QueryParams {
                trace_id: Some(trace_id.clone()),
                search_text: Some("api_request_body".to_string()),
                limit: Some(1),
                ..Default::default()
            };
            let log_body_len = state
                .storage
                .query_logs(&log_params)
                .await
                .ok()
                .and_then(|logs| logs.into_iter().next())
                .and_then(|log| {
                    log.attributes
                        .get("body_length")
                        .and_then(|v| v.parse::<u64>().ok())
                });
            let pid = root.attributes.get("prompt.id").cloned();
            (log_body_len, pid)
        } else {
            (None, None)
        };

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
            cache_creation_tokens: genai.cache_creation_tokens,
            ttft_secs: ttft,
            duration_ms,
            is_error,
            is_stall,
            response_id: genai.response_id.clone(),
            trace_id: trace_id.clone(),
            start_time_ns: root.start_time,
            body_length,
            prompt_id,
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
