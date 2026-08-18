//! Read operations for SQLite backend

use crate::error::{Result, StorageError};
use crate::{QueryParams, StorageStats};
use otelite_core::query::{Operator, QueryPredicate, QueryValue};
use otelite_core::telemetry::log::SeverityLevel;
use otelite_core::telemetry::trace::{SpanKind, SpanStatus, StatusCode};
use otelite_core::telemetry::{
    classify_span_capabilities, GenAiEmitter, GenAiSpanRole, LogRecord, Metric, MetricObservation,
    Span,
};
use rusqlite::{Connection, Row};
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashMap};

/// Query logs from the database
pub fn query_logs(conn: &Connection, params: &QueryParams) -> Result<Vec<LogRecord>> {
    let mut query = String::from("SELECT * FROM logs WHERE 1=1");
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Add time range filter
    if let Some(start) = params.start_time {
        query.push_str(" AND timestamp >= ?");
        sql_params.push(Box::new(start));
    }
    if let Some(end) = params.end_time {
        query.push_str(" AND timestamp <= ?");
        sql_params.push(Box::new(end));
    }

    // Add trace/span filter
    if let Some(ref trace_id) = params.trace_id {
        query.push_str(" AND trace_id = ?");
        sql_params.push(Box::new(trace_id.clone()));
    }
    if let Some(ref span_id) = params.span_id {
        query.push_str(" AND span_id = ?");
        sql_params.push(Box::new(span_id.clone()));
    }

    // Add severity filter
    if let Some(min_severity) = params.min_severity {
        query.push_str(" AND severity_number >= ?");
        sql_params.push(Box::new(min_severity.to_i32()));
    }

    // Add full-text search if provided
    if let Some(ref search) = params.search_text {
        query.push_str(" AND id IN (SELECT rowid FROM logs_fts WHERE body MATCH ?)");
        sql_params.push(Box::new(search.clone()));
    }

    append_predicates("logs", &params.predicates, &mut query, &mut sql_params)?;

    // Add ordering and limit
    query.push_str(" ORDER BY timestamp DESC");
    if let Some(limit) = params.limit {
        query.push_str(" LIMIT ?");
        sql_params.push(Box::new(limit as i64));
    }

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let logs = stmt
        .query_map(param_refs.as_slice(), parse_log_row)
        .map_err(|e| StorageError::QueryError(format!("Failed to execute query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse results: {}", e)))?;

    Ok(logs)
}

/// Query spans from the database
pub fn query_spans(conn: &Connection, params: &QueryParams) -> Result<Vec<Span>> {
    let mut query = String::from("SELECT * FROM spans WHERE 1=1");
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Add time range filter
    if let Some(start) = params.start_time {
        query.push_str(" AND start_time >= ?");
        sql_params.push(Box::new(start));
    }
    if let Some(end) = params.end_time {
        query.push_str(" AND end_time <= ?");
        sql_params.push(Box::new(end));
    }

    // Add trace filter
    if let Some(ref trace_id) = params.trace_id {
        query.push_str(" AND trace_id = ?");
        sql_params.push(Box::new(trace_id.clone()));
    }

    append_predicates("spans", &params.predicates, &mut query, &mut sql_params)?;

    // Add ordering and limit
    query.push_str(" ORDER BY start_time DESC");
    if let Some(limit) = params.limit {
        query.push_str(" LIMIT ?");
        sql_params.push(Box::new(limit as i64));
    }

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let spans = stmt
        .query_map(param_refs.as_slice(), parse_span_row)
        .map_err(|e| StorageError::QueryError(format!("Failed to execute query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse results: {}", e)))?;

    Ok(spans)
}

/// Query all spans belonging to the N most-recent traces matching the filters.
/// Avoids the "big trace eats the span budget" problem in list_traces.
pub fn query_spans_for_trace_list(
    conn: &Connection,
    params: &QueryParams,
    trace_limit: usize,
) -> Result<Vec<Span>> {
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    let mut subquery = String::from("SELECT trace_id FROM spans WHERE 1=1");
    if let Some(start) = params.start_time {
        subquery.push_str(" AND start_time >= ?");
        sql_params.push(Box::new(start));
    }
    if let Some(end) = params.end_time {
        subquery.push_str(" AND end_time <= ?");
        sql_params.push(Box::new(end));
    }
    if let Some(ref trace_id) = params.trace_id {
        subquery.push_str(" AND trace_id = ?");
        sql_params.push(Box::new(trace_id.clone()));
    }
    subquery.push_str(" GROUP BY trace_id ORDER BY MAX(start_time) DESC LIMIT ?");
    sql_params.push(Box::new(trace_limit as i64));

    let query = format!(
        "SELECT * FROM spans WHERE trace_id IN ({}) ORDER BY start_time DESC",
        subquery
    );

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let spans = stmt
        .query_map(param_refs.as_slice(), parse_span_row)
        .map_err(|e| StorageError::QueryError(format!("Failed to execute query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse results: {}", e)))?;

    Ok(spans)
}

/// Query metrics from the database
pub fn query_metrics(conn: &Connection, params: &QueryParams) -> Result<Vec<Metric>> {
    let mut query = String::from("SELECT * FROM metrics WHERE 1=1");
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Add time range filter
    if let Some(start) = params.start_time {
        query.push_str(" AND timestamp >= ?");
        sql_params.push(Box::new(start));
    }
    if let Some(end) = params.end_time {
        query.push_str(" AND timestamp <= ?");
        sql_params.push(Box::new(end));
    }

    append_predicates("metrics", &params.predicates, &mut query, &mut sql_params)?;

    // Add ordering and limit
    query.push_str(" ORDER BY timestamp DESC");
    if let Some(limit) = params.limit {
        query.push_str(" LIMIT ?");
        sql_params.push(Box::new(limit as i64));
    }

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let metrics = stmt
        .query_map(param_refs.as_slice(), parse_metric_row)
        .map_err(|e| StorageError::QueryError(format!("Failed to execute query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse results: {}", e)))?;

    Ok(metrics)
}

/// Query metrics returning only the most-recent data point per unique metric name.
///
/// Prevents high-frequency counters from crowding out less-frequent gauges and
/// histograms when the caller only needs the current value for each metric (e.g.,
/// the metrics list sidebar). The inner subquery selects the rowid of the row
/// with the maximum timestamp for each name before any time-range filtering.
pub fn query_latest_metrics(conn: &Connection, params: &QueryParams) -> Result<Vec<Metric>> {
    // Outer query adds optional time/predicate filters on top of the dedup subquery.
    let mut query = String::from(
        "SELECT * FROM metrics WHERE rowid IN (\
           SELECT rowid FROM metrics GROUP BY name HAVING timestamp = MAX(timestamp)\
         ) AND 1=1",
    );
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = params.start_time {
        query.push_str(" AND timestamp >= ?");
        sql_params.push(Box::new(start));
    }
    if let Some(end) = params.end_time {
        query.push_str(" AND timestamp <= ?");
        sql_params.push(Box::new(end));
    }

    append_predicates("metrics", &params.predicates, &mut query, &mut sql_params)?;

    query.push_str(" ORDER BY name ASC");
    if let Some(limit) = params.limit {
        query.push_str(" LIMIT ?");
        sql_params.push(Box::new(limit as i64));
    }

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|p| p.as_ref()).collect();

    let metrics = stmt
        .query_map(param_refs.as_slice(), parse_metric_row)
        .map_err(|e| StorageError::QueryError(format!("Failed to execute query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse results: {}", e)))?;

    Ok(metrics)
}

/// Get storage statistics
pub fn get_stats(conn: &Connection) -> Result<StorageStats> {
    // Count records
    let log_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))
        .map_err(|e| StorageError::QueryError(format!("Failed to count logs: {}", e)))?;

    let span_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM spans", [], |row| row.get(0))
        .map_err(|e| StorageError::QueryError(format!("Failed to count spans: {}", e)))?;

    let metric_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
        .map_err(|e| StorageError::QueryError(format!("Failed to count metrics: {}", e)))?;

    // Get time ranges
    let oldest_timestamp: Option<i64> = conn
        .query_row(
            "SELECT MIN(timestamp) FROM (
            SELECT timestamp FROM logs
            UNION ALL SELECT start_time as timestamp FROM spans
            UNION ALL SELECT timestamp FROM metrics
        )",
            [],
            |row| row.get(0),
        )
        .ok();

    let newest_timestamp: Option<i64> = conn
        .query_row(
            "SELECT MAX(timestamp) FROM (
            SELECT timestamp FROM logs
            UNION ALL SELECT end_time as timestamp FROM spans
            UNION ALL SELECT timestamp FROM metrics
        )",
            [],
            |row| row.get(0),
        )
        .ok();

    // Get database size (page_count * page_size)
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap_or(0);
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap_or(4096);
    let total_size_bytes = page_count * page_size;

    Ok(StorageStats {
        log_count: log_count as u64,
        span_count: span_count as u64,
        metric_count: metric_count as u64,
        oldest_timestamp,
        newest_timestamp,
        storage_size_bytes: total_size_bytes as u64,
    })
}

fn append_predicates(
    signal_type: &str,
    predicates: &[QueryPredicate],
    query: &mut String,
    sql_params: &mut Vec<Box<dyn rusqlite::ToSql>>,
) -> Result<()> {
    for predicate in predicates {
        let clause = predicate_to_sql(signal_type, predicate, sql_params)?;
        query.push_str(" AND ");
        query.push_str(&clause);
    }

    Ok(())
}

fn predicate_to_sql(
    signal_type: &str,
    predicate: &QueryPredicate,
    sql_params: &mut Vec<Box<dyn rusqlite::ToSql>>,
) -> Result<String> {
    let lhs = field_to_sql(signal_type, &predicate.field)?;
    let operator = sql_operator(&predicate.operator);

    match (&predicate.field[..], &predicate.operator, &predicate.value) {
        ("duration", op, QueryValue::Duration(value)) if signal_type == "spans" => {
            sql_params.push(Box::new(*value as i64));
            Ok(format!("((end_time - start_time) {} ?)", sql_operator(op)))
        },
        ("duration", _, _) if signal_type == "spans" => Err(StorageError::QueryError(
            "Structured query field 'duration' for spans requires a duration value like 500ms"
                .to_string(),
        )),
        (_, Operator::Contains, QueryValue::String(value)) => {
            sql_params.push(Box::new(format!("%{}%", value)));
            Ok(format!("{} LIKE ?", lhs))
        },
        (_, Operator::Contains, _) => Err(StorageError::QueryError(format!(
            "Structured query operator 'contains' for field '{}' requires a quoted string value",
            predicate.field
        ))),
        (_, _, QueryValue::String(value)) => {
            sql_params.push(Box::new(value.clone()));
            Ok(format!("{} {} ?", lhs, operator))
        },
        (_, _, QueryValue::Number(value)) => {
            sql_params.push(Box::new(*value));
            Ok(format!("{} {} ?", lhs, operator))
        },
        (_, _, QueryValue::Duration(value)) => {
            sql_params.push(Box::new(*value as i64));
            Ok(format!("{} {} ?", lhs, operator))
        },
    }
}

fn field_to_sql(signal_type: &str, field: &str) -> Result<String> {
    let direct_column = match (signal_type, field) {
        ("logs", "timestamp") => Some("timestamp"),
        ("logs", "trace_id") => Some("trace_id"),
        ("logs", "span_id") => Some("span_id"),
        ("logs", "severity") | ("logs", "severity_number") => Some("severity_number"),
        ("logs", "body") => Some("body"),
        ("spans", "trace_id") => Some("trace_id"),
        ("spans", "span_id") => Some("span_id"),
        ("spans", "parent_span_id") => Some("parent_span_id"),
        ("spans", "name") => Some("name"),
        ("spans", "kind") => Some("kind"),
        ("spans", "start_time") => Some("start_time"),
        ("spans", "end_time") => Some("end_time"),
        ("metrics", "name") => Some("name"),
        ("metrics", "description") => Some("description"),
        ("metrics", "unit") => Some("unit"),
        ("metrics", "timestamp") => Some("timestamp"),
        _ => None,
    };

    if let Some(column) = direct_column {
        return Ok(column.to_string());
    }

    if let Some(attribute_field) = field.strip_prefix("attributes.") {
        return Ok(format!(
            "json_extract(attributes, '{}')",
            json_path_for_key(attribute_field)
        ));
    }

    if let Some(resource_field) = field.strip_prefix("resource.") {
        return Ok(format!(
            "json_extract(resource, '$.attributes{}')",
            json_key_accessor(resource_field)
        ));
    }

    Ok(format!(
        "json_extract(attributes, '{}')",
        json_path_for_key(field)
    ))
}

fn json_path_for_key(field: &str) -> String {
    format!("$.\"{}\"", field)
}

fn json_key_accessor(field: &str) -> String {
    format!(".\"{}\"", field)
}

fn sql_operator(operator: &Operator) -> &'static str {
    match operator {
        Operator::Equal => "=",
        Operator::NotEqual => "!=",
        Operator::GreaterThan => ">",
        Operator::LessThan => "<",
        Operator::GreaterThanOrEqual => ">=",
        Operator::LessThanOrEqual => "<=",
        Operator::Contains => "LIKE",
    }
}

