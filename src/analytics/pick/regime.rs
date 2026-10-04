//! Trend and volatility regime from daily bars. Everything is computable as of any bar so the
//! same definitions drive the live read and the historical backtests.
//!
//! Trend: the average sign of seven time-series momentum and moving-average conditions, in
//! [-1, 1]. Time-series momentum (the sign of past returns predicting future returns) is one
//! of the better-documented effects across assets; the MA conditions are its usual proxies.
//!
//! Volatility: Yang-Zhang realized vol, an EWMA (λ = 0.94) estimate, and a horizon forecast
//! that mean-reverts the EWMA toward the one-year level.

use serde::{Deserialize, Serialize};

use crate::analytics::indicators::{ema, sma};
use crate::model::Bar;

pub const TRADING_DAYS: f64 = 252.0;

/// Precomputed series shared by the regime, level and backtest code.
pub struct Series<'a> {
    pub bars: &'a [Bar],
    pub close: Vec<f64>,
    pub ema20: Vec<f64>,
    pub sma50: Vec<f64>,
    pub sma100: Vec<f64>,
    pub sma200: Vec<f64>,
    /// Wilder ATR(14) as of each bar (`NaN` in warm-up).
    pub atr: Vec<f64>,
    /// Trend score as of each bar (`NaN` in warm-up).
    pub trend: Vec<f64>,
}

impl<'a> Series<'a> {
    pub fn new(bars: &'a [Bar]) -> Self {
        let close: Vec<f64> = bars.iter().map(|b| b.c).collect();
        let sma50 = sma(&close, 50);
        let sma200 = sma(&close, 200);
        let trend = (0..bars.len()).map(|i| trend_at(&close, &sma50, &sma200, i).0).collect();
        Self {
            ema20: ema(&close, 20),
            sma100: sma(&close, 100),
            atr: atr_series(bars, 14),
            trend,
            close,
            sma50,
            sma200,
            bars,
        }
    }
}

pub fn atr_series(bars: &[Bar], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; bars.len()];
    if bars.len() <= n {
        return out;
    }
    let tr = |i: usize| {
        let (b, p) = (&bars[i], bars[i - 1].c);
        (b.h - b.l).max((b.h - p).abs()).max((b.l - p).abs())
    };
    let mut a = (1..=n).map(tr).sum::<f64>() / n as f64;
    out[n] = a;
    for (i, o) in out.iter_mut().enumerate().skip(n + 1) {
        a = (a * (n - 1) as f64 + tr(i)) / n as f64;
        *o = a;
    }
    out
}

/// Trend score and the conditions behind it, as of bar `i`.
pub fn trend_at(close: &[f64], sma50: &[f64], sma200: &[f64], i: usize) -> (f64, Vec<(bool, String)>) {
    let c = close[i];
    let ok = |x: f64| x.is_finite();
    let ret = |n: usize, skip: usize| (i >= n).then(|| close[i - skip] / close[i - n] - 1.0);
    let mut sig: Vec<(bool, String)> = Vec::new();
    if ok(sma50[i]) {
        sig.push((
            c > sma50[i],
            format!("close {} 50d SMA {:.2}", if c > sma50[i] { "above" } else { "below" }, sma50[i]),
        ));
    }
    if ok(sma200[i]) {
        sig.push((
            c > sma200[i],
            format!("close {} 200d SMA {:.2}", if c > sma200[i] { "above" } else { "below" }, sma200[i]),
        ));
    }
    if ok(sma50[i]) && ok(sma200[i]) {
        let up = sma50[i] > sma200[i];
        sig.push((up, format!("50d SMA {} 200d SMA", if up { "above" } else { "below" })));
    }
    if i >= 20 && ok(sma50[i]) && ok(sma50[i - 20]) {
        let up = sma50[i] > sma50[i - 20];
        sig.push((up, format!("50d SMA {} over 20 days", if up { "rising" } else { "falling" })));
    }
    for (n, skip, name) in [(63, 0, "3-month"), (126, 0, "6-month"), (252, 21, "12-1 month")] {
        if let Some(r) = ret(n, skip) {
            sig.push((r > 0.0, format!("{name} return {:+.1}%", r * 100.0)));
        }
    }
    if sig.len() < 3 {
        return (f64::NAN, sig);
    }
    let score = sig.iter().map(|(up, _)| if *up { 1.0 } else { -1.0 }).sum::<f64>() / sig.len() as f64;
    (score, sig)
}

