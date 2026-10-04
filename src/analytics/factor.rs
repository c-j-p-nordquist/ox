//! Return statistics for portfolio risk: beta and fit (R²) against benchmarks, and the
//! covariance of the holdings themselves, from aligned daily log returns.

use std::collections::BTreeMap;

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;

use crate::model::Bar;

/// Daily log returns keyed by New York trading date.
pub fn log_returns(bars: &[Bar]) -> BTreeMap<NaiveDate, f64> {
    let day = |t: i64| Utc.timestamp_opt(t, 0).single().map(|d| d.with_timezone(&New_York).date_naive());
    bars.windows(2)
        .filter(|w| w[0].c > 0.0 && w[1].c > 0.0)
        .filter_map(|w| Some((day(w[1].t)?, (w[1].c / w[0].c).ln())))
        .collect()
}

/// Returns on the dates every series has, most recent `n`, one vector per series.
pub fn align(series: &[BTreeMap<NaiveDate, f64>], n: usize) -> Vec<Vec<f64>> {
    let Some(first) = series.first() else { return Vec::new() };
    let mut dates: Vec<NaiveDate> =
        first.keys().copied().filter(|d| series.iter().all(|s| s.contains_key(d))).collect();
    dates.drain(..dates.len().saturating_sub(n));
    series.iter().map(|s| dates.iter().map(|d| s[d]).collect()).collect()
}

fn mean(x: &[f64]) -> f64 {
    x.iter().sum::<f64>() / x.len().max(1) as f64
}

pub fn cov(a: &[f64], b: &[f64]) -> f64 {
    let (ma, mb) = (mean(a), mean(b));
    a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum::<f64>() / a.len().max(1) as f64
}

#[derive(Debug, Clone, Copy)]
pub struct Fit {
    pub beta: f64,
    /// Share of the asset's return variance explained by the benchmark.
    pub r2: f64,
}

pub fn fit(asset: &[f64], bench: &[f64]) -> Fit {
    let (c, va, vb) = (cov(asset, bench), cov(asset, asset), cov(bench, bench));
    if va <= 0.0 || vb <= 0.0 {
        return Fit { beta: 0.0, r2: 0.0 };
    }
    Fit { beta: c / vb, r2: c * c / (va * vb) }
}

pub fn covariance_matrix(rets: &[Vec<f64>]) -> Vec<Vec<f64>> {
    rets.iter().map(|a| rets.iter().map(|b| cov(a, b)).collect()).collect()
}

/// `xᵀ Σ x`.
pub fn quadratic(x: &[f64], m: &[Vec<f64>]) -> f64 {
    x.iter().enumerate().map(|(i, xi)| xi * m[i].iter().zip(x).map(|(mij, xj)| mij * xj).sum::<f64>()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_and_covariance() {
        let bench: Vec<f64> = (0..200).map(|i| ((i as f64) * 0.37).sin() * 0.01).collect();
        let noise: Vec<f64> = (0..200).map(|i| ((i as f64) * 1.91).cos() * 0.01).collect();
        let asset: Vec<f64> = bench.iter().zip(&noise).map(|(b, n)| 1.5 * b + n).collect();
        let f = fit(&asset, &bench);
        assert!((f.beta - 1.5).abs() < 0.1, "{}", f.beta);
        assert!(f.r2 > 0.5 && f.r2 < 0.9, "{}", f.r2);
        // Two perfectly offsetting exposures carry no variance.
        let m = covariance_matrix(&[bench.clone(), bench.clone()]);
        assert!(quadratic(&[1.0, -1.0], &m).abs() < 1e-12);
        assert!(quadratic(&[1.0, 1.0], &m) > 0.0);
    }

    #[test]
    fn align_keeps_common_dates() {
        let d = |n| NaiveDate::from_ymd_opt(2026, 1, n).unwrap();
        let a = BTreeMap::from([(d(1), 0.1), (d(2), 0.2), (d(3), 0.3)]);
        let b = BTreeMap::from([(d(2), 1.0), (d(3), 2.0), (d(4), 3.0)]);
        assert_eq!(align(&[a, b], 10), vec![vec![0.2, 0.3], vec![1.0, 2.0]]);
    }
}