// Helper functions to parse rows into telemetry types

fn parse_json_or_default<T>(json: &str, field: &str, record: &str) -> T
where
    T: DeserializeOwned + Default,
{
    serde_json::from_str(json).unwrap_or_else(|error| {
        tracing::warn!(
            field,
            record,
            %error,
            "Malformed JSON in stored telemetry field; using default value"
        );
        T::default()
    })
}

fn parse_json_or_none<T>(json: &str, field: &str, record: &str) -> Option<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(json)
        .map_err(|error| {
            tracing::warn!(
                field,
                record,
                %error,
                "Malformed JSON in stored telemetry field; omitting value"
            );
        })
        .ok()
}

fn parse_log_row(row: &Row) -> rusqlite::Result<LogRecord> {
    let timestamp: i64 = row.get("timestamp")?;
    let trace_id: Option<String> = row.get("trace_id")?;
    let span_id: Option<String> = row.get("span_id")?;
    let record = format!(
        "log timestamp={timestamp} trace_id={} span_id={}",
        trace_id.as_deref().unwrap_or("-"),
        span_id.as_deref().unwrap_or("-"),
    );
    let attributes_json: String = row.get("attributes")?;
    let attributes = parse_json_or_default(&attributes_json, "attributes", &record);

    let resource_json: String = row.get("resource")?;
    let resource = parse_json_or_none(&resource_json, "resource", &record);

    let severity_num: i32 = row.get("severity_number")?;
    let severity = SeverityLevel::from_i32(severity_num).unwrap_or(SeverityLevel::Info);

    Ok(LogRecord {
        timestamp,
        observed_timestamp: row.get("observed_timestamp")?,
        trace_id,
        span_id,
        severity,
        severity_text: row.get("severity_text")?,
        body: row.get("body")?,
        attributes,
        resource,
    })
}

fn parse_span_row(row: &Row) -> rusqlite::Result<Span> {
    let trace_id: String = row.get("trace_id")?;
    let span_id: String = row.get("span_id")?;
    let name: String = row.get("name")?;
    let record = format!("span trace_id={trace_id} span_id={span_id} name={name}");
    let attributes_json: String = row.get("attributes")?;
    let attributes = parse_json_or_default(&attributes_json, "attributes", &record);

    let events_json: String = row.get("events")?;
    let events = parse_json_or_default(&events_json, "events", &record);

    let resource_json: String = row.get("resource")?;
    let resource = parse_json_or_none(&resource_json, "resource", &record);

    let kind_num: i32 = row.get("kind")?;
    let kind = SpanKind::from_i32(kind_num).unwrap_or(SpanKind::Internal);

    let status_code_num: i32 = row.get("status_code")?;
    let status_code = StatusCode::from_i32(status_code_num).unwrap_or(StatusCode::Unset);

    let status = SpanStatus {
        code: status_code,
        message: row.get("status_message")?,
    };

    Ok(Span {
        trace_id,
        span_id,
        parent_span_id: row.get("parent_span_id")?,
        name,
        kind,
        start_time: row.get("start_time")?,
        end_time: row.get("end_time")?,
        attributes,
        events,
        status,
        resource,
    })
}

fn parse_metric_row(row: &Row) -> rusqlite::Result<Metric> {
    use otelite_core::telemetry::metric::MetricType;

    let name: String = row.get("name")?;
    let timestamp: i64 = row.get("timestamp")?;
    let record = format!("metric name={name} timestamp={timestamp}");
    let attributes_json: String = row.get("attributes")?;
    let attributes = parse_json_or_default(&attributes_json, "attributes", &record);

    let resource_json: String = row.get("resource")?;
    let resource = parse_json_or_none(&resource_json, "resource", &record);

    let metric_type_int: i32 = row.get("metric_type")?;
    let metric_type = match metric_type_int {
        0 => {
            let value: f64 = row.get("value_double")?;
            MetricType::Gauge(value)
        },
        1 => {
            let value: i64 = row.get("value_int")?;
            MetricType::Counter(value as u64)
        },
        2 => {
            let histogram_json: String = row.get("value_histogram")?;
            let (count, sum, buckets) =
                parse_json_or_default(&histogram_json, "value_histogram", &record);
            MetricType::Histogram {
                count,
                sum,
                buckets,
            }
        },
        3 => {
            let summary_json: String = row.get("value_summary")?;
            let (count, sum, quantiles) =
                parse_json_or_default(&summary_json, "value_summary", &record);
            MetricType::Summary {
                count,
                sum,
                quantiles,
            }
        },
        _ => MetricType::Gauge(0.0),
    };

    Ok(Metric {
        name,
        description: row.get("description")?,
        unit: row.get("unit")?,
        metric_type,
        timestamp,
        attributes,
        resource,
    })
}

/// SQL expressions for extracting token / model / system values from a span's
/// `attributes` JSON column, shared by all GenAI analytics queries.
///
/// The attribute vocabulary lives in [`otelite_core::semconv`]. This struct
/// projects those lists into SQL COALESCE fragments once per query.
struct TokenExprs {
    input: String,
    output: String,
    cache_creation: String,
    cache_read: String,
    model: String,
    system: String,
    /// Parenthesised OR-chain identifying LLM spans (also includes the
    /// OpenInference `openinference.span.kind` clause).
    llm_span_guard: String,
    /// LLM calls with reliable request-count and duration semantics. Includes
    /// Codex's completed sampling spans, but not token, cost, or outcome
    /// analytics because Codex does not emit those attributes.
    request_span_guard: String,
}

fn token_exprs() -> TokenExprs {
    use otelite_core::semconv;
    TokenExprs {
        input: semconv::coalesce_extract_cast("attributes", semconv::INPUT_TOKEN_KEYS, "INTEGER"),
        output: semconv::coalesce_extract_cast("attributes", semconv::OUTPUT_TOKEN_KEYS, "INTEGER"),
        cache_creation: semconv::coalesce_extract_cast(
            "attributes",
            semconv::CACHE_CREATION_TOKEN_KEYS,
            "INTEGER",
        ),
        cache_read: semconv::coalesce_extract_cast(
            "attributes",
            semconv::CACHE_READ_TOKEN_KEYS,
            "INTEGER",
        ),
        model: semconv::coalesce_extract("attributes", semconv::MODEL_KEYS),
        system: semconv::coalesce_extract("attributes", semconv::SYSTEM_KEYS),
        llm_span_guard: semconv::llm_span_guard("attributes"),
        request_span_guard: semconv::request_span_guard("attributes"),
    }
}

const CAPABILITY_QUERY_LIMIT: usize = 10_000;

#[derive(Default)]
struct CapabilityMetricAccum {
    eligible_count: usize,
    observed_count: usize,
    valid_count: usize,
    invalid_count: usize,
    degenerate_count: usize,
    source_attributes: HashMap<String, usize>,
}

impl CapabilityMetricAccum {
    fn record(
        &mut self,
        observation: MetricObservation,
        source_attribute: Option<&str>,
        degenerate: bool,
    ) {
        self.eligible_count += 1;
        if let Some(attribute) = source_attribute {
            *self
                .source_attributes
                .entry(attribute.to_string())
                .or_default() += 1;
            self.observed_count += 1;
        }
        match observation {
            MetricObservation::Valid => {
                self.valid_count += 1;
                if degenerate {
                    self.degenerate_count += 1;
                }
            },
            MetricObservation::Invalid => self.invalid_count += 1,
            MetricObservation::Absent => {},
        }
    }

    fn report(&self, ttft: bool) -> otelite_core::api::GenAiMetricCapability {
        let availability = if self.valid_count == 0 {
            "absent"
        } else if self.valid_count == self.eligible_count {
            "available"
        } else {
            "sparse"
        };
        let quality = if ttft
            && self.valid_count >= TTFT_DEGENERATE_MIN_SAMPLES
            && self.degenerate_count * 100 >= self.valid_count * 90
        {
            "degenerate"
        } else if self.invalid_count > 0 {
            "invalid"
        } else if self.valid_count > 0 {
            "reliable"
        } else {
            "not_assessed"
        };
        otelite_core::api::GenAiMetricCapability {
            eligible_count: self.eligible_count,
            observed_count: self.observed_count,
            valid_count: self.valid_count,
            invalid_count: self.invalid_count,
            availability: availability.to_string(),
            quality: quality.to_string(),
            derivation: if self.observed_count > 0 {
                "native".to_string()
            } else {
                "unavailable".to_string()
            },
            source_attributes: self.source_attributes.clone(),
        }
    }
}

#[derive(Default)]
struct CapabilityAccum {
    request_count: usize,
    input_tokens: CapabilityMetricAccum,
    output_tokens: CapabilityMetricAccum,
    cache_creation_tokens: CapabilityMetricAccum,
    cache_read_tokens: CapabilityMetricAccum,
    ttft: CapabilityMetricAccum,
}

type CapabilityGroupKey = (Option<String>, Option<String>, String, String, String);

fn first_semconv_attribute<'a>(
    attrs: &'a HashMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| attrs.get(*key).map(String::as_str))
}

fn emitter_name(emitter: GenAiEmitter) -> &'static str {
    match emitter {
        GenAiEmitter::ClaudeCode => "claude_code",
        GenAiEmitter::Codex => "codex",
        GenAiEmitter::OpenCode => "opencode",
        GenAiEmitter::StandardOtel => "standard_otel",
        GenAiEmitter::Unknown => "unknown",
        GenAiEmitter::Ambiguous => "ambiguous",
    }
}

fn capability_fingerprint(
    adapter_rule: &str,
    service_name: Option<&str>,
    scope_name: Option<&str>,
    scope_version: Option<&str>,
) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for part in [
        adapter_rule,
        service_name.unwrap_or(""),
        scope_name.unwrap_or(""),
        scope_version.unwrap_or(""),
    ] {
        for byte in part.bytes().chain([0_u8]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("genai-v1-{hash:016x}")
}

/// Query native GenAI telemetry capability coverage without cross-span correlation.
///
/// The report is calculated from the most recent bounded physical-span sample.
/// Duplicate delivery is canonicalised within that sample. `truncated` means
/// older physical spans were not examined.
pub fn query_genai_capabilities(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<otelite_core::api::GenAiCapabilityResponse> {
    let mut where_clause = String::from("WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    let sql = format!(
        "WITH recent_spans AS (
            SELECT
                trace_id, span_id, parent_span_id, name, kind, start_time, end_time,
                COALESCE(attributes, '{{}}') AS attributes,
                COALESCE(events, '[]') AS events,
                COALESCE(status_code, 0) AS status_code,
                status_message,
                COALESCE(resource, 'null') AS resource,
                created_at,
                id
            FROM spans {where_clause}
            ORDER BY start_time DESC, id DESC
            LIMIT {}
         ),
         ranked_spans AS (
            SELECT
                trace_id, span_id, parent_span_id, name, kind, start_time, end_time,
                COALESCE(attributes, '{{}}') AS attributes,
                COALESCE(events, '[]') AS events,
                COALESCE(status_code, 0) AS status_code,
                status_message,
                COALESCE(resource, 'null') AS resource,
                created_at,
                id,
                ROW_NUMBER() OVER (
                    PARTITION BY trace_id, span_id
                    ORDER BY created_at ASC, id ASC
                ) AS delivery_rank,
                COUNT(*) OVER (PARTITION BY trace_id, span_id) - 1 AS duplicate_deliveries
            FROM recent_spans
         )
         SELECT
            trace_id, span_id, parent_span_id, name, kind, start_time, end_time,
            attributes, events, status_code, status_message, resource,
            duplicate_deliveries
         FROM ranked_spans
         WHERE delivery_rank = 1
         ORDER BY start_time DESC, id DESC",
        CAPABILITY_QUERY_LIMIT + 1
    );
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|param| param.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|error| {
        StorageError::QueryError(format!("Failed to prepare GenAI capability query: {error}"))
    })?;
    let mut rows: Vec<(Span, usize)> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((parse_span_row(row)?, row.get::<_, usize>(12)?))
        })
        .map_err(|error| {
            StorageError::QueryError(format!("Failed to execute GenAI capability query: {error}"))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| {
            StorageError::QueryError(format!("Failed to parse GenAI capability rows: {error}"))
        })?;
    let truncated = rows.len() == CAPABILITY_QUERY_LIMIT + 1;
    if truncated {
        rows.pop();
    }

    let mut canonical_request_span_count = 0;
    let mut duplicate_span_count = 0;
    let mut groups: BTreeMap<CapabilityGroupKey, CapabilityAccum> = BTreeMap::new();
    for (span, duplicate_deliveries) in rows {
        let capabilities = classify_span_capabilities(&span);
        if capabilities.role != GenAiSpanRole::RequestTiming {
            continue;
        }
        let request_model =
            first_semconv_attribute(&span.attributes, otelite_core::semconv::REQUEST_MODEL_KEYS);
        if model.is_some_and(|model| request_model != Some(model)) {
            continue;
        }
        canonical_request_span_count += 1;
        duplicate_span_count += duplicate_deliveries;
        let provider =
            first_semconv_attribute(&span.attributes, otelite_core::semconv::SYSTEM_KEYS)
                .map(str::to_string);
        let model = request_model.map(str::to_string);
        let fingerprint = capability_fingerprint(
            capabilities.fingerprint.adapter_rule,
            capabilities.fingerprint.service_name.as_deref(),
            capabilities.fingerprint.scope_name.as_deref(),
            capabilities.fingerprint.scope_version.as_deref(),
        );
        let key = (
            provider,
            model,
            fingerprint,
            emitter_name(capabilities.emitter).to_string(),
            capabilities.fingerprint.adapter_rule.to_string(),
        );
        let entry = groups.entry(key).or_default();
        entry.request_count += 1;
        entry.input_tokens.record(
            capabilities.input_tokens.observation,
            capabilities.input_tokens.source_attribute,
            false,
        );
        entry.output_tokens.record(
            capabilities.output_tokens.observation,
            capabilities.output_tokens.source_attribute,
            false,
        );
        entry.cache_creation_tokens.record(
            capabilities.cache_creation_tokens.observation,
            capabilities.cache_creation_tokens.source_attribute,
            false,
        );
        entry.cache_read_tokens.record(
            capabilities.cache_read_tokens.observation,
            capabilities.cache_read_tokens.source_attribute,
            false,
        );
        let duration_secs =
            (span.end_time.saturating_sub(span.start_time)) as f64 / 1_000_000_000.0;
        let degenerate = capabilities.ttft.seconds.is_some_and(|seconds| {
            duration_secs > 0.0 && seconds / duration_secs >= TTFT_DEGENERATE_RATIO
        });
        entry.ttft.record(
            capabilities.ttft.observation,
            capabilities.ttft.source_attribute,
            degenerate,
        );
    }

    let reports = groups
        .into_iter()
        .map(
            |((provider, model, emitter_fingerprint, emitter, adapter_rule), accum)| {
                otelite_core::api::GenAiCapabilityReport {
                    provider,
                    model,
                    emitter_fingerprint,
                    emitter,
                    adapter_rule,
                    request_count: accum.request_count,
                    input_tokens: accum.input_tokens.report(false),
                    output_tokens: accum.output_tokens.report(false),
                    cache_creation_tokens: accum.cache_creation_tokens.report(false),
                    cache_read_tokens: accum.cache_read_tokens.report(false),
                    ttft: accum.ttft.report(true),
                    correlation: otelite_core::api::GenAiCorrelationProvenance {
                        rule: "none".to_string(),
                        matched_count: 0,
                        unmatched_count: 0,
                        rejected_count: 0,
                        ambiguous_count: 0,
                    },
                }
            },
        )
        .collect();
    Ok(otelite_core::api::GenAiCapabilityResponse {
        reports,
        canonical_span_count: canonical_request_span_count,
        duplicate_span_count,
        truncated,
    })
}

