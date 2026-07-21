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

        // For errored interactions, fetch the api_request_body log for body_length
        // via the consolidated trace→logs endpoint. prompt.id is available as a
        // span attribute.
        let (body_length, prompt_id) = if is_error {
            let body_len = client
                .fetch_logs_for_trace(&trace_entry.trace_id, Some(1), Some("api_request_body"))
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

    // ── Performance findings summary ──────────────────────────────────────────
    let total_dur_ms: i64 = interactions.iter().map(|i| i.duration_ms).sum();
    let slow_count = interactions
        .iter()
        .filter(|i| i.duration_ms > 30_000)
        .count();
    let cold_count = interactions
        .iter()
        .filter(|i| i.cache_read.unwrap_or(0) == 0 && i.cache_creation.unwrap_or(0) > 50_000)
        .count();
    let total_output: u64 = interactions.iter().filter_map(|i| i.output_tokens).sum();
    let dur_sorted: Vec<i64> = {
        let mut v: Vec<i64> = interactions.iter().map(|i| i.duration_ms).collect();
        v.sort_unstable();
        v
    };
    let p95_ms = dur_sorted
        .get(dur_sorted.len() * 95 / 100)
        .copied()
        .unwrap_or(0);

    println!("Performance summary:");
    println!("  Total LLM time : {}", format_duration(total_dur_ms));
    println!("  p95 turn time  : {}", format_duration(p95_ms));
    println!(
        "  Slowest turn   : {}",
        format_duration(dur_sorted.last().copied().unwrap_or(0))
    );
    if slow_count > 0 {
        println!("  Slow turns(>30s): {}/{}", slow_count, total);
    }
    if cold_count > 0 {
        println!(
            "  Cold starts    : {} turn(s) — no cache reads, full context rebuilt",
            cold_count
        );
    }
    if total_output > 50_000 {
        println!(
            "  Total output   : {} tokens — generation volume is the likely latency driver",
            format_tokens(total_output)
        );
    }
    if p95_ms > 60_000 {
        println!("  ⚠  p95 > 60s — most turns are slow, not just outliers");
    }
    println!();

    // ── Per-interaction table ─────────────────────────────────────────────────
    println!(
        "{:>4}  {:8}  {:>10}  {:>7}  {:>7}  {:>6}  {:>8}  {:>8}  {:<6}  {:<14}  Trace",
        "#",
        "Time",
        "Input tok",
        "Cache+",
        "Cached",
        "TTFT",
        "Duration",
        "Out tok",
        "Cache",
        "Status"
    );
    println!("{}", "-".repeat(100));

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
        // Cache state label
        let cache_label = {
            let read = ia.cache_read.unwrap_or(0);
            let create = ia.cache_creation.unwrap_or(0);
            let inp = ia.input_tokens.unwrap_or(0);
            let total_toks = read + create + inp;
            if total_toks == 0 {
                "  —  "
            } else if read == 0 && create > 50_000 {
                " COLD"
            } else if total_toks > 0 && read * 100 / total_toks >= 80 {
                "  HOT"
            } else if read > 0 {
                " WARM"
            } else {
                "  —  "
            }
        };
        let status = if ia.is_stall {
            "ERROR [stall]"
        } else if ia.is_error {
            "ERROR"
        } else {
            "OK"
        };
        println!(
            "{:>4}  {:8}  {:>10}  {:>7}  {:>7}  {:>6}  {:>8}  {:>8}  {:>6}  {:<14}  {}",
            ia.index,
            ia.time,
            tok_str,
            cache_plus_str,
            cached_str,
            ttft_str,
            dur_str,
            out_str,
            cache_label,
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_tokens ─────────────────────────────────────────────────────────

    #[test]
    fn test_format_tokens_zero() {
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(15_300), "15.3K");
        assert_eq!(format_tokens(999_999), "1000.0K");
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    // ── format_duration ───────────────────────────────────────────────────────

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(0), "0.0s");
        assert_eq!(format_duration(1_500), "1.5s");
        assert_eq!(format_duration(59_999), "60.0s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60_000), "1m00s");
        assert_eq!(format_duration(90_000), "1m30s");
        assert_eq!(format_duration(3_661_000), "61m01s");
    }

    // ── cache_label logic ────────────────────────────────────────────────────
    // Mirrors the inline logic in the per-interaction table render loop.

    fn cache_label_for(read: u64, create: u64, inp: u64) -> &'static str {
        let total_toks = read + create + inp;
        if total_toks == 0 {
            "  —  "
        } else if read == 0 && create > 50_000 {
            " COLD"
        } else if total_toks > 0 && read * 100 / total_toks >= 80 {
            "  HOT"
        } else if read > 0 {
            " WARM"
        } else {
            "  —  "
        }
    }

    #[test]
    fn test_cache_label_cold() {
        // No reads, large cache creation → COLD (full context rebuild)
        assert_eq!(cache_label_for(0, 100_000, 50_000), " COLD");
    }

    #[test]
    fn test_cache_label_hot() {
        // Cache read is ≥80% of total tokens
        let read = 80_000u64;
        let create = 5_000u64;
        let inp = 15_000u64;
        assert_eq!(cache_label_for(read, create, inp), "  HOT");
    }

    #[test]
    fn test_cache_label_warm() {
        // Has some reads but <80%
        assert_eq!(cache_label_for(30_000, 10_000, 60_000), " WARM");
    }

    #[test]
    fn test_cache_label_no_data() {
        // All zeros
        assert_eq!(cache_label_for(0, 0, 0), "  —  ");
    }

    #[test]
    fn test_cache_label_small_creation_not_cold() {
        // Small cache creation (≤50K) with no reads → not COLD, falls to "  —  "
        assert_eq!(cache_label_for(0, 49_999, 10_000), "  —  ");
    }

    // ── perf summary p95 index ────────────────────────────────────────────────

    #[test]
    fn test_p95_index_single_element() {
        let mut v = [1_000i64];
        v.sort_unstable();
        let p95 = v.get(v.len() * 95 / 100).copied().unwrap_or(0);
        assert_eq!(p95, 1_000);
    }

    #[test]
    fn test_p95_index_twenty_elements() {
        // 20 elements: p95 index = 20*95/100 = 19 → last element
        let mut v: Vec<i64> = (1..=20).map(|i| i * 1_000).collect();
        v.sort_unstable();
        let p95 = v.get(v.len() * 95 / 100).copied().unwrap_or(0);
        assert_eq!(p95, 20_000);
    }

    #[test]
    fn test_cold_and_slow_counts() {
        // Verify the slow_count and cold_count formulas used in the summary
        struct FakeIa {
            duration_ms: i64,
            cache_read: Option<u64>,
            cache_creation: Option<u64>,
        }
        let interactions = [
            FakeIa {
                duration_ms: 5_000,
                cache_read: Some(80_000),
                cache_creation: Some(0),
            },
            FakeIa {
                duration_ms: 35_000,
                cache_read: Some(0),
                cache_creation: Some(120_000),
            },
            FakeIa {
                duration_ms: 45_000,
                cache_read: Some(0),
                cache_creation: Some(80_000),
            },
            FakeIa {
                duration_ms: 20_000,
                cache_read: Some(50_000),
                cache_creation: Some(10_000),
            },
        ];
        let slow_count = interactions
            .iter()
            .filter(|i| i.duration_ms > 30_000)
            .count();
        let cold_count = interactions
            .iter()
            .filter(|i| i.cache_read.unwrap_or(0) == 0 && i.cache_creation.unwrap_or(0) > 50_000)
            .count();
        assert_eq!(slow_count, 2); // 35s and 45s
        assert_eq!(cold_count, 2); // 120K and 80K creation with no reads
    }
}
