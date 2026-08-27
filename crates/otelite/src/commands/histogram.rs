//! Distribution command: bin a named metric cohort (session cost, tool
//! duration, LLM duration, TTFT, output tokens) into ASCII bars + stats.

use crate::commands::usage::{fetch_pricing, parse_time_range};
use crate::error::{Error, Result};
use clap::Args;
use otelite_core::session_cost;
use otelite_storage::StorageBackend;
use std::sync::Arc;

/// Show a distribution (histogram + summary stats) of a named metric
#[derive(Debug, Args)]
pub struct HistogramCommand {
    /// Metric cohort: session_cost | tool_duration | llm_duration | ttft | output_tokens
    #[arg(value_parser = ["session_cost", "tool_duration", "llm_duration", "ttft", "output_tokens"])]
    pub metric: String,

    /// Time range to query (e.g., "1h", "24h", "7d", "30d")
    #[arg(long, default_value = "24h", value_parser = crate::commands::usage::validate_since)]
    pub since: String,

    /// Number of buckets (default 20, cap 100)
    #[arg(long, default_value = "20")]
    pub buckets: usize,

    /// Binning scale: linear | log
    #[arg(long, default_value = "linear", value_parser = ["linear", "log"])]
    pub scale: String,
}

impl HistogramCommand {
    pub async fn execute(
        &self,
        storage: Arc<dyn StorageBackend>,
        format: crate::config::OutputFormat,
    ) -> Result<()> {
        let (start_time, end_time) = parse_time_range(&self.since)?;

        let resp = if self.metric == "session_cost" {
            // Same source as the sessions cost panel (#126): opencode's own
            // cost counter ("actual") plus claude sessions priced from tokens.
            use otelite_core::distribution;

            let rows = storage
                .query_session_costs(Some(start_time), Some(end_time))
                .await
                .map_err(|e| Error::ApiError(format!("Failed to query session costs: {e}")))?;
            let pricing_db = fetch_pricing().await;
            let sessions = session_cost::build_session_costs(rows, &pricing_db);
            let values: Vec<f64> = sessions.iter().filter_map(|s| s.cost_usd).collect();
            distribution::build("session_cost", "usd", &self.scale, self.buckets, values)
        } else {
            storage
                .query_distribution(
                    &self.metric,
                    Some(start_time),
                    Some(end_time),
                    self.buckets,
                    &self.scale,
                )
                .await
                .map_err(|e| Error::ApiError(format!("Failed to query distribution: {e}")))?
        };

        use crate::config::OutputFormat;
        match format {
            OutputFormat::Json | OutputFormat::JsonCompact => {
                let json = if matches!(format, OutputFormat::JsonCompact) {
                    serde_json::to_string(&resp)
                } else {
                    serde_json::to_string_pretty(&resp)
                };
                println!(
                    "{}",
                    json.map_err(|e| Error::ApiError(format!("JSON serialization failed: {e}")))?
                );
            },
            OutputFormat::Pretty => display_distribution(&resp),
        }
        Ok(())
    }
}

fn fmt_value(v: f64, unit: &str) -> String {
    match unit {
        "usd" if v > 0.0 => format!("${:.4}", v),
        "ms" if v >= 1000.0 => format!("{:.2}s", v / 1000.0),
        "ms" => format!("{:.0}ms", v),
        "tokens" if v >= 1_000_000.0 => format!("{:.2}M", v / 1_000_000.0),
        "tokens" if v >= 1_000.0 => format!("{:.1}k", v),
        _ => format!("{:.4}", v),
    }
}

fn display_distribution(resp: &otelite_core::api::DistributionResponse) {
    println!(
        "{} distribution ({} buckets, {} scale):",
        resp.metric,
        resp.buckets.len(),
        resp.scale
    );
    if resp.buckets.is_empty() {
        println!("  no data in range");
        return;
    }
    let max_count = resp
        .buckets
        .iter()
        .map(|b| b.count)
        .max()
        .unwrap_or(1)
        .max(1);
    for b in &resp.buckets {
        let bar_len = ((b.count as f64 / max_count as f64) * 40.0).round() as usize;
        let bar = "#".repeat(bar_len);
        println!(
            "  {:>14} – {:<14} {:>6}  {}",
            fmt_value(b.min, &resp.unit),
            fmt_value(b.max, &resp.unit),
            b.count,
            bar
        );
    }
    if let Some(s) = &resp.stats {
        println!();
        println!(
            "  n={}  min={}  p50={}  p95={}  p99={}  max={}  mean={}",
            s.count,
            fmt_value(s.min, &resp.unit),
            fmt_value(s.p50, &resp.unit),
            fmt_value(s.p95, &resp.unit),
            fmt_value(s.p99, &resp.unit),
            fmt_value(s.max, &resp.unit),
            fmt_value(s.mean, &resp.unit),
        );
    }
}
