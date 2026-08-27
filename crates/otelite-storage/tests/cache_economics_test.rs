//! Tests for `query_cache_economics` (issue #130).
//!
//! Three sources, one per harness (no double counting): opencode
//! `token.usage` counter deltas, codex `turn.token_usage` histogram sums
//! (the `total` category is never counted), and claude `llm_request` span
//! token sums. Savings fields are left unenriched for the API layer.

use otelite_core::api::RoleTokenUsage;
use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

/// Window start, in nanoseconds, aligned to a 1-second boundary.
const T0: i64 = 1_700_000_000_000_000_000;
const BUCKET: i64 = 1_000_000_000; // 1 second
const END: i64 = T0 + 2_000_000_000; // 2-second window -> buckets T0, T0+1s

fn insert_metric_row(conn: &Connection, name: &str, timestamp: i64, value: i64, attributes: &str) {
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_int, attributes)
         VALUES (?1, 1, ?2, ?3, ?4)",
        rusqlite::params![name, timestamp, value, attributes],
    )
    .unwrap();
}

/// Insert a histogram metric row (`value_histogram = [count, sum, buckets]`).
fn insert_histogram_row(conn: &Connection, name: &str, timestamp: i64, sum: f64, attributes: &str) {
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_histogram, attributes)
         VALUES (?1, 2, ?2, ?3, ?4)",
        rusqlite::params![name, timestamp, format!("[1, {sum}, []]",), attributes],
    )
    .unwrap();
}

fn insert_claude_span(
    conn: &Connection,
    span_id: &str,
    start: i64,
    model: &str,
    tokens: RoleTokenUsage,
) {
    // Values are string attributes, matching the real claude_code telemetry.
    let attrs = format!(
        r#"{{"model":"{model}","gen_ai.request.model":"{model}","input_tokens":"{}","output_tokens":"{}","cache_creation_tokens":"{}","cache_read_tokens":"{}"}}"#,
        tokens.input, tokens.output, tokens.cache_write, tokens.cache_read
    );
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', ?1, 'claude_code.llm_request', 0, ?2, ?2 + 1, ?3, 1)"#,
        rusqlite::params![span_id, start, attrs],
    )
    .unwrap();
}

fn opencode_attrs(model: &str, ttype: &str, sid: &str) -> String {
    format!(r#"{{"agent":"a","model":"{model}","type":"{ttype}","session.id":"{sid}"}}"#)
}

fn find_model<'a>(
    resp: &'a otelite_core::api::CacheEconomicsResponse,
    model: &str,
) -> &'a otelite_core::api::CacheEconModelEntry {
    resp.models
        .iter()
        .find(|m| m.model == model)
        .unwrap_or_else(|| panic!("model {model:?} not found in {:?}", resp.models))
}

#[test]
fn test_cache_economics_empty() {
    let conn = setup_test_db();
    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    assert!(resp.models.is_empty());
    assert!(resp.series.is_empty());
}

#[test]
fn test_cache_economics_invalid_bucket() {
    let conn = setup_test_db();
    assert!(reader::query_cache_economics(&conn, Some(T0), Some(END), 0).is_err());
}

#[test]
fn test_cache_economics_opencode_counters() {
    let conn = setup_test_db();

    // cacheRead counter, series m/s1: baseline 100 @ T0-10 -> 300 @ T0+0.4s
    // (delta 200, bucket T0) -> 350 @ T0+1.4s (delta 50, bucket T0+1s).
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        100,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 400_000_000,
        300,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 1_400_000_000,
        350,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    // cacheCreation counter: single in-window row 10 (no baseline) -> 10.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 100_000_000,
        10,
        &opencode_attrs("m", "cacheCreation", "s1"),
    );
    // input counter: baseline 0 -> 50 (no pre-window row for this series
    // except value 0, which is its baseline).
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        0,
        &opencode_attrs("m", "input", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 100_000_000,
        50,
        &opencode_attrs("m", "input", "s1"),
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    let m = find_model(&resp, "m");
    assert_eq!(m.cache_read_tokens, 250);
    assert_eq!(m.cache_write_tokens, 10);
    assert_eq!(m.input_tokens, 50);
    // hit_rate = 250 / (250 + 50)
    assert!((m.hit_rate.unwrap() - 250.0 / 300.0).abs() < 1e-12);
    assert_eq!(m.read_write_ratio, Some(25.0));
    // savings left unenriched for the API layer
    assert_eq!(m.est_savings_usd, None);
    assert!(!m.savings_known);

    // bucket assignment: 200 read + 10 write + 50 input in bucket T0;
    // 50 read in bucket T0+1s.
    assert_eq!(resp.series.len(), 2);
    let b0 = resp.series.iter().find(|p| p.timestamp == T0).unwrap();
    let b1 = resp
        .series
        .iter()
        .find(|p| p.timestamp == T0 + BUCKET)
        .unwrap();
    assert_eq!(b0.cache_read, 200);
    assert_eq!(b0.cache_write, 10);
    assert_eq!(b0.input, 50);
    assert_eq!(b1.cache_read, 50);
    assert_eq!(b1.cache_write, 0);
    assert_eq!(b1.input, 0);
}

