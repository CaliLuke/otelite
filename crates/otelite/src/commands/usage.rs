//! Token usage command for GenAI/LLM spans

use crate::error::{Error, Result};
use clap::Args;
use comfy_table::{presets::UTF8_FULL, Cell, Color, ContentArrangement, Table};
use otelite_core::pricing::{PricingDatabase, TokenUsage};
use otelite_storage::StorageBackend;
use std::collections::HashMap;
use std::sync::Arc;

fn validate_since(s: &str) -> std::result::Result<String, String> {
    let (digits, suffix) = s.split_at(s.len().saturating_sub(1));
    let valid_suffix = matches!(suffix, "h" | "d" | "m");
    let valid_digits = !digits.is_empty() && digits.parse::<u64>().is_ok();
    if valid_suffix && valid_digits {
        Ok(s.to_string())
    } else {
        Err(format!(
            "Invalid time duration '{}'. Use a number followed by 'h' (hours), 'd' (days), or 'm' (minutes), e.g. '1h', '24h', '7d', '30d'",
            s
        ))
    }
}

/// Display token usage statistics for GenAI/LLM spans
#[derive(Debug, Args)]
pub struct UsageCommand {
    /// Time range to query (e.g., "1h", "24h", "7d", "30d")
    #[arg(long, default_value = "24h", value_parser = validate_since)]
    pub since: String,

    /// Filter by model name (e.g., "gpt-4", "claude-sonnet-4")
    #[arg(long)]
    pub model: Option<String>,

    /// Filter by system/provider (e.g., "openai", "anthropic")
    #[arg(long)]
    pub system: Option<String>,

    /// Show detailed breakdown by model
    #[arg(long)]
    pub by_model: bool,

    /// Show detailed breakdown by system
    #[arg(long)]
    pub by_system: bool,

    /// Show the top N most expensive individual LLM calls
    #[arg(long, value_name = "N")]
    pub top: Option<usize>,

    /// Show a cost breakdown aggregated by session ID
    #[arg(long)]
    pub by_session: bool,

    /// Show per-model latency stats with derived token rate, context size, and output/input ratio
    #[arg(long)]
    pub latency: bool,

    /// Show per-model truncation rate (finish_reason = max_tokens / length)
    #[arg(long)]
    pub truncation: bool,

    /// Show per-model cache hit rate (cache_read / (cache_read + input))
    #[arg(long)]
    pub cache_rate: bool,

    /// Show request parameter distribution (temperature, max_tokens)
    #[arg(long)]
    pub request_params: bool,

    /// Show conversation depth statistics (turn-count distribution)
    #[arg(long)]
    pub conv_depth: bool,

    /// Show per-tool usage with success rate
    #[arg(long)]
    pub tools: bool,

    /// Show error type breakdown (rate_limit, timeout, context_length, ...)
    #[arg(long)]
    pub error_types: bool,

    /// Show request→response model pairs (detect silent provider rerouting)
    #[arg(long)]
    pub model_drift: bool,
}

// ── serialisable output types (used for --format json) ───────────────────────

#[derive(serde::Serialize)]
struct ModelRow {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    requests: usize,
    cost: Option<f64>,
    cost_source: Option<String>,
}

#[derive(serde::Serialize)]
struct SystemRow {
    system: String,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    requests: usize,
    cost: Option<f64>,
    cost_source: Option<String>,
}

#[derive(serde::Serialize, Default)]
struct SessionRow {
    session_id: String,
    requests: usize,
    input_tokens: u64,
    output_tokens: u64,
    cost: f64,
}