/// Query token usage statistics for GenAI/LLM spans
///
/// Returns aggregated token usage grouped by model and system (provider).
pub fn query_token_usage(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<(
    otelite_core::api::TokenUsageSummary,
    Vec<otelite_core::api::ModelUsage>,
    Vec<otelite_core::api::SystemUsage>,
)> {
    let exprs = token_exprs();
    // Build WHERE clause for time filtering.
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let input_expr = exprs.input;
    let output_expr = exprs.output;
    let cache_creation_expr = exprs.cache_creation;
    let cache_read_expr = exprs.cache_read;

    // Query overall summary
    let summary_query = format!(
        "SELECT
            COALESCE(SUM({input_expr}), 0) as total_input,
            COALESCE(SUM({output_expr}), 0) as total_output,
            COUNT(*) as total_requests,
            COALESCE(SUM({cache_creation_expr}), 0) as cache_creation,
            COALESCE(SUM({cache_read_expr}), 0) as cache_read
        FROM spans
        {where_clause}"
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let summary = conn
        .query_row(&summary_query, param_refs.as_slice(), |row| {
            Ok(otelite_core::api::TokenUsageSummary {
                total_input_tokens: row.get::<_, i64>(0)? as u64,
                total_output_tokens: row.get::<_, i64>(1)? as u64,
                total_requests: row.get::<_, i64>(2)? as usize,
                total_cache_creation_tokens: row.get::<_, i64>(3)? as u64,
                total_cache_read_tokens: row.get::<_, i64>(4)? as u64,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to query token summary: {}", e)))?;

    // Query by model — fall back across common model-attribute spellings.
    let model_expr = exprs.model;
    let model_query = format!(
        "SELECT
            {model_expr} as model,
            COALESCE(SUM({input_expr}), 0) as input_tokens,
            COALESCE(SUM({output_expr}), 0) as output_tokens,
            COUNT(*) as requests
        FROM spans
        {where_clause}
        GROUP BY model
        HAVING model IS NOT NULL
        ORDER BY input_tokens + output_tokens DESC"
    );

    let mut stmt = conn
        .prepare(&model_query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare model query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let by_model = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::ModelUsage {
                model: row.get(0)?,
                input_tokens: row.get::<_, i64>(1)? as u64,
                output_tokens: row.get::<_, i64>(2)? as u64,
                requests: row.get::<_, i64>(3)? as usize,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to execute model query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse model results: {}", e)))?;

    // Query by system/provider — accept the OTel-standard names plus llm.* variants.
    let system_expr = exprs.system;
    let system_query = format!(
        "SELECT
            {system_expr} as system,
            COALESCE(SUM({input_expr}), 0) as input_tokens,
            COALESCE(SUM({output_expr}), 0) as output_tokens,
            COUNT(*) as requests
        FROM spans
        {where_clause}
        GROUP BY system
        HAVING system IS NOT NULL
        ORDER BY input_tokens + output_tokens DESC"
    );

    let mut stmt = conn
        .prepare(&system_query)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare system query: {}", e)))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let by_system = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::SystemUsage {
                system: row.get(0)?,
                input_tokens: row.get::<_, i64>(1)? as u64,
                output_tokens: row.get::<_, i64>(2)? as u64,
                requests: row.get::<_, i64>(3)? as usize,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to execute system query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse system results: {}", e)))?;

    Ok((summary, by_model, by_system))
}

/// Time-bucketed token usage grouped by model.
///
/// Bucket assignment uses SQLite integer division (floor): `bucket = (start_time / bucket_ns) * bucket_ns`.
pub fn query_cost_series(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    bucket_ns: i64,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::CostSeriesPoint>> {
    if bucket_ns <= 0 {
        return Err(StorageError::QueryError(format!(
            "bucket_ns must be positive, got {}",
            bucket_ns
        )));
    }

    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "SELECT
            (start_time / ?) * ? as bucket,
            {model} as model,
            COALESCE(SUM({input}), 0),
            COALESCE(SUM({output}), 0),
            COALESCE(SUM({cache_creation}), 0),
            COALESCE(SUM({cache_read}), 0),
            COUNT(*) as requests
        FROM spans
        {where_clause}
        GROUP BY bucket, model
        ORDER BY bucket ASC",
        model = exprs.model,
        input = exprs.input,
        output = exprs.output,
        cache_creation = exprs.cache_creation,
        cache_read = exprs.cache_read,
    );

    // bucket_ns parameters (two occurrences) must come first to match the `?` order in SQL.
    let mut all_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(params.len() + 2);
    all_params.push(Box::new(bucket_ns));
    all_params.push(Box::new(bucket_ns));
    all_params.extend(params);

    let param_refs: Vec<&dyn rusqlite::ToSql> = all_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare cost_series query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::CostSeriesPoint {
                timestamp: row.get::<_, i64>(0)?,
                model: row.get::<_, Option<String>>(1)?,
                input_tokens: row.get::<_, i64>(2)? as u64,
                output_tokens: row.get::<_, i64>(3)? as u64,
                cache_creation_tokens: row.get::<_, i64>(4)? as u64,
                cache_read_tokens: row.get::<_, i64>(5)? as u64,
                requests: row.get::<_, i64>(6)? as usize,
                // Cost enrichment happens in the API layer.
                cost: None,
                cost_source: None,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute cost_series query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse cost_series results: {}", e))
        })?;

    Ok(rows)
}

/// Top-N most expensive LLM spans by total tokens.
#[allow(clippy::too_many_arguments)]
pub fn query_top_spans(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    limit: usize,
    sort_by: otelite_core::api::TopSpanSort,
    truncated_only: bool,
) -> Result<Vec<otelite_core::api::TopSpan>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if truncated_only {
        where_clause.push_str(
            " AND (json_extract(attributes, '$.\"gen_ai.response.finish_reason\"') IN ('max_tokens','length')\
             OR (json_type(attributes, '$.\"gen_ai.response.finish_reasons\"') = 'array'\
                 AND json_extract(json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"'), '$[0]') IN ('max_tokens','length')))",
        );
    }

    use otelite_core::api::TopSpanSort;
    let order_by = match sort_by {
        TopSpanSort::TotalTokens => "total_tokens DESC".to_string(),
        TopSpanSort::Duration => "(end_time - start_time) DESC".to_string(),
        TopSpanSort::OutputInputRatio => {
            "CAST(COALESCE(output_tokens_raw, 0) AS FLOAT) / NULLIF(COALESCE(input_tokens_raw, 0) + COALESCE(cache_creation_tokens_raw, 0) + COALESCE(cache_read_tokens_raw, 0), 0) DESC".to_string()
        }
        TopSpanSort::CacheEfficiency => {
            "CAST(COALESCE(cache_read_tokens_raw, 0) AS FLOAT) / NULLIF(COALESCE(input_tokens_raw, 0) + COALESCE(cache_read_tokens_raw, 0), 0) ASC".to_string()
        }
    };

    let sql = format!(
        "SELECT
            trace_id,
            span_id,
            start_time,
            (end_time - start_time) as duration,
            {model} as model,
            {system} as system,
            json_extract(attributes, '$.\"session.id\"') as session_id,
            json_extract(attributes, '$.\"prompt.id\"') as prompt_id,
            COALESCE({input}, 0) as input_tokens,
            COALESCE({output}, 0) as output_tokens,
            COALESCE({cache_creation}, 0) as cache_creation_tokens,
            COALESCE({cache_read}, 0) as cache_read_tokens,
            COALESCE({input}, 0) + COALESCE({output}, 0) + COALESCE({cache_creation}, 0) + COALESCE({cache_read}, 0) as total_tokens,
            COALESCE(
                json_extract(attributes, '$.\"gen_ai.response.finish_reason\"'),
                CASE WHEN json_type(attributes, '$.\"gen_ai.response.finish_reasons\"') = 'array'
                     THEN json_extract(json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"'), '$[0]')
                     ELSE NULL END
            ) as finish_reason,
            json_extract(attributes, '$.\"gen_ai.conversation.id\"') as conversation_id,
            {input} as input_tokens_raw,
            {output} as output_tokens_raw,
            {cache_creation} as cache_creation_tokens_raw,
            {cache_read} as cache_read_tokens_raw
        FROM spans
        {where_clause}
        ORDER BY {order_by}
        LIMIT ?",
        model = exprs.model,
        system = exprs.system,
        input = exprs.input,
        output = exprs.output,
        cache_creation = exprs.cache_creation,
        cache_read = exprs.cache_read,
        order_by = order_by,
    );

    params.push(Box::new(limit as i64));
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare top_spans query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::TopSpan {
                trace_id: row.get(0)?,
                span_id: row.get(1)?,
                start_time: row.get::<_, i64>(2)?,
                duration: row.get::<_, i64>(3)?,
                model: row.get::<_, Option<String>>(4)?,
                system: row.get::<_, Option<String>>(5)?,
                session_id: row.get::<_, Option<String>>(6)?,
                prompt_id: row.get::<_, Option<String>>(7)?,
                input_tokens: row.get::<_, i64>(8)? as u64,
                output_tokens: row.get::<_, i64>(9)? as u64,
                cache_creation_tokens: row.get::<_, i64>(10)? as u64,
                cache_read_tokens: row.get::<_, i64>(11)? as u64,
                total_tokens: row.get::<_, i64>(12)? as u64,
                finish_reason: row.get::<_, Option<String>>(13)?,
                conversation_id: row.get::<_, Option<String>>(14)?,
                // Cost and derived fields computed in the API layer.
                cost: None,
                cost_source: None,
                cost_reason: None,
                derived_output_tokens_per_sec: None,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to execute top_spans query: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse top_spans results: {}", e))
        })?;

    Ok(rows)
}

pub fn query_top_sessions(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    limit: usize,
) -> Result<Vec<otelite_core::api::SessionCostRow>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {} AND session_id IS NOT NULL", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT
            json_extract(attributes, '$.\"session.id\"') as session_id,
            COUNT(*) as request_count,
            SUM(COALESCE({input}, 0)) as input_tokens,
            SUM(COALESCE({output}, 0)) as output_tokens,
            SUM(COALESCE({input}, 0) + COALESCE({output}, 0) + COALESCE({cache_creation}, 0) + COALESCE({cache_read}, 0)) as total_tokens
        FROM spans
        {where_clause}
        GROUP BY session_id
        ORDER BY total_tokens DESC
        LIMIT ?",
        input = exprs.input,
        output = exprs.output,
        cache_creation = exprs.cache_creation,
        cache_read = exprs.cache_read,
    );

    params.push(Box::new(limit as i64));
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare top_sessions query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::SessionCostRow {
                session_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                request_count: row.get::<_, i64>(1)? as u64,
                input_tokens: row.get::<_, i64>(2)? as u64,
                output_tokens: row.get::<_, i64>(3)? as u64,
                total_tokens: row.get::<_, i64>(4)? as u64,
                cost: None,
                cost_source: None,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute top_sessions query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse top_sessions results: {}", e))
        })?;

    Ok(rows)
}

pub fn query_top_conversations(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    limit: usize,
) -> Result<Vec<otelite_core::api::ConversationCostRow>> {
    let exprs = token_exprs();
    let conversation_id_expr = "json_extract(attributes, '$.\"gen_ai.conversation.id\"')";
    let mut where_clause = format!(
        "WHERE {} AND {} IS NOT NULL",
        exprs.llm_span_guard, conversation_id_expr
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT
            {conv_id} as conversation_id,
            COUNT(*) as request_count,
            SUM(COALESCE({input}, 0)) as input_tokens,
            SUM(COALESCE({output}, 0)) as output_tokens,
            SUM(COALESCE({input}, 0) + COALESCE({output}, 0) + COALESCE({cache_creation}, 0) + COALESCE({cache_read}, 0)) as total_tokens
        FROM spans
        {where_clause}
        GROUP BY conversation_id
        ORDER BY total_tokens DESC
        LIMIT ?",
        conv_id = conversation_id_expr,
        input = exprs.input,
        output = exprs.output,
        cache_creation = exprs.cache_creation,
        cache_read = exprs.cache_read,
    );

    params.push(Box::new(limit as i64));
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare top_conversations query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::ConversationCostRow {
                conversation_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                request_count: row.get::<_, i64>(1)? as u64,
                input_tokens: row.get::<_, i64>(2)? as u64,
                output_tokens: row.get::<_, i64>(3)? as u64,
                total_tokens: row.get::<_, i64>(4)? as u64,
                cost: None,
                cost_source: None,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute top_conversations query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse top_conversations results: {}", e))
        })?;

    Ok(rows)
}

/// Finish-reason distribution across LLM spans and Claude Code api_response_body logs.
///
/// Unions three sources:
/// 1. OTel plural `gen_ai.response.finish_reasons` (array attribute, unpacked via json_each).
/// 2. OTel singular `gen_ai.response.finish_reason` (scalar attribute).
/// 3. Claude Code `stop_reason` embedded in `claude_code.api_response_body` log bodies.
pub fn query_finish_reasons(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::FinishReasonCount>> {
    // Time/model filters are applied per sub-query. We build fragments so each UNION
    // branch only references its own table's columns (spans.start_time / logs.timestamp).
    let exprs = token_exprs();
    let mut spans_time_filter = String::new();
    let mut logs_time_filter = String::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        spans_time_filter.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        spans_time_filter.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        spans_time_filter.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }
    // The plural (json_each) branch re-uses the same spans time/model filter, so bind again.
    if let Some(start) = start_time {
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        params.push(Box::new(m.to_string()));
    }
    if let Some(start) = start_time {
        logs_time_filter.push_str(" AND timestamp >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        logs_time_filter.push_str(" AND timestamp <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "WITH reasons AS (
            SELECT json_extract(attributes, '$.\"gen_ai.response.finish_reason\"') AS reason
            FROM spans
            WHERE json_extract(attributes, '$.\"gen_ai.response.finish_reason\"') IS NOT NULL
            {spans_time_filter}

            UNION ALL

            SELECT je.value AS reason
            FROM (
                SELECT json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"') AS arr
                FROM spans
                WHERE json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"') IS NOT NULL
                  AND json_valid(json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"'))
                  AND json_type(json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"')) = 'array'
                {spans_time_filter}
            ) s, json_each(s.arr) je

            UNION ALL

            SELECT json_extract(body_json, '$.stop_reason') AS reason
            FROM (
                SELECT json_extract(attributes, '$.body') AS body_json
                FROM logs
                WHERE body = 'claude_code.api_response_body'
                  AND json_extract(attributes, '$.body') IS NOT NULL
                  AND json_valid(json_extract(attributes, '$.body'))
                  {logs_time_filter}
            ) l
            WHERE json_extract(body_json, '$.stop_reason') IS NOT NULL
        )
        SELECT reason, COUNT(*) as cnt
        FROM reasons
        WHERE reason IS NOT NULL
        GROUP BY reason
        ORDER BY cnt DESC"
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare finish_reasons query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::FinishReasonCount {
                reason: row.get::<_, String>(0)?,
                count: row.get::<_, i64>(1)? as usize,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute finish_reasons query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse finish_reasons results: {}", e))
        })?;

    Ok(rows)
}

const TTFT_DEGENERATE_RATIO: f64 = 0.9;
const TTFT_DEGENERATE_MIN_SAMPLES: usize = 10;

#[derive(Default)]
struct TtftAccum {
    values_ms: Vec<i64>,
    invalid_count: usize,
    degenerate_count: usize,
}

impl TtftAccum {
    fn record(&mut self, duration_ms: i64, ttft_ms: Option<std::result::Result<i64, ()>>) {
        let Some(ttft_ms) = ttft_ms else {
            return;
        };
        let Ok(ttft_ms) = ttft_ms else {
            self.invalid_count += 1;
            return;
        };
        let quality = otelite_core::telemetry::classify_ttft_value(
            Some(ttft_ms as f64 / 1000.0),
            duration_ms as f64 / 1000.0,
        );
        if quality != otelite_core::telemetry::TtftValueQuality::Valid {
            self.invalid_count += 1;
            return;
        }
        if duration_ms > 0 && ttft_ms as f64 / duration_ms as f64 >= TTFT_DEGENERATE_RATIO {
            self.degenerate_count += 1;
        }
        self.values_ms.push(ttft_ms);
    }

    fn is_degenerate(&self) -> bool {
        self.values_ms.len() >= TTFT_DEGENERATE_MIN_SAMPLES
            && self.degenerate_count * 100 >= self.values_ms.len() * 90
    }
}

/// Latency / TTFT percentile statistics per model for LLM spans.
#[derive(Default)]
struct LatencyAccum {
    durations_ms: Vec<i64>,
    ttft: TtftAccum,
    token_rates: Vec<f64>,
    input_tokens: Vec<i64>,
    output_input_ratios: Vec<f64>,
}

fn normalized_ttft_ms(
    otel_ttft_secs: Option<&str>,
    llm_ttft_secs: Option<&str>,
    custom_ttft_ms: Option<&str>,
) -> Option<std::result::Result<i64, ()>> {
    let (raw, multiplier) = if let Some(raw) = otel_ttft_secs {
        (raw, 1000.0)
    } else if let Some(raw) = llm_ttft_secs {
        (raw, 1000.0)
    } else {
        let raw = custom_ttft_ms?;
        (raw, 1.0)
    };
    Some(
        raw.parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .and_then(|value| (value * multiplier).round().to_string().parse::<i64>().ok())
            .ok_or(()),
    )
}

///
/// SQLite has no native percentile, so we fetch raw durations per model into memory
/// and compute percentiles in Rust after sorting.
pub fn query_latency_stats(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::LatencyStats>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.request_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "SELECT
            {model} AS model,
            (end_time - start_time) / 1000000 AS duration_ms,
            json_extract(attributes, '$.\"gen_ai.server.time_to_first_token\"') AS otel_ttft_secs,
            json_extract(attributes, '$.\"llm.time_to_first_token\"') AS llm_ttft_secs,
            json_extract(attributes, '$.\"ttft_ms\"') AS custom_ttft_ms,
            {output} AS output_tokens,
            {input} AS input_tokens,
            {cache_creation} AS cache_creation_tokens,
            {cache_read} AS cache_read_tokens
        FROM spans
        {where_clause}",
        model = exprs.model,
        output = exprs.output,
        input = exprs.input,
        cache_creation = exprs.cache_creation,
        cache_read = exprs.cache_read,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare latency_stats query: {}", e))
    })?;

    struct Row {
        model: Option<String>,
        duration_ms: i64,
        otel_ttft_secs: Option<String>,
        llm_ttft_secs: Option<String>,
        custom_ttft_ms: Option<String>,
        output_tokens: Option<i64>,
        input_tokens: Option<i64>,
        cache_creation_tokens: Option<i64>,
        cache_read_tokens: Option<i64>,
    }

    let rows: Vec<Row> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(Row {
                model: row.get::<_, Option<String>>(0)?,
                duration_ms: row.get::<_, i64>(1)?,
                otel_ttft_secs: row.get::<_, Option<String>>(2)?,
                llm_ttft_secs: row.get::<_, Option<String>>(3)?,
                custom_ttft_ms: row.get::<_, Option<String>>(4)?,
                output_tokens: row.get::<_, Option<i64>>(5)?,
                input_tokens: row.get::<_, Option<i64>>(6)?,
                cache_creation_tokens: row.get::<_, Option<i64>>(7)?,
                cache_read_tokens: row.get::<_, Option<i64>>(8)?,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute latency_stats query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse latency_stats results: {}", e))
        })?;

    let mut groups: std::collections::BTreeMap<Option<String>, LatencyAccum> =
        std::collections::BTreeMap::new();
    for r in rows {
        let entry = groups.entry(r.model).or_default();
        entry.durations_ms.push(r.duration_ms);
        entry.ttft.record(
            r.duration_ms,
            normalized_ttft_ms(
                r.otel_ttft_secs.as_deref(),
                r.llm_ttft_secs.as_deref(),
                r.custom_ttft_ms.as_deref(),
            ),
        );
        if r.duration_ms > 0 {
            if let Some(output_tokens) = r.output_tokens.filter(|tokens| *tokens > 0) {
                entry
                    .token_rates
                    .push(output_tokens as f64 / (r.duration_ms as f64 / 1000.0));
            }
        }
        if let Some(input_tokens) = r.input_tokens {
            entry.input_tokens.push(input_tokens);
        }
        let input_context_tokens = r.input_tokens.unwrap_or_default()
            + r.cache_creation_tokens.unwrap_or_default()
            + r.cache_read_tokens.unwrap_or_default();
        if input_context_tokens > 0 {
            entry
                .output_input_ratios
                .push(r.output_tokens.unwrap_or_default() as f64 / input_context_tokens as f64);
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for (model, accum) in groups {
        let mut durations = accum.durations_ms;
        let ttft_degenerate = accum.ttft.is_degenerate();
        let TtftAccum {
            values_ms: mut ttfts,
            invalid_count: invalid_ttfts,
            degenerate_count: degenerate_ttfts,
        } = accum.ttft;
        let mut token_rates = accum.token_rates;
        let mut input_tkns = accum.input_tokens;
        let mut ratios = accum.output_input_ratios;
        durations.sort_unstable();
        ttfts.sort_unstable();
        token_rates.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        input_tkns.sort_unstable();
        ratios.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let count = durations.len();
        let avg_ms = if count == 0 {
            0.0
        } else {
            durations.iter().sum::<i64>() as f64 / count as f64
        };

        let ttft_count = ttfts.len();
        let (ttft_p50, ttft_p95, ttft_p99) = if ttft_count == 0 {
            (None, None, None)
        } else {
            (
                Some(percentile(&ttfts, 0.50)),
                Some(percentile(&ttfts, 0.95)),
                Some(percentile(&ttfts, 0.99)),
            )
        };

        let (tok_p50, tok_p95, tok_p99) = if token_rates.is_empty() {
            (None, None, None)
        } else {
            (
                Some(percentile_f64(&token_rates, 0.50)),
                Some(percentile_f64(&token_rates, 0.95)),
                Some(percentile_f64(&token_rates, 0.99)),
            )
        };

        let (inp_p50, inp_p95, inp_p99) = if input_tkns.is_empty() {
            (None, None, None)
        } else {
            (
                Some(percentile(&input_tkns, 0.50)),
                Some(percentile(&input_tkns, 0.95)),
                Some(percentile(&input_tkns, 0.99)),
            )
        };

        let (rat_p50, rat_p95, rat_p99) = if ratios.is_empty() {
            (None, None, None)
        } else {
            (
                Some(percentile_f64(&ratios, 0.50)),
                Some(percentile_f64(&ratios, 0.95)),
                Some(percentile_f64(&ratios, 0.99)),
            )
        };

        out.push(otelite_core::api::LatencyStats {
            model,
            count,
            avg_ms,
            p50_ms: percentile(&durations, 0.50),
            p95_ms: percentile(&durations, 0.95),
            p99_ms: percentile(&durations, 0.99),
            ttft_count,
            ttft_invalid_count: invalid_ttfts,
            ttft_degenerate_count: degenerate_ttfts,
            ttft_degenerate,
            ttft_p50_ms: ttft_p50,
            ttft_p95_ms: ttft_p95,
            ttft_p99_ms: ttft_p99,
            derived_tokens_per_sec_p50: tok_p50,
            derived_tokens_per_sec_p95: tok_p95,
            derived_tokens_per_sec_p99: tok_p99,
            input_tokens_p50: inp_p50,
            input_tokens_p95: inp_p95,
            input_tokens_p99: inp_p99,
            output_input_ratio_p50: rat_p50,
            output_input_ratio_p95: rat_p95,
            output_input_ratio_p99: rat_p99,
        });
    }

    out.sort_by_key(|r| std::cmp::Reverse(r.count));
    Ok(out)
}

fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn percentile_f64(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Error rate per model across LLM spans.
///
/// The spans table stores status as `status_code INTEGER` (0 = Unset, 1 = Ok, 2 = Error);
/// any row with status_code = 2 counts as an error.
pub fn query_error_rate(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::ErrorRateByModel>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "SELECT
            {model} AS model,
            SUM(CASE WHEN status_code = 2 THEN 1 ELSE 0 END) AS errors,
            COUNT(*) AS total
        FROM spans
        {where_clause}
        GROUP BY model
        HAVING model IS NOT NULL
        ORDER BY errors DESC, total DESC",
        model = exprs.model,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare error_rate query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let model: Option<String> = row.get(0)?;
            let errors: i64 = row.get(1)?;
            let total: i64 = row.get(2)?;
            let error_rate = if total > 0 {
                errors as f64 / total as f64
            } else {
                0.0
            };
            Ok(otelite_core::api::ErrorRateByModel {
                model,
                total: total as usize,
                errors: errors as usize,
                error_rate,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute error_rate query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse error_rate results: {}", e))
        })?;

    Ok(rows)
}

/// Aggregated per-tool usage from tool-execution spans.
pub fn query_tool_usage(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    limit: usize,
) -> Result<Vec<otelite_core::api::ToolUsage>> {
    let mut where_clause = String::from("WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT
            COALESCE(
                json_extract(attributes, '$.\"gen_ai.tool.name\"'),
                json_extract(attributes, '$.\"tool.name\"'),
                json_extract(attributes, '$.\"tool_name\"'),
                CASE WHEN name LIKE 'claude_code.tool%' AND name != 'claude_code.tool' THEN name ELSE NULL END
            ) AS tool_name,
            COUNT(*) AS cnt,
            SUM(CASE WHEN status_code = 2 THEN 1 ELSE 0 END) AS errors,
            SUM(CASE WHEN status_code = 1 OR status_code = 0 THEN 1 ELSE 0 END) AS ok_cnt,
            COALESCE(SUM(end_time - start_time), 0) AS total_duration_ns
        FROM spans
        {where_clause}
        GROUP BY tool_name
        HAVING tool_name IS NOT NULL
        ORDER BY cnt DESC
        LIMIT ?"
    );

    params.push(Box::new(limit as i64));
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare tool_usage query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let tool_name: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            let errors: i64 = row.get(2)?;
            let ok_cnt: i64 = row.get(3)?;
            let total_ns: i64 = row.get(4)?;
            let total_ms = total_ns / 1_000_000;
            let avg_ms = if count > 0 {
                (total_ns as f64 / count as f64) / 1_000_000.0
            } else {
                0.0
            };
            Ok(otelite_core::api::ToolUsage {
                tool_name,
                count: count as usize,
                success_count: ok_cnt as usize,
                error_count: errors as usize,
                avg_duration_ms: avg_ms,
                total_duration_ms: total_ms,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute tool_usage query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse tool_usage results: {}", e))
        })?;

    Ok(rows)
}

/// Retry statistics across LLM spans.
pub fn query_retry_stats(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<otelite_core::api::RetryStats> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT
            COALESCE(
                CAST(json_extract(attributes, '$.\"attempt\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"retry_count\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"gen_ai.request.attempt\"') AS INTEGER),
                1
            ) AS attempt
        FROM spans
        {where_clause}"
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare retry_stats query: {}", e))
    })?;

    let attempts: Vec<i64> = stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute retry_stats query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse retry_stats results: {}", e))
        })?;

    let total_llm_calls = attempts.len();
    let mut retried_calls = 0usize;
    let mut extra_attempts = 0i64;
    for a in &attempts {
        let attempt = (*a).max(1);
        if attempt > 1 {
            retried_calls += 1;
            extra_attempts += attempt - 1;
        }
    }
    let retry_rate = if total_llm_calls > 0 {
        retried_calls as f64 / total_llm_calls as f64
    } else {
        0.0
    };

    Ok(otelite_core::api::RetryStats {
        total_llm_calls,
        retried_calls,
        extra_attempts: extra_attempts as usize,
        retry_rate,
    })
}

/// Aggregated retrieval / RAG statistics across retriever spans.
///
/// Retriever spans are identified by either:
/// - `openinference.span.kind = 'RETRIEVER'`, or
/// - presence of a `retrieval.query` attribute (fallback for non-OpenInference instrumentations).
///
/// OpenInference stores retrieved documents under `retrieval.documents` as a JSON
/// array of `{document.id, document.score, document.content, document.metadata}`.
/// Document count is taken from `json_array_length`, and the per-span top-1 score
/// is the `document.score` of the first element.
pub fn query_retrieval_stats(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    top_queries_limit: usize,
) -> Result<otelite_core::api::RetrievalStats> {
    let mut time_filter = String::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        time_filter.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        time_filter.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    // CTE: per-retrieval-span query, document count, and top-1 score.
    // Reused by both the summary and top-queries aggregations.
    let cte = format!(
        "WITH retrieval_spans AS (
            SELECT
                CAST(json_extract(attributes, '$.\"retrieval.query\"') AS TEXT) AS query,
                COALESCE(
                    json_array_length(json_extract(attributes, '$.\"retrieval.documents\"')),
                    0
                ) AS doc_count,
                CAST(json_extract(attributes, '$.\"retrieval.documents\"[0].\"document.score\"') AS REAL) AS top_score
            FROM spans
            WHERE (
                json_extract(attributes, '$.\"openinference.span.kind\"') = 'RETRIEVER'
                OR json_extract(attributes, '$.\"retrieval.query\"') IS NOT NULL
            )
            {time_filter}
        )"
    );

    // Summary query: totals plus averages. AVG(top_score) auto-ignores NULLs.
    let summary_sql = format!(
        "{cte}
         SELECT
             COUNT(*) AS total,
             COALESCE(AVG(CAST(doc_count AS REAL)), 0.0) AS avg_docs,
             AVG(top_score) AS avg_top_score
         FROM retrieval_spans"
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let (total_retrievals, avg_documents_per_query, avg_top_document_score) = conn
        .query_row(&summary_sql, param_refs.as_slice(), |row| {
            let total: i64 = row.get(0)?;
            let avg_docs: f64 = row.get(1)?;
            let avg_top_score: Option<f64> = row.get(2)?;
            Ok((total as usize, avg_docs, avg_top_score))
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to query retrieval summary: {}", e))
        })?;

    if total_retrievals == 0 {
        return Ok(otelite_core::api::RetrievalStats {
            total_retrievals: 0,
            avg_documents_per_query: 0.0,
            avg_top_document_score: None,
            top_queries: Vec::new(),
        });
    }

    // Top queries: group by query text, ordered by count desc.
    // The same time-filter params are bound a second time for this query.
    let top_sql = format!(
        "{cte}
         SELECT
             query,
             COUNT(*) AS cnt,
             COALESCE(AVG(CAST(doc_count AS REAL)), 0.0) AS avg_docs,
             AVG(top_score) AS avg_top_score
         FROM retrieval_spans
         WHERE query IS NOT NULL
         GROUP BY query
         ORDER BY cnt DESC
         LIMIT ?"
    );

    let mut top_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(params.len() + 1);
    if let Some(start) = start_time {
        top_params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        top_params.push(Box::new(end));
    }
    top_params.push(Box::new(top_queries_limit as i64));

    let top_param_refs: Vec<&dyn rusqlite::ToSql> = top_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&top_sql).map_err(|e| {
        StorageError::QueryError(format!(
            "Failed to prepare retrieval top_queries query: {}",
            e
        ))
    })?;

    let top_queries = stmt
        .query_map(top_param_refs.as_slice(), |row| {
            Ok(otelite_core::api::TopRetrievalQuery {
                query: row.get::<_, String>(0)?,
                count: row.get::<_, i64>(1)? as usize,
                avg_documents: row.get::<_, f64>(2)?,
                avg_top_score: row.get::<_, Option<f64>>(3)?,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!(
                "Failed to execute retrieval top_queries query: {}",
                e
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!(
                "Failed to parse retrieval top_queries results: {}",
                e
            ))
        })?;

    Ok(otelite_core::api::RetrievalStats {
        total_retrievals,
        avg_documents_per_query,
        avg_top_document_score,
        top_queries,
    })
}

/// Return up to 50 distinct resource attribute keys for the given signal table.
/// `signal` must be one of "logs", "spans", or "metrics".
pub fn distinct_resource_keys(conn: &Connection, signal: &str) -> Result<Vec<String>> {
    let table = match signal {
        "logs" => "logs",
        "spans" => "spans",
        "metrics" => "metrics",
        other => {
            return Err(StorageError::QueryError(format!(
                "Unknown signal type: {}",
                other
            )));
        },
    };

    let sql = format!(
        "SELECT DISTINCT je.key \
         FROM {table}, json_each(json_extract({table}.resource, '$.attributes')) je \
         WHERE {table}.resource IS NOT NULL \
         AND json_extract({table}.resource, '$.attributes') IS NOT NULL \
         LIMIT 50"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::QueryError(format!("Failed to prepare query: {}", e)))?;

    let keys = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| StorageError::QueryError(format!("Failed to execute query: {}", e)))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(keys)
}

/// Truncation rate (finish_reason = max_tokens / length) per model.
pub fn query_truncation_rate(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::TruncationRateByModel>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "SELECT
            {model} AS model,
            COUNT(*) AS total,
            SUM(CASE
                WHEN COALESCE(
                    json_extract(attributes, '$.\"gen_ai.response.finish_reason\"'),
                    CASE WHEN json_type(attributes, '$.\"gen_ai.response.finish_reasons\"') = 'array'
                         THEN json_extract(json_extract(attributes, '$.\"gen_ai.response.finish_reasons\"'), '$[0]')
                         ELSE NULL END
                ) IN ('max_tokens', 'length') THEN 1 ELSE 0 END) AS truncated
        FROM spans
        {where_clause}
        GROUP BY {model}
        ORDER BY total DESC",
        model = exprs.model,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare truncation_rate query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let total = row.get::<_, i64>(1)? as usize;
            let truncated = row.get::<_, i64>(2)? as usize;
            let rate = if total > 0 {
                truncated as f64 / total as f64
            } else {
                0.0
            };
            Ok(otelite_core::api::TruncationRateByModel {
                model: row.get::<_, Option<String>>(0)?,
                total,
                truncated,
                rate,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute truncation_rate query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse truncation_rate results: {}", e))
        })?;

    Ok(rows)
}

/// Cache token hit rate per model.
pub fn query_cache_hit_rate(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::CacheHitRateByModel>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "SELECT
            {model} AS model,
            SUM(COALESCE({input}, 0)) AS input_tokens,
            SUM(COALESCE({cache_read}, 0)) AS cache_read_tokens,
            SUM(COALESCE({cache_creation}, 0)) AS cache_creation_tokens
        FROM spans
        {where_clause}
        GROUP BY {model}
        ORDER BY cache_read_tokens DESC",
        model = exprs.model,
        input = exprs.input,
        cache_read = exprs.cache_read,
        cache_creation = exprs.cache_creation,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare cache_hit_rate query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let input = row.get::<_, i64>(1)? as u64;
            let cache_read = row.get::<_, i64>(2)? as u64;
            let cache_creation = row.get::<_, i64>(3)? as u64;
            let denominator = cache_read + input;
            let hit_rate = if denominator > 0 {
                Some(cache_read as f64 / denominator as f64)
            } else {
                None
            };
            Ok(otelite_core::api::CacheHitRateByModel {
                model: row.get::<_, Option<String>>(0)?,
                total_input_tokens: input,
                total_cache_read_tokens: cache_read,
                total_cache_creation_tokens: cache_creation,
                hit_rate,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute cache_hit_rate query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse cache_hit_rate results: {}", e))
        })?;

    Ok(rows)
}

/// Distribution of request parameter settings (temperature, max_tokens).
pub fn query_request_param_profile(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<otelite_core::api::RequestParamProfile> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    // Temperature distribution
    let temp_sql = format!(
        "SELECT
            ROUND(CAST(json_extract(attributes, '$.\"gen_ai.request.temperature\"') AS REAL), 2) AS temperature,
            COUNT(*) AS cnt
        FROM spans
        {where_clause}
        GROUP BY temperature
        ORDER BY cnt DESC",
    );
    let mut temp_stmt = conn.prepare(&temp_sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare temperature query: {}", e))
    })?;
    let temperature_buckets = temp_stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::TemperatureBucket {
                temperature: row.get::<_, Option<f64>>(0)?,
                count: row.get::<_, i64>(1)? as usize,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute temperature query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse temperature results: {}", e))
        })?;

    // max_tokens distribution
    let max_sql = format!(
        "SELECT
            CAST(json_extract(attributes, '$.\"gen_ai.request.max_tokens\"') AS INTEGER) AS max_tokens,
            COUNT(*) AS cnt
        FROM spans
        {where_clause}
        GROUP BY max_tokens
        ORDER BY cnt DESC",
    );
    let mut max_stmt = conn.prepare(&max_sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare max_tokens query: {}", e))
    })?;
    let max_tokens_buckets = max_stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::MaxTokensBucket {
                max_tokens: row.get::<_, Option<i64>>(0)?,
                count: row.get::<_, i64>(1)? as usize,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute max_tokens query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse max_tokens results: {}", e))
        })?;

    Ok(otelite_core::api::RequestParamProfile {
        temperature_buckets,
        max_tokens_buckets,
    })
}

