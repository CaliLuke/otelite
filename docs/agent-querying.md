# Query the Live SQLite Database

Otelite's SQLite database is a supported read-only interface for scripts and agents while
`otelite serve` or the background daemon is actively receiving telemetry. SQLite WAL mode
keeps committed writes visible to independent readers without stopping ingestion.

## Resolve the database path

The default database pathname is:

```text
~/.otelite/data/otelite.db
```

`OTELITE_DATA_DIR` changes the directory. `--storage-path <DIR>` overrides that environment
variable for `serve`, `start`, and `restart`. In either case, append `otelite.db` to the resolved
directory. Startup and `otelite status` report the resolved pathname.

For example:

```bash
OTELITE_DATA_DIR=/var/tmp/my-stack otelite serve
# Database: /var/tmp/my-stack/otelite.db

otelite serve --storage-path ./tmp/otelite
# Database: ./tmp/otelite/otelite.db
```

## Read while Otelite is running

Use SQLite's read-only mode. A read-only connection is supported while logs, traces, and
metrics continue to arrive:

```bash
sqlite3 -readonly /path/to/otelite.db \
  "SELECT timestamp, severity_text, body FROM logs ORDER BY id DESC LIMIT 20"
```

Do not omit `-readonly` in agent automation. The database is Otelite-owned; external clients
must not modify its schema, pragmas, or rows.

## Tables and stored values

The primary telemetry tables are:

| Table | One row represents | Useful columns |
|---|---|---|
| `logs` | An OTLP log record | `timestamp`, `severity_number`, `severity_text`, `body`, `trace_id`, `span_id`, `attributes`, `resource` |
| `spans` | An OTLP span | `trace_id`, `span_id`, `parent_span_id`, `name`, `start_time`, `end_time`, `status_code`, `attributes`, `events`, `resource` |
| `metrics` | An OTLP metric point | `name`, `timestamp`, `metric_type`, `value_int`, `value_double`, `value_histogram`, `value_summary`, `attributes`, `resource` |

OTLP timestamps are stored as Unix epoch nanoseconds. Structured attributes, resources,
events, histograms, and summaries are JSON text. Resource attributes use this shape:

```json
{"attributes":{"service.name":"checkout","service.version":"1.4.2"}}
```

For example, extract `service.name` with:

```sql
json_extract(resource, '$.attributes."service.name"')
```

Missing resources and attributes return SQL `NULL`.

## Inspect the schema

Inspect everything or one table without opening a writable connection:

```bash
sqlite3 -readonly /path/to/otelite.db '.schema'
sqlite3 -readonly /path/to/otelite.db '.schema logs'
sqlite3 -readonly /path/to/otelite.db '.schema spans'
sqlite3 -readonly /path/to/otelite.db '.schema metrics'
```

Machine-readable table metadata:

```bash
sqlite3 -readonly -json /path/to/otelite.db \
  "SELECT name, type, sql FROM sqlite_schema WHERE name IN ('logs','spans','metrics') ORDER BY name"
```

## Query recipes

### Recent errors

`severity_number >= 17` includes OpenTelemetry Error and Fatal levels:

```bash
sqlite3 -readonly -json /path/to/otelite.db \
  "SELECT id, timestamp, severity_text, body, trace_id
   FROM logs
   WHERE severity_number >= 17
   ORDER BY id DESC
   LIMIT 50"
```

### Records for a service

```bash
sqlite3 -readonly -json /path/to/otelite.db \
  "SELECT timestamp, severity_text, body, trace_id
   FROM logs
   WHERE json_extract(resource, '$.attributes.\"service.name\"') = 'checkout'
   ORDER BY id DESC
   LIMIT 100"
```

Use the same `json_extract` predicate against `spans.resource` or `metrics.resource`.

### A trace ID

```bash
TRACE_ID=0123456789abcdef0123456789abcdef
sqlite3 -readonly -json /path/to/otelite.db \
  "SELECT span_id, parent_span_id, name, start_time, end_time, status_code
   FROM spans
   WHERE trace_id = '$TRACE_ID'
   ORDER BY start_time"

sqlite3 -readonly -json /path/to/otelite.db \
  "SELECT timestamp, severity_text, body, span_id
   FROM logs
   WHERE trace_id = '$TRACE_ID'
   ORDER BY timestamp"
```

### A recent time window

This example reads logs from the last 15 minutes:

```bash
sqlite3 -readonly -json /path/to/otelite.db \
  "SELECT timestamp, severity_text, body
   FROM logs
   WHERE timestamp >= CAST(strftime('%s', 'now', '-15 minutes') AS INTEGER) * 1000000000
   ORDER BY timestamp DESC"
```

Apply the same cutoff to `spans.start_time` or `metrics.timestamp`.

## JSON for scripts and agents

SQLite can emit JSON directly:

```bash
DB="${OTELITE_DATA_DIR:-$HOME/.otelite/data}/otelite.db"
sqlite3 -readonly -json "$DB" \
  "SELECT id, timestamp, severity_text, body FROM logs ORDER BY id DESC LIMIT 20"
```

Otelite's supported CLI also emits JSON through the REST API:

```bash
otelite --format json logs list --limit 20
otelite --format json traces list --limit 20
otelite --format json metrics list --limit 20
```

Use direct SQLite for arbitrary read-only SQL and joins. Use the CLI when its stable query
shape already matches the task.

## Retention and long-running readers

Automatic retention removes rows older than `OTELITE_RETENTION_DAYS` on the configured
`OTELITE_PURGE_SCHEDULE`. The default window is 90 days. Set retention to `0`, or set
`OTELITE_AUTO_PURGE_ENABLED=false`, to disable scheduled deletion. Queries must not assume
old rows remain available beyond the configured window.

A read-only query sees a consistent SQLite snapshot. Re-run the query to observe telemetry
committed after that statement began; do not hold a transaction open indefinitely.