#[derive(serde::Serialize)]
struct UsageOutput {
    summary: otelite_core::api::TokenUsageSummary,
    by_model: Vec<ModelRow>,
    by_system: Vec<SystemRow>,
    by_session: Option<Vec<SessionRow>>,
    top_spans: Option<Vec<otelite_core::api::TopSpan>>,
    pricing_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_stats: Option<Vec<otelite_core::api::LatencyStats>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncation_rate: Option<Vec<otelite_core::api::TruncationRateByModel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_hit_rate: Option<Vec<otelite_core::api::CacheHitRateByModel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_param_profile: Option<otelite_core::api::RequestParamProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_depth: Option<otelite_core::api::ConversationDepthStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_usage: Option<Vec<otelite_core::api::ToolUsage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_types: Option<Vec<otelite_core::api::ErrorTypeBreakdown>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_drift: Option<Vec<otelite_core::api::ModelDriftPair>>,
}

// ── pricing fetch ─────────────────────────────────────────────────────────────

async fn fetch_pricing() -> PricingDatabase {
    const URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return PricingDatabase::empty(),
    };
    match client.get(URL).send().await {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => PricingDatabase::from_litellm_json(&body)
                .unwrap_or_else(|_| PricingDatabase::empty()),
            Err(_) => PricingDatabase::empty(),
        },
        _ => PricingDatabase::empty(),
    }
}

// ── command implementation ────────────────────────────────────────────────────