/// Turn-count distribution across conversations with a known conversation_id.
pub fn query_conversation_depth(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<otelite_core::api::ConversationDepthStats> {
    let exprs = token_exprs();
    let conv_id = "json_extract(attributes, '$.\"gen_ai.conversation.id\"')";
    let mut where_clause = format!("WHERE {} AND {} IS NOT NULL", exprs.llm_span_guard, conv_id);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT COUNT(*) AS turns
        FROM spans
        {where_clause}
        GROUP BY {conv_id}",
        conv_id = conv_id,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare conversation_depth query: {}", e))
    })?;

    let mut turn_counts: Vec<i64> = stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute conversation_depth query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse conversation_depth results: {}", e))
        })?;

    if turn_counts.is_empty() {
        return Ok(otelite_core::api::ConversationDepthStats {
            total_conversations: 0,
            avg_turns: 0.0,
            p50_turns: 0,
            p95_turns: 0,
            p99_turns: 0,
        });
    }

    turn_counts.sort_unstable();
    let n = turn_counts.len();
    let avg = turn_counts.iter().sum::<i64>() as f64 / n as f64;

    Ok(otelite_core::api::ConversationDepthStats {
        total_conversations: n,
        avg_turns: avg,
        p50_turns: percentile(&turn_counts, 0.50),
        p95_turns: percentile(&turn_counts, 0.95),
        p99_turns: percentile(&turn_counts, 0.99),
    })
}

