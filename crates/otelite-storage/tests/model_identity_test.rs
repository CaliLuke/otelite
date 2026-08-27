//! Tests for model identity (issue #119, slice #143):
//! identity = provider + request model, response model exposed separately,
//! no silent rerouting merge, alias-aware keys, provider collisions distinct.

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

/// Insert one `claude_code.llm_request` span with explicit attributes.
fn insert_span(conn: &Connection, span_id: &str, attrs: &str) {
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', ?1, 'claude_code.llm_request', 0, ?2, ?3, ?4, 1)"#,
        rusqlite::params![span_id, T0, T0 + 1_000_000_000, attrs],
    )
    .unwrap();
}

fn models_by_name(
    conn: &Connection,
    filters: &GenAiFilters,
) -> std::collections::HashMap<String, otelite_core::api::ModelUsage> {
    let (_summary, by_model, _systems) =
        reader::query_token_usage(conn, Some(T0), Some(END), filters).unwrap();
    by_model
        .into_iter()
        .map(|m| {
            let key = m.model.clone();
            (key, m)
        })
        .collect()
}

/// Two providers serving the same request model are distinct identities.
#[test]
fn test_provider_collision_keeps_identities_distinct() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "a",
        r#"{"gen_ai.request.model":"sonnet-x","gen_ai.system":"anthropic","output_tokens":"10"}"#,
    );
    insert_span(
        &conn,
        "b",
        r#"{"gen_ai.request.model":"sonnet-x","gen_ai.system":"openai","output_tokens":"20"}"#,
    );

    let m = models_by_name(&conn, &GenAiFilters::default());
    assert_eq!(m.len(), 2, "provider collision must not merge");
    assert_eq!(m["anthropic/sonnet-x"].requests, 1);
    assert_eq!(m["openai/sonnet-x"].requests, 1);
    assert_eq!(m["anthropic/sonnet-x"].output_tokens, 10);
    assert_eq!(m["openai/sonnet-x"].output_tokens, 20);
}

/// A rerouted response stays in the request-model identity; the response
/// model is exposed separately and counted in rerouted_count.
#[test]
fn test_rerouted_response_stays_in_request_identity() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "a",
        r#"{"gen_ai.request.model":"claude-sonnet","gen_ai.response.model":"claude-haiku","gen_ai.system":"anthropic","output_tokens":"10"}"#,
    );
    // Same identity, not rerouted — must not dilute the dominant response.
    insert_span(
        &conn,
        "b",
        r#"{"gen_ai.request.model":"claude-sonnet","gen_ai.response.model":"claude-sonnet","gen_ai.system":"anthropic","output_tokens":"10"}"#,
    );

    let m = models_by_name(&conn, &GenAiFilters::default());
    assert_eq!(m.len(), 1, "both calls belong to the request identity");
    let row = &m["anthropic/claude-sonnet"];
    assert_eq!(row.requests, 2);
    assert_eq!(row.rerouted_count, 1);
    assert_eq!(row.response_model.as_deref(), Some("claude-haiku"));
}

/// Dominant response model wins when several differing models appear.
#[test]
fn test_dominant_response_model() {
    let conn = setup_test_db();
    for (id, resp) in [("a", "haiku-1"), ("b", "haiku-1"), ("c", "haiku-2")] {
        insert_span(
            &conn,
            id,
            &format!(
                r#"{{"gen_ai.request.model":"opus","gen_ai.response.model":"{resp}","gen_ai.system":"anthropic","output_tokens":"1"}}"#
            ),
        );
    }
    let m = models_by_name(&conn, &GenAiFilters::default());
    let row = &m["anthropic/opus"];
    assert_eq!(row.rerouted_count, 3);
    assert_eq!(row.response_model.as_deref(), Some("haiku-1"));
}

/// A span carrying only a response model keeps it as its identity (the only
/// identifier available) and is never counted as rerouted.
#[test]
fn test_response_only_span_identity() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "a",
        r#"{"gen_ai.response.model":"haiku","output_tokens":"10"}"#,
    );

    let m = models_by_name(&conn, &GenAiFilters::default());
    assert_eq!(m.len(), 1);
    let row = &m["haiku"];
    assert_eq!(row.requests, 1);
    assert_eq!(row.rerouted_count, 0, "no request model → not rerouted");
    assert!(row.response_model.is_none());
}

/// Aliased key spellings (llm.*) build the same composite identity and feed
/// the drift view.
#[test]
fn test_aliased_keys_build_identity_and_drift() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "a",
        r#"{"llm.model_name":"gpt-x","llm.system":"openai","llm.response.model":"gpt-x-mini"}"#,
    );

    let m = models_by_name(&conn, &GenAiFilters::default());
    assert_eq!(m.len(), 1);
    let row = &m["openai/gpt-x"];
    assert_eq!(row.rerouted_count, 1);
    assert_eq!(row.response_model.as_deref(), Some("gpt-x-mini"));

    let drift =
        reader::query_model_drift(&conn, Some(T0), Some(END), &GenAiFilters::default()).unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].request_model.as_deref(), Some("gpt-x"));
    assert_eq!(drift[0].response_model.as_deref(), Some("gpt-x-mini"));
    assert!(drift[0].differs);
}

/// Bare-model filters (raw attribute predicates) still select cohorts whose
/// identities are composite.
#[test]
fn test_bare_model_filter_selects_composite_identity() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "a",
        r#"{"gen_ai.request.model":"sonnet-x","gen_ai.system":"anthropic","output_tokens":"10"}"#,
    );
    insert_span(
        &conn,
        "b",
        r#"{"gen_ai.request.model":"other","gen_ai.system":"anthropic","output_tokens":"1"}"#,
    );

    let m = models_by_name(
        &conn,
        &GenAiFilters {
            model: Some("sonnet-x".into()),
            ..Default::default()
        },
    );
    assert_eq!(m.len(), 1);
    assert_eq!(m["anthropic/sonnet-x"].requests, 1);
}

/// Latency stats group by the same identity as token usage.
#[test]
fn test_latency_stats_use_identity() {
    let conn = setup_test_db();
    insert_span(
        &conn,
        "a",
        r#"{"gen_ai.request.model":"sonnet-x","gen_ai.system":"anthropic","output_tokens":"10"}"#,
    );
    insert_span(
        &conn,
        "b",
        r#"{"gen_ai.request.model":"sonnet-x","gen_ai.system":"openai","output_tokens":"20"}"#,
    );

    let stats =
        reader::query_latency_stats(&conn, Some(T0), Some(END), &GenAiFilters::default()).unwrap();
    let labels: Vec<String> = stats
        .iter()
        .map(|s| s.model.clone().unwrap_or_default())
        .collect();
    assert_eq!(labels.len(), 2);
    assert!(labels.iter().any(|l| l == "anthropic/sonnet-x"));
    assert!(labels.iter().any(|l| l == "openai/sonnet-x"));
}