impl UsageCommand {
    /// Execute the usage command
    pub async fn execute(
        &self,
        storage: Arc<dyn StorageBackend>,
        format: crate::config::OutputFormat,
    ) -> Result<()> {
        let (start_time, end_time) = parse_time_range(&self.since)?;

        let (summary, by_model_raw, by_system_raw) = storage
            .query_token_usage(Some(start_time), Some(end_time), None)
            .await
            .map_err(|e| Error::ApiError(format!("Failed to query token usage: {}", e)))?;

        // Filter results if requested
        let by_model_raw: Vec<otelite_core::api::ModelUsage> = if let Some(ref f) = self.model {
            by_model_raw
                .into_iter()
                .filter(|m| m.model.contains(f))
                .collect()
        } else {
            by_model_raw
        };

        let by_system_raw: Vec<otelite_core::api::SystemUsage> = if let Some(ref f) = self.system {
            by_system_raw
                .into_iter()
                .filter(|s| s.system.contains(f))
                .collect()
        } else {
            by_system_raw
        };

        let pricing_db = fetch_pricing().await;
        let pricing_source = if pricing_db.is_litellm() {
            "LiteLLM".to_string()
        } else {
            "fallback (hardcoded Claude rates)".to_string()
        };

        // Enrich model/system rows with cost
        let by_model: Vec<ModelRow> = by_model_raw
            .iter()
            .map(|m| {
                let usage = TokenUsage {
                    input: m.input_tokens,
                    output: m.output_tokens,
                    ..Default::default()
                };
                let cr = pricing_db.compute_cost(Some(&m.model), usage, None);
                ModelRow {
                    model: m.model.clone(),
                    input_tokens: m.input_tokens,
                    output_tokens: m.output_tokens,
                    total_tokens: m.input_tokens + m.output_tokens,
                    requests: m.requests,
                    cost: cr.cost,
                    cost_source: Some(cr.source.as_str().to_string()),
                }
            })
            .collect();

        let by_system: Vec<SystemRow> = by_system_raw
            .iter()
            .map(|s| {
                let usage = TokenUsage {
                    input: s.input_tokens,
                    output: s.output_tokens,
                    ..Default::default()
                };
                let cr = pricing_db.compute_cost(None, usage, Some(&s.system));
                SystemRow {
                    system: s.system.clone(),
                    input_tokens: s.input_tokens,
                    output_tokens: s.output_tokens,
                    total_tokens: s.input_tokens + s.output_tokens,
                    requests: s.requests,
                    cost: cr.cost,
                    cost_source: Some(cr.source.as_str().to_string()),
                }
            })
            .collect();

        // --top N
        let top_spans: Option<Vec<otelite_core::api::TopSpan>> = if let Some(n) = self.top {
            let mut spans = storage
                .query_top_spans(
                    Some(start_time),
                    Some(end_time),
                    n,
                    otelite_core::api::TopSpanSort::TotalTokens,
                    false,
                )
                .await
                .map_err(|e| Error::ApiError(format!("Failed to query top spans: {}", e)))?;
            for span in &mut spans {
                let usage = TokenUsage {
                    input: span.input_tokens,
                    output: span.output_tokens,
                    cache_creation: span.cache_creation_tokens,
                    cache_read: span.cache_read_tokens,
                };
                let cr =
                    pricing_db.compute_cost(span.model.as_deref(), usage, span.system.as_deref());
                span.cost = cr.cost;
                span.cost_source = Some(cr.source.as_str().to_string());
                span.cost_reason = cr.reason;
            }
            spans.sort_by(|a, b| {
                b.cost
                    .unwrap_or(0.0)
                    .partial_cmp(&a.cost.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            Some(spans)
        } else {
            None
        };

        // --latency
        let latency_stats: Option<Vec<otelite_core::api::LatencyStats>> = if self.latency {
            Some(
                storage
                    .query_latency_stats(Some(start_time), Some(end_time), None)
                    .await
                    .map_err(|e| {
                        Error::ApiError(format!("Failed to query latency stats: {}", e))
                    })?,
            )
        } else {
            None
        };

        // --truncation
        let truncation_rate: Option<Vec<otelite_core::api::TruncationRateByModel>> =
            if self.truncation {
                Some(
                    storage
                        .query_truncation_rate(Some(start_time), Some(end_time), None)
                        .await
                        .map_err(|e| {
                            Error::ApiError(format!("Failed to query truncation rate: {}", e))
                        })?,
                )
            } else {
                None
            };

        // --cache-rate
        let cache_hit_rate: Option<Vec<otelite_core::api::CacheHitRateByModel>> = if self.cache_rate
        {
            Some(
                storage
                    .query_cache_hit_rate(Some(start_time), Some(end_time), None)
                    .await
                    .map_err(|e| {
                        Error::ApiError(format!("Failed to query cache hit rate: {}", e))
                    })?,
            )
        } else {
            None
        };

        // --request-params
        let request_param_profile: Option<otelite_core::api::RequestParamProfile> =
            if self.request_params {
                Some(
                    storage
                        .query_request_param_profile(Some(start_time), Some(end_time))
                        .await
                        .map_err(|e| {
                            Error::ApiError(format!("Failed to query request param profile: {}", e))
                        })?,
                )
            } else {
                None
            };

        // --conv-depth
        let conversation_depth: Option<otelite_core::api::ConversationDepthStats> =
            if self.conv_depth {
                Some(
                    storage
                        .query_conversation_depth(Some(start_time), Some(end_time))
                        .await
                        .map_err(|e| {
                            Error::ApiError(format!("Failed to query conversation depth: {}", e))
                        })?,
                )
            } else {
                None
            };

        // --tools
        let tool_usage: Option<Vec<otelite_core::api::ToolUsage>> = if self.tools {
            Some(
                storage
                    .query_tool_usage(Some(start_time), Some(end_time), 50)
                    .await
                    .map_err(|e| Error::ApiError(format!("Failed to query tool usage: {}", e)))?,
            )
        } else {
            None
        };

        // --error-types
        let error_types: Option<Vec<otelite_core::api::ErrorTypeBreakdown>> = if self.error_types {
            Some(
                storage
                    .query_error_types(Some(start_time), Some(end_time), None)
                    .await
                    .map_err(|e| Error::ApiError(format!("Failed to query error types: {}", e)))?,
            )
        } else {
            None
        };

        // --model-drift
        let model_drift: Option<Vec<otelite_core::api::ModelDriftPair>> = if self.model_drift {
            Some(
                storage
                    .query_model_drift(Some(start_time), Some(end_time))
                    .await
                    .map_err(|e| Error::ApiError(format!("Failed to query model drift: {}", e)))?,
            )
        } else {
            None
        };

        // --by-session
        let by_session: Option<Vec<SessionRow>> = if self.by_session {
            let spans = storage
                .query_top_spans(
                    Some(start_time),
                    Some(end_time),
                    200,
                    otelite_core::api::TopSpanSort::TotalTokens,
                    false,
                )
                .await
                .map_err(|e| Error::ApiError(format!("Failed to query spans: {}", e)))?;
            let mut map: HashMap<String, SessionRow> = HashMap::new();
            for span in &spans {
                let sid = span
                    .session_id
                    .clone()
                    .unwrap_or_else(|| "(no session)".to_string());
                let usage = TokenUsage {
                    input: span.input_tokens,
                    output: span.output_tokens,
                    cache_creation: span.cache_creation_tokens,
                    cache_read: span.cache_read_tokens,
                };
                let cr =
                    pricing_db.compute_cost(span.model.as_deref(), usage, span.system.as_deref());
                let row = map.entry(sid.clone()).or_insert_with(|| SessionRow {
                    session_id: sid,
                    ..Default::default()
                });
                row.requests += 1;
                row.input_tokens += span.input_tokens;
                row.output_tokens += span.output_tokens;
                row.cost += cr.cost.unwrap_or(0.0);
            }
            let mut rows: Vec<SessionRow> = map.into_values().collect();
            rows.sort_by(|a, b| {
                b.cost
                    .partial_cmp(&a.cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            Some(rows)
        } else {
            None
        };

        use crate::config::OutputFormat;
        match format {
            OutputFormat::Json | OutputFormat::JsonCompact => {
                let output = UsageOutput {
                    summary,
                    by_model,
                    by_system,
                    by_session,
                    top_spans,
                    pricing_source,
                    latency_stats,
                    truncation_rate,
                    cache_hit_rate,
                    request_param_profile,
                    conversation_depth,
                    tool_usage,
                    error_types,
                    model_drift,
                };
                let json = if matches!(format, OutputFormat::JsonCompact) {
                    serde_json::to_string(&output)
                } else {
                    serde_json::to_string_pretty(&output)
                };
                println!(
                    "{}",
                    json.map_err(|e| Error::ApiError(format!("JSON serialization failed: {}", e)))?
                );
            },
            OutputFormat::Pretty => {
                println!("\n{}", format_header(&self.since));
                println!();

                display_summary(&summary);
                println!();

                if self.by_model
                    || self.model.is_some()
                    || (!self.by_system && self.system.is_none())
                {
                    display_by_model(&by_model);
                    println!();
                }

                if self.by_system || self.system.is_some() {
                    display_by_system(&by_system);
                    println!();
                }

                if let Some(ref spans) = top_spans {
                    display_top_spans(spans, self.top.unwrap_or(20));
                    println!();
                }

                if let Some(ref rows) = by_session {
                    display_by_session(rows);
                    println!();
                }

                if let Some(ref stats) = latency_stats {
                    display_latency_stats(stats);
                    println!();
                }

                if let Some(ref rows) = truncation_rate {
                    display_truncation_rate(rows);
                    println!();
                }

                if let Some(ref rows) = cache_hit_rate {
                    display_cache_hit_rate(rows);
                    println!();
                }

                if let Some(ref profile) = request_param_profile {
                    display_request_params(profile);
                    println!();
                }

                if let Some(ref depth) = conversation_depth {
                    display_conv_depth(depth);
                    println!();
                }

                if let Some(ref rows) = tool_usage {
                    display_tool_usage(rows);
                    println!();
                }

                if let Some(ref rows) = error_types {
                    display_error_types(rows);
                    println!();
                }

                if let Some(ref rows) = model_drift {
                    display_model_drift(rows);
                    println!();
                }

                println!("Pricing source: {}", pricing_source);
            },
        }

        Ok(())
    }
}

// ── display helpers ───────────────────────────────────────────────────────────

fn parse_time_range(range: &str) -> Result<(i64, i64)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::ApiError(format!("Failed to get current time: {}", e)))?
        .as_nanos() as i64;

    let duration_ns = if let Some(stripped) = range.strip_suffix('h') {
        let hours: i64 = stripped
            .parse()
            .map_err(|_| Error::ApiError("Invalid hour format".to_string()))?;
        hours * 3600 * 1_000_000_000
    } else if let Some(stripped) = range.strip_suffix('d') {
        let days: i64 = stripped
            .parse()
            .map_err(|_| Error::ApiError("Invalid day format".to_string()))?;
        days * 24 * 3600 * 1_000_000_000
    } else if let Some(stripped) = range.strip_suffix('m') {
        let minutes: i64 = stripped
            .parse()
            .map_err(|_| Error::ApiError("Invalid minute format".to_string()))?;
        minutes * 60 * 1_000_000_000
    } else {
        return Err(Error::ApiError(
            "Invalid time range format. Use format like '1h', '24h', '7d', '30d'".to_string(),
        ));
    };

    let start_time = now - duration_ns;
    Ok((start_time, now))
}

fn format_header(range: &str) -> String {
    format!("Token Usage Summary (Last {})", range)
}

fn display_summary(summary: &otelite_core::api::TokenUsageSummary) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Metric").fg(Color::Cyan),
        Cell::new("Value").fg(Color::Cyan),
    ]);

    table.add_row(vec![
        "Total Input Tokens",
        &format_number(summary.total_input_tokens),
    ]);
    table.add_row(vec![
        "Total Output Tokens",
        &format_number(summary.total_output_tokens),
    ]);
    table.add_row(vec![
        "Total Tokens",
        &format_number(summary.total_input_tokens + summary.total_output_tokens),
    ]);
    table.add_row(vec!["Total Requests", &summary.total_requests.to_string()]);

    if summary.total_cache_creation_tokens > 0 {
        table.add_row(vec![
            "Cache Creation Tokens",
            &format_number(summary.total_cache_creation_tokens),
        ]);
    }
    if summary.total_cache_read_tokens > 0 {
        table.add_row(vec![
            "Cache Read Tokens",
            &format_number(summary.total_cache_read_tokens),
        ]);
    }

    println!("{}", table);
}

fn display_by_model(models: &[ModelRow]) {
    if models.is_empty() {
        println!("No model data available");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Input").fg(Color::Cyan),
        Cell::new("Output").fg(Color::Cyan),
        Cell::new("Total").fg(Color::Cyan),
        Cell::new("Requests").fg(Color::Cyan),
        Cell::new("Cost").fg(Color::Cyan),
    ]);

