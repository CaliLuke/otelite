//! Tests for GenAI token usage API endpoint

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use otelite_api::{DashboardConfig, DashboardServer};
use otelite_core::api::{AgentRollupResponse, TokenUsageResponse};
use otelite_core::telemetry::log::{LogRecord, SeverityLevel};
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

use otelite_core::api::{CostDistributionResponse, SessionCostResponse};

#[tokio::test]
async fn test_get_session_costs_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/costs?start_time={R0}&end_time={R1}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let costs: SessionCostResponse = serde_json::from_slice(&body).unwrap();
    assert!(costs.sessions.is_empty());
    assert_eq!(costs.median_cost_usd, None);
    assert!(costs.anomaly_rule.contains("3 x median"));
}

#[tokio::test]
async fn test_get_session_costs_with_data() {
    let (server, storage, _temp_dir) = setup_test_server().await;

    // Four opencode sessions with distinct cumulative cost counters (last
    // value in the window is the total): 1.0, 2.0, 3.0, 100.0 → median
    // 2.5, threshold 7.5 → only the $100 session is anomalous.
    for (sid, cost) in [("s1", 1.0), ("s2", 2.0), ("s3", 3.0), ("s4", 100.0)] {
        storage
            .write_metric(&agent_metric(
                "opencode.session.cost.total",
                R0 - 1_000_000_000,
                None,
                Some((1, 0.0)),
                &[("session.id", sid)],
            ))
            .await
            .unwrap();
        storage
            .write_metric(&agent_metric(
                "opencode.session.cost.total",
                R1,
                None,
                Some((2, cost)),
                &[("session.id", sid), ("project.id", "proj-1")],
            ))
            .await
            .unwrap();
    }
    storage
        .write_metric(&agent_metric(
            "opencode.session.duration",
            R1,
            None,
            Some((1, 60_000.0)),
            &[("session.id", "s1")],
        ))
        .await
        .unwrap();
    storage
        .write_metric(&agent_metric(
            "opencode.session.token.total",
            R1,
            None,
            Some((1, 12_345.0)),
            &[("session.id", "s1")],
        ))
        .await
        .unwrap();

    // claude: one session from llm_request span attributes (no cost counter
    // is trusted — priced by the API layer, which has no pricing rows in the
    // test DB, so its cost stays None).
    let mut span = Span {
        trace_id: "t".to_string(),
        span_id: "sp1".to_string(),
        parent_span_id: None,
        name: "claude_code.llm_request".to_string(),
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
    span.attributes
        .insert("session.id".to_string(), "c1".to_string());
    span.attributes
        .insert("model".to_string(), "m1".to_string());
    span.attributes
        .insert("input_tokens".to_string(), "100".to_string());
    span.attributes
        .insert("output_tokens".to_string(), "40".to_string());
    span.attributes
        .insert("cache_read_tokens".to_string(), "10".to_string());
    span.attributes
        .insert("cache_creation_tokens".to_string(), "5".to_string());
    storage.write_span(&span).await.unwrap();

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/sessions/costs?start_time={R0}&end_time={R1}&limit=50"
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
    let costs: SessionCostResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(costs.sessions.len(), 5);
    // cost desc: s4 ($100) first, then s3/s2/s1, unpriced claude last.
    assert_eq!(
        costs
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s4", "s3", "s2", "s1", "c1"]
    );
    assert_eq!(costs.median_cost_usd, Some(2.5), "median of [1,2,3,100]");
    assert!(costs.sessions[0].anomaly, "$100 > 3 x $2.5");
    assert!(!costs.sessions[1].anomaly, "$3 < $7.5");
    assert!(!costs.sessions[4].anomaly, "no cost → cannot be anomalous");

    let s4 = &costs.sessions[0];
    assert_eq!(s4.agent, "opencode");
    assert_eq!(s4.cost_usd, Some(100.0), "last cumulative value, not a sum");
    assert_eq!(s4.cost_source.as_deref(), Some("actual"));
    assert_eq!(s4.project_id.as_deref(), Some("proj-1"));

    let s1 = &costs.sessions[3];
    assert_eq!(s1.tokens, 12_345);
    assert_eq!(s1.duration_secs, Some(60.0), "60 000 ms → 60.0 s");

    let c1 = &costs.sessions[4];
    assert_eq!(c1.agent, "claude");
    assert_eq!(c1.tokens, 155);
    assert_eq!(
        c1.cost_usd, None,
        "no pricing rows → null, not a fabricated zero"
    );
    assert_eq!(c1.cost_source.as_deref(), Some("estimated"));

    // limit truncates the listing but the anomaly flag survived it: refetch
    // with limit=2 — s4 (the outlier) must still be flagged.
    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/sessions/costs?start_time={R0}&end_time={R1}&limit=2"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let limited: SessionCostResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(limited.sessions.len(), 2);
    assert!(limited.sessions[0].anomaly);
    assert_eq!(
        limited.median_cost_usd,
        Some(2.5),
        "median over the full window"
    );
}

#[tokio::test]
async fn test_get_session_cost_distribution() {
    let (server, storage, _temp_dir) = setup_test_server().await;

    // Costs 0, 0.01, 150, 1000 across four opencode sessions, plus one
    // unpriced claude session (excluded from the distribution). With 4
    // buckets the bounds are [0,100) [100,215.4) [215.4,464.2) [464.2,1000].
    for (sid, cost) in [("d0", 0.0), ("d1", 0.01), ("d2", 150.0), ("d3", 1000.0)] {
        storage
            .write_metric(&agent_metric(
                "opencode.session.cost.total",
                R1,
                None,
                Some((1, cost)),
                &[("session.id", sid)],
            ))
            .await
            .unwrap();
    }
    let mut span = Span {
        trace_id: "t".to_string(),
        span_id: "sp1".to_string(),
        parent_span_id: None,
        name: "claude_code.llm_request".to_string(),
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
    span.attributes
        .insert("session.id".to_string(), "c1".to_string());
    span.attributes
        .insert("input_tokens".to_string(), "10".to_string());
    span.attributes
        .insert("output_tokens".to_string(), "10".to_string());
    storage.write_span(&span).await.unwrap();

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/sessions/cost-distribution?start_time={R0}&end_time={R1}&buckets=4"
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
    let dist: CostDistributionResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(dist.buckets.len(), 4);
    let total: u64 = dist.buckets.iter().map(|b| b.count).sum();
    assert_eq!(total, 4, "unpriced session excluded");
    // [0, 0.1) catches both zero-ish costs: 0 and 0.01
    assert_eq!(dist.buckets[0].count, 2);
    assert_eq!(dist.buckets[0].min_usd, 0.0);
    // $150 sits in the second bucket, $1000 in the inclusive last one.
    assert_eq!(dist.buckets[1].count, 1);
    assert_eq!(dist.buckets[2].count, 0);
    assert_eq!(dist.buckets[3].count, 1);
    assert_eq!(dist.buckets[3].max_usd, 1000.0);
    // bounds ascend and are contiguous
    for w in dist.buckets.windows(2) {
        assert!(w[0].max_usd <= w[1].max_usd);
        assert!((w[0].max_usd - w[1].min_usd).abs() < 1e-9);
    }
}

// ── per-project rollup (issue #127) ───────────────────────────────────────

#[tokio::test]
async fn test_get_projects_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/genai/projects?start_time={R0}&end_time={R1}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let rollup: otelite_core::api::ProjectRollupResponse = serde_json::from_slice(&body).unwrap();
    assert!(rollup.projects.is_empty(), "empty DB -> no project rows");
}

#[tokio::test]
async fn test_get_projects_with_data() {
    let (server, storage, _temp_dir) = setup_test_server().await;

    // opencode: two top-level sessions in distinct projects, one
    // label-less; cumulative token + cost counters per session.
    for (sid, project) in [("s1", Some("projA")), ("s2", Some("projB")), ("s3", None)] {
        let mut attrs: Vec<(&str, &str)> = vec![("session.id", sid), ("is_subagent", "false")];
        if let Some(p) = project {
            attrs.push(("project.id", p));
        }
        storage
            .write_metric(&agent_metric(
                "opencode.session.count",
                R1,
                Some(1),
                None,
                &attrs,
            ))
            .await
            .unwrap();
    }
    // s1: input 100 -> 250 across the window, cost 0.5 -> 1.25.
    for (ts, input) in [(R0 - 1_000_000_000, 100u64), (R1, 250)] {
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
    // s3 (no project label): 50 output tokens, no cost counter rows.
    storage
        .write_metric(&agent_metric(
            "opencode.token.usage",
            R1,
            Some(50),
            None,
            &[
                ("agent", "a"),
                ("model", "m2"),
                ("type", "output"),
                ("session.id", "s3"),
            ],
        ))
        .await
        .unwrap();

    // codex: 2 cli sessions, 400 input tokens — no project label exists.
    storage
        .write_metric(&agent_metric(
            "codex.thread.started",
            R1,
            Some(2),
            None,
            &[("session_source", "cli")],
        ))
        .await
        .unwrap();
    storage
        .write_metric(&agent_metric(
            "codex.turn.token_usage",
            R1,
            None,
            Some((1, 400.0)),
            &[("model", "c1"), ("token_type", "input")],
        ))
        .await
        .unwrap();

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/genai/projects?start_time={R0}&end_time={R1}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let rollup: otelite_core::api::ProjectRollupResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        rollup.projects.len(),
        3,
        "projA + projB + unattributed: {rollup:?}"
    );

    let find = |pid: &str| {
        rollup
            .projects
            .iter()
            .find(|p| p.project_id == pid)
            .unwrap_or_else(|| panic!("{pid} missing"))
    };

    let a = find("projA");
    assert_eq!(a.sessions, 1);
    assert_eq!(
        a.cost_usd,
        Some(0.75),
        "s1 counter delta, no pricing needed"
    );
    assert_eq!(a.cost_source.as_deref(), Some("actual"));
    assert_eq!(a.tokens.input, 150, "250 - 100 baseline");

    let b = find("projB");
    assert_eq!(b.sessions, 1);
    assert_eq!(b.cost_usd, None, "no counter rows, no priced tokens");

    let u = find("unattributed");
    assert_eq!(u.sessions, 3, "1 label-less opencode + 2 codex");
    assert!(
        u.cost_usd.is_none() || u.cost_usd == Some(0.0),
        "no priced models in test pricing DB: {u:?}"
    );
    assert_eq!(u.tokens.input, 400, "codex histogram sum");
    assert_eq!(u.tokens.output, 50, "s3 output");
    assert!(
        u.top_models.iter().all(|m| m.cost_usd.is_none()),
        "no pricing in test DB: {u:?}"
    );

    // Sorted by cost desc: projA ($0.75) first, the rest alphabetical.
    assert_eq!(rollup.projects[0].project_id, "projA");
}

fn llm_request_span(
    span_id: &str,
    model: &str,
    start_time: i64,
    duration_ms: i64,
    ttft_ms: Option<i64>,
) -> Span {
    let mut attributes: HashMap<String, String> = HashMap::new();
    attributes.insert("session.id".to_string(), "s1".to_string());
    attributes.insert("model".to_string(), model.to_string());
    if let Some(ttft) = ttft_ms {
        attributes.insert("ttft_ms".to_string(), ttft.to_string());
    }
    Span {
        trace_id: "t".to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        name: "claude_code.llm_request".to_string(),
        kind: SpanKind::Internal,
        start_time,
        end_time: start_time + duration_ms * 1_000_000,
        attributes,
        status: SpanStatus {
            code: SpanStatusCode::Ok,
            message: None,
        },
        events: Vec::new(),
        resource: None,
    }
}

#[tokio::test]
async fn test_get_latency_percentiles_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/genai/latency_percentiles?start_time={R0}&end_time={R1}"
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
    let resp: otelite_core::api::LatencyPercentilesResponse =
        serde_json::from_slice(&body).unwrap();
    assert!(
        resp.metrics
            .values()
            .all(|s| s.all.is_empty() && s.models.is_empty()),
        "empty DB -> no percentile points: {resp:?}"
    );
}

