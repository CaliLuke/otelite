//! Tests for `query_provider_mix` (provider × model mix, issue #129).
//!
//! Three sources, each contributing through exactly one path (no double
//! counting): opencode counter deltas (provider from `opencode.model.usage`),
//! codex turn histogram (no provider attribute → "(unknown)"), and claude
//! llm_request spans (provider from `gen_ai.system`).

use otelite_core::api::RoleTokenUsage;
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

/// Insert a histogram metric row (`value_histogram = [count, sum, buckets]`).
fn insert_histogram_row(conn: &Connection, name: &str, timestamp: i64, sum: f64, attributes: &str) {
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_histogram, attributes)
         VALUES (?1, 2, ?2, ?3, ?4)",
        rusqlite::params![name, timestamp, format!("[1, {sum}, []]",), attributes],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)] // test helper: each named arg maps to one telemetry field
fn insert_claude_span(
    conn: &Connection,
    span_id: &str,
    start: i64,
    model: &str,
    system: Option<&str>,
    session: Option<&str>,
    tokens: RoleTokenUsage,
) {
    // Values are string attributes, matching the real claude_code telemetry.
    let mut attrs = format!(
        r#"{{"model":"{model}","gen_ai.request.model":"{model}","input_tokens":"{}","output_tokens":"{}","cache_creation_tokens":"{}","cache_read_tokens":"{}""#,
        tokens.input, tokens.output, tokens.cache_write, tokens.cache_read
    );
    if let Some(s) = system {
        attrs.push_str(&format!(r#","gen_ai.system":"{s}""#));
    }
    if let Some(s) = session {
        attrs.push_str(&format!(r#","session.id":"{s}""#));
    }
    attrs.push('}');
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', ?1, 'claude_code.llm_request', 0, ?2, ?2 + 1, ?3, 1)"#,
        rusqlite::params![span_id, start, attrs],
    )
    .unwrap();
}

fn find_provider<'a>(
    resp: &'a otelite_core::api::ProviderMixResponse,
    provider: &str,
) -> &'a otelite_core::api::ProviderMixEntry {
    resp.providers
        .iter()
        .find(|p| p.provider == provider)
        .unwrap_or_else(|| panic!("provider {provider:?} not found in {:?}", resp.providers))
}

fn find_model<'a>(
    entry: &'a otelite_core::api::ProviderMixEntry,
    model: &str,
) -> &'a otelite_core::api::ProviderModelEntry {
    entry
        .models
        .iter()
        .find(|m| m.model == model)
        .unwrap_or_else(|| panic!("model {model:?} not found in {:?}", entry.models))
}

#[test]
fn test_provider_mix_empty() {
    let conn = setup_test_db();
    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert!(resp.providers.is_empty());
    assert_eq!(resp.total_tokens, 0);
    assert_eq!(resp.method, "direct");
}

#[test]
fn test_provider_mix_opencode_attribution() {
    let conn = setup_test_db();

    // orchestrator / model-a / s1: input counter 0 @ T0-10 (baseline,
    // pre-window) -> 400 @ T0+2 (in-window). Delta = 400.
    let attrs = |agent: &str, model: &str, ttype: &str, sid: &str| {
        format!(r#"{{"agent":"{agent}","model":"{model}","type":"{ttype}","session.id":"{sid}"}}"#)
    };
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        0,
        &attrs("orchestrator", "model-a", "input", "s1"),
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        400,
        &attrs("orchestrator", "model-a", "input", "s1"),
    );
    // Same series, output: single in-window row 50 (no baseline) -> 50.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        50,
        &attrs("orchestrator", "model-a", "output", "s1"),
    );
    // Presence rows: model-a is served by provider "bv" (2 rows -> weight 2).
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        r#"{"agent":"orchestrator","model":"model-a","provider":"bv","session.id":"s1"}"#,
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        r#"{"agent":"orchestrator","model":"model-a","provider":"bv","session.id":"s1"}"#,
    );

    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(resp.method, "direct", "single provider per model");
    assert_eq!(resp.total_tokens, 450);
    assert_eq!(resp.providers.len(), 1);

    let bv = find_provider(&resp, "bv");
    assert!((bv.share_pct.unwrap() - 100.0).abs() < 1e-9);
    assert!(bv.cost_usd.is_none(), "cost is enriched by the API layer");
    let model_a = find_model(bv, "model-a");
    assert_eq!(model_a.tokens.input, 400);
    assert_eq!(model_a.tokens.output, 50);
    assert_eq!(model_a.sessions, 1);
    assert!(model_a.cost_usd.is_none());
}