/// LLM span latency per time bucket grouped by model.
///
/// Fetches raw (bucket, model, duration_ms, ttft_ms, is_error) rows then aggregates in Rust
/// so that p95 can be computed without a SQLite percentile extension.
#[allow(clippy::too_many_arguments)]
pub fn query_latency_series(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    bucket_secs: u64,
    model: Option<&str>,
    all_spans: bool,
) -> Result<Vec<otelite_core::api::LatencySeriesPoint>> {
    let exprs = token_exprs();
    let bucket_ns = bucket_secs as i64 * 1_000_000_000;
    // In all_spans mode group by span name; otherwise group by model.
    let group_col = if all_spans {
        "name".to_string()
    } else {
        exprs.model.clone()
    };
    let mut where_clause = if all_spans {
        "WHERE 1=1".to_string()
    } else {
        format!("WHERE {}", exprs.request_span_guard)
    };
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if !all_spans {
        if let Some(m) = model {
            where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
            params.push(Box::new(m.to_string()));
        }
    }

    let sql = format!(
        "SELECT
            (start_time / {bucket_ns}) * {bucket_ns} AS bucket,
            {group_col} AS group_label,
            (end_time - start_time) / 1000000 AS duration_ms,
            json_extract(attributes, '$.\"gen_ai.server.time_to_first_token\"') AS otel_ttft_secs,
            json_extract(attributes, '$.\"llm.time_to_first_token\"') AS llm_ttft_secs,
            json_extract(attributes, '$.\"ttft_ms\"') AS custom_ttft_ms,
            CASE WHEN status_code = 2 THEN 1 ELSE 0 END AS is_error
        FROM spans
        {where_clause}
        ORDER BY bucket ASC",
        bucket_ns = bucket_ns,
        group_col = group_col,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare latency_series query: {}", e))
    })?;

    struct RawRow {
        bucket: i64,
        label: Option<String>,
        duration_ms: i64,
        otel_ttft_secs: Option<String>,
        llm_ttft_secs: Option<String>,
        custom_ttft_ms: Option<String>,
        is_error: bool,
    }

    let raw: Vec<RawRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(RawRow {
                bucket: row.get::<_, i64>(0)?,
                label: row.get::<_, Option<String>>(1)?,
                duration_ms: row.get::<_, i64>(2)?,
                otel_ttft_secs: row.get::<_, Option<String>>(3)?,
                llm_ttft_secs: row.get::<_, Option<String>>(4)?,
                custom_ttft_ms: row.get::<_, Option<String>>(5)?,
                is_error: row.get::<_, i64>(6)? != 0,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute latency_series query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse latency_series results: {}", e))
        })?;

    type BucketKey = (i64, Option<String>);
    type BucketAccum = (Vec<i64>, TtftAccum, usize);

    let mut groups: std::collections::BTreeMap<BucketKey, BucketAccum> =
        std::collections::BTreeMap::new();
    for r in raw {
        let entry = groups.entry((r.bucket, r.label)).or_default();
        entry.0.push(r.duration_ms);
        entry.1.record(
            r.duration_ms,
            normalized_ttft_ms(
                r.otel_ttft_secs.as_deref(),
                r.llm_ttft_secs.as_deref(),
                r.custom_ttft_ms.as_deref(),
            ),
        );
        if r.is_error {
            entry.2 += 1;
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for ((bucket, label), (mut durations, ttft, error_count)) in groups {
        if durations.is_empty() {
            continue;
        }
        let ttft_degenerate = ttft.is_degenerate();
        let TtftAccum {
            values_ms: mut ttfts,
            invalid_count: ttft_invalid_count,
            degenerate_count: ttft_degenerate_count,
        } = ttft;
        durations.sort_unstable();
        ttfts.sort_unstable();

        let count = durations.len();
        let min_ms = durations[0];
        let max_ms = durations[count - 1];
        let avg_ms = durations.iter().sum::<i64>() as f64 / count as f64;
        let p95_ms = percentile(&durations, 0.95);

        let (avg_ttft_ms, p95_ttft_ms) = if ttfts.is_empty() {
            (None, None)
        } else {
            let avg = ttfts.iter().sum::<i64>() as f64 / ttfts.len() as f64;
            let p95 = percentile(&ttfts, 0.95);
            (Some(avg), Some(p95))
        };

        let (model, name) = if all_spans {
            (None, label)
        } else {
            (label, None)
        };

        out.push(otelite_core::api::LatencySeriesPoint {
            timestamp: bucket,
            model,
            name,
            count,
            error_count,
            min_ms,
            avg_ms,
            p95_ms,
            max_ms,
            avg_ttft_ms,
            p95_ttft_ms,
            ttft_count: ttfts.len(),
            ttft_invalid_count,
            ttft_degenerate_count,
            ttft_degenerate,
        });
    }

    Ok(out)
}

/// Call volume per time bucket grouped by model (LLM mode) or span name (all-spans mode).
pub fn query_calls_series(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    bucket_secs: u64,
    all_spans: bool,
) -> Result<Vec<otelite_core::api::CallsSeriesPoint>> {
    let exprs = token_exprs();
    let bucket_ns = bucket_secs as i64 * 1_000_000_000;
    let group_col = if all_spans {
        "name".to_string()
    } else {
        exprs.model.clone()
    };
    let mut where_clause = if all_spans {
        "WHERE 1=1".to_string()
    } else {
        format!("WHERE {}", exprs.request_span_guard)
    };
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT
            (start_time / {bucket_ns}) * {bucket_ns} AS bucket,
            {group_col} AS label,
            COUNT(*) AS requests
        FROM spans
        {where_clause}
        GROUP BY bucket, {group_col}
        ORDER BY bucket ASC",
        bucket_ns = bucket_ns,
        group_col = group_col,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare calls_series query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let label: Option<String> = row.get(1)?;
            let requests = row.get::<_, i64>(2)? as usize;
            let (model, name) = if all_spans {
                (None, label)
            } else {
                (label, None)
            };
            Ok(otelite_core::api::CallsSeriesPoint {
                timestamp: row.get::<_, i64>(0)?,
                model,
                name,
                requests,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute calls_series query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse calls_series results: {}", e))
        })?;

    Ok(rows)
}

/// LLM latency broken down by input-token context size bin × model.
///
/// Bins: 0–1K, 1K–10K, 10K–50K, 50K–100K, 100K+
/// p95 is computed in Rust over raw rows per bin.
pub fn query_latency_by_context(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::LatencyByContextBin>> {
    let exprs = token_exprs();
    let mut where_clause = format!(
        "WHERE {} AND ({}) IS NOT NULL AND ({}) > 0",
        exprs.llm_span_guard, exprs.input, exprs.input
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "SELECT
            {model} AS model,
            COALESCE({input}, 0) AS input_tokens,
            (end_time - start_time) / 1000000 AS duration_ms,
            json_extract(attributes, '$.\"gen_ai.server.time_to_first_token\"') AS otel_ttft_secs,
            json_extract(attributes, '$.\"llm.time_to_first_token\"') AS llm_ttft_secs,
            json_extract(attributes, '$.\"ttft_ms\"') AS custom_ttft_ms
        FROM spans
        {where_clause}",
        model = exprs.model,
        input = exprs.input,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare latency_by_context query: {}", e))
    })?;

    struct RawRow {
        model: Option<String>,
        input_tokens: i64,
        duration_ms: i64,
        otel_ttft_secs: Option<String>,
        llm_ttft_secs: Option<String>,
        custom_ttft_ms: Option<String>,
    }

    let raw: Vec<RawRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(RawRow {
                model: row.get(0)?,
                input_tokens: row.get::<_, i64>(1)?,
                duration_ms: row.get::<_, i64>(2)?,
                otel_ttft_secs: row.get::<_, Option<String>>(3)?,
                llm_ttft_secs: row.get::<_, Option<String>>(4)?,
                custom_ttft_ms: row.get::<_, Option<String>>(5)?,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute latency_by_context query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse latency_by_context results: {}", e))
        })?;

    const BINS: &[(u64, u64, &str)] = &[
        (0, 1_000, "0–1K"),
        (1_000, 10_000, "1K–10K"),
        (10_000, 50_000, "10K–50K"),
        (50_000, 100_000, "50K–100K"),
        (100_000, u64::MAX, "100K+"),
    ];

    type BinKey = (usize, Option<String>); // (bin_index, model)
    type BinAccum = (Vec<i64>, TtftAccum); // (durations, ttfts)

    let mut groups: std::collections::BTreeMap<BinKey, BinAccum> =
        std::collections::BTreeMap::new();

    for r in raw {
        let bin_idx = BINS
            .iter()
            .position(|(lo, hi, _)| {
                let t = r.input_tokens as u64;
                t >= *lo && t < *hi
            })
            .unwrap_or(BINS.len() - 1);
        let entry = groups.entry((bin_idx, r.model)).or_default();
        entry.0.push(r.duration_ms);
        entry.1.record(
            r.duration_ms,
            normalized_ttft_ms(
                r.otel_ttft_secs.as_deref(),
                r.llm_ttft_secs.as_deref(),
                r.custom_ttft_ms.as_deref(),
            ),
        );
    }

    let mut out = Vec::with_capacity(groups.len());
    for ((bin_idx, model), (mut durations, ttft)) in groups {
        if durations.is_empty() {
            continue;
        }
        let ttft_degenerate = ttft.is_degenerate();
        let TtftAccum {
            values_ms: mut ttfts,
            invalid_count: ttft_invalid_count,
            degenerate_count: ttft_degenerate_count,
        } = ttft;
        durations.sort_unstable();
        ttfts.sort_unstable();

        let (lo, hi, label) = BINS[bin_idx];
        let count = durations.len();
        let avg_ms = durations.iter().sum::<i64>() as f64 / count as f64;
        let p95_ms = percentile(&durations, 0.95);
        let max_ms = durations[count - 1];
        let avg_ttft_ms = if ttfts.is_empty() {
            None
        } else {
            Some(ttfts.iter().sum::<i64>() as f64 / ttfts.len() as f64)
        };

        out.push(otelite_core::api::LatencyByContextBin {
            bin: label.to_string(),
            min_tokens: lo,
            max_tokens: hi,
            model,
            count,
            avg_ms,
            p95_ms,
            max_ms,
            avg_ttft_ms,
            ttft_count: ttfts.len(),
            ttft_invalid_count,
            ttft_degenerate_count,
            ttft_degenerate,
        });
    }

    // Sort by bin index so output is always 0–1K → 1K–10K → …
    out.sort_by_key(|r| {
        BINS.iter()
            .position(|(_, _, lbl)| lbl == &r.bin.as_str())
            .unwrap_or(usize::MAX)
    });

    Ok(out)
}

/// Per-(model, error_type) breakdown of error spans, with bucketing into actionable categories.
///
/// Spans are errors when `status_code = 2`. The error-type label is derived by COALESCE:
///   1. `error.type` (OTel standard)
///   2. `exception.type`
///   3. `http.response.status_code`
///   4. `http.status_code` (legacy)
///   5. literal "unknown"
///
/// Bucketing is heuristic — different SDKs use different labels. Raw `error_type` is also
/// returned so callers can inspect unparsed values.
pub fn query_error_types(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    model: Option<&str>,
) -> Result<Vec<otelite_core::api::ErrorTypeBreakdown>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {} AND status_code = 2", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }
    if let Some(m) = model {
        where_clause.push_str(&format!(" AND ({}) = ?", exprs.model));
        params.push(Box::new(m.to_string()));
    }

    let sql = format!(
        "WITH error_spans AS (
            SELECT
                {model} AS model,
                COALESCE(
                    json_extract(attributes, '$.\"error.type\"'),
                    json_extract(attributes, '$.\"exception.type\"'),
                    CAST(json_extract(attributes, '$.\"http.response.status_code\"') AS TEXT),
                    CAST(json_extract(attributes, '$.\"http.status_code\"') AS TEXT),
                    'unknown'
                ) AS error_type
            FROM spans
            {where_clause}
        )
        SELECT model, error_type,
            CASE
                WHEN LOWER(error_type) LIKE '%rate%limit%'
                  OR error_type LIKE '%429%'
                  OR LOWER(error_type) LIKE '%throttl%'         THEN 'rate_limit'
                WHEN LOWER(error_type) LIKE '%timeout%'
                  OR error_type IN ('408', '504')
                  OR LOWER(error_type) LIKE '%deadline%'        THEN 'timeout'
                WHEN LOWER(error_type) LIKE '%context%length%'
                  OR LOWER(error_type) LIKE '%context%window%'
                  OR LOWER(error_type) LIKE '%max%token%'
                  OR LOWER(error_type) LIKE '%too%long%'        THEN 'context_length'
                WHEN LOWER(error_type) LIKE '%content_filter%'
                  OR LOWER(error_type) LIKE '%moderation%'
                  OR LOWER(error_type) LIKE '%content_policy%'
                  OR LOWER(error_type) LIKE '%safety%'          THEN 'content_filter'
                WHEN error_type IN ('401', '403')
                  OR LOWER(error_type) LIKE '%unauthor%'
                  OR LOWER(error_type) LIKE '%forbid%'
                  OR LOWER(error_type) LIKE '%invalid%api%key%' THEN 'auth'
                WHEN CAST(error_type AS INTEGER) BETWEEN 500 AND 599 THEN 'server_error'
                ELSE 'unknown'
            END AS bucket,
            COUNT(*) AS count
        FROM error_spans
        GROUP BY model, error_type, bucket
        ORDER BY count DESC",
        model = exprs.model,
        where_clause = where_clause,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare error_types query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(otelite_core::api::ErrorTypeBreakdown {
                model: row.get::<_, Option<String>>(0)?,
                error_type: row.get::<_, String>(1)?,
                bucket: row.get::<_, String>(2)?,
                count: row.get::<_, i64>(3)? as usize,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute error_types query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse error_types results: {}", e))
        })?;

    Ok(rows)
}