    for m in models {
        table.add_row(vec![
            &m.model,
            &format_number(m.input_tokens),
            &format_number(m.output_tokens),
            &format_number(m.total_tokens),
            &m.requests.to_string(),
            &format_cost(m.cost),
        ]);
    }

    println!("Breakdown by Model:");
    println!("{}", table);
}

fn display_by_system(systems: &[SystemRow]) {
    if systems.is_empty() {
        println!("No system data available");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("System").fg(Color::Cyan),
        Cell::new("Input").fg(Color::Cyan),
        Cell::new("Output").fg(Color::Cyan),
        Cell::new("Total").fg(Color::Cyan),
        Cell::new("Requests").fg(Color::Cyan),
        Cell::new("Cost").fg(Color::Cyan),
    ]);

    for s in systems {
        let display_name = otelite_core::telemetry::GenAiSpanInfo::format_system_name(&s.system);
        table.add_row(vec![
            &display_name,
            &format_number(s.input_tokens),
            &format_number(s.output_tokens),
            &format_number(s.total_tokens),
            &s.requests.to_string(),
            &format_cost(s.cost),
        ]);
    }

    println!("Breakdown by System:");
    println!("{}", table);
}

fn display_top_spans(spans: &[otelite_core::api::TopSpan], n: usize) {
    if spans.is_empty() {
        println!("No LLM spans found in range");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("#").fg(Color::Cyan),
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Session").fg(Color::Cyan),
        Cell::new("Input").fg(Color::Cyan),
        Cell::new("Output").fg(Color::Cyan),
        Cell::new("Cache").fg(Color::Cyan),
        Cell::new("Cost").fg(Color::Cyan),
        Cell::new("Duration").fg(Color::Cyan),
    ]);

    for (i, span) in spans.iter().enumerate() {
        let duration_ms = span.duration / 1_000_000;
        let session = span.session_id.as_deref().unwrap_or("—").to_string();
        let model = span.model.as_deref().unwrap_or("—").to_string();
        table.add_row(vec![
            &(i + 1).to_string(),
            &model,
            &truncate(&session, 24),
            &format_number(span.input_tokens),
            &format_number(span.output_tokens),
            &format_number(span.cache_read_tokens),
            &format_cost(span.cost),
            &format!("{}ms", duration_ms),
        ]);
    }

    println!("Top {} LLM Calls by Cost:", n);
    println!("{}", table);
}

