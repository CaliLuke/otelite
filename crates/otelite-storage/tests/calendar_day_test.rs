//! Tests for calendar-day bucketing (issue #119, slice #141): IANA
//! timezone boundaries, DST 23/25-hour days, explicit bucket end
//! timestamps, empty buckets with null percentiles, and `[start, end)`
//! attribution by call start time.

use otelite_core::filters::GenAiFilters;
use otelite_storage::sqlite::{reader, schema};
use rusqlite::Connection;

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&conn).unwrap();
    conn
}

/// One `claude_code.llm_request` span with an exact duration.
#[allow(clippy::too_many_arguments)] // six precise span inputs, all load-bearing
fn insert_span(
    conn: &Connection,
    span_id: &str,
    model: &str,
    start_ns: i64,
    duration_ns: i64,
    output_tokens: i64,
) {
    let attrs = format!(
        r#"{{"model":"{model}","gen_ai.request.model":"{model}","output_tokens":"{output_tokens}"}}"#
    );
    conn.execute(
        r#"INSERT INTO spans (trace_id, span_id, name, kind, start_time, end_time, attributes, status_code)
           VALUES ('t', ?1, 'claude_code.llm_request', 0, ?2, ?3, ?4, 1)"#,
        rusqlite::params![span_id, start_ns, start_ns + duration_ns, attrs],
    )
    .unwrap();
}

const NS_PER_HOUR: i64 = 3_600_000_000_000;

/// Epoch nanoseconds, precomputed (UTC).
mod t {
    pub const D0: i64 = 1_785_542_400_000_000_000; // 2026-08-01T00:00:00Z
    pub const D1: i64 = 1_785_628_800_000_000_000; // 2026-08-02T00:00:00Z
    pub const D2: i64 = 1_785_715_200_000_000_000; // 2026-08-03T00:00:00Z
    pub const D3: i64 = 1_785_801_600_000_000_000; // 2026-08-04T00:00:00Z
                                                   // Europe/London spring forward: local day 2026-03-29 is 23 hours.
    pub const LND_MAR29_START: i64 = 1_774_742_400_000_000_000; // 2026-03-29T00:00:00Z (GMT)
    pub const LND_MAR29_END: i64 = 1_774_825_200_000_000_000; // 2026-03-29T23:00:00Z (next local midnight, BST)
                                                              // Local midnight ending the 24-hour day of 2026-03-30 (BST):
    pub const LND_MAR30_END: i64 = 1_774_911_600_000_000_000; // 2026-03-30T23:00:00Z
                                                              // Europe/London fall back: local day 2026-10-25 is 25 hours.
    pub const LND_OCT25_START: i64 = 1_792_882_800_000_000_000; // 2026-10-24T23:00:00Z (local midnight, BST)
    pub const LND_OCT25_END: i64 = 1_792_972_800_000_000_000; // 2026-10-26T00:00:00Z (next local midnight, GMT)
}

/// The 2026-03-29 local day in Europe/London is 23 hours (spring forward at
/// 01:00 GMT); the following day is 24 hours. Buckets tile contiguously.
#[test]
fn test_calendar_day_london_spring_forward_23h() {
    let conn = setup_test_db();
    let resp = reader::query_latency_percentiles(
        &conn,
        Some(t::LND_MAR29_START),
        Some(t::LND_MAR30_END),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        Some("Europe/London"),
    )
    .unwrap();
    let points = &resp.metrics["duration"].all;
    assert_eq!(points.len(), 2, "two local days in the window");

    let (b0, b1) = (&points[0], &points[1]);
    assert_eq!(b0.ts, t::LND_MAR29_START);
    assert_eq!(b0.end_ts, t::LND_MAR29_END);
    assert_eq!(
        b0.end_ts - b0.ts,
        23 * NS_PER_HOUR,
        "spring-forward day is 23 hours"
    );
    assert_eq!(b1.ts, b0.end_ts, "buckets tile with no gap");
    assert_eq!(b1.end_ts - b1.ts, 24 * NS_PER_HOUR);
    // No data: count 0, null percentiles, no throughput.
    for p in points {
        assert_eq!(p.count, 0);
        assert!(p.p50_ms.is_none());
        assert!(p.throughput_p50_tok_s.is_none());
    }
}