pub fn trend_label(score: f64) -> &'static str {
    match score {
        s if s >= 0.6 => "strong uptrend",
        s if s >= 0.3 => "uptrend",
        s if s > -0.3 => "no clear trend",
        s if s > -0.6 => "downtrend",
        _ => "strong downtrend",
    }
}

/// Yang-Zhang annualized volatility over the last `n` bars ending at `i`.
pub fn yang_zhang(bars: &[Bar], i: usize, n: usize) -> Option<f64> {
    if i < n || n < 2 {
        return None;
    }
    let w = &bars[i + 1 - n..=i];
    let prev = &bars[i - n..i];
    let o: Vec<f64> = w.iter().zip(prev).map(|(b, p)| (b.o / p.c).ln()).collect();
    let c: Vec<f64> = w.iter().map(|b| (b.c / b.o).ln()).collect();
    let rs: f64 =
        w.iter().map(|b| (b.h / b.c).ln() * (b.h / b.o).ln() + (b.l / b.c).ln() * (b.l / b.o).ln()).sum::<f64>()
            / n as f64;
    let var = |x: &[f64]| {
        let m = x.iter().sum::<f64>() / x.len() as f64;
        x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() - 1) as f64
    };
    let k = 0.34 / (1.34 + (n as f64 + 1.0) / (n as f64 - 1.0));
    let v = var(&o) + k * var(&c) + (1.0 - k) * rs;
    (v > 0.0 && v.is_finite()).then(|| (v * TRADING_DAYS).sqrt())
}

/// EWMA (RiskMetrics λ) annualized volatility of close-to-close log returns up to bar `i`.
pub fn ewma_vol(close: &[f64], i: usize, lambda: f64) -> Option<f64> {
    if i < 30 {
        return None;
    }
    let start = i.saturating_sub(250).max(1);
    let r: Vec<f64> = (start..=i).map(|j| (close[j] / close[j - 1]).ln()).collect();
    let mut v = r[..20].iter().map(|x| x * x).sum::<f64>() / 20.0;
    for x in &r[20..] {
        v = lambda * v + (1.0 - lambda) * x * x;
    }
    Some((v * TRADING_DAYS).sqrt())
}

