//! Technical indicators. Outputs align with the input; warm-up values are `NaN`.

use chrono::{TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::Serialize;

use crate::model::Bar;

pub fn ema(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if n == 0 || values.len() < n {
        return out;
    }
    let k = 2.0 / (n as f64 + 1.0);
    let mut prev = values[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = prev;
    for i in n..values.len() {
        prev = values[i] * k + prev * (1.0 - k);
        out[i] = prev;
    }
    out
}

pub fn sma(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if n == 0 || values.len() < n {
        return out;
    }
    let mut sum: f64 = values[..n].iter().sum();
    out[n - 1] = sum / n as f64;
    for i in n..values.len() {
        sum += values[i] - values[i - n];
        out[i] = sum / n as f64;
    }
    out
}

/// Wilder's RSI.
pub fn rsi(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if n == 0 || values.len() <= n {
        return out;
    }
    let (mut gain, mut loss) = (0.0, 0.0);
    for i in 1..=n {
        let d = values[i] - values[i - 1];
        if d > 0.0 {
            gain += d;
        } else {
            loss -= d;
        }
    }
    gain /= n as f64;
    loss /= n as f64;
    let value = |g: f64, l: f64| if l == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + g / l) };
    out[n] = value(gain, loss);
    for i in n + 1..values.len() {
        let d = values[i] - values[i - 1];
        gain = (gain * (n - 1) as f64 + d.max(0.0)) / n as f64;
        loss = (loss * (n - 1) as f64 + (-d).max(0.0)) / n as f64;
        out[i] = value(gain, loss);
    }
    out
}

/// Regular-session VWAP, anchored at each 09:30 open; `NaN` for pre/post-market bars.
pub fn vwap(bars: &[Bar]) -> Vec<f64> {
    use super::market::{bar_session, Session};
    let mut out = Vec::with_capacity(bars.len());
    let (mut pv, mut vol, mut day) = (0.0, 0.0, None);
    for b in bars {
        if bar_session(b.t) != Session::Regular {
            out.push(f64::NAN);
            continue;
        }
        let d = Utc.timestamp_opt(b.t, 0).single().map(|t| t.with_timezone(&New_York).date_naive());
        if d != day {
            day = d;
            pv = 0.0;
            vol = 0.0;
        }
        let typical = (b.h + b.l + b.c) / 3.0;
        pv += typical * b.v;
        vol += b.v;
        out.push(if vol > 0.0 { pv / vol } else { typical });
    }
    out
}

pub struct Macd {
    pub macd: Vec<f64>,
    pub signal: Vec<f64>,
}

pub fn macd(values: &[f64]) -> Macd {
    let fast = ema(values, 12);
    let slow = ema(values, 26);
    let line: Vec<f64> = fast.iter().zip(&slow).map(|(a, b)| a - b).collect();
    let start = line.iter().position(|x| !x.is_nan()).unwrap_or(line.len());
    let mut signal = vec![f64::NAN; line.len()];
    let sig = ema(&line[start..], 9);
    signal[start..].copy_from_slice(&sig);
    Macd { macd: line, signal }
}

/// Latest indicator readings, for `--json` output and quick summaries.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub close: f64,
    pub ema9: Option<f64>,
    pub ema21: Option<f64>,
    pub ema50: Option<f64>,
    pub sma200: Option<f64>,
    pub rsi14: Option<f64>,
    pub vwap: Option<f64>,
    pub macd: Option<f64>,
    pub macd_signal: Option<f64>,
    pub trend: &'static str,
}

pub fn snapshot(bars: &[Bar], with_vwap: bool) -> Option<Snapshot> {
    let last = bars.last()?;
    let closes: Vec<f64> = bars.iter().map(|b| b.c).collect();
    let tail = |v: Vec<f64>| v.last().copied().filter(|x| !x.is_nan());
    let e9 = tail(ema(&closes, 9));
    let e21 = tail(ema(&closes, 21));
    let m = macd(&closes);
    let trend = match (e9, e21) {
        (Some(a), Some(b)) if last.c > a && a > b => "up",
        (Some(a), Some(b)) if last.c < a && a < b => "down",
        (Some(_), Some(_)) => "mixed",
        _ => "n/a",
    };
    Some(Snapshot {
        close: last.c,
        ema9: e9,
        ema21: e21,
        ema50: tail(ema(&closes, 50)),
        sma200: tail(sma(&closes, 200)),
        rsi14: tail(rsi(&closes, 14)),
        vwap: if with_vwap { tail(vwap(bars)) } else { None },
        macd: tail(m.macd),
        macd_signal: tail(m.signal),
        trend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_seeds_with_sma() {
        let e = ema(&[1.0, 2.0, 3.0, 4.0], 3);
        assert!(e[1].is_nan());
        assert_eq!(e[2], 2.0);
        assert_eq!(e[3], 3.0);
    }

    #[test]
    fn rsi_bounds() {
        let up: Vec<f64> = (0..30).map(|x| x as f64).collect();
        assert_eq!(*rsi(&up, 14).last().unwrap(), 100.0);
        let zig: Vec<f64> = (0..30).map(|x| if x % 2 == 0 { 1.0 } else { 2.0 }).collect();
        let r = *rsi(&zig, 14).last().unwrap();
        assert!((r - 50.0).abs() < 5.0, "{r}");
    }
}
