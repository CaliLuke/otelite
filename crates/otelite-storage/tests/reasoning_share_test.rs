//! Tests for `query_reasoning_share` (issue #131).
//!
//! Sources: opencode `token.usage` counters (types reasoning/output),
//! codex `turn.token_usage` histograms (reasoning_output/output; the
//! `total` category is never counted), and codex `handle_responses` spans
//! for the global per-effort breakdown. claude_code is absent by design
//! (no thinking-token attributes in its spans).

use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

/// Window start, in nanoseconds, aligned to a 1-second boundary.
const T0: i64 = 1_700_000_000_000_000_000;
const END: i64 = T0 + 2_000_000_000; // 2-second window

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

fn insert_handle_responses_span(
    conn: &Connection,
    span_id: &str,
    start: i64,
    effort: Option<&str>,
    reasoning_tokens: Option<u64>,
) {
    // Flat dot-containing keys, matching the real codex telemetry.
    let mut attrs = String::from("{\"otel.scope.name\":\"codex_cli_rs\"");
    if let Some(effort) = effort {
        attrs.push_str(&format!(",\"codex.request.reasoning_effort\":\"{effort}\""));
    }
    if let Some(rt) = reasoning_tokens {
        attrs.push_str(&format!(
            ",\"codex.usage.reasoning_output_tokens\":\"{rt}\""
        ));
    }
    attrs.push('}');
    conn.execute(
        "INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
         VALUES ('t', ?1, 'handle_responses', 0, ?2, ?2 + 1, ?3, 1)",
        rusqlite::params![span_id, start, attrs],
    )
    .unwrap();
}

fn opencode_attrs(model: &str, ttype: &str, sid: &str) -> String {
    format!(r#"{{"agent":"a","model":"{model}","type":"{ttype}","session.id":"{sid}"}}"#)
}

fn codex_attrs(model: &str, tt: &str) -> String {
    format!(r#"{{"model":"{model}","token_type":"{tt}"}}"#)
}

fn find_model<'a>(
    resp: &'a otelite_core::api::ReasoningShareResponse,
    model: &str,
) -> &'a otelite_core::api::ReasoningShareByModel {
    resp.models
        .iter()
        .find(|m| m.model == model)
        .unwrap_or_else(|| panic!("model {model:?} not found in {:?}", resp.models))
}

#[test]
fn test_reasoning_share_empty() {
    let conn = setup_test_db();
    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    assert!(resp.models.is_empty());
    assert!(resp.effort.is_empty());
}

#[test]
fn test_reasoning_share_opencode_counters() {
    let conn = setup_test_db();

    // reasoning counter: baseline 100 @ T0-10 -> 300 @ T0 (delta 200).
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        100,
        &opencode_attrs("m", "reasoning", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        300,
        &opencode_attrs("m", "reasoning", "s1"),
    );
    // output counter: no baseline, 0 -> 500.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &opencode_attrs("m", "output", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 1_500_000_000,
        500,
        &opencode_attrs("m", "output", "s1"),
    );
    // Flat zero reasoning counter for another model must contribute nothing.
    for i in 0..3 {
        insert_metric_row(
            &conn,
            "opencode.token.usage",
            T0 + i * 100_000_000,
            0,
            &opencode_attrs("q", "reasoning", "s1"),
        );
    }

    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    let m = find_model(&resp, "m");
    assert_eq!(m.reasoning_tokens, 200);
    assert_eq!(m.output_tokens, 500);
    assert!((m.share_pct.unwrap() - 40.0).abs() < 1e-9);
    assert_eq!(m.cost_usd, None); // unenriched for the API layer
                                  // q's flat zero counter must not create a model entry
    assert!(resp.models.iter().all(|x| x.model != "q"));
}

