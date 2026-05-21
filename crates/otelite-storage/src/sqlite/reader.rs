//! Read operations for SQLite backend

use crate::error::{Result, StorageError};
use crate::{QueryParams, StorageStats};
use otelite_core::query::{Operator, QueryPredicate, QueryValue};
use otelite_core::telemetry::log::SeverityLevel;
use otelite_core::telemetry::trace::{SpanKind, SpanStatus, StatusCode};
use otelite_core::telemetry::{LogRecord, Metric, Span};
use rusqlite::{Connection, Row};

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

fn parse_log_row(row: &Row) -> rusqlite::Result<LogRecord> {
    let attributes_json: String = row.get("attributes")?;
    let attributes = serde_json::from_str(&attributes_json).unwrap_or_default();

    let resource_json: String = row.get("resource")?;
    let resource = serde_json::from_str(&resource_json).ok();

    let severity_num: i32 = row.get("severity_number")?;
    let severity = SeverityLevel::from_i32(severity_num).unwrap_or(SeverityLevel::Info);

    Ok(LogRecord {
        timestamp: row.get("timestamp")?,
        observed_timestamp: row.get("observed_timestamp")?,
        trace_id: row.get("trace_id")?,
        span_id: row.get("span_id")?,
        severity,
        severity_text: row.get("severity_text")?,
        body: row.get("body")?,
        attributes,
        resource,
    })
}