fn display_by_session(rows: &[SessionRow]) {
    if rows.is_empty() {
        println!("No session data found in range");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Session ID").fg(Color::Cyan),
        Cell::new("Requests").fg(Color::Cyan),
        Cell::new("Input").fg(Color::Cyan),
        Cell::new("Output").fg(Color::Cyan),
        Cell::new("Cost").fg(Color::Cyan),
    ]);

    for row in rows {
        table.add_row(vec![
            &truncate(&row.session_id, 32),
            &row.requests.to_string(),
            &format_number(row.input_tokens),
            &format_number(row.output_tokens),
            &format!("${:.4}", row.cost),
        ]);
    }

    println!("Breakdown by Session:");
    println!("{}", table);
}

fn format_cost(cost: Option<f64>) -> String {
    match cost {
        Some(c) => format!("${:.4}", c),
        None => "—".to_string(),
    }
}

fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();

    for (count, c) in s.chars().rev().enumerate() {
        if count > 0 && count % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }

    result.chars().rev().collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn display_latency_stats(stats: &[otelite_core::api::LatencyStats]) {
    if stats.is_empty() {
        println!("No latency data available");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Calls").fg(Color::Cyan),
        Cell::new("p50 ms").fg(Color::Cyan),
        Cell::new("p95 ms").fg(Color::Cyan),
        Cell::new("p99 ms").fg(Color::Cyan),
        Cell::new("Tok/s* p50").fg(Color::Yellow),
        Cell::new("Tok/s* p95").fg(Color::Yellow),
        Cell::new("Context p50").fg(Color::Cyan),
        Cell::new("Context p95").fg(Color::Cyan),
        Cell::new("Out/In p50").fg(Color::Cyan),
        Cell::new("TTFT p50").fg(Color::Yellow),
        Cell::new("TTFT p95").fg(Color::Yellow),
    ]);

    for s in stats {
        let model = s.model.as_deref().unwrap_or("(unknown)");
        let ttft_p50 = if s.ttft_count > 0 {
            s.ttft_p50_ms
                .map_or("—".to_string(), |v| format!("{}ms", v))
        } else {
            "—".to_string()
        };
        let ttft_p95 = if s.ttft_count > 0 {
            s.ttft_p95_ms
                .map_or("—".to_string(), |v| format!("{}ms", v))
        } else {
            "—".to_string()
        };
        table.add_row(vec![
            model,
            &s.count.to_string(),
            &s.p50_ms.to_string(),
            &s.p95_ms.to_string(),
            &s.p99_ms.to_string(),
            &s.derived_tokens_per_sec_p50
                .map_or("—".to_string(), |v| format!("{:.0}", v)),
            &s.derived_tokens_per_sec_p95
                .map_or("—".to_string(), |v| format!("{:.0}", v)),
            &s.input_tokens_p50
                .map_or("—".to_string(), |v| format_number(v as u64)),
            &s.input_tokens_p95
                .map_or("—".to_string(), |v| format_number(v as u64)),
            &s.output_input_ratio_p50
                .map_or("—".to_string(), |v| format!("{:.2}×", v)),
            &ttft_p50,
            &ttft_p95,
        ]);
    }

    println!("Latency Stats by Model (* = derived, span duration includes network+queue time):");
    println!("{}", table);
}

