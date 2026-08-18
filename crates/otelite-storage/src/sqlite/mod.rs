//! SQLite backend implementation

use crate::StorageConfig;
use async_trait::async_trait;
use chrono::Timelike;
use otelite_core::storage::{
    PurgeAllStats, PurgeOptions, QueryParams, Result, StorageBackend, StorageError, StorageStats,
};
use otelite_core::telemetry::{LogRecord, Metric, Span};
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Arc;

pub mod purge;
pub mod reader;
pub mod schema;
pub mod writer;

/// SQLite storage backend
pub struct SqliteBackend {
    config: StorageConfig,
    conn: Arc<Mutex<Option<Connection>>>,
    purge_lock: Arc<purge::PurgeLock>,
    purge_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl SqliteBackend {
    /// Create a new SQLite backend with the given configuration
    pub fn new(config: StorageConfig) -> Self {
        Self {
            config,
            conn: Arc::new(Mutex::new(None)),
            purge_lock: Arc::new(purge::PurgeLock::new()),
            purge_handle: Arc::new(Mutex::new(None)),
        }
    }

    fn db_path(&self) -> PathBuf {
        if self
            .config
            .data_dir
            .to_string_lossy()
            .starts_with(":memory:")
        {
            self.config.data_dir.clone()
        } else {
            self.config.data_dir.join("otelite.db")
        }
    }
}

#[async_trait]
impl StorageBackend for SqliteBackend {
    async fn initialize(&mut self) -> Result<()> {
        let db_path = self.db_path();

        if !db_path.to_string_lossy().starts_with(":memory:") {
            std::fs::create_dir_all(&self.config.data_dir).map_err(|e| {
                StorageError::InitializationError(format!("Failed to create data directory: {}", e))
            })?;
        }

        let conn = Connection::open(&db_path).map_err(|e| {
            StorageError::InitializationError(format!("Failed to open database: {}", e))
        })?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(|e| {
                StorageError::InitializationError(format!("Failed to configure SQLite: {}", e))
            })?;

        schema::initialize_schema(&conn).map_err(StorageError::from)?;

        *self.conn.lock() = Some(conn);

        if self.config.retention_days > 0 {
            self.start_purge_scheduler(db_path);
        }

        Ok(())
    }

    async fn write_log(&self, log: &LogRecord) -> Result<()> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::WriteError("Database not initialized".to_string()))?;

