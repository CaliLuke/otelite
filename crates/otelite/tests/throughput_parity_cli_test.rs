//! CLI side of the versioned parity fixture (issue #119 slice #144).
//!
//! Loads the same fixture as `crates/otelite-api/tests/throughput_parity_test.rs`,
//! builds a temp database from its spans, runs the compiled `otelite usage`
//! binary against it (`OTELITE_DATA_DIR` override), and deep-compares the
//! emitted JSON with the frozen CLI expected values. The network-dependent
//! pricing fields listed in the fixture's `normalization` paths are stripped
//! from both sides before comparing.

use assert_cmd::Command;
use otelite_core::telemetry::trace::{Span, SpanKind, SpanStatus, StatusCode as SpanStatusCode};
use otelite_storage::sqlite::SqliteBackend;
use otelite_storage::{StorageBackend, StorageConfig};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Fixture {
    version: u32,
    window: Window,
    spans: Vec<FixtureSpan>,
    cli: serde_json::Value,
    cli_empty: serde_json::Value,
    cli_args: Vec<String>,
    cli_empty_args: Vec<String>,
    normalization: Vec<String>,
}

#[derive(Deserialize)]
struct Window {
    start_ns: i64,
    // `end_ns` is part of the fixture contract; the CLI test derives its
    // windows from the args, so only `start_ns` is consumed here.
    #[allow(dead_code)]
    end_ns: i64,
}

#[derive(Deserialize)]
struct FixtureSpan {
    span_id: String,
    model: String,
    system: String,
    hour: i64,
    #[serde(default)]
    offset_s: i64,
    duration_ms: i64,
    input: i64,
    #[serde(default)]
    output: Option<i64>,
    #[serde(default)]
    ttft_ms: Option<i64>,
    #[serde(default)]
    cache_read: Option<i64>,
    #[serde(default)]
    cache_creation: Option<i64>,
    #[serde(default)]
    response_model: Option<String>,
}

fn span_from_fixture(s: &FixtureSpan, window: &Window) -> Span {
    let mut attributes: HashMap<String, String> = HashMap::new();
    attributes.insert("model".into(), s.model.clone());
    attributes.insert("gen_ai.request.model".into(), s.model.clone());
    attributes.insert("gen_ai.system".into(), s.system.clone());
    attributes.insert("gen_ai.usage.input_tokens".into(), s.input.to_string());
    if let Some(o) = s.output {
        attributes.insert("gen_ai.usage.output_tokens".into(), o.to_string());
    }
    if let Some(t) = s.ttft_ms {
        attributes.insert("ttft_ms".into(), t.to_string());
    }
    if let Some(c) = s.cache_read {
        attributes.insert("gen_ai.usage.cache_read.input_tokens".into(), c.to_string());
    }
    if let Some(c) = s.cache_creation {
        attributes.insert(
            "gen_ai.usage.cache_creation.input_tokens".into(),
            c.to_string(),
        );
    }
    if let Some(rm) = &s.response_model {
        attributes.insert("gen_ai.response.model".into(), rm.clone());
    }
    let start = window.start_ns + (s.hour * 3600 + s.offset_s) * 1_000_000_000;
    Span {
        resource: None,
        trace_id: "t".into(),
        span_id: s.span_id.clone(),
        parent_span_id: None,
        name: "claude_code.llm_request".into(),
        kind: SpanKind::Internal,
        start_time: start,
        end_time: start + s.duration_ms * 1_000_000,
        attributes,
        events: vec![],
        status: SpanStatus {
            code: SpanStatusCode::Ok,
            message: None,
        },
    }
}

/// Strip the normalization paths ("$.a", "$.a[*].b") from a JSON value.
fn remove_path(v: &mut serde_json::Value, segs: &[&str]) {
    if segs.is_empty() {
        return;
    }
    match (v, segs.len() == 1) {
        (serde_json::Value::Object(map), true) => {
            map.remove(segs[0]);
        },
        (serde_json::Value::Array(items), true) => {
            for item in items.iter_mut() {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove(segs[0]);
                }
            }
        },
        (arr, false) if segs[0] == "*" => {
            let key = segs[1];
            if let serde_json::Value::Array(items) = arr {
                for item in items.iter_mut() {
                    if let Some(obj) = item.as_object_mut() {
                        obj.remove(key);
                    }
                }
            }
        },
        (serde_json::Value::Object(map), false) => {
            if let Some(child) = map.get_mut(segs[0]) {
                remove_path(child, &segs[1..]);
            }
        },
        _ => {},
    }
}

/// Apply the fixture's normalization paths to a JSON value in place.
fn normalize(v: &mut serde_json::Value, paths: &[String]) {
    for path in paths {
        let segs: Vec<&str> = path.trim_start_matches("$.").split('.').collect();
        remove_path(v, &segs);
    }
}

async fn build_db(data_dir: &std::path::Path, fixture: &Fixture) {
    let storage_config = StorageConfig::default().with_data_dir(data_dir.to_path_buf());
    let mut storage = SqliteBackend::new(storage_config);
    storage.initialize().await.unwrap();
    for s in &fixture.spans {
        storage
            .write_span(&span_from_fixture(s, &fixture.window))
            .await
            .unwrap();
    }
}

fn load_fixture() -> Fixture {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../otelite-api/tests/fixtures/throughput_parity_v1.json"
    ))
    .unwrap();
    let fixture: Fixture = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        fixture.version, 1,
        "bump the fixture version and re-freeze expected JSON"
    );
    fixture
}

#[tokio::test]
async fn cli_matches_parity_fixture() {
    let fixture = load_fixture();
    let temp = tempfile::TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    build_db(&data_dir, &fixture).await;

    // Isolate the binary: temp data dir and temp HOME (no real config).
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let output = Command::cargo_bin("otelite")
        .expect("otelite binary should build")
        .args(&fixture.cli_args)
        .env("OTELITE_DATA_DIR", &data_dir)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("CLI stdout must be JSON");

    let mut expected = fixture.cli.clone();
    let mut actual_cmp = actual;
    normalize(&mut expected, &fixture.normalization);
    normalize(&mut actual_cmp, &fixture.normalization);
    assert_eq!(
        actual_cmp, expected,
        "CLI JSON drifted from the v1 parity fixture — regenerate it only after a deliberate change"
    );
}

#[tokio::test]
async fn cli_empty_window_matches_parity_fixture() {
    let fixture = load_fixture();
    let temp = tempfile::TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    build_db(&data_dir, &fixture).await;

    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let output = Command::cargo_bin("otelite")
        .expect("otelite binary should build")
        .args(&fixture.cli_empty_args)
        .env("OTELITE_DATA_DIR", &data_dir)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("CLI stdout must be JSON");

    let mut expected = fixture.cli_empty.clone();
    let mut actual_cmp = actual;
    normalize(&mut expected, &fixture.normalization);
    normalize(&mut actual_cmp, &fixture.normalization);
    assert_eq!(
        actual_cmp, expected,
        "CLI empty-window JSON drifted from the v1 parity fixture"
    );
}
