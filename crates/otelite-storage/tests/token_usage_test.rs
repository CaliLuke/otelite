//! Tests for token usage query functionality

use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

#[test]
fn test_query_token_usage_empty() {
    let conn = setup_test_db();
    let (summary, by_model, by_system) =
        reader::query_token_usage(&conn, None, None, None).unwrap();

    assert_eq!(summary.total_input_tokens, 0);
    assert_eq!(summary.total_output_tokens, 0);
    assert_eq!(summary.total_requests, 0);
    assert_eq!(summary.total_cache_creation_tokens, 0);
    assert_eq!(summary.total_cache_read_tokens, 0);
    assert_eq!(by_model.len(), 0);
    assert_eq!(by_system.len(), 0);
}

#[test]
fn test_query_token_usage_with_data() {
    let conn = setup_test_db();

    // Insert test spans with GenAI attributes
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace1', 'span1', 'llm.call', 0, 1000, 2000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","gen_ai.usage.input_tokens":"1000","gen_ai.usage.output_tokens":"500"}',
                   1)"#,
        [],
    )
    .unwrap();

    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace2', 'span2', 'llm.call', 0, 3000, 4000,
                   '{"gen_ai.system":"anthropic","gen_ai.request.model":"claude-sonnet-4","gen_ai.usage.input_tokens":"2000","gen_ai.usage.output_tokens":"800"}',
                   1)"#,
        [],
    )
    .unwrap();

    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace3', 'span3', 'llm.call', 0, 5000, 6000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","gen_ai.usage.input_tokens":"1500","gen_ai.usage.output_tokens":"600"}',
                   1)"#,
        [],
    )
    .unwrap();

    let (summary, by_model, by_system) =
        reader::query_token_usage(&conn, None, None, None).unwrap();

    // Check summary
    assert_eq!(summary.total_input_tokens, 4500); // 1000 + 2000 + 1500
    assert_eq!(summary.total_output_tokens, 1900); // 500 + 800 + 600
    assert_eq!(summary.total_requests, 3);

    // Check by_model (sorted by total tokens desc)
    assert_eq!(by_model.len(), 2);
    assert_eq!(by_model[0].model, "gpt-4");
    assert_eq!(by_model[0].input_tokens, 2500); // 1000 + 1500
    assert_eq!(by_model[0].output_tokens, 1100); // 500 + 600
    assert_eq!(by_model[0].requests, 2);

    assert_eq!(by_model[1].model, "claude-sonnet-4");
    assert_eq!(by_model[1].input_tokens, 2000);
    assert_eq!(by_model[1].output_tokens, 800);
    assert_eq!(by_model[1].requests, 1);

    // Check by_system (sorted by total tokens desc)
    assert_eq!(by_system.len(), 2);
    assert_eq!(by_system[0].system, "openai");
    assert_eq!(by_system[0].input_tokens, 2500);
    assert_eq!(by_system[0].output_tokens, 1100);
    assert_eq!(by_system[0].requests, 2);

    assert_eq!(by_system[1].system, "anthropic");
    assert_eq!(by_system[1].input_tokens, 2000);
    assert_eq!(by_system[1].output_tokens, 800);
    assert_eq!(by_system[1].requests, 1);
}

#[test]
fn test_query_token_usage_with_time_filter() {
    let conn = setup_test_db();

    // Insert spans at different times
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace1', 'span1', 'llm.call', 0, 1000, 2000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","gen_ai.usage.input_tokens":"1000","gen_ai.usage.output_tokens":"500"}',
                   1)"#,
        [],
    )
    .unwrap();

    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace2', 'span2', 'llm.call', 0, 5000, 6000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","gen_ai.usage.input_tokens":"2000","gen_ai.usage.output_tokens":"800"}',
                   1)"#,
        [],
    )
    .unwrap();

    // Query with time filter (only first span)
    let (summary, by_model, _) =
        reader::query_token_usage(&conn, Some(0), Some(3000), None).unwrap();

    assert_eq!(summary.total_input_tokens, 1000);
    assert_eq!(summary.total_output_tokens, 500);
    assert_eq!(summary.total_requests, 1);
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_model[0].model, "gpt-4");
}

#[test]
fn test_query_token_usage_ignores_non_genai_spans() {
    let conn = setup_test_db();

    // Insert GenAI span
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace1', 'span1', 'llm.call', 0, 1000, 2000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4","gen_ai.usage.input_tokens":"1000","gen_ai.usage.output_tokens":"500"}',
                   1)"#,
        [],
    )
    .unwrap();

    // Insert non-GenAI span (no gen_ai.system attribute)
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace2', 'span2', 'http.request', 0, 3000, 4000,
                   '{"http.method":"GET","http.url":"/api/users"}',
                   1)"#,
        [],
    )
    .unwrap();

    let (summary, by_model, by_system) =
        reader::query_token_usage(&conn, None, None, None).unwrap();

    // Should only count the GenAI span
    assert_eq!(summary.total_input_tokens, 1000);
    assert_eq!(summary.total_output_tokens, 500);
    assert_eq!(summary.total_requests, 1);
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_system.len(), 1);
}