/// All observed (request_model, response_model) pairs with a `differs` flag.
///
/// Returns ALL pairs including matching ones — callers filter if they only want drifted pairs.
/// `differs` is true when both fields are non-null and differ, indicating silent provider rerouting.
pub fn query_model_drift(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<Vec<otelite_core::api::ModelDriftPair>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(start) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(start));
    }
    if let Some(end) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(end));
    }

    let sql = format!(
        "SELECT
            json_extract(attributes, '$.\"gen_ai.request.model\"') AS request_model,
            json_extract(attributes, '$.\"gen_ai.response.model\"') AS response_model,
            COUNT(*) AS count
        FROM spans
        {where_clause}
        GROUP BY request_model, response_model
        HAVING request_model IS NOT NULL OR response_model IS NOT NULL
        ORDER BY count DESC",
        where_clause = where_clause,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare model_drift query: {}", e))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let request_model: Option<String> = row.get(0)?;
            let response_model: Option<String> = row.get(1)?;
            let count: i64 = row.get(2)?;
            let differs = request_model.is_some()
                && response_model.is_some()
                && request_model != response_model;
            Ok(otelite_core::api::ModelDriftPair {
                request_model,
                response_model,
                count: count as usize,
                differs,
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute model_drift query: {}", e))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse model_drift results: {}", e))
        })?;

    Ok(rows)
}