/// Average annualized volatility expected over the next `h` trading days: the EWMA variance
/// decays toward the long-run (one-year) variance with daily persistence `phi`.
pub fn forecast_vol(ewma: f64, long_run: f64, h: f64, phi: f64) -> f64 {
    let (v0, lr) = (ewma * ewma, long_run * long_run);
    let h = h.max(1.0);
    let avg = lr + (v0 - lr) * (1.0 - phi.powf(h)) / (h * (1.0 - phi));
    avg.max(1e-8).sqrt()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regime {
    pub trend_score: f64,
    pub trend: String,
    /// The conditions behind the score, each with whether it is bullish.
    pub trend_signals: Vec<(bool, String)>,
    pub atr: f64,
    pub atr_pct: f64,
    pub rv20: Option<f64>,
    pub rv252: Option<f64>,
    pub ewma_vol: Option<f64>,
    /// Forecast average realized vol over the next 30 calendar (~21 trading) days.
    pub rv_forecast_30d: Option<f64>,
    pub iv30: Option<f64>,
    /// IV30 / forecast RV. Implied normally exceeds realized by ~10–20% (variance risk premium).
    pub iv_rv_ratio: Option<f64>,
    pub iv_rank: Option<f64>,
    pub iv_percentile: Option<f64>,
    pub iv_history_days: Option<usize>,
    /// cheap / fair / rich, from IV vs forecast RV (and IV rank when there is enough history).
    pub vol_pricing: String,
    pub vol_note: String,
}

pub fn regime(s: &Series, iv30: Option<f64>, iv_rank: Option<&crate::history::IvRank>) -> Option<Regime> {
    let i = s.bars.len().checked_sub(1)?;
    let (score, signals) = trend_at(&s.close, &s.sma50, &s.sma200, i);
    let atr = s.atr[i];
    if !score.is_finite() || !atr.is_finite() {
        return None;
    }
    let rv252 = yang_zhang(s.bars, i, 252.min(i));
    let ew = ewma_vol(&s.close, i, 0.94);
    let fc = match (ew, rv252) {
        (Some(e), Some(l)) => Some(forecast_vol(e, l, 21.0, 0.97)),
        (e, _) => e,
    };
    let ratio = iv30.zip(fc).map(|(iv, rv)| iv / rv);
    let rank_ok = iv_rank.filter(|r| r.days >= 60);
    let (pricing, note) = match ratio {
        Some(r) if r < 0.95 => {
            ("cheap", format!("IV {:.0}% is below forecast realized vol: options are cheap", iv30.unwrap() * 100.0))
        }
        Some(r) if r > 1.35 || rank_ok.is_some_and(|k| k.rank > 70.0) => (
            "rich",
            format!(
                "IV {:.0}% is {:.0}% above forecast realized vol: favor selling or spreading premium",
                iv30.unwrap() * 100.0,
                (r - 1.0) * 100.0
            ),
        ),
        Some(r) => (
            "fair",
            format!(
                "IV {:.0}% vs forecast realized {:.0}% (×{r:.2}): normal premium",
                iv30.unwrap() * 100.0,
                fc.unwrap() * 100.0
            ),
        ),
        None => ("unknown", "no implied vol available".into()),
    };
    Some(Regime {
        trend_score: score,
        trend: trend_label(score).into(),
        trend_signals: signals,
        atr,
        atr_pct: atr / s.close[i] * 100.0,
        rv20: yang_zhang(s.bars, i, 20),
        rv252,
        ewma_vol: ew,
        rv_forecast_30d: fc,
        iv30,
        iv_rv_ratio: ratio,
        iv_rank: iv_rank.map(|r| r.rank),
        iv_percentile: iv_rank.map(|r| r.percentile),
        iv_history_days: iv_rank.map(|r| r.days),
        vol_pricing: pricing.into(),
        vol_note: note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(f: impl Fn(usize) -> f64, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = f(i);
                Bar { t: i as i64 * 86_400, o: c, h: c * 1.01, l: c * 0.99, c, v: 1e6 }
            })
            .collect()
    }

    #[test]
    fn trend_sign_follows_drift() {
        let up = bars(|i| 100.0 * 1.002f64.powi(i as i32), 400);
        let s = Series::new(&up);
        assert!(*s.trend.last().unwrap() > 0.9);
        let down = bars(|i| 100.0 * 0.998f64.powi(i as i32), 400);
        let s = Series::new(&down);
        assert!(*s.trend.last().unwrap() < -0.9);
    }

    #[test]
    fn forecast_mean_reverts() {
        assert!((forecast_vol(0.2, 0.2, 21.0, 0.97) - 0.2).abs() < 1e-9);
        let f = forecast_vol(0.4, 0.2, 21.0, 0.97);
        assert!(f < 0.4 && f > 0.2);
        let far = forecast_vol(0.4, 0.2, 2000.0, 0.97);
        assert!((far - 0.2).abs() < 0.01);
    }

    #[test]
    fn yang_zhang_matches_known_vol() {
        // Deterministic alternating ±1% daily returns ≈ 15.9% annualized close-to-close.
        let b: Vec<Bar> = (0..60)
            .map(|i| {
                let c = if i % 2 == 0 { 100.0 } else { 101.0 };
                Bar { t: i, o: c, h: c, l: c, c, v: 1.0 }
            })
            .collect();
        let v = yang_zhang(&b, 59, 40).unwrap();
        assert!((v - 0.158).abs() < 0.02, "{v}");
    }
}