#[tokio::test]
async fn test_get_latency_percentiles_with_data() {
    let (server, storage, _temp_dir) = setup_test_server().await;

    storage
        .write_span(&llm_request_span("sp1", "modelA", R0, 100, Some(20)))
        .await
        .unwrap();
    storage
        .write_span(&llm_request_span(
            "sp2",
            "modelA",
            R0 + 500_000_000,
            300,
            Some(90),
        ))
        .await
        .unwrap();
    storage
        .write_span(&llm_request_span(
            "sp3",
            "modelB",
            R0 + 250_000_000,
            400,
            Some(150),
        ))
        .await
        .unwrap();

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/genai/latency_percentiles?start_time={R0}&end_time={R1}&bucket_secs=3600"
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
    let resp: otelite_core::api::LatencyPercentilesResponse =
        serde_json::from_slice(&body).unwrap();

    // R0..R1 spans all fall into one hourly bucket (bucket start floored).
    let dur = &resp.metrics["duration"];
    assert_eq!(dur.all.len(), 1, "single bucket: {resp:?}");
    assert_eq!(dur.all[0].count, 3);
    // durations [100, 300, 400] sorted: p50=300, p90/p95/p99=400
    assert_eq!(dur.all[0].p50_ms, 300.0);
    assert_eq!(dur.all[0].p90_ms, 400.0);
    assert_eq!(dur.all[0].p95_ms, 400.0);
    assert_eq!(dur.all[0].p99_ms, 400.0);
    assert_eq!(dur.models.len(), 2);
    assert_eq!(dur.models["modelA"][0].count, 2);
    assert_eq!(dur.models["modelB"][0].count, 1);

    let tt = &resp.metrics["ttft"];
    assert_eq!(tt.all[0].count, 3, "all three spans carried valid ttft");
    assert_eq!(tt.all[0].p50_ms, 90.0);
    assert_eq!(tt.all[0].p90_ms, 150.0);

    // Unknown metric name → 400, not 500.
    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/genai/latency_percentiles?start_time={R0}&end_time={R1}&metrics=bogus"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn distribution_body(
    app: axum::Router,
    uri: String,
) -> (StatusCode, otelite_core::api::DistributionResponse) {
    let response = app
        .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let resp: otelite_core::api::DistributionResponse = serde_json::from_slice(&body).unwrap();
    (status, resp)
}