// ── New analytics queries ─────────────────────────────────────────────────────

/// Approval/rejection summary for claude_code.tool.blocked_on_user spans.
pub fn query_tool_approvals(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<otelite_core::api::ToolApprovalStats> {
    let mut where_clause = String::from("WHERE name = 'claude_code.tool.blocked_on_user'");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(s));
    }
    if let Some(e) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(e));
    }

    let sql = format!(
        "SELECT
            json_extract(attributes, '$.decision') AS decision,
            json_extract(attributes, '$.source')   AS source,
            json_extract(attributes, '$.tool_name') AS tool_name,
            COUNT(*) AS cnt
         FROM spans
         {where_clause}
         GROUP BY decision, source, tool_name
         ORDER BY cnt DESC"
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare tool_approvals query: {e}"))
    })?;

    struct Row {
        decision: Option<String>,
        source: Option<String>,
        tool_name: Option<String>,
        cnt: usize,
    }
    let rows: Vec<Row> = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(Row {
                decision: r.get(0)?,
                source: r.get(1)?,
                tool_name: r.get(2)?,
                cnt: r.get::<_, i64>(3)? as usize,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to execute tool_approvals: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse tool_approvals: {e}")))?;

    let mut stats = otelite_core::api::ToolApprovalStats::default();
    let mut rejected_map: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for row in rows {
        let decision = row.decision.as_deref().unwrap_or("unknown");
        let source = row.source.as_deref().unwrap_or("unknown");
        match decision {
            "accept" if source == "config" => stats.auto_accepted += row.cnt,
            "accept" => stats.user_accepted += row.cnt,
            "reject" => {
                stats.rejected += row.cnt;
                if let Some(t) = &row.tool_name {
                    *rejected_map.entry(t.clone()).or_default() += row.cnt;
                }
            },
            _ => stats.unknown += row.cnt,
        }
        stats.total += row.cnt;
    }

    let mut top: Vec<_> = rejected_map
        .into_iter()
        .map(|(tool_name, count)| otelite_core::api::ToolApprovalEntry { tool_name, count })
        .collect();
    top.sort_by_key(|a| std::cmp::Reverse(a.count));
    top.truncate(10);
    stats.top_rejected = top;
    Ok(stats)
}

/// Distribution of stop_reason values across LLM spans.
pub fn query_stop_reasons(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<Vec<otelite_core::api::StopReasonCount>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(s));
    }
    if let Some(e) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(e));
    }

    let sql = format!(
        "SELECT
            COALESCE(
                json_extract(attributes, '$.stop_reason'),
                json_extract(attributes, '$.\"gen_ai.response.finish_reason\"'),
                '(none)'
            ) AS reason,
            COUNT(*) AS cnt
         FROM spans
         {where_clause}
         GROUP BY reason
         ORDER BY cnt DESC"
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare stop_reasons query: {e}"))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(otelite_core::api::StopReasonCount {
                reason: r.get(0)?,
                count: r.get::<_, i64>(1)? as usize,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to execute stop_reasons: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse stop_reasons: {e}")))?;
    Ok(rows)
}

/// Token usage broken down by llm_request.context attribute.
pub fn query_context_type_split(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<Vec<otelite_core::api::ContextTypeSplit>> {
    let exprs = token_exprs();
    let mut where_clause = format!("WHERE {}", exprs.llm_span_guard);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(s));
    }
    if let Some(e) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(e));
    }

    let sql = format!(
        "SELECT
            COALESCE(json_extract(attributes, '$.\"llm_request.context\"'), '(unknown)') AS context,
            COUNT(*) AS calls,
            COALESCE(SUM({input}), 0)  AS input_tokens,
            COALESCE(SUM({output}), 0) AS output_tokens,
            AVG((end_time - start_time) / 1000000.0) AS avg_ms
         FROM spans
         {where_clause}
         GROUP BY context
         ORDER BY calls DESC",
        input = exprs.input,
        output = exprs.output,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare context_type_split query: {e}"))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(otelite_core::api::ContextTypeSplit {
                context: r.get(0)?,
                calls: r.get::<_, i64>(1)? as usize,
                input_tokens: r.get::<_, i64>(2)? as u64,
                output_tokens: r.get::<_, i64>(3)? as u64,
                avg_ms: r.get::<_, f64>(4).unwrap_or(0.0),
            })
        })
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to execute context_type_split: {e}"))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            StorageError::QueryError(format!("Failed to parse context_type_split: {e}"))
        })?;
    Ok(rows)
}