#[test]
fn test_cache_economics_opencode_in_window_reset() {
    let conn = setup_test_db();

    // Series with a high pre-window baseline, then a restart inside the
    // window: 20 @ T0 (below baseline -> counts its full value), 80 @
    // T0+1.4s (delta 60). Total = 80.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        1000,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        20,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 1_400_000_000,
        80,
        &opencode_attrs("m", "cacheRead", "s1"),
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    let m = find_model(&resp, "m");
    assert_eq!(m.cache_read_tokens, 80);
    let b0 = resp.series.iter().find(|p| p.timestamp == T0).unwrap();
    let b1 = resp
        .series
        .iter()
        .find(|p| p.timestamp == T0 + BUCKET)
        .unwrap();
    assert_eq!(b0.cache_read, 20);
    assert_eq!(b1.cache_read, 60);
}

#[test]
fn test_cache_economics_codex_histogram() {
    let conn = setup_test_db();

    // codex turn histogram; the `total` row is the sum of the parts and
    // output is not a cache category — both must be ignored.
    let att = |tt: &str| format!(r#"{{"model":"c-m","token_type":"{tt}"}}"#);
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        100.0,
        &att("cached_input"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        40.0,
        &att("cache_write_input"),
    );
    insert_histogram_row(&conn, "codex.turn.token_usage", T0, 300.0, &att("input"));
    insert_histogram_row(&conn, "codex.turn.token_usage", T0, 440.0, &att("total"));
    insert_histogram_row(&conn, "codex.turn.token_usage", T0, 50.0, &att("output"));
    // A second turn in the next bucket.
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0 + 1_500_000_000,
        25.0,
        &att("cached_input"),
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    let m = find_model(&resp, "c-m");
    assert_eq!(m.cache_read_tokens, 125);
    assert_eq!(m.cache_write_tokens, 40);
    assert_eq!(m.input_tokens, 300);
    // hit_rate = 125 / (125 + 300)
    assert!((m.hit_rate.unwrap() - 125.0 / 425.0).abs() < 1e-12);
    assert!((m.read_write_ratio.unwrap() - 125.0 / 40.0).abs() < 1e-12);

    let b0 = resp.series.iter().find(|p| p.timestamp == T0).unwrap();
    let b1 = resp
        .series
        .iter()
        .find(|p| p.timestamp == T0 + BUCKET)
        .unwrap();
    assert_eq!(b0.cache_read, 100);
    assert_eq!(b0.cache_write, 40);
    assert_eq!(b1.cache_read, 25);
}

#[test]
fn test_cache_economics_claude_spans() {
    let conn = setup_test_db();

    insert_claude_span(
        &conn,
        "sp1",
        T0,
        "k",
        RoleTokenUsage {
            input: 700,
            output: 100,
            cache_read: 100,
            cache_write: 20,
            reasoning: 0,
        },
    );
    insert_claude_span(
        &conn,
        "sp2",
        T0 + 1_500_000_000,
        "k",
        RoleTokenUsage {
            input: 100,
            output: 50,
            cache_read: 300,
            cache_write: 10,
            reasoning: 0,
        },
    );
    // A span for a different model must not leak into k.
    insert_claude_span(
        &conn,
        "sp3",
        T0,
        "other",
        RoleTokenUsage {
            input: 10,
            output: 10,
            cache_read: 10,
            cache_write: 10,
            reasoning: 0,
        },
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    let k = find_model(&resp, "k");
    assert_eq!(k.cache_read_tokens, 400);
    assert_eq!(k.cache_write_tokens, 30);
    assert_eq!(k.input_tokens, 800);
    assert!((k.hit_rate.unwrap() - 400.0 / 1200.0).abs() < 1e-12);
    assert!((k.read_write_ratio.unwrap() - 400.0 / 30.0).abs() < 1e-12);

    let other = find_model(&resp, "other");
    assert_eq!(other.cache_read_tokens, 10);
}

#[test]
fn test_cache_economics_same_model_across_sources() {
    let conn = setup_test_db();

    // Same model name reported by all three harnesses must be summed, not
    // double-counted or dropped.
    // opencode counter: cacheRead 0 -> 100.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        0,
        &opencode_attrs("x", "cacheRead", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        100,
        &opencode_attrs("x", "cacheRead", "s1"),
    );
    // codex histogram: cached_input 50.
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        50.0,
        r#"{"model":"x","token_type":"cached_input"}"#,
    );
    // claude span: cache_read 25.
    insert_claude_span(
        &conn,
        "sp1",
        T0,
        "x",
        RoleTokenUsage {
            input: 100,
            output: 0,
            cache_read: 25,
            cache_write: 0,
            reasoning: 0,
        },
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    let x = find_model(&resp, "x");
    assert_eq!(x.cache_read_tokens, 175);
    assert_eq!(x.input_tokens, 100);
}

#[test]
fn test_cache_economics_window_exclusion() {
    let conn = setup_test_db();

    // Entirely pre-window counter activity must not leak into the window:
    // baseline 100 -> 500 both before T0, nothing after.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 100,
        100,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 50,
        500,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    // Pre-window span.
    insert_claude_span(
        &conn,
        "sp0",
        T0 - 1000,
        "m",
        RoleTokenUsage {
            input: 0,
            output: 0,
            cache_read: 999,
            cache_write: 0,
            reasoning: 0,
        },
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    assert!(
        resp.models.is_empty(),
        "no in-window activity: expected empty models, got {:?}",
        resp.models
    );
    assert!(resp.series.is_empty());
}

/// Regression: the original span-based per-model hit-rate query (the
/// endpoint path used when `by_model` is not passed) is unchanged.
#[test]
fn test_cache_hit_rate_original_query_unchanged() {
    let conn = setup_test_db();

    insert_claude_span(
        &conn,
        "sp1",
        T0,
        "k",
        RoleTokenUsage {
            input: 700,
            output: 100,
            cache_read: 100,
            cache_write: 20,
            reasoning: 0,
        },
    );
    insert_claude_span(
        &conn,
        "sp2",
        T0 + 1,
        "k",
        RoleTokenUsage {
            input: 100,
            output: 50,
            cache_read: 300,
            cache_write: 10,
            reasoning: 0,
        },
    );

    let rows = reader::query_cache_hit_rate(&conn, Some(T0), Some(END), None).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.model.as_deref(), Some("k"));
    assert_eq!(row.total_input_tokens, 800);
    assert_eq!(row.total_cache_read_tokens, 400);
    assert_eq!(row.total_cache_creation_tokens, 30);
    // hit_rate = 400 / (400 + 800)
    assert!((row.hit_rate.unwrap() - 400.0 / 1200.0).abs() < 1e-12);

    // Model filter still works.
    let filtered = reader::query_cache_hit_rate(&conn, Some(T0), Some(END), Some("nope")).unwrap();
    assert!(filtered.is_empty());
}

#[test]
fn test_cache_economics_corrupt_rows_ignored() {
    let conn = setup_test_db();

    // Malformed attributes must not abort the query: json_valid gating makes
    // the label expressions NULL. One valid opencode row still counts.
    insert_metric_row(&conn, "opencode.token.usage", T0, 42, "not-json");
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        6,
        &opencode_attrs("m", "cacheRead", "s1"),
    );
    // Codex row with malformed histogram is skipped, valid one counts.
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_histogram, attributes)
         VALUES ('codex.turn.token_usage', 2, ?1, 'garbage', ?2)",
        rusqlite::params![T0, r#"{"model":"c-m","token_type":"cached_input"}"#],
    )
    .unwrap();
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        7.0,
        r#"{"model":"c-m","token_type":"cached_input"}"#,
    );

    let resp = reader::query_cache_economics(&conn, Some(T0), Some(END), BUCKET).unwrap();
    let m = find_model(&resp, "m");
    assert_eq!(m.cache_read_tokens, 6);
    let c = find_model(&resp, "c-m");
    assert_eq!(c.cache_read_tokens, 7);
}
