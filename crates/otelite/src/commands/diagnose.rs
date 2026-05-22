//! `otelite diagnose <session-id>` — one-shot forensic report for a session.

use crate::config::Config;
use crate::error::Result;
use chrono::{DateTime, Local, Utc};
use otelite_client::models::SpanEntry;
use otelite_client::ApiClient;
use otelite_core::telemetry::{extract_ttft_secs, GenAiSpanInfo};

/// Per-interaction row derived from a trace's root span.
struct Interaction {
    index: usize,
    time: String,
    model: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    ttft_secs: Option<f64>,
    duration_ms: i64,
    is_error: bool,
    is_stall: bool,
    response_id: Option<String>,
    trace_id: String,
    start_time_ns: i64,
    body_length: Option<u64>,
    prompt_id: Option<String>,
}

fn root_llm_span(spans: &[SpanEntry]) -> Option<&SpanEntry> {
    // Prefer root spans (no parent) with gen_ai.* attributes.
    // Fall back to any gen_ai span if all spans have parents.
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

pub async fn handle_diagnose(
    client: &ApiClient,
    _config: &Config,
    session_id: &str,
    suggest: bool,
) -> Result<()> {
    // Fetch all traces for this session (up to 500 — sessions this large are anomalous).
    let traces_resp = client
        .fetch_traces(vec![
            ("session_id", session_id.to_string()),
            ("limit", "500".to_string()),
        ])
        .await?;

    if traces_resp.traces.is_empty() {
        eprintln!("No traces found for session {}", session_id);
        eprintln!("Verify the session ID and that `otelite serve` received data for this session.");
        return Ok(());
    }

    // Resolve each trace to get span-level attributes.
    let mut interactions: Vec<Interaction> = Vec::new();
    let mut sorted = traces_resp.traces.clone();
    sorted.sort_by_key(|t| t.start_time);

    for (idx, trace_entry) in sorted.iter().enumerate() {
        let detail = match client.fetch_trace_by_id(&trace_entry.trace_id).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "  warning: could not fetch trace {}: {}",
                    &trace_entry.trace_id[..8],
                    e
                );
                continue;
            },
        };

        let root = match root_llm_span(&detail.spans) {
            Some(s) => s,
            None => continue, // no LLM span in this trace, skip
        };

        let genai = GenAiSpanInfo::from_attributes(&root.attributes);
        let ttft = extract_ttft_secs(&root.attributes);
        let duration_ms = root.duration / 1_000_000;
        let is_error = root.status.code == "Error";
        let is_stall = is_error && ttft.is_some() && duration_ms > 30_000;

        let dt = DateTime::<Utc>::from_timestamp_nanos(root.start_time);
        let time_str = dt.with_timezone(&Local).format("%H:%M:%S").to_string();

        // For errored interactions, fetch the api_request_body log for body_length.
        // prompt.id is available as a span attribute.
        let (body_length, prompt_id) = if is_error {
            let log_params = vec![
                ("trace_id", trace_entry.trace_id.clone()),
                ("search", "api_request_body".to_string()),
                ("limit", "1".to_string()),
            ];
            let body_len = client
                .fetch_logs(log_params)
                .await
                .ok()
                .and_then(|r| r.logs.into_iter().next())
                .and_then(|log| {
                    log.attributes
                        .get("body_length")
                        .and_then(|v| v.parse::<u64>().ok())
                });
            let pid = root.attributes.get("prompt.id").cloned();
            (body_len, pid)
        } else {
            (None, None)
        };

        interactions.push(Interaction {
            index: idx + 1,
            time: time_str,
            model: genai
                .model
                .clone()
                .or_else(|| root.attributes.get("gen_ai.request.model").cloned()),
            input_tokens: genai.input_tokens,
            output_tokens: genai.output_tokens,
            cache_read: genai.cache_read_tokens,
            cache_creation: genai.cache_creation_tokens,
            ttft_secs: ttft,
            duration_ms,
            is_error,
            is_stall,
            response_id: genai.response_id.clone(),
            trace_id: trace_entry.trace_id.clone(),
            start_time_ns: root.start_time,
            body_length,
            prompt_id,
        });
    }

    if interactions.is_empty() {
        eprintln!(
            "Traces found for session {} but none contain GenAI spans.",
            session_id
        );
        return Ok(());
    }

    // ── Header ────────────────────────────────────────────────────────────────
    let models: Vec<&str> = interactions
        .iter()
        .filter_map(|i| i.model.as_deref())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let first_ts = interactions.first().map(|i| i.start_time_ns).unwrap_or(0);
    let last_ts = interactions.last().map(|i| i.start_time_ns).unwrap_or(0);
    let start_str = DateTime::<Utc>::from_timestamp_nanos(first_ts)
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();
    let end_str = DateTime::<Utc>::from_timestamp_nanos(last_ts)
        .with_timezone(&Local)
        .format("%H:%M")
        .to_string();

    let total = interactions.len();
    let errors = interactions.iter().filter(|i| i.is_error).count();
    let stalls = interactions.iter().filter(|i| i.is_stall).count();
    let mut model_str = models.join(", ");
    if model_str.is_empty() {
        model_str = "(unknown)".to_string();
    }

    println!("Session: {}", session_id);
    println!(
        "Model:   {}   Interactions: {}   {}–{}",
        model_str, total, start_str, end_str
    );
    if errors > 0 {
        println!("Errors:  {}   Stalls: {}", errors, stalls);
    }
    println!();

    // ── Per-interaction table ─────────────────────────────────────────────────
    println!(
        "{:>4}  {:8}  {:>10}  {:>7}  {:>7}  {:>6}  {:>8}  {:>8}  {:<14}  Trace",
        "#", "Time", "Input tok", "Cache+", "Cached", "TTFT", "Duration", "Out tok", "Status"
    );
    println!("{}", "-".repeat(93));

    for ia in &interactions {
        let tok_str = ia
            .input_tokens
            .map(format_tokens)
            .unwrap_or_else(|| "—".to_string());
        let cache_plus_str = ia
            .cache_creation
            .map(format_tokens)
            .unwrap_or_else(|| "—".to_string());
        let cached_str = ia
            .cache_read
            .map(format_tokens)
            .unwrap_or_else(|| "—".to_string());
        let out_str = ia
            .output_tokens
            .map(format_tokens)
            .unwrap_or_else(|| "—".to_string());
        let ttft_str = ia
            .ttft_secs
            .map(|t| format!("{:.1}s", t))
            .unwrap_or_else(|| "—".to_string());
        let dur_str = format_duration(ia.duration_ms);
        let status = if ia.is_stall {
            "ERROR [stall]"
        } else if ia.is_error {
            "ERROR"
        } else {
            "OK"
        };
        println!(
            "{:>4}  {:8}  {:>10}  {:>7}  {:>7}  {:>6}  {:>8}  {:>8}  {:<14}  {}",
            ia.index,
            ia.time,
            tok_str,
            cache_plus_str,
            cached_str,
            ttft_str,
            dur_str,
            out_str,
            status,
            &ia.trace_id[..12],
        );
    }
    println!();

    // ── Context growth ────────────────────────────────────────────────────────
    let input_series: Vec<u64> = interactions.iter().filter_map(|i| i.input_tokens).collect();
    if input_series.len() >= 2 {
        let first_tok = *input_series.first().unwrap();
        let last_tok = *input_series.last().unwrap();
        let peak_tok = *input_series.iter().max().unwrap();
        println!(
            "Context growth: {}K → {}K tokens across {} interactions (peak: {}K)",
            first_tok / 1000,
            last_tok / 1000,
            total,
            peak_tok / 1000,
        );
        println!();
    }

    // ── Streaming stall summary ───────────────────────────────────────────────
    if stalls > 0 {
        println!("⚠  {} streaming stall(s) detected.", stalls);
        let stall_interactions: Vec<&Interaction> =
            interactions.iter().filter(|i| i.is_stall).collect();
        for ia in &stall_interactions {
            let tok_str = ia
                .input_tokens
                .map(|t| format!("~{}K tokens", t / 1000))
                .unwrap_or_default();
            println!(
                "   Interaction #{}: {}ms duration{}",
                ia.index,
                ia.duration_ms,
                if tok_str.is_empty() {
                    String::new()
                } else {
                    format!(", {}", tok_str)
                }
            );
        }
        if suggest {
            let max_stall_dur = stall_interactions
                .iter()
                .map(|i| i.duration_ms)
                .max()
                .unwrap_or(0);
            let recommended_timeout = ((max_stall_dur / 1000) + 200).max(500);
            println!();
            println!(
                "   Suggestion: raise the stream-idle timeout on the proxy/load-balancer to at least {}s",
                recommended_timeout
            );
            println!(
                "   (longest stall was {}s; a 300s hop-level timeout is a common trigger)",
                max_stall_dur / 1000
            );
        }
        println!();
    }

    // ── Escalation block ──────────────────────────────────────────────────────
    println!("Escalation info");
    println!("  Session:   {}", session_id);
    if !model_str.is_empty() && model_str != "(unknown)" {
        println!("  Model:     {}", model_str);
    }

    let error_interactions: Vec<&Interaction> =
        interactions.iter().filter(|i| i.is_error).collect();
    if !error_interactions.is_empty() {
        let timestamps: Vec<String> = error_interactions
            .iter()
            .map(|i| {
                DateTime::<Utc>::from_timestamp_nanos(i.start_time_ns)
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string()
            })
            .collect();
        println!("  Timestamps:   {}", timestamps.join(", "));

        let body_lengths: Vec<String> = error_interactions
            .iter()
            .filter_map(|i| i.body_length)
            .map(|b| format!("{} bytes (~{}K tokens)", b, b / 4000))
            .collect();
        if !body_lengths.is_empty() {
            println!("  Body size:    {}", body_lengths.join(", "));
        }

        let prompt_ids: Vec<&str> = error_interactions
            .iter()
            .filter_map(|i| i.prompt_id.as_deref())
            .collect();
        if !prompt_ids.is_empty() {
            println!("  Prompt IDs:   {}", prompt_ids.join(", "));
        }

        let response_ids: Vec<&str> = error_interactions
            .iter()
            .filter_map(|i| i.response_id.as_deref())
            .collect();
        if !response_ids.is_empty() {
            println!("  Response IDs: {}", response_ids.join(", "));
        }

        let trace_ids: Vec<String> = error_interactions
            .iter()
            .map(|i| i.trace_id[..16].to_string())
            .collect();
        println!("  Trace IDs:    {}", trace_ids.join(", "));
    }

    if let Some(max_in) = interactions.iter().filter_map(|i| i.input_tokens).max() {
        println!("  Peak input:   {}K tokens", max_in / 1000);
    }

    Ok(())
}

fn format_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}K", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

fn format_duration(ms: i64) -> String {
    if ms >= 60_000 {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}