#[test]
fn test_provider_mix_window_excludes_out_of_window_activity() {
    let conn = setup_test_db();
    // All activity at T0 + 10; window is [T0, T0 + 2].
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 10,
        500,
        r#"{"agent":"orchestrator","model":"model-a","type":"input","session.id":"s1"}"#,
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 10,
        0,
        r#"{"model":"model-a","provider":"bv","session.id":"s1"}"#,
    );

    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert!(resp.providers.is_empty());
    assert_eq!(resp.total_tokens, 0);
}

#[test]
fn test_provider_mix_codex_unknown_provider_total_type_skipped() {
    let conn = setup_test_db();
    // codex.turn.token_usage histograms for gpt-5.6-terra:
    //  input 1000, output 200, cached_input 3000, cache_write_input 500,
    //  reasoning_output 700, total 1300 (must be skipped: sum of the parts).
    let m = |ttype: &str| format!(r#"{{"model":"gpt-5.6-terra","token_type":"{ttype}"}}"#);
    insert_histogram_row(&conn, "codex.turn.token_usage", T0, 1000.0, &m("input"));
    insert_histogram_row(&conn, "codex.turn.token_usage", T0, 200.0, &m("output"));
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        3000.0,
        &m("cached_input"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        500.0,
        &m("cache_write_input"),
    );
    insert_histogram_row(
        &conn,
        "codex.turn.token_usage",
        T0,
        700.0,
        &m("reasoning_output"),
    );
    insert_histogram_row(&conn, "codex.turn.token_usage", T0, 1300.0, &m("total"));

    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    let unknown = find_provider(&resp, "(unknown)");
    let model = find_model(unknown, "gpt-5.6-terra");
    assert_eq!(model.tokens.input, 1000);
    assert_eq!(model.tokens.output, 200);
    assert_eq!(model.tokens.cache_read, 3000);
    assert_eq!(model.tokens.cache_write, 500);
    assert_eq!(model.tokens.reasoning, 700);
    assert_eq!(
        model.tokens.total(),
        5400,
        "total (1300) must not be double-counted"
    );
    assert_eq!(model.sessions, 0, "codex rows carry no session id");
}

#[test]
fn test_provider_mix_claude_system_provider() {
    let conn = setup_test_db();
    // Two claude_code.llm_request spans, same model+system, two sessions.
    insert_claude_span(
        &conn,
        "sp1",
        T0,
        "claude-opus-5",
        Some("anthropic"),
        Some("sess-1"),
        RoleTokenUsage {
            input: 100,
            output: 20,
            cache_read: 0,
            cache_write: 5,
            reasoning: 0,
        },
    );
    insert_claude_span(
        &conn,
        "sp2",
        T0 + 1,
        "claude-opus-5",
        Some("anthropic"),
        Some("sess-2"),
        RoleTokenUsage {
            input: 300,
            output: 40,
            cache_read: 10,
            cache_write: 15,
            reasoning: 0,
        },
    );

    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    let anthropic = find_provider(&resp, "anthropic");
    let model = find_model(anthropic, "claude-opus-5");
    assert_eq!(model.tokens.input, 400);
    assert_eq!(model.tokens.output, 60);
    assert_eq!(
        model.tokens.cache_write, 20,
        "cache_creation maps to cache_write"
    );
    assert_eq!(model.tokens.cache_read, 10);
    assert_eq!(model.sessions, 2);
}

#[test]
fn test_provider_mix_multi_provider_split() {
    let conn = setup_test_db();
    // model-x is served by bv (2 usage rows) and omlx (6 usage rows) in the
    // window -> 1:3 token-share split. Counter: input 0 @ T0-10 -> 800 @ T0+2
    // (delta 800).
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 - 10,
        0,
        r#"{"agent":"orchestrator","model":"model-x","type":"input","session.id":"s1"}"#,
    );
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        800,
        r#"{"agent":"orchestrator","model":"model-x","type":"input","session.id":"s1"}"#,
    );
    for _ in 0..2 {
        insert_metric_row(
            &conn,
            "opencode.model.usage",
            T0 + 1,
            0,
            r#"{"model":"model-x","provider":"bv","session.id":"s1"}"#,
        );
    }
    for _ in 0..6 {
        insert_metric_row(
            &conn,
            "opencode.model.usage",
            T0 + 1,
            0,
            r#"{"model":"model-x","provider":"omlx","session.id":"s1"}"#,
        );
    }

    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(
        resp.method, "token-share-split",
        "a multi-provider model must flag the split method"
    );
    // 800 tokens split 1:3 -> bv 200, omlx 600.
    let bv = find_provider(&resp, "bv");
    assert_eq!(find_model(bv, "model-x").tokens.input, 200);
    let omlx = find_provider(&resp, "omlx");
    assert_eq!(find_model(omlx, "model-x").tokens.input, 600);
    // Shares: bv 25%, omlx 75%.
    assert!((bv.share_pct.unwrap() - 25.0).abs() < 1e-9);
    assert!((omlx.share_pct.unwrap() - 75.0).abs() < 1e-9);
    assert_eq!(resp.total_tokens, 800);
}

