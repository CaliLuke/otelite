// Integration tests for HTTP/JSON OTLP endpoints

mod http_test_utils;

use http_test_utils::{
    create_invalid_json, create_logs_json, create_malformed_json, create_metrics_json,
    create_traces_json,
};
use otelite_receiver::config::ReceiverConfig;
use otelite_receiver::http::HttpServer;
use otelite_storage::{sqlite::SqliteBackend, StorageBackend, StorageConfig};
use reqwest::{Method, StatusCode};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;

/// Starts an HTTP server with isolated file-backed SQLite storage.
///
/// The returned directory must remain alive until the server has processed all
/// requests because SQLite opens the database lazily.
async fn start_test_server() -> (String, HttpServer, TempDir) {
    start_test_server_with_config(ReceiverConfig::new()).await
}

async fn start_test_server_with_config(
    mut config: ReceiverConfig,
) -> (String, HttpServer, TempDir) {
    config.http_addr = "127.0.0.1:0".parse().expect("Failed to parse address");
    let server = HttpServer::new(config);

    // Create storage backend
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let storage_config = StorageConfig::default().with_data_dir(temp_dir.path().to_path_buf());
    let mut storage = SqliteBackend::new(storage_config);
    storage
        .initialize()
        .await
        .expect("Failed to initialize storage");
    let storage: Arc<dyn StorageBackend> = Arc::new(storage);

    server.start(storage).await.expect("Failed to start server");

    // Wait for server to be ready and get actual bound address
    sleep(Duration::from_millis(100)).await;
    let addr = server
        .local_addr()
        .await
        .expect("Failed to get local address");

    (format!("http://{}", addr), server, temp_dir)
}

fn create_log_json(body: &str) -> String {
    let mut request: serde_json::Value =
        serde_json::from_str(&create_logs_json()).expect("valid log fixture");
    request["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"] =
        serde_json::Value::String(body.to_string());
    request.to_string()
}

async fn ingest_log(client: &reqwest::Client, base_url: &str, body: &str) {
    let response = client
        .post(format!("{base_url}/v1/logs"))
        .header("Content-Type", "application/json")
        .body(create_log_json(body))
        .send()
        .await
        .expect("OTLP log request must succeed");
    assert_eq!(response.status(), StatusCode::OK);
}

fn open_read_only(database_path: &Path) -> Connection {
    Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("live database must accept an independent read-only connection")
}

#[tokio::test]
async fn test_http_json_metrics_success() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let json_data = create_metrics_json();

    let response = client
        .post(format!("{}/v1/metrics", base_url))
        .header("Content-Type", "application/json")
        .body(json_data)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("Failed to parse response");
    assert_eq!(body["status"], "success");
}

#[tokio::test]
async fn test_http_json_logs_success() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let json_data = create_logs_json();

    let response = client
        .post(format!("{}/v1/logs", base_url))
        .header("Content-Type", "application/json")
        .body(json_data)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("Failed to parse response");
    assert_eq!(body["status"], "success");
}

#[tokio::test]
async fn test_http_json_traces_success() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let json_data = create_traces_json();

    let response = client
        .post(format!("{}/v1/traces", base_url))
        .header("Content-Type", "application/json")
        .body(json_data)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("Failed to parse response");
    assert_eq!(body["status"], "success");
}