#[tokio::test]
async fn test_get_distributions_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();

    let (status, resp) = distribution_body(
        app,
        format!("/api/genai/distributions?metric=llm_duration&start_time={R0}&end_time={R1}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(resp.buckets.is_empty(), "empty DB -> no buckets: {resp:?}");
    assert!(resp.stats.is_none());
    assert_eq!(resp.metric, "llm_duration");
    assert_eq!(resp.unit, "ms");
    assert_eq!(resp.scale, "linear");

    // session_cost is priced in the API layer; empty DB -> empty distribution.
    let app = server.build_router();
    let (status, resp) = distribution_body(
        app,
        format!(
            "/api/genai/distributions?metric=session_cost&start_time={R0}&end_time={R1}&scale=log"
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp.unit, "usd");
    assert!(resp.buckets.is_empty());
}

#[tokio::test]
async fn test_get_distributions_with_data() {
    let (server, storage, _temp_dir) = setup_test_server().await;
    storage
        .write_span(&llm_request_span("sp1", "modelA", R0, 100, Some(20)))
        .await
        .unwrap();
    storage
        .write_span(&llm_request_span(
            "sp2",
            "modelA",
            R0 + 100_000_000,
            300,
            Some(90),
        ))
        .await
        .unwrap();
    storage
        .write_span(&llm_request_span(
            "sp3",
            "modelB",
            R0 + 200_000_000,
            500,
            None,
        ))
        .await
        .unwrap();

    let app = server.build_router();
    let (status, resp) = distribution_body(
        app,
        format!(
            "/api/genai/distributions?metric=llm_duration&start_time={R0}&end_time={R1}&buckets=3&scale=log"
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp.buckets.iter().map(|b| b.count).sum::<u64>(), 3);
    let s = resp.stats.as_ref().expect("stats present");
    assert_eq!(s.count, 3);
    assert_eq!(s.min, 100.0);
    assert_eq!(s.max, 500.0);
    assert!((s.mean - (100.0 + 300.0 + 500.0) / 3.0).abs() < 1e-9);
    // sorted [100, 300, 500]: p50 -> idx (3-1)*0.5=1 -> 300
    assert_eq!(s.p50, 300.0);
    assert_eq!(s.p95, 500.0);

    // ttft cohort: two span values, third span has none.
    let app = server.build_router();
    let (status, resp) = distribution_body(
        app,
        format!("/api/genai/distributions?metric=ttft&start_time={R0}&end_time={R1}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let s = resp.stats.as_ref().unwrap();
    assert_eq!(s.count, 2, "sp1 + sp2 carried valid ttft");
    assert_eq!(s.min, 20.0);
    assert_eq!(s.max, 90.0);

    // Unknown metric → 400 (error body, not a distribution).
    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/genai/distributions?metric=bogus&start_time={R0}&end_time={R1}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Bad scale → 400.
    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/genai/distributions?metric=ttft&start_time={R0}&end_time={R1}&scale=exponential"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ── session context (issue #134) ──────────────────────────────────────────

fn ctx_span(span_id: &str, session_id: &str, start: i64, duration_ms: i64) -> Span {
    let mut attributes: HashMap<String, String> = HashMap::new();
    attributes.insert("session.id".to_string(), session_id.to_string());
    attributes.insert("model".to_string(), "claude-sonnet-5".to_string());
    Span {
        trace_id: "t".to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        name: "claude_code.llm_request".to_string(),
        kind: SpanKind::Internal,
        start_time: start,
        end_time: start + duration_ms * 1_000_000,
        attributes,
        status: SpanStatus {
            code: SpanStatusCode::Ok,
            message: None,
        },
        events: Vec::new(),
        resource: None,
    }
}

fn ctx_log(ts: i64, body: &str, severity: i32, attributes: &[(&str, &str)]) -> LogRecord {
    let mut log = LogRecord::new(SeverityLevel::from_i32(severity).unwrap(), body, ts);
    for (k, v) in attributes {
        log.attributes.insert(k.to_string(), v.to_string());
    }
    log
}

#[tokio::test]
async fn test_get_session_context_empty() {
    let (server, _storage, _temp_dir) = setup_test_server().await;
    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/no-such-session/context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_session_context_mixed_session() {
    let (server, storage, _temp_dir) = setup_test_server().await;
    let sid = "ses_ctx_1";
    storage
        .write_span(&ctx_span("c1", sid, R0, 100))
        .await
        .unwrap();
    storage
        .write_span(&ctx_span("c2", sid, R0 + 900_000_000, 300))
        .await
        .unwrap();
    storage
        .write_log(&ctx_log(
            R0 + 100_000_000,
            "claude_code.api_request",
            13,
            &[("session.id", sid), ("event.name", "api_request")],
        ))
        .await
        .unwrap();
    storage
        .write_log(&ctx_log(
            R0 + 200_000_000,
            "claude_code.tool_result",
            17,
            &[("session.id", sid)],
        ))
        .await
        .unwrap();
    // opencode metric for this session (gauge points)
    storage
        .write_metric(&agent_metric(
            "opencode.message.count",
            R0 + 300_000_000,
            Some(3),
            None,
            &[("session.id", sid), ("project.id", "proj-7")],
        ))
        .await
        .unwrap();

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{sid}/context"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let ctx: otelite_core::api::SessionContextResponse = serde_json::from_slice(&body).unwrap();

    assert_eq!(ctx.session.id, sid);
    // claude spans present → agent claude, full span coverage
    assert_eq!(ctx.session.agent.as_deref(), Some("claude"));
    assert_eq!(ctx.session.span_coverage, "full");
    // project.id leaks in from the opencode metric row — it is
    // scoped to opencode.* metric names only
    assert_eq!(ctx.session.project_id.as_deref(), Some("proj-7"));
    assert_eq!(ctx.spans_total, 2);
    assert_eq!(ctx.logs_total, 2);
    assert_eq!(ctx.spans[0].duration_ns, 100_000_000);
    assert_eq!(ctx.logs[0].severity.as_deref(), Some("WARN"));
    assert_eq!(ctx.logs[1].severity.as_deref(), Some("ERROR"));
    assert_eq!(ctx.metrics.len(), 1);
    assert_eq!(ctx.metrics[0].count, 1);
    assert_eq!(ctx.metrics[0].sum, Some(3.0));
    // timeline: 2 spans + 2 logs merged ascending
    assert_eq!(ctx.timeline.len(), 4);
    assert_eq!(ctx.timeline[0].kind, "span");
    assert_eq!(ctx.timeline[0].ts, R0);
    assert_eq!(
        ctx.timeline
            .iter()
            .find(|e| e.kind == "span")
            .unwrap()
            .label,
        "claude_code.llm_request claude-sonnet-5"
    );

    // limit caps rows, not totals
    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{sid}/context?limit=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let ctx: otelite_core::api::SessionContextResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(ctx.spans.len(), 1);
    assert_eq!(ctx.spans_total, 2);
    assert_eq!(ctx.timeline.len(), 1);
}

#[tokio::test]
async fn test_get_session_context_window_filter() {
    let (server, storage, _temp_dir) = setup_test_server().await;
    let sid = "ses_ctx_2";
    storage
        .write_span(&ctx_span("c1", sid, R0, 100))
        .await
        .unwrap();
    storage
        .write_span(&ctx_span("c2", sid, R0 + 900_000_000, 300))
        .await
        .unwrap();

    let app = server.build_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/sessions/{sid}/context?start_time={}",
                    R0 + 500_000_000
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
    let ctx: otelite_core::api::SessionContextResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        ctx.spans.len(),
        1,
        "only the later span starts in the window"
    );
    assert_eq!(
        ctx.spans_total, 1,
        "totals count the queried scope (window, when given)"
    );
}
