//! Regression tests for the `query_finish_reasons` aggregation.
//!
//! The plural `gen_ai.response.finish_reasons` attribute is meant to be a JSON array,
//! but some instrumentations emit it as a scalar string. Previously `json_each` on such
//! values raised "malformed JSON" and the whole endpoint returned 500. The guard now
//! restricts iteration to values with `json_type = 'array'`.

use otelite_core::filters::GenAiFilters;
use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

fn insert_span(conn: &Connection, span_id: &str, attributes: &str) {
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', ?1, 'llm.call', 0, 1000, 2000, ?2, 1)"#,
        rusqlite::params![span_id, attributes],
    )
    .unwrap();
}

/// Insert a `claude_code.api_response_body` log whose `attributes.body` is an
/// arbitrary string (may or may not be valid JSON — the test point is that
/// query_finish_reasons must not error on malformed body JSON).
fn insert_claude_code_response_log(conn: &Connection, body_value: &str) {
    // Store the body field as a JSON-encoded string inside the attributes object,
    // matching how the writer emits these logs.
    let attrs = format!(
        r#"{{"body":{}}}"#,
        serde_json::to_string(body_value).unwrap()
    );
    conn.execute(
        r#"INSERT INTO logs (timestamp, severity_number, body, attributes)
           VALUES (1000, 9, 'claude_code.api_response_body', ?1)"#,
        rusqlite::params![attrs],
    )
    .unwrap();
}

#[test]
fn test_finish_reasons_empty() {
    let conn = setup_test_db();
    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn test_finish_reasons_plural_array() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "s1",
        r#"{"gen_ai.response.finish_reasons":["stop","length"]}"#,
    );
    insert_span(
        &conn,
        "s2",
        r#"{"gen_ai.response.finish_reasons":["stop"]}"#,
    );

    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    let stop = rows.iter().find(|r| r.reason == "stop").unwrap();
    assert_eq!(stop.count, 2);
    let length = rows.iter().find(|r| r.reason == "length").unwrap();
    assert_eq!(length.count, 1);
}

#[test]
fn test_finish_reasons_singular_scalar() {
    let conn = setup_test_db();
    insert_span(&conn, "s1", r#"{"gen_ai.response.finish_reason":"stop"}"#);
    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    let stop = rows.iter().find(|r| r.reason == "stop").unwrap();
    assert_eq!(stop.count, 1);
}

/// The regression: a span whose `finish_reasons` attribute is a scalar string, not
/// an array. The old query called `json_each` on this and raised "malformed JSON".
#[test]
fn test_finish_reasons_scalar_in_plural_field_does_not_error() {
    let conn = setup_test_db();
    insert_span(&conn, "s1", r#"{"gen_ai.response.finish_reasons":"stop"}"#);
    insert_span(&conn, "s2", r#"{"gen_ai.response.finish_reason":"length"}"#);

    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "length");
    assert_eq!(rows[0].count, 1);
}

/// Another regression shape: attribute is a JSON object rather than an array.
#[test]
fn test_finish_reasons_object_in_plural_field_does_not_error() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "s1",
        r#"{"gen_ai.response.finish_reasons":{"not":"an array"}}"#,
    );
    insert_span(
        &conn,
        "s2",
        r#"{"gen_ai.response.finish_reasons":["stop"]}"#,
    );

    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "stop");
}

/// Regression: a claude_code.api_response_body log whose body text is truncated
/// / malformed JSON previously crashed the whole finish_reasons query because
/// the logs UNION branch calls json_extract on the body string unguarded.
#[test]
fn test_finish_reasons_malformed_response_body_is_skipped() {
    let conn = setup_test_db();
    // A well-formed log (counts toward the result).
    insert_claude_code_response_log(
        &conn,
        r#"{"stop_reason":"end_turn","model":"claude-sonnet-4"}"#,
    );
    // Two malformed / truncated bodies — these must not error out the query.
    insert_claude_code_response_log(&conn, r#"{"stop_reason":"max_tokens","mod"#);
    insert_claude_code_response_log(&conn, "not even close to json");

    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "end_turn");
    assert_eq!(rows[0].count, 1);
}

/// Sanity: when the body JSON is well-formed but has no stop_reason, the row
/// doesn't contribute to finish_reasons counts.
#[test]
fn test_finish_reasons_body_without_stop_reason() {
    let conn = setup_test_db();
    insert_claude_code_response_log(&conn, r#"{"model":"claude-sonnet-4"}"#);
    let rows = reader::query_finish_reasons(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert!(rows.is_empty());
}