#[test]
fn test_query_token_usage_handles_missing_token_fields() {
    let conn = setup_test_db();

    // Insert span with gen_ai.system but no token counts
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('trace1', 'span1', 'llm.call', 0, 1000, 2000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"gpt-4"}',
                   1)"#,
        [],
    )
    .unwrap();

    let (summary, by_model, _by_system) =
        reader::query_token_usage(&conn, None, None, None).unwrap();

    // Should handle missing fields gracefully (COALESCE to 0)
    assert_eq!(summary.total_input_tokens, 0);
    assert_eq!(summary.total_output_tokens, 0);
    assert_eq!(summary.total_requests, 1);
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_model[0].model, "gpt-4");
    assert_eq!(by_model[0].input_tokens, 0);
    assert_eq!(by_model[0].output_tokens, 0);
}

#[test]
fn test_analytics_adapters_select_codex_requests_and_opencode_llm_calls() {
    let conn = setup_test_db();

    // Codex tags many internal spans with `model`. Only run_sampling_request is
    // one completed model call; the nested transport span must not be counted.
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('codex-trace', 'codex-request', 'run_sampling_request', 0, 0, 4000000000,
                   '{"model":"codex-test-model","otel.scope.name":"codex_cli_rs"}', 1)"#,
        [],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, parent_span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('codex-trace', 'codex-http', 'codex-request', 'model_client.stream_responses_api', 0, 0, 3000000000,
                   '{"model":"codex-test-model","otel.scope.name":"codex_cli_rs"}', 1)"#,
        [],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('other-trace', 'other-request', 'run_sampling_request', 0, 0, 5000000000,
                   '{"model":"codex-test-model","otel.scope.name":"other_exporter"}', 1)"#,
        [],
    )
    .unwrap();

    // OpenCode already emits OpenInference LLM spans. Keep that existing
    // adapter path covered alongside the Codex-specific one.
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('opencode-trace', 'opencode-request', 'opencode.llm', 0, 0, 2000000000,
                   '{"openinference.span.kind":"LLM","llm.system":"opencode-go","llm.model_name":"opencode-test-model","llm.usage.prompt_tokens":"100","llm.usage.completion_tokens":"40"}', 1)"#,
        [],
    )
    .unwrap();

    let (summary, by_model, by_system) =
        reader::query_token_usage(&conn, None, None, None).unwrap();
    assert_eq!(summary.total_requests, 1);
    assert_eq!(summary.total_input_tokens, 100);
    assert_eq!(summary.total_output_tokens, 40);

    assert!(
        !by_model.iter().any(|row| row.model == "codex-test-model"),
        "token analytics must not present unavailable Codex usage as zero"
    );

    let opencode = by_model
        .iter()
        .find(|row| row.model == "opencode-test-model")
        .expect("OpenCode model should appear in analytics");
    assert_eq!(opencode.requests, 1);
    assert_eq!(opencode.input_tokens, 100);
    assert_eq!(opencode.output_tokens, 40);
    assert_eq!(by_system.len(), 1);
    assert_eq!(by_system[0].system, "opencode-go");

    let latency = reader::query_latency_stats(&conn, None, None, None).unwrap();
    let codex_latency = latency
        .iter()
        .find(|row| row.model.as_deref() == Some("codex-test-model"))
        .expect("Codex latency should use run_sampling_request");
    assert_eq!(codex_latency.count, 1);
    assert_eq!(codex_latency.avg_ms, 4000.0);
    assert_eq!(codex_latency.input_tokens_p50, None);
    assert_eq!(codex_latency.derived_tokens_per_sec_p50, None);
    assert_eq!(codex_latency.output_input_ratio_p50, None);

    let series = reader::query_latency_series(&conn, None, None, 3600, None, false).unwrap();
    assert_eq!(series.len(), 2);
    assert!(series.iter().any(|row| {
        row.model.as_deref() == Some("codex-test-model") && row.count == 1 && row.avg_ms == 4000.0
    }));

    let calls = reader::query_calls_series(&conn, None, None, 3600, false).unwrap();
    assert!(calls
        .iter()
        .any(|row| { row.model.as_deref() == Some("codex-test-model") && row.requests == 1 }));

    // Codex does not emit usage attributes, so it must not appear in cost
    // analytics as a fabricated zero-cost request.
    let cost_series =
        reader::query_cost_series(&conn, None, None, 3600 * 1_000_000_000, None).unwrap();
    assert_eq!(cost_series.len(), 1);
    assert_eq!(cost_series[0].model.as_deref(), Some("opencode-test-model"));

    let top_spans = reader::query_top_spans(
        &conn,
        None,
        None,
        10,
        otelite_core::api::TopSpanSort::TotalTokens,
        false,
    )
    .unwrap();
    assert_eq!(top_spans.len(), 1);
    assert_eq!(top_spans[0].model.as_deref(), Some("opencode-test-model"));
}