/// The 2026-10-25 local day in Europe/London is 25 hours (fall back at
/// 02:00 BST).
#[test]
fn test_calendar_day_london_fall_back_25h() {
    let conn = setup_test_db();
    let resp = reader::query_latency_percentiles(
        &conn,
        Some(t::LND_OCT25_START),
        Some(t::LND_OCT25_END),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        Some("Europe/London"),
    )
    .unwrap();
    let points = &resp.metrics["duration"].all;
    assert_eq!(points.len(), 1);
    let p = &points[0];
    assert_eq!(p.ts, t::LND_OCT25_START);
    assert_eq!(p.end_ts, t::LND_OCT25_END);
    assert_eq!(
        p.end_ts - p.ts,
        25 * NS_PER_HOUR,
        "fall-back day is 25 hours"
    );
}

/// Calls are attributed to `[start, end)` by call start time: a
/// boundary-crossing call counts exactly once, in the day it started. Empty
/// days are present with count 0 and null percentiles.
#[test]
fn test_calendar_boundary_attribution_and_empty_buckets() {
    let conn = setup_test_db();
    // 2026-08-01T01:00Z, in day D0.
    insert_span(&conn, "a", "m", t::D0 + NS_PER_HOUR, 1_000_000_000, 10);
    // 2026-08-02T01:00Z, in day D1.
    insert_span(&conn, "b", "m", t::D1 + NS_PER_HOUR, 1_000_000_000, 20);
    // 2026-08-02T23:59Z, crosses into D2 but starts in D1.
    insert_span(
        &conn,
        "c",
        "m",
        t::D1 + 23 * NS_PER_HOUR + 59 * 60_000_000_000,
        120_000_000_000,
        30,
    );

    let resp = reader::query_latency_percentiles(
        &conn,
        Some(t::D0),
        Some(t::D3),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        Some("UTC"),
    )
    .unwrap();
    let all = &resp.metrics["duration"].all;
    assert_eq!(all.len(), 3, "three local days");

    assert_eq!(all[0].ts, t::D0);
    assert_eq!(all[0].count, 1);
    assert!(all[0].p50_ms.is_some());

    assert_eq!(all[1].ts, t::D1);
    assert_eq!(all[1].count, 2, "boundary-crossing call attributed to D1");

    assert_eq!(all[2].ts, t::D2);
    assert_eq!(all[2].count, 0, "crossing call not counted in D2");
    assert!(all[2].p10_ms.is_none(), "empty bucket: null p10");
    assert!(all[2].p50_ms.is_none(), "empty bucket: null p50");
    assert!(all[2].p95_ms.is_none(), "empty bucket: null p95");
    assert!(all[2].throughput_p50_tok_s.is_none());
    assert_eq!(all[2].throughput_sample_count, 0);

    // No double counting across the whole window.
    let total: u64 = all.iter().map(|p| p.count).sum();
    assert_eq!(total, 3, "each call counted exactly once");

    // Per-model series carries the same grid (empty days included).
    let per_model = resp.metrics["duration"].models.get("m").unwrap();
    assert_eq!(per_model.len(), 3);
    assert_eq!(per_model[2].count, 0);
}

/// Partial first and last days are clipped to the query window, so the
/// buckets tile `[start, end)` exactly.
#[test]
fn test_calendar_partial_day_clipping() {
    let conn = setup_test_db();
    let half_day = 12 * NS_PER_HOUR;
    insert_span(
        &conn,
        "a",
        "m",
        t::D0 + half_day + NS_PER_HOUR,
        1_000_000_000,
        10,
    );
    insert_span(&conn, "b", "m", t::D1 + NS_PER_HOUR, 1_000_000_000, 20);

    let resp = reader::query_latency_percentiles(
        &conn,
        Some(t::D0 + half_day),
        Some(t::D1 + half_day),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        Some("UTC"),
    )
    .unwrap();
    let all = &resp.metrics["duration"].all;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].ts, t::D0 + half_day);
    assert_eq!(all[0].end_ts, t::D1, "first bucket clipped to the day end");
    assert_eq!(all[1].ts, t::D1);
    assert_eq!(
        all[1].end_ts,
        t::D1 + half_day,
        "last bucket clipped to the window end"
    );
    assert_eq!(all[0].count, 1);
    assert_eq!(all[1].count, 1);
}