#[test]
fn test_provider_mix_harnesses_combine_under_shared_provider() {
    let conn = setup_test_db();
    // opencode serves aws/claude-opus-5 via provider "anthropic" (2 counter
    // rows, delta 100 input) and claude_code serves claude-opus-5 via
    // gen_ai.system "anthropic" (300 input). Both land under provider
    // "anthropic" as distinct models.
    insert_metric_row(
        &conn,
        "opencode.token.usage",
        T0 + 2,
        100,
        r#"{"agent":"orchestrator","model":"aws/claude-opus-5","type":"input","session.id":"s1"}"#,
    );
    insert_metric_row(
        &conn,
        "opencode.model.usage",
        T0 + 1,
        0,
        r#"{"model":"aws/claude-opus-5","provider":"anthropic","session.id":"s1"}"#,
    );
    insert_claude_span(
        &conn,
        "sp1",
        T0,
        "claude-opus-5",
        Some("anthropic"),
        Some("sess-1"),
        RoleTokenUsage {
            input: 300,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
    );

    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(resp.method, "direct");
    let anthropic = find_provider(&resp, "anthropic");
    assert_eq!(anthropic.models.len(), 2, "distinct models, one provider");
    assert_eq!(find_model(anthropic, "aws/claude-opus-5").tokens.input, 100);
    assert_eq!(find_model(anthropic, "claude-opus-5").tokens.input, 300);
    // Sorted by tokens desc: claude-opus-5 (300) first.
    assert_eq!(anthropic.models[0].model, "claude-opus-5");
    assert!((anthropic.share_pct.unwrap() - 100.0).abs() < 1e-9);
}

#[test]
fn test_provider_mix_malformed_rows_do_not_raise() {
    let conn = setup_test_db();
    // Corrupt attributes on all three sources: json_valid-gated extraction
    // must yield NULL (never an error).
    insert_metric_row(&conn, "opencode.token.usage", T0, 100, "{corrupt");
    insert_metric_row(&conn, "opencode.token.usage", T0 + 2, 200, "{corrupt");
    insert_metric_row(&conn, "opencode.model.usage", T0 + 1, 0, "{corrupt");
    // Corrupt histogram: value_histogram is not valid JSON.
    conn.execute(
        "INSERT INTO metrics (name, metric_type, timestamp, value_histogram, attributes)
         VALUES ('codex.turn.token_usage', 2, ?1, '{corrupt', ?2)",
        rusqlite::params![T0, r#"{"model":"gpt-5.6-terra","token_type":"input"}"#],
    )
    .unwrap();
    // Corrupt claude span attributes.
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', 'sp1', 'claude_code.llm_request', 0, ?1, ?1 + 1, '{corrupt', 1)"#,
        rusqlite::params![T0],
    )
    .unwrap();

    // The query must complete. The corrupt rows produce no model/system, so
    // nothing countable is attributed (never misfiled).
    let resp = reader::query_provider_mix(&conn, Some(T0), Some(T0 + 2)).unwrap();
    assert_eq!(
        resp.total_tokens, 0,
        "corrupt rows must not contribute tokens"
    );
}

#[test]
fn test_provider_mix_role_token_usage_total() {
    // Sanity: the shared DTO total is the sum of the five categories.
    let t = RoleTokenUsage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        reasoning: 5,
    };
    assert_eq!(t.total(), 15);
}