#[test]
fn test_reasoning_share_codex_histogram() {
    let conn = setup_test_db();

    // Per-turn histogram: input and total must be ignored.
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        100.0,
        &codex_attrs("c", "input"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        250.0,
        &codex_attrs("c", "reasoning_output"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        400.0,
        &codex_attrs("c", "output"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        950.0,
        &codex_attrs("c", "total"),
    );
    // Second turn in the window.
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0 + 1_500_000_000,
        50.0,
        &codex_attrs("c", "reasoning_output"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0 + 1_500_000_000,
        100.0,
        &codex_attrs("c", "output"),
    );

    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    let c = find_model(&resp, "c");
    assert_eq!(c.reasoning_tokens, 300);
    assert_eq!(c.output_tokens, 500);
    assert!((c.share_pct.unwrap() - 60.0).abs() < 1e-9);
}

#[test]
fn test_reasoning_share_same_model_across_sources() {
    let conn = setup_test_db();

    // opencode counter: reasoning 0 -> 100.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &opencode_attrs("x", "reasoning", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        100,
        &opencode_attrs("x", "reasoning", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &opencode_attrs("x", "output", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        200,
        &opencode_attrs("x", "output", "s1"),
    );
    // codex histogram: reasoning 50, output 100.
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        50.0,
        &codex_attrs("x", "reasoning_output"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        100.0,
        &codex_attrs("x", "output"),
    );

    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    let x = find_model(&resp, "x");
    assert_eq!(x.reasoning_tokens, 150);
    assert_eq!(x.output_tokens, 300);
    assert!((x.share_pct.unwrap() - 50.0).abs() < 1e-9);
}

#[test]
fn test_reasoning_share_effort_grouping() {
    let conn = setup_test_db();

    insert_handle_responses_span(&conn, "sp1", T0, Some("medium"), Some(100));
    insert_handle_responses_span(&conn, "sp2", T0, Some("medium"), Some(50));
    insert_handle_responses_span(&conn, "sp3", T0, Some("medium"), None); // no rtok attr
    insert_handle_responses_span(&conn, "sp4", T0, Some("high"), Some(200));
    insert_handle_responses_span(&conn, "sp5", T0, None, Some(999)); // no effort: excluded
                                                                     // Pre-window span: excluded.
    insert_handle_responses_span(&conn, "sp6", T0 - 1_000_000_000, Some("low"), Some(777));

    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    assert_eq!(resp.effort.len(), 2);
    let medium = resp.effort.iter().find(|e| e.effort == "medium").unwrap();
    assert_eq!(medium.calls, 3);
    assert_eq!(medium.reasoning_tokens, 150);
    let high = resp.effort.iter().find(|e| e.effort == "high").unwrap();
    assert_eq!(high.calls, 1);
    assert_eq!(high.reasoning_tokens, 200);
    // sorted by calls desc
    assert_eq!(resp.effort[0].effort, "medium");
}

#[test]
fn test_reasoning_share_window_exclusion() {
    let conn = setup_test_db();

    // Entirely pre-window activity.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 100,
        0,
        &opencode_attrs("m", "reasoning", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 50,
        400,
        &opencode_attrs("m", "reasoning", "s1"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0 - 100,
        900.0,
        &codex_attrs("c", "reasoning_output"),
    );
    insert_handle_responses_span(&conn, "sp0", T0 - 1_000_000_000, Some("low"), Some(1));

    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    assert!(resp.models.is_empty());
    assert!(resp.effort.is_empty());
}

#[test]
fn test_reasoning_share_corrupt_rows_ignored() {
    let conn = setup_test_db();

    // Malformed attributes are skipped (json_valid gating), valid rows count.
    insert_metric_row(&conn, "opencode.token.usage", T0, 42, "not-json");
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        6,
        &opencode_attrs("m", "output", "s1"),
    );
    // Malformed histogram is skipped.
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_histogram, attributes)
         VALUES ('codex.turn.token_usage', 2, ?1, 'garbage', ?2)",
        rusqlite::params![T0, &codex_attrs("c", "reasoning_output")],
    )
    .unwrap();
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        7.0,
        &codex_attrs("c", "reasoning_output"),
    );

    let resp = reader::query_reasoning_share(&conn, Some(T0), Some(END)).unwrap();
    let m = find_model(&resp, "m");
    assert_eq!(m.output_tokens, 6);
    assert_eq!(m.reasoning_tokens, 0);
    assert_eq!(m.share_pct, Some(0.0)); // reasoning 0 / output 6
    let c = find_model(&resp, "c");
    assert_eq!(c.reasoning_tokens, 7);
    assert_eq!(c.output_tokens, 0);
    assert_eq!(c.share_pct, None); // no output tokens: share undefined
}
