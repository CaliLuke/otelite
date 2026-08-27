//! Tests for per-call derived output throughput percentiles (issue #119,
//! slice #140): p10/p50/p90 from raw nanosecond durations, sample counts,
//! and the documented rounded-rank estimator at n = 9/10/11.

use otelite_core::filters::GenAiFilters;
use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

const T0: i64 = 1_700_000_000_000_000_000;
const END: i64 = T0 + 10_000_000_000;

/// Insert one `claude_code.llm_request` span with an exact duration.
/// `output_tokens: None` omits the attribute entirely.
#[allow(clippy::too_many_arguments)] // six precise span inputs, all load-bearing
fn insert_span(
    conn: &Connection,
    span_id: &str,
    model: &str,
    start_ns: i64,
    duration_ns: i64,
    output_tokens: Option<i64>,
) {
    let attrs = match output_tokens {
        Some(tokens) => format!(
            r#"{{"model":"{model}","gen_ai.request.model":"{model}","output_tokens":"{tokens}"}}"#
        ),
        None => format!(r#"{{"model":"{model}","gen_ai.request.model":"{model}"}}"#),
    };
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', ?1, 'claude_code.llm_request', 0, ?2, ?3, ?4, 1)"#,
        rusqlite::params![span_id, start_ns, start_ns + duration_ns, attrs],
    )
    .unwrap();
}

/// A 1-second span producing `tokens` tokens has a rate of `tokens` tok/s.
fn insert_rate(conn: &Connection, span_id: &str, model: &str, start_ns: i64, tokens: i64) {
    insert_span(conn, span_id, model, start_ns, 1_000_000_000, Some(tokens));
}

/// Rounded-rank estimator: idx = round((n-1)·p), clamped. These vectors pin
/// the exact index at each sample size the issue calls out (9/10/11).
#[test]
fn test_throughput_percentile_fixed_vectors_9_10_11() {
    let conn = setup_test_db();

    // n = 9, rates 1..=9: p10 idx=round(8*0.10)=1 -> 2; p50 idx=round(8*0.50)=4 -> 5;
    // p90 idx=round(8*0.90)=7 -> 8.
    for (i, tokens) in (1..=9i64).enumerate() {
        insert_rate(&conn, &format!("s9-{i}"), "m9", T0 + i as i64, tokens);
    }
    // n = 10, rates 1..=10: p10 idx=round(9*0.10)=1 -> 2; p50 idx=round(9*0.50)=5 (half-away-from-zero) -> 6;
    // p90 idx=round(9*0.90)=8 -> 9.
    for (i, tokens) in (1..=10i64).enumerate() {
        insert_rate(&conn, &format!("s10-{i}"), "m10", T0 + i as i64, tokens);
    }
    // n = 11, rates 1..=11: p10 idx=round(10*0.10)=1 -> 2; p50 idx=round(10*0.50)=5 -> 6;
    // p90 idx=round(10*0.90)=9 -> 10.
    for (i, tokens) in (1..=11i64).enumerate() {
        insert_rate(&conn, &format!("s11-{i}"), "m11", T0 + i as i64, tokens);
    }

    let rows =
        reader::query_latency_stats(&conn, Some(T0), Some(END), &GenAiFilters::default()).unwrap();
    let by_model: std::collections::HashMap<_, _> =
        rows.iter().map(|r| (r.model.clone().unwrap(), r)).collect();

    let m9 = by_model.get("m9").unwrap();
    assert_eq!(m9.throughput_sample_count, 9);
    assert_eq!(m9.derived_tokens_per_sec_p10.unwrap(), 2.0);
    assert_eq!(m9.derived_tokens_per_sec_p50.unwrap(), 5.0);
    assert_eq!(m9.derived_tokens_per_sec_p90.unwrap(), 8.0);

    let m10 = by_model.get("m10").unwrap();
    assert_eq!(m10.throughput_sample_count, 10);
    assert_eq!(m10.derived_tokens_per_sec_p10.unwrap(), 2.0);
    assert_eq!(m10.derived_tokens_per_sec_p50.unwrap(), 6.0);
    assert_eq!(m10.derived_tokens_per_sec_p90.unwrap(), 9.0);

    let m11 = by_model.get("m11").unwrap();
    assert_eq!(m11.throughput_sample_count, 11);
    assert_eq!(m11.derived_tokens_per_sec_p10.unwrap(), 2.0);
    assert_eq!(m11.derived_tokens_per_sec_p50.unwrap(), 6.0);
    assert_eq!(m11.derived_tokens_per_sec_p90.unwrap(), 10.0);
}