#[tokio::test]
async fn test_http_json_invalid_json() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let invalid_json = create_invalid_json();

    let response = client
        .post(format!("{}/v1/metrics", base_url))
        .header("Content-Type", "application/json")
        .body(invalid_json)
        .send()
        .await
        .expect("Failed to send request");

    // Should return 400 Bad Request for invalid JSON
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_http_json_malformed_structure() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let malformed_json = create_malformed_json();

    let response = client
        .post(format!("{}/v1/metrics", base_url))
        .header("Content-Type", "application/json")
        .body(malformed_json)
        .send()
        .await
        .expect("Failed to send request");

    // Should reject malformed JSON structure with 400 Bad Request
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_http_json_empty_body() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/v1/metrics", base_url))
        .header("Content-Type", "application/json")
        .body("")
        .send()
        .await
        .expect("Failed to send request");

    // Empty body is treated as valid JSON (empty object) by serde_json
    // Our current implementation accepts it and returns OK with empty protobuf structures
    // This is acceptable behavior - empty telemetry data is valid
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_http_json_charset_handling() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let json_data = create_metrics_json();

    let response = client
        .post(format!("{}/v1/metrics", base_url))
        .header("Content-Type", "application/json; charset=utf-8")
        .body(json_data)
        .send()
        .await
        .expect("Failed to send request");

    // Should handle charset parameter in Content-Type
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_http_json_all_signals() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();

    // Test metrics
    let metrics_response = client
        .post(format!("{}/v1/metrics", base_url))
        .header("Content-Type", "application/json")
        .body(create_metrics_json())
        .send()
        .await
        .expect("Failed to send metrics");
    assert_eq!(metrics_response.status(), StatusCode::OK);

    // Test logs
    let logs_response = client
        .post(format!("{}/v1/logs", base_url))
        .header("Content-Type", "application/json")
        .body(create_logs_json())
        .send()
        .await
        .expect("Failed to send logs");
    assert_eq!(logs_response.status(), StatusCode::OK);

    // Test traces
    let traces_response = client
        .post(format!("{}/v1/traces", base_url))
        .header("Content-Type", "application/json")
        .body(create_traces_json())
        .send()
        .await
        .expect("Failed to send traces");
    assert_eq!(traces_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_http_json_concurrent_requests() {
    let (base_url, _server, _temp_dir) = start_test_server().await;

    let client = reqwest::Client::new();
    let mut handles = vec![];

    // Send 10 concurrent JSON requests
    for _ in 0..10 {
        let client = client.clone();
        let url = format!("{}/v1/metrics", base_url);
        let json_data = create_metrics_json();

        let handle = tokio::spawn(async move {
            client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(json_data)
                .send()
                .await
                .expect("Failed to send request")
        });

        handles.push(handle);
    }

    // Wait for all requests to complete
    for handle in handles {
        let response = handle.await.expect("Task panicked");
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn independent_read_only_connection_sees_live_ingested_log() {
    let (base_url, server, temp_dir) = start_test_server().await;
    let client = reqwest::Client::new();
    ingest_log(&client, &base_url, "visible during live ingestion").await;

    let connection = open_read_only(&temp_dir.path().join("otelite.db"));
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("external read must report the active journal mode");
    let locking_mode: String = connection
        .query_row("PRAGMA locking_mode", [], |row| row.get(0))
        .expect("external read must report a non-exclusive locking mode");
    let body: String = connection
        .query_row(
            "SELECT body FROM logs ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("external read must see the newly ingested log before shutdown");

    assert_eq!(journal_mode, "wal");
    assert_eq!(locking_mode, "normal");
    assert_eq!(body, "visible during live ingestion");
    server.shutdown();
}

#[tokio::test]
async fn repeated_read_only_queries_see_continued_ingestion() {
    let (base_url, server, temp_dir) = start_test_server().await;
    let client = reqwest::Client::new();
    let connection = open_read_only(&temp_dir.path().join("otelite.db"));

    for (expected_count, body) in [
        (1, "live read 1"),
        (2, "live read 2"),
        (3, "live read 3"),
        (4, "live read 4"),
        (5, "live read 5"),
    ] {
        ingest_log(&client, &base_url, body).await;
        let (count, latest): (i64, String) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM logs), body FROM logs ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("every external read must succeed while ingestion continues");
        assert_eq!(count, expected_count);
        assert_eq!(latest, body);
    }

    server.shutdown();
}

#[tokio::test]
async fn loopback_origins_can_preflight_every_otlp_signal_route() {
    let (base_url, server, _temp_dir) = start_test_server().await;
    let client = reqwest::Client::new();

    for path in ["/v1/logs", "/v1/traces", "/v1/metrics"] {
        let response = client
            .request(Method::OPTIONS, format!("{base_url}{path}"))
            .header("Origin", "http://localhost:5173")
            .header("Access-Control-Request-Method", "POST")
            .header("Access-Control-Request-Headers", "content-type")
            .send()
            .await
            .expect("browser preflight must receive a response");
        assert!(response.status().is_success(), "path: {path}");
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:5173"),
            "path: {path}"
        );
        assert!(
            response
                .headers()
                .get("Access-Control-Allow-Methods")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("POST")),
            "path: {path}"
        );
    }

    server.shutdown();
}

#[tokio::test]
async fn configured_origin_can_post_otlp_json() {
    let config = ReceiverConfig::new()
        .with_cors_allowed_origins(vec!["https://telemetry.example.com".to_string()]);
    let (base_url, server, _temp_dir) = start_test_server_with_config(config).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/logs"))
        .header("Origin", "https://telemetry.example.com")
        .header("Content-Type", "application/json")
        .body(create_logs_json())
        .send()
        .await
        .expect("allowed browser POST must receive a response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .and_then(|value| value.to_str().ok()),
        Some("https://telemetry.example.com")
    );
    server.shutdown();
}

#[tokio::test]
async fn remote_origin_is_not_allowed_by_default() {
    let (base_url, server, _temp_dir) = start_test_server().await;

    let response = reqwest::Client::new()
        .request(Method::OPTIONS, format!("{base_url}/v1/logs"))
        .header("Origin", "https://remote.example.com")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .await
        .expect("disallowed browser preflight must receive a response");

    assert!(response
        .headers()
        .get("Access-Control-Allow-Origin")
        .is_none());
    server.shutdown();
}
