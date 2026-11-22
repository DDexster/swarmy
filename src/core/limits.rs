//! Weekly / five-hour limit windows and their attribution to projects.
//!
//! Anthropic reports only a utilization percentage per window. How the percentage is metered is
//! not published, so the attribution assumes it is proportional to API-equivalent cost; every
//! derived number is an estimate and is shown with "≈".

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use super::period::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum WindowKind {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
}

impl WindowKind {
    pub fn from_key(k: &str) -> Option<Self> {
        Some(match k {
            "five_hour" => WindowKind::FiveHour,
            "seven_day" => WindowKind::SevenDay,
            "seven_day_opus" => WindowKind::SevenDayOpus,
            "seven_day_sonnet" => WindowKind::SevenDaySonnet,
            _ => return None,
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            WindowKind::FiveHour => "five_hour",
            WindowKind::SevenDay => "seven_day",
            WindowKind::SevenDayOpus => "seven_day_opus",
            WindowKind::SevenDaySonnet => "seven_day_sonnet",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WindowKind::FiveHour => "5-hour",
            WindowKind::SevenDay => "weekly",
            WindowKind::SevenDayOpus => "weekly opus",
            WindowKind::SevenDaySonnet => "weekly sonnet",
        }
    }

    pub fn length(self) -> Duration {
        match self {
            WindowKind::FiveHour => Duration::hours(5),
            _ => Duration::days(7),
        }
    }
}

/// One utilization reading as Claude Code cached it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Reading {
    pub kind: WindowKind,
    /// Percent, 0..=100 (can exceed 100 with extra usage).
    pub utilization: f64,
    pub resets_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
}

impl Reading {
    pub fn window(&self) -> Span {
        Span {
            start: self.resets_at - self.kind.length(),
            end: self.resets_at,
        }
    }

    /// The window has rolled over since the reading was taken.
    pub fn is_reset(&self, now: DateTime<Utc>) -> bool {
        now >= self.resets_at
    }
}

/// The effective start of `cur`'s window, and the moment of a reset done by hand.
///
/// Claude Code can reset a 5-hour (and, sometimes, the weekly) limit early. Utilization then falls
/// to near zero inside the window, so spend before the reset must not count. A fall of more than
/// two points between consecutive readings of the same window marks a reset; it happened between
/// the two readings, and the earlier reading's time is taken as the start (a slight
/// over-count when readings are far apart).
pub fn effective_start(
    history: &[Reading],
    cur: &Reading,
) -> (DateTime<Utc>, Option<DateTime<Utc>>) {
    let nominal = cur.window().start;
    let mut hist: Vec<&Reading> = history
        .iter()
        .filter(|h| h.kind == cur.kind && h.fetched_at <= cur.fetched_at)
        .collect();
    if !hist.iter().any(|h| h.fetched_at == cur.fetched_at) {
        hist.push(cur);
    }
    hist.sort_by_key(|h| h.fetched_at);
    let mut start = nominal;
    let mut reset = None;
    for pair in hist.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let same_window = a.resets_at > b.fetched_at; // a's window had not rolled over yet
        if same_window && b.utilization + 2.0 < a.utilization && a.fetched_at > start {
            start = a.fetched_at;
            reset = Some(a.fetched_at);
        }
    }
    (start, reset)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Share {
    pub key: String,
    pub cost: f64,
    /// Share of the window's spend, 0..=1.
    pub share: f64,
    /// ≈ points of the limit this key consumed.
    pub limit_points: f64,
}

/// Splits `utilization` points between keys in proportion to their cost.
pub fn attribute(utilization: f64, costs: &[(String, f64)]) -> Vec<Share> {
    let total: f64 = costs.iter().map(|(_, c)| c.max(0.0)).sum();
    let mut out: Vec<Share> = costs
        .iter()
        .map(|(k, c)| {
            let share = if total > 0.0 { c.max(0.0) / total } else { 0.0 };
            Share {
                key: k.clone(),
                cost: *c,
                share,
                limit_points: utilization * share,
            }
        })
        .collect();
    out.sort_by(|a, b| b.cost.total_cmp(&a.cost));
    out
}

/// USD of API-equivalent spend per 1 point of utilization, from one reading.
pub fn usd_per_point(utilization: f64, cost_until_reading: f64) -> Option<f64> {
    (utilization >= 1.0 && cost_until_reading > 0.0).then(|| cost_until_reading / utilization)
}