fn display_truncation_rate(rows: &[otelite_core::api::TruncationRateByModel]) {
    let any_truncated = rows.iter().any(|r| r.truncated > 0);
    if !any_truncated {
        println!("Truncation Rate by Model: no truncations observed");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Total Calls").fg(Color::Cyan),
        Cell::new("Truncated").fg(Color::Cyan),
        Cell::new("Rate").fg(Color::Cyan),
    ]);

    for r in rows {
        let model = r.model.as_deref().unwrap_or("(unknown)");
        let rate_pct = r.rate * 100.0;
        let rate_color = if rate_pct > 5.0 {
            Color::Red
        } else if rate_pct > 1.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        table.add_row(vec![
            Cell::new(model),
            Cell::new(r.total),
            Cell::new(r.truncated),
            Cell::new(format!("{:.1}%", rate_pct)).fg(rate_color),
        ]);
    }

    println!("Truncation Rate by Model (finish_reason = max_tokens/length):");
    println!("{}", table);
}

fn display_cache_hit_rate(rows: &[otelite_core::api::CacheHitRateByModel]) {
    let any_cache = rows.iter().any(|r| r.total_cache_read_tokens > 0);
    if !any_cache {
        println!("Cache Hit Rate: no cache reads observed");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Input Tokens").fg(Color::Cyan),
        Cell::new("Cache Read").fg(Color::Cyan),
        Cell::new("Cache Created").fg(Color::Cyan),
        Cell::new("Hit Rate").fg(Color::Cyan),
    ]);

    for r in rows {
        let model = r.model.as_deref().unwrap_or("(unknown)");
        let hit_pct = r.hit_rate.unwrap_or(0.0) * 100.0;
        let rate_color = if hit_pct >= 20.0 {
            Color::Green
        } else if hit_pct >= 5.0 {
            Color::Yellow
        } else {
            Color::White
        };
        table.add_row(vec![
            Cell::new(model),
            Cell::new(format_number(r.total_input_tokens)),
            Cell::new(format_number(r.total_cache_read_tokens)),
            Cell::new(format_number(r.total_cache_creation_tokens)),
            Cell::new(format!("{:.1}%", hit_pct)).fg(rate_color),
        ]);
    }

    println!("Cache Hit Rate by Model (cache_read / (cache_read + input)):");
    println!("{}", table);
}

