//! Tests for `query_agent_roles` (sub-agent role attribution, issue #128).
//!
//! opencode emits cumulative counters keyed by the full label set
//! (agent, model, type, session.id); the query must window them correctly
//! and attribute sessions/models from `opencode.model.usage` presence rows.

use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

const T0: i64 = 1_700_000_000_000_000_000;

fn insert_metric_row(conn: &Connection, name: &str, timestamp: i64, value: i64, attributes: &str) {
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_int, attributes)
         VALUES (?1, 1, ?2, ?3, ?4)",
        rusqlite::params![name, timestamp, value, attributes],
    )
    .unwrap();
}

fn token_attrs(agent: Option<&str>, model: &str, token_type: &str, session: &str) -> String {
    match agent {
        Some(a) => format!(
            r#"{{"agent":"{a}","model":"{model}","type":"{token_type}","session.id":"{session}"}}"#
        ),
        None => format!(r#"{{"model":"{model}","type":"{token_type}","session.id":"{session}"}}"#),
    }
}

fn model_usage_attrs(agent: Option<&str>, model: &str, session: &str) -> String {
    match agent {
        Some(a) => format!(r#"{{"agent":"{a}","model":"{model}","session.id":"{session}"}}"#),
        None => format!(r#"{{"model":"{model}","session.id":"{session}"}}"#),
    }
}

#[test]
fn test_agent_roles_empty() {
    let conn = setup_test_db();
    let resp = reader::query_agent_roles(&conn, None, None).unwrap();
    assert!(resp.roles.is_empty());
    assert!(resp.unknown_share_pct.is_none());
    assert_eq!(resp.agents_covered, vec!["opencode".to_string()]);
}

#[test]
fn test_agent_roles_aggregation() {
    let conn = setup_test_db();

    // orchestrator / s1: input counter 100 @ T0 (in-window, no earlier
    // baseline) -> 300 @ T0+2. Window starts at T0, so the whole 300 is
    // in-window usage.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        100,
        &token_attrs(Some("orchestrator"), "model-a", "input", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        300,
        &token_attrs(Some("orchestrator"), "model-a", "input", "s1"),
    );
    // orchestrator / s1: output counter, single in-window row (no baseline).
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        50,
        &token_attrs(Some("orchestrator"), "model-a", "output", "s1"),
    );
    // orchestrator / s2: cacheRead counter 0 -> 1000 (delta 1000)
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &token_attrs(Some("orchestrator"), "model-a", "cacheRead", "s2"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        1000,
        &token_attrs(Some("orchestrator"), "model-a", "cacheRead", "s2"),
    );
    // reviewer-deep / s3: reasoning counter 0 -> 50 (delta 50)
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &token_attrs(Some("reviewer-deep"), "model-b", "reasoning", "s3"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        50,
        &token_attrs(Some("reviewer-deep"), "model-b", "reasoning", "s3"),
    );

    // Presence rows (opencode.model.usage) for sessions and models.
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        &model_usage_attrs(Some("orchestrator"), "model-a", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        &model_usage_attrs(Some("orchestrator"), "model-b", "s2"),
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        &model_usage_attrs(Some("reviewer-deep"), "model-b", "s3"),
    );

    let resp = reader::query_agent_roles(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(resp.roles.len(), 2);

    // Sorted by total tokens desc: orchestrator (300+50+1000=1350) first.
    // (Window starts at T0 where the first counter value sits, so there is no
    // earlier baseline and the in-window last value is the delta.)
    let orch = &resp.roles[0];
    assert_eq!(orch.role, "orchestrator");
    assert_eq!(orch.tokens.input, 300);
    assert_eq!(orch.tokens.output, 50);
    assert_eq!(orch.tokens.cache_read, 1000);
    assert_eq!(orch.tokens.cache_write, 0);
    assert_eq!(orch.tokens.reasoning, 0);
    assert_eq!(orch.sessions, 2, "s1 + s2");
    assert!((orch.share_pct.unwrap() - 1350.0 / 1400.0 * 100.0).abs() < 1e-9);
    // top_models: model-a carries all of orchestrator's tokens.
    assert_eq!(orch.top_models[0].model, "model-a");
    assert_eq!(orch.top_models[0].tokens.input, 300);
    assert!(orch.cost.is_none(), "cost is enriched by the API layer");

    let review = &resp.roles[1];
    assert_eq!(review.role, "reviewer-deep");
    assert_eq!(review.tokens.reasoning, 50);
    assert_eq!(review.sessions, 1);
}

#[test]
fn test_agent_roles_window_excludes_out_of_window_activity() {
    let conn = setup_test_db();
    // All activity at T0+10, window is [T0, T0+2].
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 10,
        500,
        &token_attrs(Some("orchestrator"), "model-a", "input", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 10,
        0,
        &model_usage_attrs(Some("orchestrator"), "model-a", "s1"),
    );

    let resp = reader::query_agent_roles(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert!(resp.roles.is_empty());
}

#[test]
fn test_agent_roles_missing_agent_label_is_unknown() {
    let conn = setup_test_db();
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &token_attrs(None, "model-a", "input", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        80,
        &token_attrs(None, "model-a", "input", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        &model_usage_attrs(None, "model-a", "s1"),
    );

    let resp = reader::query_agent_roles(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(resp.roles.len(), 1);
    assert_eq!(resp.roles[0].role, "unknown");
    assert_eq!(resp.roles[0].tokens.input, 80);
    assert_eq!(resp.unknown_share_pct, Some(100.0));
}

#[test]
fn test_agent_roles_ignores_unknown_token_types() {
    let conn = setup_test_db();
    // "weird" is not a known token type: it must be ignored, not misfiled.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0,
        0,
        &token_attrs(Some("orchestrator"), "model-a", "weird", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        999,
        &token_attrs(Some("orchestrator"), "model-a", "weird", "s1"),
    );

    let resp = reader::query_agent_roles(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(resp.roles.len(), 1, "role row exists via the token delta");
    assert_eq!(
        resp.roles[0].tokens.total(),
        0,
        "unknown token type must not be counted"
    );
    assert!(resp.unknown_share_pct.is_none());
}

#[test]
fn test_agent_roles_malformed_attributes_do_not_raise() {
    let conn = setup_test_db();
    // Corrupt attributes must not break the query (json_valid-gated extraction).
    insert_metric_row(&conn, "opencode.token.usage", T0, 100, "{corrupt");
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        200,
        &token_attrs(Some("orchestrator"), "model-a", "input", "s1"),
    );
    insert_metric_row(&conn, "opencode.model.usage", T0 + 1, 0, "{corrupt");

    let resp = reader::query_agent_roles(&conn, Some(T0), Some(T0 + 2)).unwrap();
    // The corrupt token row has a NULL `type` label -> its delta is ignored
    // (never misfiled), so the "unknown" role carries no countable tokens.
    // The important assertion: the query completed without error.
    let unknown = resp.roles.iter().find(|r| r.role == "unknown");
    assert!(unknown.is_none() || unknown.unwrap().tokens.total() == 0);
    let orch = resp
        .roles
        .iter()
        .find(|r| r.role == "orchestrator")
        .expect("valid row still aggregated");
    assert_eq!(orch.tokens.input, 200);
}
