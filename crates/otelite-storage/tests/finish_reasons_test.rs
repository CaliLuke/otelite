//! Regression tests for the `query_finish_reasons` aggregation.
//!
//! The plural `gen_ai.response.finish_reasons` attribute is meant to be a JSON array,
//! but some instrumentations emit it as a scalar string. Previously `json_each` on such
//! values raised "malformed JSON" and the whole endpoint returned 500. The guard now
//! restricts iteration to values with `json_type = 'array'`.

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

#[test]
fn test_finish_reasons_empty() {
    let conn = setup_test_db();
    let rows = reader::query_finish_reasons(&conn, None, None).unwrap();
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

    let rows = reader::query_finish_reasons(&conn, None, None).unwrap();
    let stop = rows.iter().find(|r| r.reason == "stop").unwrap();
    assert_eq!(stop.count, 2);
    let length = rows.iter().find(|r| r.reason == "length").unwrap();
    assert_eq!(length.count, 1);
}

#[test]
fn test_finish_reasons_singular_scalar() {
    let conn = setup_test_db();
    insert_span(&conn, "s1", r#"{"gen_ai.response.finish_reason":"stop"}"#);
    let rows = reader::query_finish_reasons(&conn, None, None).unwrap();
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

    let rows = reader::query_finish_reasons(&conn, None, None).unwrap();
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

    let rows = reader::query_finish_reasons(&conn, None, None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "stop");
}