fn display_request_params(profile: &otelite_core::api::RequestParamProfile) {
    if !profile.temperature_buckets.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic);

        table.set_header(vec![
            Cell::new("Temperature").fg(Color::Cyan),
            Cell::new("Calls").fg(Color::Cyan),
        ]);
        for b in &profile.temperature_buckets {
            let temp = b
                .temperature
                .map_or("not set".to_string(), |v| format!("{}", v));
            table.add_row(vec![&temp, &b.count.to_string()]);
        }
        println!("Temperature Distribution:");
        println!("{}", table);
    }

    if !profile.max_tokens_buckets.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic);

        table.set_header(vec![
            Cell::new("max_tokens").fg(Color::Cyan),
            Cell::new("Calls").fg(Color::Cyan),
        ]);
        for b in &profile.max_tokens_buckets {
            let mt = b
                .max_tokens
                .map_or("not set".to_string(), |v| format_number(v as u64));
            table.add_row(vec![&mt, &b.count.to_string()]);
        }
        println!("max_tokens Distribution:");
        println!("{}", table);
    }
}

fn display_conv_depth(depth: &otelite_core::api::ConversationDepthStats) {
    if depth.total_conversations == 0 {
        println!("Conversation Depth: no conversations with conversation_id observed");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Metric").fg(Color::Cyan),
        Cell::new("Value").fg(Color::Cyan),
    ]);

    table.add_row(vec![
        "Total conversations",
        &depth.total_conversations.to_string(),
    ]);
    table.add_row(vec!["Avg turns", &format!("{:.1}", depth.avg_turns)]);
    table.add_row(vec!["p50 turns", &depth.p50_turns.to_string()]);
    table.add_row(vec!["p95 turns", &depth.p95_turns.to_string()]);
    table.add_row(vec!["p99 turns", &depth.p99_turns.to_string()]);

    println!("Conversation Depth (turns per conversation_id):");
    println!("{}", table);
}

