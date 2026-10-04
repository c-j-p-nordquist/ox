//! Black-Scholes-Merton pricing, Greeks and implied volatility.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;

use crate::model::Right;

const SQRT_2PI: f64 = 2.506_628_274_631_000_7;
/// Floor on time to expiry (15 minutes) so gamma stays finite into the close.
pub const MIN_T: f64 = 15.0 / (60.0 * 24.0 * 365.0);

pub fn norm_pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / SQRT_2PI
}

/// Complementary error function, Numerical Recipes Chebyshev fit (|err| < 1.2e-7).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 {
        r
    } else {
        2.0 - r
    }
}

pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

/// Year fraction from `now` until 16:00 New York time on `expiry`, floored at [`MIN_T`].
pub fn years_to_expiry(expiry: NaiveDate, now: DateTime<Utc>) -> f64 {
    let close = expiry.and_hms_opt(16, 0, 0).unwrap();
    let close = New_York
        .from_local_datetime(&close)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&close));
    let secs = (close - now).num_seconds() as f64;
    (secs / (365.0 * 86_400.0)).max(MIN_T)
}

/// Option value at spot `s` with `t` years left; intrinsic at (or past) expiry.
pub fn value(right: Right, s: f64, k: f64, t: f64, r: f64, sigma: f64) -> f64 {
    if t <= MIN_T || sigma <= 0.0 {
        return match right {
            Right::Call => (s - k).max(0.0),
            Right::Put => (k - s).max(0.0),
        };
    }
    greeks(right, s, k, t, r, 0.0, sigma).price
}

/// Calendar days from today (New York) to expiry; 0 on expiration day.
pub fn dte(expiry: NaiveDate, now: DateTime<Utc>) -> i64 {
    (expiry - now.with_timezone(&New_York).date_naive()).num_days()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Greeks {
    pub price: f64,
    pub delta: f64,
    pub gamma: f64,
    /// Per calendar day.
    pub theta: f64,
    /// Per 1 vol point.
    pub vega: f64,
}

fn d1_d2(s: f64, k: f64, t: f64, r: f64, q: f64, sigma: f64) -> (f64, f64) {
    let vt = sigma * t.sqrt();
    let d1 = ((s / k).ln() + (r - q + 0.5 * sigma * sigma) * t) / vt;
    (d1, d1 - vt)
}

/// Gamma only; the hot path for GEX profiles.
#[inline]
pub fn gamma(s: f64, k: f64, t: f64, r: f64, q: f64, sigma: f64) -> f64 {
    if s <= 0.0 || k <= 0.0 || sigma <= 0.0 {
        return 0.0;
    }
    let (d1, _) = d1_d2(s, k, t, r, q, sigma);
    (-q * t).exp() * norm_pdf(d1) / (s * sigma * t.sqrt())
}

pub fn greeks(right: Right, s: f64, k: f64, t: f64, r: f64, q: f64, sigma: f64) -> Greeks {
    if s <= 0.0 || k <= 0.0 || sigma <= 0.0 || t <= 0.0 {
        return Greeks::default();
    }
    let (d1, d2) = d1_d2(s, k, t, r, q, sigma);
    let dq = (-q * t).exp();
    let dr = (-r * t).exp();
    let pdf = norm_pdf(d1);
    let gamma = dq * pdf / (s * sigma * t.sqrt());
    let vega = s * dq * pdf * t.sqrt() / 100.0;
    let common = -s * dq * pdf * sigma / (2.0 * t.sqrt());
    let (price, delta, theta_yr) = match right {
        Right::Call => (
            s * dq * norm_cdf(d1) - k * dr * norm_cdf(d2),
            dq * norm_cdf(d1),
            common - r * k * dr * norm_cdf(d2) + q * s * dq * norm_cdf(d1),
        ),
        Right::Put => (
            k * dr * norm_cdf(-d2) - s * dq * norm_cdf(-d1),
            -dq * norm_cdf(-d1),
            common + r * k * dr * norm_cdf(-d2) - q * s * dq * norm_cdf(-d1),
        ),
    };
    Greeks { price, delta, gamma, theta: theta_yr / 365.0, vega }
}

/// Implied volatility by bisection; `None` if the price is outside no-arbitrage bounds.
pub fn implied_vol(right: Right, price: f64, s: f64, k: f64, t: f64, r: f64, q: f64) -> Option<f64> {
    if price <= 0.0 || s <= 0.0 || k <= 0.0 {
        return None;
    }
    let (mut lo, mut hi) = (1e-4, 5.0);
    let f = |v: f64| greeks(right, s, k, t, r, q, v).price - price;
    if f(lo) > 0.0 || f(hi) < 0.0 {
        return None;
    }
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if f(mid) > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
        if hi - lo < 1e-6 {
            break;
        }
    }
    Some(0.5 * (lo + hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdf_is_accurate() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((norm_cdf(1.96) - 0.975_002_1).abs() < 1e-6);
        assert!((norm_cdf(-1.0) - 0.158_655_3).abs() < 1e-6);
    }

    #[test]
    fn textbook_call() {
        // Hull: S=42, K=40, r=10%, sigma=20%, T=0.5 -> call 4.76, put 0.81
        let c = greeks(Right::Call, 42.0, 40.0, 0.5, 0.10, 0.0, 0.20);
        let p = greeks(Right::Put, 42.0, 40.0, 0.5, 0.10, 0.0, 0.20);
        assert!((c.price - 4.76).abs() < 0.01);
        assert!((p.price - 0.81).abs() < 0.01);
        assert!((c.delta - 0.779).abs() < 0.001);
        assert!((c.gamma - gamma(42.0, 40.0, 0.5, 0.10, 0.0, 0.20)).abs() < 1e-12);
    }

    #[test]
    fn iv_round_trip() {
        let px = greeks(Right::Put, 100.0, 95.0, 0.25, 0.04, 0.0, 0.33).price;
        let iv = implied_vol(Right::Put, px, 100.0, 95.0, 0.25, 0.04, 0.0).unwrap();
        assert!((iv - 0.33).abs() < 1e-4);
    }
}
