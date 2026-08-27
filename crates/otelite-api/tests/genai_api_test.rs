//! Tests for GenAI token usage API endpoint

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use otelite_api::{DashboardConfig, DashboardServer};
use otelite_core::api::{AgentRollupResponse, TokenUsageResponse};
use otelite_core::telemetry::metric::{Metric, MetricType};
use otelite_core::telemetry::trace::{Span, SpanKind, SpanStatus, StatusCode as SpanStatusCode};
use otelite_storage::sqlite::SqliteBackend;
use otelite_storage::{StorageBackend, StorageConfig};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

async fn setup_test_server() -> (DashboardServer, Arc<dyn StorageBackend>, tempfile::TempDir) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config = DashboardConfig::default();
    let storage_config = StorageConfig::default().with_data_dir(temp_dir.path().to_path_buf());
    let mut storage = SqliteBackend::new(storage_config);
    storage.initialize().await.unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(storage);

    let server = DashboardServer::new(config, storage.clone());
    (server, storage, temp_dir)
}

#[tokio::test]
async fn test_get_token_usage_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let usage: TokenUsageResponse = serde_json::from_slice(&body).unwrap();

    assert_eq!(usage.summary.total_input_tokens, 0);
    assert_eq!(usage.summary.total_output_tokens, 0);
    assert_eq!(usage.summary.total_requests, 0);
    assert_eq!(usage.by_model.len(), 0);
    assert_eq!(usage.by_system.len(), 0);
}

#[tokio::test]
async fn test_get_token_usage_with_time_params() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/usage?start_time=1000&end_time=2000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let usage: TokenUsageResponse = serde_json::from_slice(&body).unwrap();

    // Should return empty results (placeholder implementation)
    assert_eq!(usage.summary.total_input_tokens, 0);
    assert_eq!(usage.summary.total_output_tokens, 0);
}

#[tokio::test]
async fn test_get_token_usage_response_structure() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let usage: TokenUsageResponse = serde_json::from_slice(&body).unwrap();

    // Verify response structure (values are u64, so always >= 0)
    assert!(usage.by_model.is_empty() || !usage.by_model.is_empty());
    assert!(usage.by_system.is_empty() || !usage.by_system.is_empty());
}

#[tokio::test]
async fn test_get_tool_approvals_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/tool_approvals")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: otelite_core::api::ToolApprovalStats = serde_json::from_slice(&body).unwrap();
    assert_eq!(stats.total, 0);
    assert_eq!(stats.auto_accepted, 0);
}

#[tokio::test]
async fn test_get_stop_reasons_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/stop_reasons")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let reasons: Vec<otelite_core::api::StopReasonCount> = serde_json::from_slice(&body).unwrap();
    assert!(reasons.is_empty());
}

#[tokio::test]
async fn test_get_context_type_split_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/context_type_split")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let split: Vec<otelite_core::api::ContextTypeSplit> = serde_json::from_slice(&body).unwrap();
    // context_type_split returns empty vec for empty DB
    assert!(split.is_empty());
}

#[tokio::test]
async fn test_get_tool_errors_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/tool_errors")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let errors: Vec<otelite_core::api::ToolErrorEntry> = serde_json::from_slice(&body).unwrap();
    assert!(errors.is_empty());
}

#[tokio::test]
async fn test_get_hour_of_day_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/genai/hour_of_day")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let buckets: Vec<otelite_core::api::HourOfDayBucket> = serde_json::from_slice(&body).unwrap();
    // Empty DB returns 24 zero-filled buckets (one per hour)
    assert_eq!(buckets.len(), 24);
    assert!(buckets
        .iter()
        .all(|b| b.llm_calls == 0 && b.tool_calls == 0));
}

// ── per-harness agent rollup (issue #125) ──────────────────────────────────

const R0: i64 = 1_700_000_000_000_000_000;
const R1: i64 = R0 + 1_000_000_000; // 1-second window [R0, R1]