/// Rates must come from the per-call distribution, not total tokens divided
/// by total duration. 10 + 100 + 1000 tokens over 3 seconds aggregates to
/// 370 tok/s; the per-call p50 is 100.
#[test]
fn test_throughput_is_per_call_not_aggregate() {
    let conn = setup_test_db();
    insert_rate(&conn, "a", "m", T0, 10);
    insert_rate(&conn, "b", "m", T0 + 1, 100);
    insert_rate(&conn, "c", "m", T0 + 2, 1000);

    let rows =
        reader::query_latency_stats(&conn, Some(T0), Some(END), &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.throughput_sample_count, 3);
    assert_eq!(row.derived_tokens_per_sec_p10.unwrap(), 10.0);
    assert_eq!(row.derived_tokens_per_sec_p50.unwrap(), 100.0);
    assert_eq!(row.derived_tokens_per_sec_p90.unwrap(), 1000.0);
    // The aggregate figure (1110 / 3s = 370) must not appear anywhere.
    assert_ne!(row.derived_tokens_per_sec_p50.unwrap(), 370.0);
}

/// The rate divides by the raw nanosecond duration. 10 tokens in 1.5 ms is
/// 6666.67 tok/s; integer-millisecond truncation (1 ms) would give 10000.
#[test]
fn test_throughput_rate_uses_raw_nanoseconds() {
    let conn = setup_test_db();
    insert_span(&conn, "fast", "m", T0, 1_500_000, Some(10));

    let rows =
        reader::query_latency_stats(&conn, Some(T0), Some(END), &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    let rate = rows[0].derived_tokens_per_sec_p50.unwrap();
    assert!(
        (rate - 10e9 / 1_500_000.0).abs() < 1e-6,
        "expected 6666.67 tok/s, got {rate}"
    );
    assert!(
        (rate - 10_000.0).abs() > 1_000.0,
        "integer-ms truncation detected: {rate}"
    );
}

/// Eligibility: positive output AND positive duration. Zero-output and
/// zero-duration calls stay in `count` but not in `throughput_sample_count`.
#[test]
fn test_throughput_eligibility_and_sample_count() {
    let conn = setup_test_db();
    insert_rate(&conn, "e1", "m", T0, 10);
    insert_rate(&conn, "e2", "m", T0 + 1, 20);
    insert_rate(&conn, "e3", "m", T0 + 2, 30);
    insert_span(&conn, "no-output", "m", T0 + 3, 1_000_000_000, Some(0));
    insert_span(&conn, "no-attr", "m", T0 + 4, 1_000_000_000, None);
    insert_span(&conn, "zero-duration", "m", T0 + 5, 0, Some(50));

    let rows =
        reader::query_latency_stats(&conn, Some(T0), Some(END), &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.count, 6);
    assert_eq!(row.throughput_sample_count, 3);
    assert_eq!(row.derived_tokens_per_sec_p10.unwrap(), 10.0);
    assert_eq!(row.derived_tokens_per_sec_p50.unwrap(), 20.0);
    assert_eq!(row.derived_tokens_per_sec_p90.unwrap(), 30.0);
}

/// Series points carry the throughput triple, the lower-tail p10, and a
/// throughput sample count distinct from the duration count.
#[test]
fn test_latency_percentiles_series_throughput_fields() {
    let conn = setup_test_db();
    // One bucket (1-hour grid at T0): two throughput-eligible calls and one
    // duration-only call.
    insert_rate(&conn, "a", "m", T0, 10);
    insert_rate(&conn, "b", "m", T0 + 1, 100);
    insert_span(&conn, "c", "m", T0 + 2, 5_000, None);

    let resp = reader::query_latency_percentiles(
        &conn,
        Some(T0),
        Some(END),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        None,
    )
    .unwrap();

    let series = resp.metrics.get("duration").expect("duration series");
    let all = series.all.as_slice();
    assert_eq!(all.len(), 1);
    let point = &all[0];
    assert_eq!(point.count, 3);
    assert_eq!(point.throughput_sample_count, 2);
    assert_eq!(point.throughput_p10_tok_s.unwrap(), 10.0);
    assert_eq!(point.throughput_p50_tok_s.unwrap(), 100.0);
    assert_eq!(point.throughput_p90_tok_s.unwrap(), 100.0);
    // Lower tail of the duration values (0, 0, 5 ms) — present and <= p50.
    assert!(point.p10_ms <= point.p50_ms);

    let per_model = series
        .models
        .get("m")
        .expect("per-model series for m")
        .as_slice();
    assert_eq!(per_model.len(), 1);
    assert_eq!(per_model[0].throughput_sample_count, 2);
}

/// A bucket with no throughput-eligible calls: counts intact, throughput
/// fields absent (None / 0) rather than zero-valued measurements.
#[test]
fn test_latency_percentiles_series_throughput_none_when_ineligible() {
    let conn = setup_test_db();
    insert_span(&conn, "c", "m", T0, 5_000, None);

    let resp = reader::query_latency_percentiles(
        &conn,
        Some(T0),
        Some(END),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        None,
    )
    .unwrap();

    let series = resp.metrics.get("duration").unwrap();
    let point = &series.all.as_slice()[0];
    assert_eq!(point.count, 1);
    assert_eq!(point.throughput_sample_count, 0);
    assert!(point.throughput_p10_tok_s.is_none());
    assert!(point.throughput_p50_tok_s.is_none());
    assert!(point.throughput_p90_tok_s.is_none());
}

/// Serde compatibility (new client reading an older server response): the
/// pre-#119 payload has none of the new fields and must still deserialize.
#[test]
fn test_new_fields_deserialize_from_pre119_payloads() {
    // Pre-#119 LatencyStats JSON.
    let legacy_stats = r#"{
        "model": "m", "count": 2, "avg_ms": 100.0,
        "p50_ms": 90, "p95_ms": 110, "p99_ms": 120,
        "ttft_count": 0, "ttft_invalid_count": 0,
        "ttft_degenerate_count": 0, "ttft_degenerate": false,
        "ttft_p50_ms": null, "ttft_p95_ms": null, "ttft_p99_ms": null,
        "derived_tokens_per_sec_p50": 50.0,
        "derived_tokens_per_sec_p95": 60.0,
        "derived_tokens_per_sec_p99": 70.0,
        "input_tokens_p50": 1000, "input_tokens_p95": 2000, "input_tokens_p99": 3000,
        "output_input_ratio_p50": 0.1, "output_input_ratio_p95": 0.2, "output_input_ratio_p99": 0.3
    }"#;
    let stats: otelite_core::api::LatencyStats = serde_json::from_str(legacy_stats).unwrap();
    assert!(stats.derived_tokens_per_sec_p10.is_none());
    assert!(stats.derived_tokens_per_sec_p90.is_none());
    assert_eq!(stats.throughput_sample_count, 0);

    // Pre-#119 latency percentile point JSON.
    let legacy_point = r#"{"ts": 1700000000000000000, "p50_ms": 1.5, "p90_ms": 2.0,
                           "p95_ms": 2.5, "p99_ms": 3.0, "count": 4}"#;
    let point: otelite_core::api::LatencyPercentilePoint =
        serde_json::from_str(legacy_point).unwrap();
    assert_eq!(point.p10_ms, None, "absent p10_ms defaults to null");
    assert!(point.throughput_p10_tok_s.is_none());
    assert_eq!(point.throughput_sample_count, 0);
}