/// Median of several `usd_per_point` estimates (one per stored reading).
pub fn calibrate(samples: &[f64]) -> Option<f64> {
    let mut v: Vec<f64> = samples
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x > 0.0)
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// Utilization now: the last reading plus what was spent since, converted at `usd_per_point`.
pub fn estimate_now(reading: f64, spent_since: f64, usd_per_point: Option<f64>) -> f64 {
    match usd_per_point {
        Some(r) if r > 0.0 => reading + spent_since / r,
        _ => reading,
    }
}

/// Time until the limit reaches 100% at the given burn rate (points per hour).
pub fn eta(current: f64, points_per_hour: f64) -> Option<Duration> {
    if current >= 100.0 {
        return Some(Duration::zero());
    }
    (points_per_hour > 1e-9)
        .then(|| Duration::seconds(((100.0 - current) / points_per_hour * 3600.0) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_by_hand() {
        // 77% used; ukroblik $810, car-anatomy $120, other $70 (total $1000).
        // ukroblik: 0.81 → 62.37 points; car-anatomy: 0.12 → 9.24; other: 0.07 → 5.39.
        let s = attribute(
            77.0,
            &[
                ("other".into(), 70.0),
                ("ukroblik".into(), 810.0),
                ("car-anatomy".into(), 120.0),
            ],
        );
        assert_eq!(s[0].key, "ukroblik");
        assert!((s[0].share - 0.81).abs() < 1e-12);
        assert!((s[0].limit_points - 62.37).abs() < 1e-9);
        assert!((s[1].limit_points - 9.24).abs() < 1e-9);
        assert!((s[2].limit_points - 5.39).abs() < 1e-9);
        let total: f64 = s.iter().map(|x| x.limit_points).sum();
        assert!((total - 77.0).abs() < 1e-9);
    }

    #[test]
    fn zero_spend_attributes_nothing() {
        let s = attribute(10.0, &[("a".into(), 0.0)]);
        assert_eq!(s[0].limit_points, 0.0);
    }

    #[test]
    fn calibration_and_estimate() {
        // $1000 spent for 50% → $20 per point. $100 more since then → +5 points.
        let r = usd_per_point(50.0, 1000.0).unwrap();
        assert_eq!(r, 20.0);
        assert_eq!(estimate_now(50.0, 100.0, Some(r)), 55.0);
        assert_eq!(calibrate(&[30.0, 10.0, 20.0]), Some(20.0));
        assert_eq!(calibrate(&[10.0, 20.0]), Some(15.0));
        assert_eq!(calibrate(&[]), None);
        // Below 1% the ratio is too noisy to trust.
        assert_eq!(usd_per_point(0.5, 10.0), None);
    }

    #[test]
    fn manual_reset_moves_the_window_start() {
        let t = |h: i64| {
            DateTime::parse_from_rfc3339("2026-11-20T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
                + Duration::hours(h)
        };
        let r = |h: i64, u: f64| Reading {
            kind: WindowKind::SevenDay,
            utilization: u,
            resets_at: t(100),
            fetched_at: t(h),
        };
        let hist = [r(10, 40.0), r(20, 70.0), r(22, 3.0), r(30, 12.0)];
        let (start, reset) = effective_start(&hist, &hist[3]);
        assert_eq!(start, t(20), "the reading before the fall");
        assert_eq!(reset, Some(t(20)));
        // Before the reset the nominal window applies.
        assert_eq!(
            effective_start(&hist, &hist[1]),
            (t(100) - Duration::days(7), None)
        );
        // A normal roll-over is not a reset.
        let next = Reading {
            kind: WindowKind::SevenDay,
            utilization: 1.0,
            resets_at: t(268),
            fetched_at: t(101),
        };
        let (s2, r2) = effective_start(&[hist[3], next], &next);
        assert_eq!((s2, r2), (t(100), None));
    }

    #[test]
    fn eta_by_hand() {
        // 70% now, 3 points per hour → 10 hours.
        assert_eq!(eta(70.0, 3.0), Some(Duration::hours(10)));
        assert_eq!(eta(100.0, 0.0), Some(Duration::zero()));
        assert_eq!(eta(50.0, 0.0), None);
    }
}