fn parse_span_row(row: &Row) -> rusqlite::Result<Span> {
    let attributes_json: String = row.get("attributes")?;
    let attributes = serde_json::from_str(&attributes_json).unwrap_or_default();

    let events_json: String = row.get("events")?;
    let events = serde_json::from_str(&events_json).unwrap_or_default();

    let resource_json: String = row.get("resource")?;
    let resource = serde_json::from_str(&resource_json).ok();

    let kind_num: i32 = row.get("kind")?;
    let kind = SpanKind::from_i32(kind_num).unwrap_or(SpanKind::Internal);

    let status_code_num: i32 = row.get("status_code")?;
    let status_code = StatusCode::from_i32(status_code_num).unwrap_or(StatusCode::Unset);

    let status = SpanStatus {
        code: status_code,
        message: row.get("status_message")?,
    };

    Ok(Span {
        trace_id: row.get("trace_id")?,
        span_id: row.get("span_id")?,
        parent_span_id: row.get("parent_span_id")?,
        name: row.get("name")?,
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

    let attributes_json: String = row.get("attributes")?;
    let attributes = serde_json::from_str(&attributes_json).unwrap_or_default();

    let resource_json: String = row.get("resource")?;
    let resource = serde_json::from_str(&resource_json).ok();

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
                serde_json::from_str(&histogram_json).unwrap_or((0, 0.0, Vec::new()));
            MetricType::Histogram {
                count,
                sum,
                buckets,
            }
        },
        3 => {
            let summary_json: String = row.get("value_summary")?;
            let (count, sum, quantiles) =
                serde_json::from_str(&summary_json).unwrap_or((0, 0.0, Vec::new()));
            MetricType::Summary {
                count,
                sum,
                quantiles,
            }
        },
        _ => MetricType::Gauge(0.0),
    };

    Ok(Metric {
        name: row.get("name")?,
        description: row.get("description")?,
        unit: row.get("unit")?,
        metric_type,
        timestamp: row.get("timestamp")?,
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
    }
}

/// Query token usage statistics for GenAI/LLM spans
///
/// Returns aggregated token usage grouped by model and system (provider).
/// Only includes spans with `gen_ai.system` attribute.
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
            "CAST(COALESCE(output_tokens_raw, 0) AS FLOAT) / NULLIF(COALESCE(input_tokens_raw, 0), 0) DESC".to_string()
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

/// Latency / TTFT percentile statistics per model for LLM spans.
// (durations_ms, ttfts_ms, token_rates, input_tokens, output_input_ratios)
type LatencyAccum = (Vec<i64>, Vec<i64>, Vec<f64>, Vec<i64>, Vec<f64>);

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
            (end_time - start_time) / 1000000 AS duration_ms,
            COALESCE(
                CAST(json_extract(attributes, '$.\"gen_ai.server.time_to_first_token\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"llm.time_to_first_token\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"ttft_ms\"') AS INTEGER)
            ) AS ttft_ms,
            COALESCE({output}, 0) AS output_tokens,
            COALESCE({input}, 0) AS input_tokens
        FROM spans
        {where_clause}",
        model = exprs.model,
        output = exprs.output,
        input = exprs.input,
    );

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        StorageError::QueryError(format!("Failed to prepare latency_stats query: {}", e))
    })?;

    struct Row {
        model: Option<String>,
        duration_ms: i64,
        ttft_ms: Option<i64>,
        output_tokens: i64,
        input_tokens: i64,
    }

    let rows: Vec<Row> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(Row {
                model: row.get::<_, Option<String>>(0)?,
                duration_ms: row.get::<_, i64>(1)?,
                ttft_ms: row.get::<_, Option<i64>>(2)?,
                output_tokens: row.get::<_, i64>(3)?,
                input_tokens: row.get::<_, i64>(4)?,
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
        entry.0.push(r.duration_ms);
        if let Some(t) = r.ttft_ms {
            entry.1.push(t);
        }
        if r.output_tokens > 0 && r.duration_ms > 0 {
            entry
                .2
                .push(r.output_tokens as f64 / (r.duration_ms as f64 / 1000.0));
        }
        entry.3.push(r.input_tokens);
        if r.input_tokens > 0 {
            entry.4.push(r.output_tokens as f64 / r.input_tokens as f64);
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for (model, (mut durations, mut ttfts, mut token_rates, mut input_tkns, mut ratios)) in groups {
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
        format!("WHERE {}", exprs.llm_span_guard)
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
            COALESCE(
                CAST(json_extract(attributes, '$.\"gen_ai.server.time_to_first_token\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"llm.time_to_first_token\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"ttft_ms\"') AS INTEGER)
            ) AS ttft_ms,
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
        ttft_ms: Option<i64>,
        is_error: bool,
    }

    let raw: Vec<RawRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(RawRow {
                bucket: row.get::<_, i64>(0)?,
                label: row.get::<_, Option<String>>(1)?,
                duration_ms: row.get::<_, i64>(2)?,
                ttft_ms: row.get::<_, Option<i64>>(3)?,
                is_error: row.get::<_, i64>(4)? != 0,
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
    type BucketAccum = (Vec<i64>, Vec<i64>, usize);

    let mut groups: std::collections::BTreeMap<BucketKey, BucketAccum> =
        std::collections::BTreeMap::new();
    for r in raw {
        let entry = groups.entry((r.bucket, r.label)).or_default();
        entry.0.push(r.duration_ms);
        if let Some(t) = r.ttft_ms {
            entry.1.push(t);
        }
        if r.is_error {
            entry.2 += 1;
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for ((bucket, label), (mut durations, mut ttfts, error_count)) in groups {
        if durations.is_empty() {
            continue;
        }
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
        format!("WHERE {}", exprs.llm_span_guard)
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
            COALESCE(
                CAST(json_extract(attributes, '$.\"gen_ai.server.time_to_first_token\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"llm.time_to_first_token\"') AS INTEGER),
                CAST(json_extract(attributes, '$.\"ttft_ms\"') AS INTEGER)
            ) AS ttft_ms
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
        ttft_ms: Option<i64>,
    }

    let raw: Vec<RawRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(RawRow {
                model: row.get(0)?,
                input_tokens: row.get::<_, i64>(1)?,
                duration_ms: row.get::<_, i64>(2)?,
                ttft_ms: row.get(3)?,
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
    type BinAccum = (Vec<i64>, Vec<i64>); // (durations, ttfts)

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
        if let Some(t) = r.ttft_ms {
            entry.1.push(t);
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for ((bin_idx, model), (mut durations, mut ttfts)) in groups {
        if durations.is_empty() {
            continue;
        }
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
}
