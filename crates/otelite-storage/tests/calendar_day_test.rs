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