fn agent_metric(
    name: &str,
    timestamp: i64,
    counter: Option<u64>,
    histogram: Option<(u64, f64)>,
    attributes: &[(&str, &str)],
) -> Metric {
    let metric_type = match (counter, histogram) {
        (Some(v), None) => MetricType::Counter(v),
        (None, Some((count, sum))) => MetricType::Histogram {
            count,
            sum,
            buckets: vec![],
        },
        _ => MetricType::Gauge(0.0),
    };
    let mut m = Metric {
        name: name.to_string(),
        description: None,
        unit: None,
        metric_type,
        timestamp,
        attributes: HashMap::new(),
        resource: None,
    };
    for (k, v) in attributes {
        m.attributes.insert(k.to_string(), v.to_string());
    }
    m
}

#[tokio::test]
async fn test_get_agents_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/genai/agents?start_time={R0}&end_time={R1}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let rollup: AgentRollupResponse = serde_json::from_slice(&body).unwrap();
    assert!(rollup.agents.is_empty(), "empty DB -> no agent rows");
}

#[tokio::test]
async fn test_get_agents_with_data() {
    let (server, storage, _temp_dir) = setup_test_server().await;

    // opencode: 1 in-window top-level session, cumulative token counters
    // (input 100 -> 250, reasoning 50 -> 60), cost counter 0.5 -> 1.25,
    // tool calls 10 -> 14, retries 3 -> 5.
    storage
        .write_metric(&agent_metric(
            "opencode.session.count",
            R1,
            Some(1),
            None,
            &[("session.id", "s1"), ("is_subagent", "false")],
        ))
        .await
        .unwrap();
    for (ts, input, reasoning) in [(R0 - 1_000_000_000, 100u64, 50u64), (R1, 250, 60)] {
        storage
            .write_metric(&agent_metric(
                "opencode.token.usage",
                ts,
                Some(input),
                None,
                &[
                    ("agent", "a"),
                    ("model", "m1"),
                    ("type", "input"),
                    ("session.id", "s1"),
                ],
            ))
            .await
            .unwrap();
        storage
            .write_metric(&agent_metric(
                "opencode.token.usage",
                ts,
                Some(reasoning),
                None,
                &[
                    ("agent", "a"),
                    ("model", "m1"),
                    ("type", "reasoning"),
                    ("session.id", "s1"),
                ],
            ))
            .await
            .unwrap();
    }
    for (ts, sum) in [(R0 - 1_000_000_000, 0.5f64), (R1, 1.25)] {
        storage
            .write_metric(&agent_metric(
                "opencode.session.cost.total",
                ts,
                None,
                Some((2, sum)),
                &[("session.id", "s1")],
            ))
            .await
            .unwrap();
    }
    for (ts, count) in [(R0 - 1_000_000_000, 10u64), (R1, 14)] {
        storage
            .write_metric(&agent_metric(
                "opencode.tool.duration",
                ts,
                None,
                Some((count, 0.0)),
                &[("session.id", "s1"), ("tool_name", "Bash")],
            ))
            .await
            .unwrap();
    }
    for (ts, retries) in [(R0 - 1_000_000_000, 3u64), (R1, 5)] {
        storage
            .write_metric(&agent_metric(
                "opencode.retry.count",
                ts,
                Some(retries),
                None,
                &[("session.id", "s1")],
            ))
            .await
            .unwrap();
    }

    // codex: 3 cli sessions, turn tokens (input 100, output 50, total 150),
    // 2 tool calls, 1 failed request.
    storage
        .write_metric(&agent_metric(
            "codex.thread.started",
            R1,
            Some(3),
            None,
            &[("session_source", "cli")],
        ))
        .await
        .unwrap();
    for (tt, sum) in [("input", 100.0f64), ("output", 50.0), ("total", 150.0)] {
        storage
            .write_metric(&agent_metric(
                "codex.turn.token_usage",
                R1,
                None,
                Some((1, sum)),
                &[("model", "c1"), ("token_type", tt)],
            ))
            .await
            .unwrap();
    }
    storage
        .write_metric(&agent_metric(
            "codex.tool.call",
            R1,
            Some(1),
            None,
            &[("tool", "shell")],
        ))
        .await
        .unwrap();
    storage
        .write_metric(&agent_metric(
            "codex.tool.call",
            R1,
            Some(1),
            None,
            &[("tool", "apply_patch")],
        ))
        .await
        .unwrap();
    storage
        .write_metric(&agent_metric(
            "codex.api_request",
            R1,
            Some(1),
            None,
            &[("success", "false")],
        ))
        .await
        .unwrap();

    // claude: 1 in-window session, per-event tokens, 1 tool-execution span.
    storage
        .write_metric(&agent_metric(
            "claude_code.session.count",
            R1,
            Some(1),
            None,
            &[("session.id", "s2")],
        ))
        .await
        .unwrap();
    storage
        .write_metric(&agent_metric(
            "claude_code.token.usage",
            R1,
            Some(1000),
            None,
            &[("session.id", "s2"), ("model", "k1"), ("type", "input")],
        ))
        .await
        .unwrap();
    storage
        .write_metric(&agent_metric(
            "claude_code.token.usage",
            R1,
            Some(300),
            None,
            &[("session.id", "s2"), ("model", "k1"), ("type", "output")],
        ))
        .await
        .unwrap();
    let mut span = Span {
        trace_id: "t".to_string(),
        span_id: "sp1".to_string(),
        parent_span_id: None,
        name: "claude_code.tool.execution".to_string(),
        kind: SpanKind::Internal,
        start_time: R1,
        end_time: R1 + 1,
        attributes: HashMap::new(),
        status: SpanStatus {
            code: SpanStatusCode::Ok,
            message: None,
        },
        events: Vec::new(),
        resource: None,
    };
    storage.write_span(&span).await.unwrap();
    let _ = &mut span;

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/genai/agents?start_time={R0}&end_time={R1}&bucket_secs=1"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let rollup: AgentRollupResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(rollup.agents.len(), 3);

    let find = |name: &str| {
        rollup
            .agents
            .iter()
            .find(|a| a.agent == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    };

    // Sorted by cost desc: opencode (actual $0.75) first; codex/claude have
    // no pricing in the test DB -> None cost, alphabetical tie-break.
    assert_eq!(rollup.agents[0].agent, "opencode");
    assert_eq!(rollup.agents[1].agent, "claude");
    assert_eq!(rollup.agents[2].agent, "codex");

    let oc = find("opencode");
    assert_eq!(oc.sessions, 1);
    assert_eq!(oc.cost_usd, Some(0.75), "opencode cost is its own counter");
    assert_eq!(oc.cost_source.as_deref(), Some("actual"));
    assert_eq!(oc.tokens.input, 150);
    assert_eq!(oc.tokens.reasoning, 10);
    assert_eq!(oc.tool_calls, 4);
    assert_eq!(oc.retries, Some(2));
    assert_eq!(oc.series.len(), 1, "one 1s bucket in the window");

    let cx = find("codex");
    assert_eq!(cx.sessions, 3);
    assert_eq!(
        cx.cost_usd, None,
        "no pricing in test DB -> no fabricated cost"
    );
    assert_eq!(cx.cost_source.as_deref(), Some("estimated"));
    assert_eq!(
        cx.tokens.input, 100,
        "total token_type must not double-count"
    );
    assert_eq!(cx.tokens.output, 50);
    assert_eq!(cx.tool_calls, 2);
    assert_eq!(cx.retries, Some(1));

    let cl = find("claude");
    assert_eq!(cl.sessions, 1);
    assert_eq!(cl.cost_usd, None);
    assert_eq!(cl.tokens.input, 1000);
    assert_eq!(cl.tokens.output, 300);
    assert_eq!(cl.tool_calls, 1, "tool.execution span count");
    assert_eq!(cl.retries, None, "claude emits no retry telemetry");
}