fn display_tool_usage(rows: &[otelite_core::api::ToolUsage]) {
    if rows.is_empty() {
        println!("Tool Usage: no tool calls observed");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Tool").fg(Color::Cyan),
        Cell::new("Calls").fg(Color::Cyan),
        Cell::new("Success%").fg(Color::Cyan),
        Cell::new("Errors").fg(Color::Cyan),
        Cell::new("Avg ms").fg(Color::Cyan),
    ]);

    for r in rows {
        let success_pct = if r.count > 0 {
            r.success_count as f64 / r.count as f64 * 100.0
        } else {
            0.0
        };
        let color = if success_pct < 90.0 {
            Color::Red
        } else if success_pct < 99.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        table.add_row(vec![
            Cell::new(&r.tool_name),
            Cell::new(r.count),
            Cell::new(format!("{:.1}%", success_pct)).fg(color),
            Cell::new(r.error_count),
            Cell::new(format!("{:.0}", r.avg_duration_ms)),
        ]);
    }

    println!("Tool Usage (success rate = calls without error / total calls):");
    println!("{}", table);
}

fn display_error_types(rows: &[otelite_core::api::ErrorTypeBreakdown]) {
    if rows.is_empty() {
        println!("Error Types: no error spans observed");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Bucket").fg(Color::Red),
        Cell::new("Error Type").fg(Color::Cyan),
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Count").fg(Color::Yellow),
    ]);

    for r in rows {
        let model = r.model.as_deref().unwrap_or("(unknown)");
        table.add_row(vec![
            Cell::new(&r.bucket).fg(Color::Red),
            Cell::new(&r.error_type),
            Cell::new(model),
            Cell::new(r.count).fg(Color::Yellow),
        ]);
    }

    println!("Error Type Breakdown (sorted by count; raw error_type shown for inspection):");
    println!("{}", table);
}

fn display_model_drift(rows: &[otelite_core::api::ModelDriftPair]) {
    let drifted: Vec<_> = rows.iter().filter(|r| r.differs).collect();
    if drifted.is_empty() {
        println!("Model Drift: no silent rerouting detected (request and response models match)");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(vec![
        Cell::new("Requested Model").fg(Color::Cyan),
        Cell::new("Served Model").fg(Color::Yellow),
        Cell::new("Count").fg(Color::Cyan),
    ]);

    for r in drifted {
        let req = r.request_model.as_deref().unwrap_or("(unknown)");
        let resp = r.response_model.as_deref().unwrap_or("(unknown)");
        table.add_row(vec![
            Cell::new(req),
            Cell::new(resp).fg(Color::Yellow),
            Cell::new(r.count),
        ]);
    }

    println!("Model Drift — provider served a different model than requested:");
    println!("{}", table);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_time_range_hours() {
        let (start, end) = parse_time_range("24h").unwrap();
        let diff = end - start;
        let expected = 24 * 3600 * 1_000_000_000i64;
        assert_eq!(diff, expected);
    }

    #[test]
    fn test_parse_time_range_days() {
        let (start, end) = parse_time_range("7d").unwrap();
        let diff = end - start;
        let expected = 7 * 24 * 3600 * 1_000_000_000i64;
        assert_eq!(diff, expected);
    }

    #[test]
    fn test_parse_time_range_minutes() {
        let (start, end) = parse_time_range("30m").unwrap();
        let diff = end - start;
        let expected = 30 * 60 * 1_000_000_000i64;
        assert_eq!(diff, expected);
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(123), "123");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(Some(0.1234)), "$0.1234");
        assert_eq!(format_cost(None), "—");
    }
}