/// Top error messages from failed tool executions.
pub fn query_tool_errors(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
    limit: usize,
) -> Result<Vec<otelite_core::api::ToolErrorEntry>> {
    let mut where_clause = String::from(
        "WHERE name = 'claude_code.tool.execution'
           AND json_extract(attributes, '$.success') = 'false'
           AND json_extract(attributes, '$.error') IS NOT NULL",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = start_time {
        where_clause.push_str(" AND start_time >= ?");
        params.push(Box::new(s));
    }
    if let Some(e) = end_time {
        where_clause.push_str(" AND end_time <= ?");
        params.push(Box::new(e));
    }

    // Truncate long error messages at 120 chars for grouping
    let sql = format!(
        "SELECT
            COALESCE(json_extract(attributes, '$.tool_name'), '(unknown)') AS tool_name,
            SUBSTR(json_extract(attributes, '$.error'), 1, 120)           AS error_msg,
            COUNT(*) AS cnt
         FROM spans
         {where_clause}
         GROUP BY tool_name, error_msg
         ORDER BY cnt DESC
         LIMIT ?"
    );

    params.push(Box::new(limit as i64));
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare tool_errors query: {e}"))
    })?;

    let rows = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(otelite_core::api::ToolErrorEntry {
                tool_name: r.get(0)?,
                error_message: r.get(1)?,
                count: r.get::<_, i64>(2)? as usize,
            })
        })
        .map_err(|e| StorageError::QueryError(format!("Failed to execute tool_errors: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryError(format!("Failed to parse tool_errors: {e}")))?;
    Ok(rows)
}

/// Hour-of-day activity buckets (0–23, UTC).
pub fn query_hour_of_day(
    conn: &Connection,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> Result<Vec<otelite_core::api::HourOfDayBucket>> {
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    let mut time_filter = String::new();
    if let Some(s) = start_time {
        time_filter.push_str(" AND start_time >= ?");
        params.push(Box::new(s));
    }
    if let Some(e) = end_time {
        time_filter.push_str(" AND end_time <= ?");
        params.push(Box::new(e));
    }

    // Duplicate params for the two sub-queries (SQLite doesn't support named params easily here)
    let llm_filter = format!("WHERE name = 'claude_code.llm_request'{}", time_filter);
    let tool_filter = format!("WHERE name = 'claude_code.tool.execution'{}", time_filter);

    // Build hour table by merging two separate queries in Rust — simpler than a FULL OUTER JOIN
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let llm_sql = format!(
        "SELECT CAST(strftime('%H', start_time/1000000000, 'unixepoch') AS INTEGER) AS h, COUNT(*) AS cnt
         FROM spans {llm_filter} GROUP BY h"
    );
    let tool_sql = format!(
        "SELECT CAST(strftime('%H', start_time/1000000000, 'unixepoch') AS INTEGER) AS h, COUNT(*) AS cnt
         FROM spans {tool_filter} GROUP BY h"
    );

    let mut llm_by_hour = [0usize; 24];
    let mut tool_by_hour = [0usize; 24];

    {
        let mut stmt = conn.prepare(&llm_sql).map_err(|e| {
            StorageError::QueryError(format!("Failed to prepare hour_of_day llm query: {e}"))
        })?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| StorageError::QueryError(format!("{e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::QueryError(format!("{e}")))?;
        for (h, cnt) in rows {
            if (0..24).contains(&(h as usize)) {
                llm_by_hour[h as usize] = cnt as usize;
            }
        }
    }
    {
        let mut stmt = conn.prepare(&tool_sql).map_err(|e| {
            StorageError::QueryError(format!("Failed to prepare hour_of_day tool query: {e}"))
        })?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| StorageError::QueryError(format!("{e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::QueryError(format!("{e}")))?;
        for (h, cnt) in rows {
            if (0..24).contains(&(h as usize)) {
                tool_by_hour[h as usize] = cnt as usize;
            }
        }
    }

    Ok((0u8..24u8)
        .map(|h| otelite_core::api::HourOfDayBucket {
            hour: h,
            llm_calls: llm_by_hour[h as usize],
            tool_calls: tool_by_hour[h as usize],
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::schema;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_query_logs_empty() {
        let conn = setup_test_db();
        let params = QueryParams::default();
        let logs = query_logs(&conn, &params).unwrap();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_parse_log_row_tolerates_malformed_json() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO logs (
                timestamp, severity_number, body, attributes, resource
            ) VALUES (100, 9, 'corrupt log', '{', '[')",
            [],
        )
        .unwrap();

        let log = conn
            .query_row("SELECT * FROM logs", [], parse_log_row)
            .unwrap();

        assert!(log.attributes.is_empty());
        assert_eq!(log.resource, None);
    }

    #[test]
    fn test_parse_span_row_tolerates_malformed_json() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO spans (
                trace_id, span_id, name, kind, start_time, end_time,
                attributes, events, resource, status_code
            ) VALUES ('trace', 'span', 'corrupt span', 0, 100, 200, '{', '[', '{', 1)",
            [],
        )
        .unwrap();

        let span = conn
            .query_row("SELECT * FROM spans", [], parse_span_row)
            .unwrap();

        assert!(span.attributes.is_empty());
        assert!(span.events.is_empty());
        assert_eq!(span.resource, None);
    }

    #[test]
    fn test_parse_metric_row_tolerates_malformed_json() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO metrics (
                name, metric_type, timestamp, value_histogram, attributes, resource
            ) VALUES ('corrupt.histogram', 2, 100, '{', '{', '[')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO metrics (
                name, metric_type, timestamp, value_summary, attributes, resource
            ) VALUES ('corrupt.summary', 3, 200, '[', '{', '[')",
            [],
        )
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT * FROM metrics ORDER BY timestamp")
            .unwrap();
        let metrics = stmt
            .query_map([], parse_metric_row)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(metrics.iter().all(|metric| metric.attributes.is_empty()));
        assert!(metrics.iter().all(|metric| metric.resource.is_none()));
        assert!(matches!(
            metrics[0].metric_type,
            otelite_core::telemetry::metric::MetricType::Histogram {
                count: 0,
                sum: 0.0,
                ref buckets
            } if buckets.is_empty()
        ));
        assert!(matches!(
            metrics[1].metric_type,
            otelite_core::telemetry::metric::MetricType::Summary {
                count: 0,
                sum: 0.0,
                ref quantiles
            } if quantiles.is_empty()
        ));
    }

    #[test]
    fn test_get_stats_empty() {
        let conn = setup_test_db();
        let stats = get_stats(&conn).unwrap();
        assert_eq!(stats.log_count, 0);
        assert_eq!(stats.span_count, 0);
        assert_eq!(stats.metric_count, 0);
    }

    #[test]
    fn test_field_to_sql_for_attribute_field() {
        let sql = field_to_sql("logs", "gen_ai.system").unwrap();
        assert_eq!(sql, "json_extract(attributes, '$.\"gen_ai.system\"')");
    }

    #[test]
    fn test_field_to_sql_for_explicit_attribute_prefix() {
        let sql = field_to_sql("logs", "attributes.http.method").unwrap();
        assert_eq!(sql, "json_extract(attributes, '$.\"http.method\"')");
    }

    #[test]
    fn test_field_to_sql_for_resource_prefix() {
        let sql = field_to_sql("logs", "resource.service.name").unwrap();
        assert_eq!(
            sql,
            "json_extract(resource, '$.attributes.\"service.name\"')"
        );
    }

    #[test]
    fn test_json_key_accessor_quotes_dotted_keys() {
        assert_eq!(json_key_accessor("service.name"), ".\"service.name\"");
    }

    #[test]
    fn test_predicate_to_sql_for_attribute_equality() {
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let sql = predicate_to_sql(
            "logs",
            &QueryPredicate {
                field: "gen_ai.system".to_string(),
                operator: Operator::Equal,
                value: QueryValue::String("anthropic".to_string()),
            },
            &mut params,
        )
        .unwrap();

        assert_eq!(sql, "json_extract(attributes, '$.\"gen_ai.system\"') = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_predicate_to_sql_for_resource_equality() {
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let sql = predicate_to_sql(
            "logs",
            &QueryPredicate {
                field: "resource.service.name".to_string(),
                operator: Operator::Equal,
                value: QueryValue::String("gateway".to_string()),
            },
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "json_extract(resource, '$.attributes.\"service.name\"') = ?"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_span_duration_predicate_requires_duration_value() {
        let mut params = Vec::new();
        let err = predicate_to_sql(
            "spans",
            &QueryPredicate {
                field: "duration".to_string(),
                operator: Operator::GreaterThan,
                value: QueryValue::Number(100.0),
            },
            &mut params,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("requires a duration value like 500ms"));
    }

    #[test]
    fn test_query_logs_with_structured_attribute_and_resource_predicates() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO logs (
                timestamp, observed_timestamp, trace_id, span_id,
                severity_number, severity_text, body, attributes, resource, scope
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                1000_i64,
                1000_i64,
                "trace-a",
                "span-a",
                SeverityLevel::Info.to_i32(),
                "INFO",
                "matching log body",
                r#"{"gen_ai.system":"anthropic"}"#,
                r#"{"attributes":{"service.name":"gateway"}}"#,
                "{}",
            ],
        )
        .unwrap();

        let params = QueryParams {
            predicates: vec![
                QueryPredicate {
                    field: "gen_ai.system".to_string(),
                    operator: Operator::Equal,
                    value: QueryValue::String("anthropic".to_string()),
                },
                QueryPredicate {
                    field: "resource.service.name".to_string(),
                    operator: Operator::Equal,
                    value: QueryValue::String("gateway".to_string()),
                },
            ],
            ..Default::default()
        };

        let attr_match: Option<String> = conn
            .query_row(
                "SELECT json_extract(attributes, '$.\"gen_ai.system\"') FROM logs WHERE timestamp = 1000",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let resource_match: Option<String> = conn
            .query_row(
                "SELECT json_extract(resource, '$.attributes.\"service.name\"') FROM logs WHERE timestamp = 1000",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attr_match.as_deref(), Some("anthropic"));
        assert_eq!(resource_match.as_deref(), Some("gateway"));

        let logs = query_logs(&conn, &params).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].body, "matching log body");
    }

    static SPAN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn next_id() -> String {
        let n = SPAN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("id-{n}")
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_llm_span(
        conn: &Connection,
        model: &str,
        input: i64,
        output: i64,
        stop_reason: Option<&str>,
        context: Option<&str>,
    ) {
        let attrs = serde_json::json!({
            "model": model,
            "input_tokens": input,
            "output_tokens": output,
            "stop_reason": stop_reason,
            "llm_request.context": context,
        });
        conn.execute(
            "INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, resource, events, links, scope)
             VALUES (?, ?, 'claude_code.llm_request', 0, 1000000000, 2000000000, ?, '{}', '[]', '[]', '{}')",
            rusqlite::params![next_id(), next_id(), attrs.to_string()],
        ).unwrap();
    }

    fn insert_tool_decision(conn: &Connection, decision: &str, source: &str, tool_name: &str) {
        let attrs = serde_json::json!({
            "decision": decision,
            "source": source,
            "tool_name": tool_name,
        });
        conn.execute(
            "INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, resource, events, links, scope)
             VALUES (?, ?, 'claude_code.tool.blocked_on_user', 0, 1000000000, 2000000000, ?, '{}', '[]', '[]', '{}')",
            rusqlite::params![next_id(), next_id(), attrs.to_string()],
        ).unwrap();
    }

    fn insert_failed_tool(conn: &Connection, tool_name: &str, error: &str) {
        let attrs = serde_json::json!({
            "tool_name": tool_name,
            "success": "false",
            "error": error,
        });
        conn.execute(
            "INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, resource, events, links, scope)
             VALUES (?, ?, 'claude_code.tool.execution', 0, 1000000000, 2000000000, ?, '{}', '[]', '[]', '{}')",
            rusqlite::params![next_id(), next_id(), attrs.to_string()],
        ).unwrap();
    }

    #[test]
    fn test_query_tool_approvals_empty() {
        let conn = setup_test_db();
        let stats = query_tool_approvals(&conn, None, None).unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.auto_accepted, 0);
        assert_eq!(stats.rejected, 0);
    }

    #[test]
    fn test_query_tool_approvals_counts() {
        let conn = setup_test_db();
        insert_tool_decision(&conn, "accept", "config", "Bash");
        insert_tool_decision(&conn, "accept", "config", "Read");
        insert_tool_decision(&conn, "accept", "user", "Write");
        insert_tool_decision(&conn, "reject", "user", "Bash");

        let stats = query_tool_approvals(&conn, None, None).unwrap();
        assert_eq!(stats.total, 4);
        assert_eq!(stats.auto_accepted, 2); // accept + source=config
        assert_eq!(stats.user_accepted, 1); // accept + source=user
        assert_eq!(stats.rejected, 1);
        assert_eq!(stats.unknown, 0);
        assert_eq!(stats.top_rejected.len(), 1);
        assert_eq!(stats.top_rejected[0].tool_name, "Bash");
    }

    #[test]
    fn test_query_stop_reasons_empty() {
        let conn = setup_test_db();
        let rows = query_stop_reasons(&conn, None, None).unwrap();
        // No LLM spans → empty vec (no stop_reason attribute, no groupable rows)
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn test_query_stop_reasons_with_data() {
        let conn = setup_test_db();
        insert_llm_span(&conn, "claude-sonnet", 100, 50, Some("tool_use"), None);
        insert_llm_span(&conn, "claude-sonnet", 200, 80, Some("end_turn"), None);
        insert_llm_span(&conn, "claude-sonnet", 150, 60, Some("tool_use"), None);

        let rows = query_stop_reasons(&conn, None, None).unwrap();
        let tool_use = rows
            .iter()
            .find(|r| r.reason == "tool_use")
            .map(|r| r.count);
        let end_turn = rows
            .iter()
            .find(|r| r.reason == "end_turn")
            .map(|r| r.count);
        assert_eq!(tool_use, Some(2));
        assert_eq!(end_turn, Some(1));
    }

    #[test]
    fn test_query_context_type_split_empty() {
        let conn = setup_test_db();
        let rows = query_context_type_split(&conn, None, None).unwrap();
        // Empty DB → no rows (nothing to group by context)
        assert!(rows.is_empty());
    }

    #[test]
    fn test_query_context_type_split_groups_by_context() {
        let conn = setup_test_db();
        insert_llm_span(&conn, "model-a", 100, 50, None, Some("interaction"));
        insert_llm_span(&conn, "model-b", 200, 80, None, Some("interaction"));
        insert_llm_span(&conn, "model-c", 150, 60, None, Some("sub_agent"));

        let rows = query_context_type_split(&conn, None, None).unwrap();
        let interaction = rows.iter().find(|r| r.context == "interaction");
        let sub_agent = rows.iter().find(|r| r.context == "sub_agent");
        assert!(interaction.is_some(), "interaction row missing");
        assert_eq!(interaction.unwrap().calls, 2);
        assert!(sub_agent.is_some(), "sub_agent row missing");
        assert_eq!(sub_agent.unwrap().calls, 1);
    }

    #[test]
    fn test_query_tool_errors_empty() {
        let conn = setup_test_db();
        let rows = query_tool_errors(&conn, None, None, 10).unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn test_query_tool_errors_with_data() {
        let conn = setup_test_db();
        insert_failed_tool(&conn, "Bash", "Shell command failed");
        insert_failed_tool(&conn, "Bash", "Shell command failed");
        insert_failed_tool(&conn, "Read", "File not found");

        let rows = query_tool_errors(&conn, None, None, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tool_name, "Bash");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[1].tool_name, "Read");
        assert_eq!(rows[1].count, 1);
    }

    #[test]
    fn test_query_hour_of_day_returns_24_buckets() {
        let conn = setup_test_db();
        let rows = query_hour_of_day(&conn, None, None).unwrap();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[0].hour, 0);
        assert_eq!(rows[23].hour, 23);
    }

    #[test]
    fn test_query_hour_of_day_empty_db_all_zero() {
        let conn = setup_test_db();
        let rows = query_hour_of_day(&conn, None, None).unwrap();
        assert!(rows.iter().all(|r| r.llm_calls == 0 && r.tool_calls == 0));
    }

    #[test]
    fn test_query_hour_of_day_data_driven() {
        let conn = setup_test_db();
        // Unix timestamp for 2024-01-01 14:00:00 UTC in nanoseconds → hour 14
        // 1704117600 seconds * 1_000_000_000 ns/s
        let ts_ns: i64 = 1_704_117_600_000_000_000;
        let attrs = serde_json::json!({"model": "test", "input_tokens": 10, "output_tokens": 5});
        conn.execute(
            "INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, resource, events, links, scope)
             VALUES (?, ?, 'claude_code.llm_request', 0, ?, ?, ?, '{}', '[]', '[]', '{}')",
            rusqlite::params![next_id(), next_id(), ts_ns, ts_ns + 1_000_000_000, attrs.to_string()],
        ).unwrap();
        // Insert a tool execution at the same hour
        conn.execute(
            "INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, resource, events, links, scope)
             VALUES (?, ?, 'claude_code.tool.execution', 0, ?, ?, '{}', '{}', '[]', '[]', '{}')",
            rusqlite::params![next_id(), next_id(), ts_ns, ts_ns + 500_000_000],
        ).unwrap();

        let rows = query_hour_of_day(&conn, None, None).unwrap();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[14].hour, 14);
        assert_eq!(rows[14].llm_calls, 1, "hour 14 should have 1 LLM call");
        assert_eq!(rows[14].tool_calls, 1, "hour 14 should have 1 tool call");
        // All other hours should be zero
        for (i, b) in rows.iter().enumerate() {
            if i != 14 {
                assert_eq!(b.llm_calls, 0, "hour {i} should have 0 LLM calls");
                assert_eq!(b.tool_calls, 0, "hour {i} should have 0 tool calls");
            }
        }
    }
}