/// Calendar mode requires an explicit window; unknown timezones are
/// rejected at the storage layer too.
#[test]
fn test_calendar_validation_errors() {
    let conn = setup_test_db();
    assert!(
        reader::query_latency_percentiles(
            &conn,
            None,
            None,
            3600,
            &["duration"],
            &GenAiFilters::default(),
            Some("UTC"),
        )
        .is_err(),
        "calendar mode without an explicit window must fail"
    );
    assert!(
        reader::query_latency_percentiles(
            &conn,
            Some(t::D0),
            Some(t::D3),
            3600,
            &["duration"],
            &GenAiFilters::default(),
            Some("Not/AZone"),
        )
        .is_err(),
        "unknown IANA timezone must fail"
    );
}

/// Rolling mode is unchanged: only non-empty buckets, with an explicit
/// `end_ts` one bucket width after the start.
#[test]
fn test_rolling_mode_end_ts_and_no_empty_buckets() {
    let conn = setup_test_db();
    insert_span(&conn, "a", "m", t::D0 + 60_000_000_000, 1_000_000_000, 10);
    insert_span(&conn, "b", "m", t::D1 + 2 * NS_PER_HOUR, 1_000_000_000, 20);

    let resp = reader::query_latency_percentiles(
        &conn,
        Some(t::D0),
        Some(t::D3),
        3600,
        &["duration"],
        &GenAiFilters::default(),
        None,
    )
    .unwrap();
    let all = &resp.metrics["duration"].all;
    assert_eq!(all.len(), 2, "rolling mode: no empty buckets");
    for p in all {
        assert_eq!(p.end_ts, p.ts + 3_600_000_000_000);
        assert!(p.p50_ms.is_some());
    }
}

// ── issue #142: repeated model patterns + series calendar/throughput ────────

/// A model cohort (exact or glob) must filter the summary totals and every
/// latency detail view on the same set of spans.
#[test]
fn test_model_cohort_filters_summary_and_latency_consistently() {
    let conn = setup_test_db();
    // modelA-* spans: two calls on D0. modelB: one call on D0.
    insert_span(
        &conn,
        "a1",
        "modelA-1",
        t::D0 + NS_PER_HOUR,
        1_000_000_000,
        10,
    );
    insert_span(
        &conn,
        "a2",
        "modelA-2",
        t::D0 + 2 * NS_PER_HOUR,
        2_000_000_000,
        20,
    );
    insert_span(
        &conn,
        "b1",
        "modelB",
        t::D0 + 3 * NS_PER_HOUR,
        3_000_000_000,
        5,
    );

    let unfiltered =
        reader::query_token_usage(&conn, Some(t::D0), Some(t::D3), &GenAiFilters::default())
            .unwrap();
    assert_eq!(unfiltered.0.total_requests, 3);

    for (name, filters, expected_calls) in [
        (
            "exact",
            GenAiFilters {
                model: Some("modelA-1".into()),
                ..Default::default()
            },
            1u64,
        ),
        (
            "glob",
            GenAiFilters {
                models: Some(vec!["modelA-*".into()]),
                ..Default::default()
            },
            2,
        ),
    ] {
        // Summary totals are computed on the cohort, not post-filtered.
        let (summary, by_model, _systems) =
            reader::query_token_usage(&conn, Some(t::D0), Some(t::D3), &filters).unwrap();
        assert_eq!(
            summary.total_requests as u64, expected_calls,
            "{name}: summary cohort"
        );
        let mut names: Vec<String> = by_model.iter().map(|m| m.model.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            if name == "exact" {
                vec!["modelA-1".to_string()]
            } else {
                vec!["modelA-1".to_string(), "modelA-2".to_string()]
            },
            "{name}: by_model rows"
        );
        assert!(
            by_model.iter().all(|m| m.model != "modelB"),
            "{name}: modelB must not leak into detail rows"
        );

        // Latency stats cohort.
        let stats = reader::query_latency_stats(&conn, Some(t::D0), Some(t::D3), &filters).unwrap();
        let stats_total: u64 = stats.iter().map(|s| s.count as u64).sum();
        assert_eq!(stats_total, expected_calls, "{name}: latency stats cohort");

        // Percentiles cohort.
        let resp = reader::query_latency_percentiles(
            &conn,
            Some(t::D0),
            Some(t::D3),
            3600,
            &["duration"],
            &filters,
            None,
        )
        .unwrap();
        let total: u64 = resp.metrics["duration"].all.iter().map(|p| p.count).sum();
        assert_eq!(total, expected_calls, "{name}: percentile cohort");
    }
}

