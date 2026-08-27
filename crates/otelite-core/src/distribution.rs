//! Generic distribution binning and summary statistics (issue #133).
//!
//! Pure functions over flat value lists so the storage layer, the CLI and
//! the API can all serve the same JSON shape. Percentiles use the same
//! nearest-rank formula as the latency endpoints.

use crate::api::{DistributionBucket, DistributionResponse, DistributionStats};

/// Nearest-rank percentile of a slice (need not be pre-sorted).
/// Returns 0.0 for an empty slice.
pub fn percentile_f64(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Bin `values` into `n` equal-width (linear) or log-spaced (log) buckets.
///
/// Log scale reuses the session-cost bucketing: bucket 0 starts at 0 and
/// the remaining buckets span equal decades up to the maximum, so skewed
/// cost/token distributions do not collapse into one bin. Negative values
/// (not expected from the named cohorts) fall into bucket 0.
pub fn bin_values(values: &[f64], n: usize, scale: &str) -> Vec<DistributionBucket> {
    if values.is_empty() || n < 1 {
        return Vec::new();
    }
    let max = values.iter().cloned().fold(f64::MIN, f64::max);
    let bounds: Vec<(f64, f64)> = match scale {
        "log" => crate::session_cost::log_cost_buckets(max, n),
        _ => {
            let min = values.iter().cloned().fold(f64::MAX, f64::min);
            if (max - min).abs() < f64::EPSILON {
                vec![(min, min)]
            } else {
                let step = (max - min) / n as f64;
                (0..n)
                    .map(|i| (min + i as f64 * step, min + (i + 1) as f64 * step))
                    .collect()
            }
        },
    };
    let mut counts = vec![0u64; bounds.len()];
    for v in values {
        let idx = crate::session_cost::cost_bucket_index(&bounds, *v);
        counts[idx] += 1;
    }
    bounds
        .into_iter()
        .zip(counts)
        .map(|((min, max), count)| DistributionBucket { min, max, count })
        .collect()
}

/// Summary statistics over `values`; None when empty.
pub fn summarize(values: &[f64]) -> Option<DistributionStats> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let sum: f64 = sorted.iter().sum();
    Some(DistributionStats {
        min: sorted[0],
        max: *sorted.last().unwrap(),
        mean: sum / sorted.len() as f64,
        p50: percentile_f64(&sorted, 0.50),
        p95: percentile_f64(&sorted, 0.95),
        p99: percentile_f64(&sorted, 0.99),
        count: sorted.len() as u64,
    })
}

/// Build the wire response from a named cohort's values.
pub fn build(
    metric: &str,
    unit: &str,
    scale: &str,
    buckets: usize,
    values: Vec<f64>,
) -> DistributionResponse {
    DistributionResponse {
        metric: metric.to_string(),
        unit: unit.to_string(),
        scale: scale.to_string(),
        buckets: bin_values(&values, buckets, scale),
        stats: summarize(&values),
        filters_applied: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        assert_eq!(percentile_f64(&[], 0.5), 0.0);
        assert_eq!(percentile_f64(&[7.0], 0.99), 7.0);
        // [1..=100]: p50 -> idx (100-1)*0.5 = 49.5.round() = 50 -> sorted[50] = 51
        let v: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        assert_eq!(percentile_f64(&v, 0.50), 51.0);
        assert_eq!(percentile_f64(&v, 1.0), 100.0);
    }

    #[test]
    fn linear_bins_equal_width() {
        let values = vec![0.0, 1.0, 9.0, 10.0];
        let bins = bin_values(&values, 2, "linear");
        assert_eq!(bins.len(), 2);
        assert!((bins[0].min - 0.0).abs() < 1e-12);
        assert!((bins[0].max - 5.0).abs() < 1e-12);
        assert!((bins[1].max - 10.0).abs() < 1e-12);
        assert_eq!(bins[0].count + bins[1].count, 4);
        // 10.0 lands in the last (inclusive) bucket
        assert_eq!(bins[1].count, 2);
    }

    #[test]
    fn linear_bins_single_value() {
        let bins = bin_values(&[3.5, 3.5], 20, "linear");
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0].count, 2);
        assert!((bins[0].min - 3.5).abs() < 1e-12);
    }

    #[test]
    fn log_bins_keep_shape() {
        // 9 values 0.01..0.9 plus one at 100: log scale keeps the small
        // values spread across decades instead of one bin.
        let values: Vec<f64> = (1..=9).map(|i| i as f64 * 0.1).chain([100.0]).collect();
        let bins = bin_values(&values, 4, "log");
        assert_eq!(bins.len(), 4);
        assert!(bins[0].min == 0.0);
        assert!((bins.last().unwrap().max - 100.0).abs() < 1e-9);
        assert_eq!(bins.iter().map(|b| b.count).sum::<u64>(), 10);
        // the 100.0 value must not be lost in the last bucket
        assert_eq!(bins[3].count, 1);
    }

    #[test]
    fn empty_values_no_bins_no_stats() {
        let resp = build("ttft", "ms", "linear", 20, Vec::new());
        assert!(resp.buckets.is_empty());
        assert!(resp.stats.is_none());
    }

    #[test]
    fn summarize_stats() {
        let stats = summarize(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.max, 4.0);
        assert!((stats.mean - 2.5).abs() < 1e-12);
        assert_eq!(stats.count, 4);
        assert_eq!(
            stats.p50, 3.0,
            "idx (4-1)*0.5=1.5.round()=2 -> sorted[2]=3.0"
        );
        assert_eq!(stats.p95, 4.0);
        assert_eq!(stats.p99, 4.0);
    }
}
