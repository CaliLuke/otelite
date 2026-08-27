# Throughput analysis

How the derived output-throughput figures in the dashboard (web + TUI) and the
`otelite usage` CLI are computed, what they do and do not measure, and how the
time bucketing works.

## What tok/s means here

The dashboard and CLI show **derived end-to-end output throughput** in
tokens/second. For each LLM request span:

```text
throughput = output_tokens / (end_time − start_time)
```

- `output_tokens` — coalesced from the semantic-convention aliases
  (`gen_ai.usage.output_tokens` → `gen_ai.usage.completion_tokens` → …, see
  `crates/otelite-core/src/semconv.rs`).
- Duration is the **raw nanosecond span duration**, converted only at the
  division. Percentiles are computed from the per-call rates, **never** from
  aggregate tokens ÷ aggregate duration (pooling would weight long calls
  differently and hide the distribution).
- A call is **throughput-eligible** when it has `output_tokens > 0` and a
  positive duration. Every other call (no output tokens reported, zero or
  missing duration) is excluded from the throughput percentiles but still
  counts toward call counts and duration percentiles.

### It is not a provider generation rate

The span duration covers the whole client-observed request: connection,
provider queueing, time to first token, generation, and network transfer.
So `tok/s` is an **end-to-end** figure — it is what a user effectively
receives, but it is lower than the provider's internal generation rate and
moves with queueing and network conditions. Tooltips and panel titles say
this explicitly; do not read these numbers as hardware or engine speed.

Provider-reported generation rates are not ingested by otelite; if a
framework records them, they would surface as separate attributes, not as
these percentiles.

## Latency components behind the span duration

For one call the span duration decomposes roughly as:

```text
duration = client overhead + network RTT + provider queue + TTFT + generation
```

That is why the latency panels show both:

- **duration percentiles** (p10/p50/p90/p95/p99) — the whole call, and
- **TTFT percentiles** — the time-to-first-token component.

When TTFT is unreliable (buffered/streaming-hidden responses report a TTFT
close to the total duration), the TTFT accumulator flags the model as
degenerate — `ttft_degenerate_count` of `ttft_count` samples, flagged when at
least 10 samples exist and ≥ 90 % are degenerate — and the display shows
`buffered (NN%)` instead of a misleading TTFT number.

## Percentile estimator

Percentiles use the nearest-rank estimator over the sorted per-bucket values:

```text
index = round((n − 1) × q)      // 0-based into the ascending sort
```

- `p10` — lower tail (how slow the slowest-decile call is);
- `p50` — median;
- `p90` / `p95` / `p99` — upper-reference (how bad the worst decile gets).

**Confidence:** when the bucket has fewer than 10 samples, the estimate is
weak (a single call can move p10 by 50 %). All surfaces mark this: the web
tables append `†` to the sample count, the CLI does the same in its headers
footnote, and the TUI reuses the same wording. Empty buckets (calendar mode)
carry `count: 0` and null percentiles; the UIs render them as `—` (web omits
entirely-empty days from the daily throughput table, the API grid keeps the
explicit `count: 0` row so clients can distinguish "no calls" from "no data").

## Outcome inclusion

Latency, TTFT, and throughput statistics include **calls of every outcome** —
successful, errored, and unset-status spans are all counted (the request-span
guard filters by span identity, not by status). Errored calls are genuinely
part of the user-observed latency distribution; hiding them would flatter
every percentile. Failures are separately visible through the error panels
(`error_count` per series bucket, error-type breakdown).

Throughput-eligible errored calls (output tokens reported before the error)
count toward the throughput percentiles like any other call.

## Model identity

Throughput and latency rows are keyed by the **model identity**:

```text
identity = "<provider>/<request model>"     // e.g. "openai/gpt-4o"
          "<request model>"                 // when no provider attribute exists
```

- Built **only from request-side keys** (`gen_ai.system` +
  `gen_ai.request.model`/`model` aliases). The response model is reported
  separately (`response_model`) and never substitutes when a request model
  exists.
- When both a request and a response model are known and they differ, the
  call is counted in `rerouted_count` and, when there is a dominant
  *differing* response model for the identity, it is shown in the
  `response_model` column. Rerouting is a property of the traffic, not of
  the model — the row stays under the identity the client asked for.
- Provider mix keeps the **raw inner model name** as its label, since the
  provider is already the outer dimension of that view.
- Filters (`--model` / the web filter bar) match the **raw request-model
  attribute**, so `--model gpt-4` still selects the `openai/gpt-4` cohort.

Full background: issue #143.

## Time bucketing: rolling vs calendar-day

### Rolling (default)

Buckets are fixed-width windows aligned to the epoch
(`timestamp = floor(start / bucket) * bucket`), width `bucket_secs`
(default 3600). Only non-empty buckets are returned. This is stable across
timezones and DST, but bucket boundaries do not align with human days.

### Calendar-day

`calendar_day=1&timezone=<IANA>` aligns buckets to **local midnight** in the
given timezone. Rules:

- Buckets are generated for every calendar day the query window spans, so
  the grid is **full** — days with zero calls are present with
  `count: 0` and null percentiles.
- A call is attributed to the day of its **start** instant.
- DST transitions produce 23-hour or 25-hour days; `end_ts − ts` reflects
  the actual day length, so clients must not assume 86 400 s buckets.
- The timezone must be a valid IANA name; unknown values are a 400.
- The daily throughput table (web) is fetched in calendar-day mode with the
  browser's local timezone (via `Intl.DateTimeFormat().resolvedOptions().timeZone`);
  the TUI uses `$TZ` when set and valid, otherwise `UTC`, and shows the
  timezone in the panel title. The CLI requires an explicit `--timezone`
  with `--calendar-day`.
- The default when no timezone is requested anywhere is **UTC**.

### Window semantics

An explicit `[start, end)` window bounds every query (the web passes the
selected window; the CLI's `--start/--end`; the TUI daily panel uses a fixed
7-day window ending now). A query with no explicit window falls back to the
server's default recent range. Empty windows return zeroed summaries, empty
detail arrays, and — in calendar-day mode — the full grid of empty days for
the window's dates.

## Parity

The exact API and CLI JSON for a versioned fixture of spans
(`crates/otelite-api/tests/fixtures/throughput_parity_v1.json`, `version: 1`)
is frozen by tests:

- API side: `crates/otelite-api/tests/throughput_parity_test.rs` deep-compares
  all five rendered endpoints (token usage, latency stats, rolling and
  calendar-day percentiles, latency series) plus the empty-window states.
- CLI side: `crates/otelite/tests/throughput_parity_cli_test.rs` runs the
  compiled binary against the same fixture database and deep-compares the
  `json-compact` output.
- Web side: `crates/otelite-api/tests/js/daily_throughput.test.mjs`
  (`node --test`) pins the rendered values and wording of the daily
  throughput table.

The fixture's `normalization` paths (the network-dependent LiteLLM pricing
fields) are the only fields excluded from exact comparison; everything else
must match byte-for-byte after JSON normalisation. When changing any of
these outputs on purpose, bump the fixture `version`, regenerate the
expected JSON, and update the hand-checked semantic assertions.