/// The latency series supports calendar-day buckets (local midnights) and
/// per-bucket throughput from per-call rates; empty days are absent (a
/// trend, unlike the percentile grid).
#[test]
fn test_latency_series_calendar_day_and_throughput() {
    let conn = setup_test_db();
    // London in August is BST: local midnight starting 2026-08-01 is
    // 2026-07-31T23:00:00Z.
    let d0_local_midnight = t::D0 - NS_PER_HOUR;
    let d1_local_midnight = t::D1 - NS_PER_HOUR;
    let d2_local_midnight = t::D2 - NS_PER_HOUR;
    insert_span(
        &conn,
        "a",
        "m",
        d0_local_midnight + 2 * NS_PER_HOUR,
        1_000_000_000,
        10,
    ); // 10 tok/s
    insert_span(
        &conn,
        "b",
        "m",
        d0_local_midnight + 3 * NS_PER_HOUR,
        2_000_000_000,
        40,
    ); // 20 tok/s
    insert_span(
        &conn,
        "c",
        "m",
        d1_local_midnight + NS_PER_HOUR,
        1_000_000_000,
        0,
    ); // ineligible

    let points = reader::query_latency_series(
        &conn,
        Some(d0_local_midnight),
        Some(d2_local_midnight),
        3600,
        &GenAiFilters::default(),
        false,
        Some("Europe/London"),
    )
    .unwrap();
    assert_eq!(points.len(), 2, "calendar days with data only");
    assert_eq!(
        points[0].timestamp, d0_local_midnight,
        "local-midnight boundary"
    );
    assert_eq!(points[1].timestamp, d1_local_midnight);
    assert_eq!(points[0].count, 2);
    assert_eq!(points[0].throughput_sample_count, 2);
    // Rank estimator, n=2: p10=sorted[0], p50/p90=sorted[1].
    assert_eq!(points[0].throughput_p10_tok_s, Some(10.0));
    assert_eq!(points[0].throughput_p50_tok_s, Some(20.0));
    assert_eq!(points[0].throughput_p90_tok_s, Some(20.0));
    assert_eq!(points[1].count, 1);
    assert_eq!(points[1].throughput_sample_count, 0);
    assert!(points[1].throughput_p50_tok_s.is_none());

    // Rolling mode on the same data: epoch-grid buckets, throughput present.
    let rolling = reader::query_latency_series(
        &conn,
        Some(d0_local_midnight),
        Some(d2_local_midnight),
        3600,
        &GenAiFilters::default(),
        false,
        None,
    )
    .unwrap();
    assert!(rolling.len() >= 2);
    // The two day-0 calls sit in different 1-hour buckets; each carries
    // its own single-call rate.
    let rates: Vec<Option<f64>> = rolling
        .iter()
        .filter(|p| p.count == 1 && p.throughput_sample_count == 1)
        .map(|p| p.throughput_p50_tok_s)
        .collect();
    assert_eq!(
        rates,
        vec![Some(10.0), Some(20.0)],
        "per-call rates in rolling buckets"
    );
}