        writer::write_log(conn, log).map_err(StorageError::from)
    }

    async fn write_span(&self, span: &Span) -> Result<()> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::WriteError("Database not initialized".to_string()))?;

        writer::write_span(conn, span).map_err(StorageError::from)
    }

    async fn write_metric(&self, metric: &Metric) -> Result<()> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::WriteError("Database not initialized".to_string()))?;

        writer::write_metric(conn, metric).map_err(StorageError::from)
    }

    async fn query_logs(&self, params: &QueryParams) -> Result<Vec<LogRecord>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_logs(conn, params).map_err(StorageError::from)
    }

    async fn query_spans(&self, params: &QueryParams) -> Result<Vec<Span>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_spans(conn, params).map_err(StorageError::from)
    }

    async fn query_spans_for_trace_list(
        &self,
        params: &QueryParams,
        trace_limit: usize,
    ) -> Result<Vec<Span>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_spans_for_trace_list(conn, params, trace_limit).map_err(StorageError::from)
    }

    async fn query_metrics(&self, params: &QueryParams) -> Result<Vec<Metric>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_metrics(conn, params).map_err(StorageError::from)
    }

    async fn query_latest_metrics(&self, params: &QueryParams) -> Result<Vec<Metric>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_latest_metrics(conn, params).map_err(StorageError::from)
    }

    async fn stats(&self) -> Result<StorageStats> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::get_stats(conn).map_err(StorageError::from)
    }

    async fn purge(&self, options: &PurgeOptions) -> Result<u64> {
        let _guard = self
            .purge_lock
            .try_lock()
            .await
            .map_err(StorageError::from)?;

        let mut conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| StorageError::WriteError("Database not initialized".to_string()))?;

        let cutoff_timestamp = if let Some(older_than) = options.older_than {
            older_than
        } else {
            let cutoff =
                chrono::Utc::now() - chrono::Duration::days(self.config.retention_days as i64);
            cutoff.timestamp_nanos_opt().unwrap_or(0)
        };

        let record = purge::purge_old_data(
            conn,
            cutoff_timestamp,
            10000,
            &options.signal_types,
            options.dry_run,
        )
        .map_err(StorageError::from)?;

        if !options.dry_run {
            purge::vacuum(conn).map_err(StorageError::from)?;
        }

        let total_deleted = record.logs_deleted + record.spans_deleted + record.metrics_deleted;
        Ok(total_deleted as u64)
    }

    async fn purge_all(&self) -> Result<PurgeAllStats> {
        let _guard = self
            .purge_lock
            .try_lock()
            .await
            .map_err(StorageError::from)?;

        let mut conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| StorageError::WriteError("Database not initialized".to_string()))?;

        let tx = conn
            .transaction()
            .map_err(|e| StorageError::WriteError(format!("Failed to start transaction: {}", e)))?;

        let logs_deleted = tx
            .execute("DELETE FROM logs", [])
            .map_err(|e| StorageError::WriteError(format!("Failed to delete logs: {}", e)))?
            as u64;
        let spans_deleted = tx
            .execute("DELETE FROM spans", [])
            .map_err(|e| StorageError::WriteError(format!("Failed to delete spans: {}", e)))?
            as u64;
        let metrics_deleted = tx
            .execute("DELETE FROM metrics", [])
            .map_err(|e| StorageError::WriteError(format!("Failed to delete metrics: {}", e)))?
            as u64;

        tx.commit().map_err(|e| {
            StorageError::WriteError(format!("Failed to commit transaction: {}", e))
        })?;

        purge::vacuum(conn).map_err(StorageError::from)?;

        Ok(PurgeAllStats {
            logs_deleted,
            spans_deleted,
            metrics_deleted,
        })
    }

    async fn distinct_resource_keys(&self, signal: &str) -> Result<Vec<String>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::distinct_resource_keys(conn, signal).map_err(StorageError::from)
    }

    async fn query_token_usage(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<(
        otelite_core::api::TokenUsageSummary,
        Vec<otelite_core::api::ModelUsage>,
        Vec<otelite_core::api::SystemUsage>,
    )> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_token_usage(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_cost_series(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        bucket_ns: i64,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::CostSeriesPoint>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_cost_series(conn, start_time, end_time, bucket_ns, model)
            .map_err(StorageError::from)
    }

    async fn query_top_spans(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: usize,
        sort_by: otelite_core::api::TopSpanSort,
        truncated_only: bool,
    ) -> Result<Vec<otelite_core::api::TopSpan>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_top_spans(conn, start_time, end_time, limit, sort_by, truncated_only)
            .map_err(StorageError::from)
    }

    async fn query_top_sessions(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: usize,
    ) -> Result<Vec<otelite_core::api::SessionCostRow>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_top_sessions(conn, start_time, end_time, limit).map_err(StorageError::from)
    }

    async fn query_top_conversations(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: usize,
    ) -> Result<Vec<otelite_core::api::ConversationCostRow>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_top_conversations(conn, start_time, end_time, limit)
            .map_err(StorageError::from)
    }

    async fn query_finish_reasons(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::FinishReasonCount>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_finish_reasons(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_latency_stats(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::LatencyStats>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_latency_stats(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_genai_capabilities(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<otelite_core::api::GenAiCapabilityResponse> {
        let db_path = self.db_path();
        let model = model.map(str::to_string);
        if db_path.to_string_lossy().starts_with(":memory:") {
            let conn_guard = self.conn.lock();
            let conn = conn_guard
                .as_ref()
                .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
            return reader::query_genai_capabilities(conn, start_time, end_time, model.as_deref())
                .map_err(StorageError::from);
        }

        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).map_err(|error| {
                StorageError::QueryError(format!(
                    "Failed to open read connection for GenAI capability query: {error}"
                ))
            })?;
            reader::query_genai_capabilities(&conn, start_time, end_time, model.as_deref())
                .map_err(StorageError::from)
        })
        .await
        .map_err(|error| {
            StorageError::QueryError(format!("GenAI capability query worker failed: {error}"))
        })?
    }

    async fn query_error_rate(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::ErrorRateByModel>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_error_rate(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_tool_usage(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: usize,
    ) -> Result<Vec<otelite_core::api::ToolUsage>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_tool_usage(conn, start_time, end_time, limit).map_err(StorageError::from)
    }

    async fn query_retry_stats(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<otelite_core::api::RetryStats> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_retry_stats(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_retrieval_stats(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        top_queries_limit: usize,
    ) -> Result<otelite_core::api::RetrievalStats> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;

        reader::query_retrieval_stats(conn, start_time, end_time, top_queries_limit)
            .map_err(StorageError::from)
    }

    async fn query_truncation_rate(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::TruncationRateByModel>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_truncation_rate(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_cache_hit_rate(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::CacheHitRateByModel>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_cache_hit_rate(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_request_param_profile(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<otelite_core::api::RequestParamProfile> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_request_param_profile(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_conversation_depth(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<otelite_core::api::ConversationDepthStats> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_conversation_depth(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_latency_series(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        bucket_secs: u64,
        model: Option<&str>,
        all_spans: bool,
    ) -> Result<Vec<otelite_core::api::LatencySeriesPoint>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_latency_series(conn, start_time, end_time, bucket_secs, model, all_spans)
            .map_err(StorageError::from)
    }

    async fn query_calls_series(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        bucket_secs: u64,
        all_spans: bool,
    ) -> Result<Vec<otelite_core::api::CallsSeriesPoint>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_calls_series(conn, start_time, end_time, bucket_secs, all_spans)
            .map_err(StorageError::from)
    }

    async fn query_latency_by_context(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::LatencyByContextBin>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_latency_by_context(conn, start_time, end_time, model)
            .map_err(StorageError::from)
    }

    async fn query_error_types(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        model: Option<&str>,
    ) -> Result<Vec<otelite_core::api::ErrorTypeBreakdown>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_error_types(conn, start_time, end_time, model).map_err(StorageError::from)
    }

    async fn query_model_drift(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<otelite_core::api::ModelDriftPair>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_model_drift(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_tool_approvals(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<otelite_core::api::ToolApprovalStats> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_tool_approvals(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_stop_reasons(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<otelite_core::api::StopReasonCount>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_stop_reasons(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_context_type_split(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<otelite_core::api::ContextTypeSplit>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_context_type_split(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn query_tool_errors(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        limit: usize,
    ) -> Result<Vec<otelite_core::api::ToolErrorEntry>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_tool_errors(conn, start_time, end_time, limit).map_err(StorageError::from)
    }

    async fn query_hour_of_day(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<otelite_core::api::HourOfDayBucket>> {
        let conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_ref()
            .ok_or_else(|| StorageError::QueryError("Database not initialized".to_string()))?;
        reader::query_hour_of_day(conn, start_time, end_time).map_err(StorageError::from)
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(handle) = self.purge_handle.lock().take() {
            handle.abort();
        }

        let mut conn_guard = self.conn.lock();
        if let Some(conn) = conn_guard.take() {
            conn.close()
                .map_err(|(_, e)| StorageError::DatabaseError(e.to_string()))?;
        }
        Ok(())
    }
}

impl SqliteBackend {
    fn start_purge_scheduler(&self, db_path: PathBuf) {
        let config = self.config.clone();
        let purge_lock = self.purge_lock.clone();

        let handle = tokio::spawn(async move {
            // Dedicated connection: purge never competes with the main conn mutex.
            let mut conn = match Connection::open(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Purge scheduler: failed to open DB connection: {}", e);
                    return;
                },
            };
            if let Err(e) = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            {
                tracing::warn!("Purge scheduler: failed to set WAL mode: {}", e);
            }

            loop {
                let now = chrono::Local::now();
                let next_purge = if now.hour() < 2 {
                    now.date_naive().and_hms_opt(2, 0, 0).unwrap()
                } else {
                    (now.date_naive() + chrono::Duration::days(1))
                        .and_hms_opt(2, 0, 0)
                        .unwrap()
                };
                let next_purge =
                    chrono::TimeZone::from_local_datetime(&chrono::Local, &next_purge).unwrap();
                let duration = (next_purge - now)
                    .to_std()
                    .unwrap_or(std::time::Duration::from_secs(86400));

                tokio::time::sleep(duration).await;

                if let Ok(_guard) = purge_lock.try_lock().await {
                    let cutoff =
                        chrono::Utc::now() - chrono::Duration::days(config.retention_days as i64);
                    let cutoff_timestamp = cutoff.timestamp_nanos_opt().unwrap_or(0);

                    if let Ok(record) = purge::purge_old_data(
                        &mut conn,
                        cutoff_timestamp,
                        10000,
                        &[
                            crate::SignalType::Logs,
                            crate::SignalType::Traces,
                            crate::SignalType::Metrics,
                        ],
                        false,
                    ) {
                        tracing::info!(
                            "Automatic purge completed: {} logs, {} spans, {} metrics deleted",
                            record.logs_deleted,
                            record.spans_deleted,
                            record.metrics_deleted
                        );
                        // VACUUM requires exclusive access and cannot run while the main
                        // connection is open. Run a passive checkpoint instead to keep
                        // the WAL from growing unboundedly after bulk deletes.
                        if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);") {
                            tracing::warn!("Purge scheduler: WAL checkpoint failed: {}", e);
                        }
                    }
                }
            }
        });

        *self.purge_handle.lock() = Some(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_sqlite_backend_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig::default().with_data_dir(temp_dir.path().to_path_buf());

        let backend = SqliteBackend::new(config);
        assert!(backend.conn.lock().is_none());
    }

    #[tokio::test]
    async fn test_sqlite_backend_initialization() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig::default().with_data_dir(temp_dir.path().to_path_buf());

        let mut backend = SqliteBackend::new(config);
        let result = backend.initialize().await;
        assert!(result.is_ok());
        assert!(backend.conn.lock().is_some());
    }

    #[tokio::test]
    async fn test_stats_returns_counts() {
        use otelite_core::telemetry::log::SeverityLevel;
        use std::collections::HashMap;

        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig::default().with_data_dir(temp_dir.path().to_path_buf());

        let mut backend = SqliteBackend::new(config);
        backend.initialize().await.unwrap();

        let log = LogRecord {
            timestamp: 1000,
            observed_timestamp: Some(1000),
            severity: SeverityLevel::Info,
            severity_text: Some("INFO".to_string()),
            body: "test log".to_string(),
            trace_id: None,
            span_id: None,
            attributes: HashMap::new(),
            resource: None,
        };
        backend.write_log(&log).await.unwrap();

        let stats = backend.stats().await.unwrap();

        assert_eq!(stats.log_count, 1);
        assert_eq!(stats.span_count, 0);
        assert_eq!(stats.metric_count, 0);
        assert!(stats.storage_size_bytes > 0);
    }
}