#[test]
fn test_genai_capability_report_keeps_codex_usage_unavailable_and_deduplicates_spans() {
    let conn = setup_test_db();
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('codex-trace', 'request', 'run_sampling_request', 0, 0, 4000000000,
                   '{"model":"codex-test-model","otel.scope.name":"codex_cli_rs"}', 1)"#,
        [],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('codex-trace', 'request', 'run_sampling_request', 0, 0, 4000000000,
                   '{"model":"codex-test-model","otel.scope.name":"codex_cli_rs"}', 1)"#,
        [],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('opencode-trace', 'request', 'opencode.llm', 0, 0, 2000000000,
                   '{"openinference.span.kind":"LLM","llm.system":"opencode-go","llm.model_name":"opencode-test-model","llm.usage.prompt_tokens":"0","llm.usage.completion_tokens":"40"}', 1)"#,
        [],
    )
    .unwrap();

    let report = reader::query_genai_capabilities(&conn, None, None, None).unwrap();
    assert_eq!(report.canonical_span_count, 2);
    assert_eq!(report.duplicate_span_count, 1);
    let codex = report
        .reports
        .iter()
        .find(|row| row.emitter == "codex")
        .unwrap();
    assert_eq!(codex.output_tokens.availability, "absent");
    assert_eq!(codex.output_tokens.derivation, "unavailable");
    assert_eq!(codex.correlation.rule, "none");

    let opencode = report
        .reports
        .iter()
        .find(|row| row.emitter == "opencode")
        .unwrap();
    assert_eq!(opencode.input_tokens.availability, "available");
    assert_eq!(opencode.input_tokens.valid_count, 1);
    assert_eq!(opencode.input_tokens.derivation, "native");
    assert_eq!(
        opencode
            .input_tokens
            .source_attributes
            .get("llm.usage.prompt_tokens"),
        Some(&1)
    );
}

#[test]
fn test_latency_stats_normalizes_ttft_and_flags_degenerate_groups() {
    let conn = setup_test_db();

    for index in 0..10 {
        conn.execute(
            r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
               VALUES (?1, ?2, 'claude_code.llm_request', 0, 0, 1000000000,
                       '{"model":"buffered-model","input_tokens":"100","ttft_ms":"950"}', 1)"#,
            rusqlite::params![format!("buffered-trace-{index}"), format!("buffered-span-{index}")],
        )
        .unwrap();
    }
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('otel-trace', 'otel-span', 'chat', 0, 0, 1000000000,
                   '{"gen_ai.system":"openai","gen_ai.request.model":"otel-model",
                     "gen_ai.server.time_to_first_token":"0.5"}', 1)"#,
        [],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('invalid-trace', 'invalid-span', 'claude_code.llm_request', 0, 0, 1000000000,
                   '{"model":"invalid-model","input_tokens":"100","ttft_ms":"1500"}', 1)"#,
        [],
    )
    .unwrap();

    let stats = reader::query_latency_stats(&conn, None, None, None).unwrap();
    let buffered = stats
        .iter()
        .find(|row| row.model.as_deref() == Some("buffered-model"))
        .unwrap();
    assert_eq!(buffered.ttft_count, 10);
    assert_eq!(buffered.ttft_p50_ms, Some(950));
    assert_eq!(buffered.ttft_degenerate_count, 10);
    assert!(buffered.ttft_degenerate);

    let otel = stats
        .iter()
        .find(|row| row.model.as_deref() == Some("otel-model"))
        .unwrap();
    assert_eq!(otel.ttft_p50_ms, Some(500));
    assert!(!otel.ttft_degenerate);

    let invalid = stats
        .iter()
        .find(|row| row.model.as_deref() == Some("invalid-model"))
        .unwrap();
    assert_eq!(invalid.ttft_count, 0);
    assert_eq!(invalid.ttft_invalid_count, 1);

    let series = reader::query_latency_series(&conn, None, None, 3600, None, false).unwrap();
    let buffered_series = series
        .iter()
        .find(|row| row.model.as_deref() == Some("buffered-model"))
        .unwrap();
    assert_eq!(buffered_series.ttft_count, 10);
    assert_eq!(buffered_series.ttft_degenerate_count, 10);
    assert!(buffered_series.ttft_degenerate);

    let context = reader::query_latency_by_context(&conn, None, None, None).unwrap();
    let buffered_context = context
        .iter()
        .find(|row| row.model.as_deref() == Some("buffered-model"))
        .unwrap();
    assert_eq!(buffered_context.ttft_count, 10);
    assert_eq!(buffered_context.ttft_degenerate_count, 10);
    assert!(buffered_context.ttft_degenerate);
}
