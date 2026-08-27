//! Tests for query_error_types and query_model_drift

use otelite_core::filters::GenAiFilters;
use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

fn insert_llm_span(conn: &Connection, span_id: &str, status_code: i64, attributes: &str) {
    conn.execute(
        &format!(
            r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
               VALUES ('trace1', '{}', 'llm.call', 0, 1000, 2000, '{}', {})"#,
            span_id, attributes, status_code
        ),
        [],
    )
    .unwrap();
}

// ── query_error_types ────────────────────────────────────────────────────────

#[test]
fn test_query_error_types_empty() {
    let conn = setup_test_db();
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn test_query_error_types_no_errors_when_all_ok() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        1,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4"}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert!(rows.is_empty(), "OK spans should not appear in error_types");
}

#[test]
fn test_query_error_types_rate_limit_bucket() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","error.type":"RateLimitError"}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bucket, "rate_limit");
    assert_eq!(rows[0].error_type, "RateLimitError");
    assert_eq!(rows[0].count, 1);
}

#[test]
fn test_query_error_types_http_429_bucket() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","http.response.status_code":429}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bucket, "rate_limit");
}

#[test]
fn test_query_error_types_timeout_bucket() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","error.type":"TimeoutError"}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bucket, "timeout");
}

#[test]
fn test_query_error_types_server_error_bucket() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","http.response.status_code":500}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bucket, "server_error");
}

#[test]
fn test_query_error_types_unknown_bucket_fallback() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","error.type":"SomeWeirdError"}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bucket, "unknown");
    assert_eq!(rows[0].error_type, "SomeWeirdError");
}

#[test]
fn test_query_error_types_multiple_buckets_sorted_by_count() {
    let conn = setup_test_db();
    // 3 rate_limit errors
    for i in 0..3 {
        insert_llm_span(
            &conn,
            &format!("span_rl_{}", i),
            2,
            r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","error.type":"RateLimitError"}"#,
        );
    }
    // 1 timeout error
    insert_llm_span(
        &conn,
        "span_to_1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","error.type":"TimeoutError"}"#,
    );
    let rows = reader::query_error_types(&conn, None, None, &GenAiFilters::default()).unwrap();
    // rate_limit (count=3) should come before timeout (count=1)
    assert!(rows[0].count >= rows[1].count);
    let buckets: Vec<&str> = rows.iter().map(|r| r.bucket.as_str()).collect();
    assert!(buckets.contains(&"rate_limit"));
    assert!(buckets.contains(&"timeout"));
}

#[test]
fn test_query_error_types_filters_by_model() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        2,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","error.type":"RateLimitError"}"#,
    );
    insert_llm_span(
        &conn,
        "span2",
        2,
        r#"{"gen_ai.system":"anthropic","gen_ai.request.model":"claude-sonnet-4","error.type":"RateLimitError"}"#,
    );
    let rows = reader::query_error_types(
        &conn,
        None,
        None,
        &GenAiFilters {
            model: Some("gpt-4".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    // Identity is `provider/model` when a provider is recorded (#143).
    assert_eq!(rows[0].model.as_deref(), Some("openai/gpt-4"));
}

// ── query_model_drift ────────────────────────────────────────────────────────

#[test]
fn test_query_model_drift_empty() {
    let conn = setup_test_db();
    let rows = reader::query_model_drift(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn test_query_model_drift_no_response_model() {
    let conn = setup_test_db();
    // Only request model set — no response model → no drift
    insert_llm_span(
        &conn,
        "span1",
        1,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4"}"#,
    );
    let rows = reader::query_model_drift(&conn, None, None, &GenAiFilters::default()).unwrap();
    // Row exists (request_model not null) but differs = false
    let drifted: Vec<_> = rows.iter().filter(|r| r.differs).collect();
    assert!(drifted.is_empty());
}

#[test]
fn test_query_model_drift_matching_models_no_drift() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        1,
        r#"{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","gen_ai.response.model":"gpt-4"}"#,
    );
    let rows = reader::query_model_drift(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].differs);
}

#[test]
fn test_query_model_drift_differing_models_detected() {
    let conn = setup_test_db();
    insert_llm_span(
        &conn,
        "span1",
        1,
        r#"{"gen_ai.system":"anthropic","gen_ai.request.model":"claude-3-5-sonnet","gen_ai.response.model":"claude-3-5-sonnet-20241022"}"#,
    );
    let rows = reader::query_model_drift(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].differs,
        "Different req/resp models should have differs=true"
    );
    assert_eq!(rows[0].request_model.as_deref(), Some("claude-3-5-sonnet"));
    assert_eq!(
        rows[0].response_model.as_deref(),
        Some("claude-3-5-sonnet-20241022")
    );
}

#[test]
fn test_query_model_drift_groups_identical_pairs() {
    let conn = setup_test_db();
    // Two spans with the same drift pair
    for i in 0..2 {
        insert_llm_span(
            &conn,
            &format!("span{}", i),
            1,
            r#"{"gen_ai.system":"anthropic","gen_ai.request.model":"claude-3-5-sonnet","gen_ai.response.model":"claude-3-5-sonnet-20241022"}"#,
        );
    }
    let rows = reader::query_model_drift(&conn, None, None, &GenAiFilters::default()).unwrap();
    assert_eq!(rows.len(), 1, "Identical pairs should be grouped");
    assert_eq!(rows[0].count, 2);
}
