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
    /// Every kind; the index's `CHECK` on `readings.kind` is generated from it.
    pub const ALL: [WindowKind; 4] = [
        WindowKind::FiveHour,
        WindowKind::SevenDay,
        WindowKind::SevenDayOpus,
        WindowKind::SevenDaySonnet,
    ];

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

/// How full a limit window is: one set of thresholds for every front end (dashboard colours,
/// CLI tones, status-line ANSI codes, alerts). Each front end maps a level to its own colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Level {
    /// Under 60%.
    Ok,
    /// From 60% to under 85%.
    Warn,
    /// From 85% to under 100%.
    Bad,
    /// 100% or more: "limit reached".
    Reached,
}

impl Level {
    pub const WARN_AT: f64 = 60.0;
    pub const BAD_AT: f64 = 85.0;
    pub const REACHED_AT: f64 = 100.0;

    pub fn of(pct: f64) -> Level {
        if pct >= Self::REACHED_AT {
            Level::Reached
        } else if pct >= Self::BAD_AT {
            Level::Bad
        } else if pct >= Self::WARN_AT {
            Level::Warn
        } else {
            Level::Ok
        }
    }
}

/// The window `r` belongs to, rolled forward to the one that contains `now` when the reading is
/// from an earlier window (the window keeps its length and cadence).
pub fn current_window(r: &Reading, now: DateTime<Utc>) -> Span {
    let mut w = r.window();
    while w.end <= now {
        w = Span {
            start: w.end,
            end: w.end + r.kind.length(),
        };
    }
    w
}

/// A reset happened between two analyses of the same kind of window: its (effective) start moved
/// forward, either because the window rolled over or because the limit was reset by hand (see
/// [`effective_start`]).
pub fn was_reset(before: Span, after: Span) -> bool {
    after.start > before.start
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
    if Level::of(current) == Level::Reached {
        return Some(Duration::zero());
    }
    (points_per_hour > 1e-9).then(|| {
        Duration::seconds(((Level::REACHED_AT - current) / points_per_hour * 3600.0) as i64)
    })
}

/// When the window runs out at the current pace: the [`eta`], kept only when it comes before the
/// window resets (after the reset the pace no longer matters).
pub fn runs_out(
    current: f64,
    points_per_hour: Option<f64>,
    now: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Option<Duration> {
    points_per_hour
        .and_then(|pph| eta(current, pph))
        .filter(|d| now + *d < window_end)
}

/// Where the window ends up at the current pace: utilization at its reset, uncapped (over 100
/// means it runs out first). `None` without a positive pace.
pub fn projection_at_reset(
    current: f64,
    points_per_hour: Option<f64>,
    now: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Option<f64> {
    points_per_hour.filter(|p| *p > 0.0).map(|pph| {
        let left = (window_end - now).num_seconds().max(0) as f64 / 3600.0;
        current + pph * left
    })
}

// ───────────────────────── estimating one window ─────────────────────────
//
// The rules below are pure: `crate::limits` queries the index for their inputs
// (`WindowInputs`) and turns the result into a report.

/// Burn rate is measured over the last this many minutes.
pub const BURN_MINUTES: i64 = 60;

/// Another account of the same plan tier serves as the reference ratio only when its USD per
/// point beats this account's own by more than this factor (20%): within that, the two differ by
/// noise, not by hidden usage.
pub const REFERENCE_MARGIN: f64 = 1.2;

/// At most this many recent readings (with distinct values) calibrate an account's ratio.
const CALIBRATION_READINGS: usize = 12;

/// Readings closer than this (points) count as one value when calibrating.
const CALIBRATION_STEP: f64 = 0.5;

/// Where a reading's window stands now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlace {
    /// The window that contains now; its start is moved to a reset by hand.
    pub window: Span,
    /// When the limit was reset by hand inside this window.
    pub manual_reset: Option<DateTime<Utc>>,
    /// The window has rolled over since the reading (a new window without a reading yet).
    pub reset_passed: bool,
}

/// The window of `r` now: rolled forward when it reset since the reading, else its nominal window
/// with the start moved to a reset by hand found in `history` (readings of the same kind).
pub fn window_of(r: &Reading, history: &[Reading], now: DateTime<Utc>) -> WindowPlace {
    let reset_passed = r.is_reset(now);
    let mut window = current_window(r, now);
    let mut manual_reset = None;
    if !reset_passed {
        let (start, reset) = effective_start(history, r);
        window.start = start;
        manual_reset = reset;
    }
    WindowPlace {
        window,
        manual_reset,
        reset_passed,
    }
}

/// What the estimate counts from: the reading and its time, or zero from the window start when the
/// window rolled over since the reading.
pub fn counting_from(r: &Reading, place: &WindowPlace) -> (f64, DateTime<Utc>) {
    if place.reset_passed {
        (0.0, place.window.start)
    } else {
        (r.utilization, r.fetched_at)
    }
}

/// The readings that calibrate an account's USD per point, each with the span whose spend it
/// reflects (from its effective window start to the reading): the most recent readings with
/// distinct values, newest first. `history` holds the stored readings of `r`'s kind, oldest first;
/// `r` is added when not stored yet.
pub fn calibration_points(history: &[Reading], r: &Reading) -> Vec<(Span, f64)> {
    let mut history = history.to_vec();
    if !history.iter().any(|h| h.fetched_at == r.fetched_at) {
        history.push(*r);
    }
    let mut recent: Vec<Reading> = Vec::new();
    for h in history.iter().rev() {
        if recent
            .last()
            .is_none_or(|l: &Reading| (l.utilization - h.utilization).abs() >= CALIBRATION_STEP)
        {
            recent.push(*h);
        }
        if recent.len() >= CALIBRATION_READINGS {
            break;
        }
    }
    recent
        .into_iter()
        .map(|h| {
            let (start, _) = effective_start(&history, &h);
            (
                Span {
                    start,
                    end: h.fetched_at,
                },
                h.utilization,
            )
        })
        .collect()
}

/// The account's own USD per point: the median over `(utilization, spend until it)` pairs.
pub fn own_ratio(points: &[(f64, f64)]) -> Option<f64> {
    let samples: Vec<f64> = points
        .iter()
        .filter_map(|(u, c)| usd_per_point(*u, *c))
        .collect();
    calibrate(&samples)
}

/// Whether a tier reference ratio replaces the account's own (see [`REFERENCE_MARGIN`]).
pub fn reference_wins(own: Option<f64>, reference: f64) -> bool {
    own.is_none_or(|o| reference > o * REFERENCE_MARGIN)
}

/// The step of a window's curve: about 170 points whatever its length, at least a minute.
pub fn curve_step(window: Span) -> Duration {
    Duration::minutes(((window.end - window.start).num_minutes() / 168).max(1))
}

/// Everything the estimate of one window needs, as queried from the index.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInputs {
    pub reading: Reading,
    pub place: WindowPlace,
    /// USD per point measured on this account alone ([`own_ratio`]).
    pub own_usd_per_point: Option<f64>,
    /// Spend since the point [`counting_from`] names.
    pub spent_since: f64,
    /// Spend from the window start until now.
    pub cost_in_window: f64,
    /// Spend over the last [`BURN_MINUTES`].
    pub cost_burn: f64,
    /// Spend in the window per project.
    pub by_project: Vec<(String, f64)>,
    /// Spend per [`curve_step`] from the window start (empty when the window rolled over).
    pub series: Vec<f64>,
    pub series_step: Duration,
}

/// The estimate of one window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowEstimate {
    /// USD per point used: the reference's when given, else the account's own.
    pub usd_per_point: Option<f64>,
    pub estimate_now: f64,
    /// ≈ points explained by spend visible on this machine.
    pub visible_points: Option<f64>,
    /// ≈ points from claude.ai, the desktop app, other computers…
    pub outside_points: Option<f64>,
    pub burn_usd_per_hour: f64,
    pub points_per_hour: Option<f64>,
    /// When it runs out at this pace, if before the reset ([`runs_out`]).
    pub eta: Option<Duration>,
    pub shares: Vec<Share>,
    /// ≈ API-equivalent dollars of a full window.
    pub window_usd: Option<f64>,
    /// Cumulative spend through the window, scaled to end at `estimate_now`.
    pub curve: Vec<f64>,
}

/// Estimates one window. `reference` is the tier reference ratio already chosen with
/// [`reference_wins`].
pub fn window_estimate(
    i: &WindowInputs,
    reference: Option<f64>,
    now: DateTime<Utc>,
) -> WindowEstimate {
    let usd_per_point = reference.or(i.own_usd_per_point);
    let (base, _) = counting_from(&i.reading, &i.place);
    let estimate_now = estimate_now(base, i.spent_since, usd_per_point);
    // What the spend seen here explains can not exceed the estimate; the rest is elsewhere.
    let visible_points = usd_per_point.map(|u| (i.cost_in_window / u).min(estimate_now));
    let outside_points = visible_points.map(|v| (estimate_now - v).max(0.0));
    let burn_usd_per_hour = i.cost_burn * 60.0 / BURN_MINUTES as f64;
    let points_per_hour = usd_per_point.map(|u| burn_usd_per_hour / u);
    let eta = runs_out(estimate_now, points_per_hour, now, i.place.window.end);
    let shares = attribute(visible_points.unwrap_or(estimate_now), &i.by_project);
    let total: f64 = i.series.iter().sum();
    let scale = if total > 0.0 {
        estimate_now / total
    } else {
        0.0
    };
    let mut acc = 0.0;
    let curve = i
        .series
        .iter()
        .map(|c| {
            acc += c;
            acc * scale
        })
        .collect();
    WindowEstimate {
        usd_per_point,
        estimate_now,
        visible_points,
        outside_points,
        burn_usd_per_hour,
        points_per_hour,
        eta,
        shares,
        window_usd: usd_per_point.map(|u| u * 100.0),
        curve,
    }
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

    #[test]
    fn levels_at_their_boundaries() {
        assert_eq!(Level::of(0.0), Level::Ok);
        assert_eq!(Level::of(59.99), Level::Ok);
        assert_eq!(Level::of(60.0), Level::Warn);
        assert_eq!(Level::of(84.99), Level::Warn);
        assert_eq!(Level::of(85.0), Level::Bad);
        assert_eq!(Level::of(99.99), Level::Bad);
        assert_eq!(Level::of(100.0), Level::Reached);
        assert_eq!(
            Level::of(130.0),
            Level::Reached,
            "extra usage goes past 100"
        );
        assert!(Level::Reached > Level::Bad && Level::Bad > Level::Warn && Level::Warn > Level::Ok);
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn current_window_rolls_forward() {
        // A weekly reading that resets on Mon 11-23 00:00: its window is 11-16 → 11-23.
        let r = Reading {
            kind: WindowKind::SevenDay,
            utilization: 40.0,
            resets_at: ts("2026-11-23T00:00:00Z"),
            fetched_at: ts("2026-11-20T00:00:00Z"),
        };
        let w = current_window(&r, ts("2026-11-22T12:00:00Z"));
        assert_eq!(
            (w.start, w.end),
            (ts("2026-11-16T00:00:00Z"), ts("2026-11-23T00:00:00Z"))
        );
        // At the reset itself the next window starts (end is exclusive).
        let w = current_window(&r, ts("2026-11-23T00:00:00Z"));
        assert_eq!(
            (w.start, w.end),
            (ts("2026-11-23T00:00:00Z"), ts("2026-11-30T00:00:00Z"))
        );
        // Two windows later: 11-30 → 12-07.
        let w = current_window(&r, ts("2026-12-01T09:00:00Z"));
        assert_eq!(
            (w.start, w.end),
            (ts("2026-11-30T00:00:00Z"), ts("2026-12-07T00:00:00Z"))
        );
        // A 5-hour window rolls by 5 hours: reset 10:00, now 16:30 → 15:00 → 20:00.
        let five = Reading {
            kind: WindowKind::FiveHour,
            resets_at: ts("2026-11-20T10:00:00Z"),
            ..r
        };
        let w = current_window(&five, ts("2026-11-20T16:30:00Z"));
        assert_eq!(
            (w.start, w.end),
            (ts("2026-11-20T15:00:00Z"), ts("2026-11-20T20:00:00Z"))
        );
    }

    #[test]
    fn reset_is_a_start_moving_forward() {
        let span = |a: &str, b: &str| Span {
            start: ts(a),
            end: ts(b),
        };
        let week = span("2026-11-16T00:00:00Z", "2026-11-23T00:00:00Z");
        assert!(!was_reset(week, week));
        // Rolled over.
        assert!(was_reset(
            week,
            span("2026-11-23T00:00:00Z", "2026-11-30T00:00:00Z")
        ));
        // Reset by hand on 11-19 06:00: same end, later start.
        assert!(was_reset(
            week,
            span("2026-11-19T06:00:00Z", "2026-11-23T00:00:00Z")
        ));
    }

    #[test]
    fn runs_out_and_projection_by_hand() {
        let now = ts("2026-11-20T12:00:00Z");
        let end = ts("2026-11-21T00:00:00Z"); // 12 h left
        // 70% at 2 points/h: runs out in 15 h, after the reset → no runs-out; at the reset
        // 70 + 2 × 12 = 94%.
        assert_eq!(runs_out(70.0, Some(2.0), now, end), None);
        assert_eq!(projection_at_reset(70.0, Some(2.0), now, end), Some(94.0));
        // 70% at 5 points/h: (100 − 70) / 5 = 6 h → before the reset; projection 70 + 60 = 130.
        assert_eq!(
            runs_out(70.0, Some(5.0), now, end),
            Some(Duration::hours(6))
        );
        assert_eq!(projection_at_reset(70.0, Some(5.0), now, end), Some(130.0));
        // No pace: no projection, and runs out only when already reached.
        assert_eq!(projection_at_reset(70.0, Some(0.0), now, end), None);
        assert_eq!(projection_at_reset(70.0, None, now, end), None);
        assert_eq!(runs_out(70.0, None, now, end), None);
        assert_eq!(runs_out(100.0, Some(0.0), now, end), Some(Duration::zero()));
    }

    // ── estimating one window ──

    fn hour(h: i64) -> DateTime<Utc> {
        ts("2026-11-20T00:00:00Z") + Duration::hours(h)
    }

    /// A weekly reading that resets at hour 100.
    fn wk(h: i64, used: f64) -> Reading {
        Reading {
            kind: WindowKind::SevenDay,
            utilization: used,
            resets_at: hour(100),
            fetched_at: hour(h),
        }
    }

    #[test]
    fn reference_wins_only_beyond_twenty_percent() {
        // No own ratio: any reference is better than nothing.
        assert!(reference_wins(None, 0.3));
        // Own $1.00 per point: a reference has to exceed $1.20.
        assert!(reference_wins(Some(1.0), 1.3));
        assert!(!reference_wins(Some(1.0), 1.2), "exactly 20% more is noise");
        assert!(!reference_wins(Some(1.0), 1.1));
        assert!(!reference_wins(Some(1.0), 0.5));
    }

    #[test]
    fn window_of_a_reading() {
        let hist = [wk(10, 40.0), wk(20, 70.0), wk(22, 3.0)];
        // Inside the window, after a reset by hand between hours 20 and 22: counts from 20.
        let p = window_of(&hist[2], &hist, hour(50));
        assert_eq!(p.window.start, hour(20));
        assert_eq!(p.window.end, hour(100));
        assert_eq!(p.manual_reset, Some(hour(20)));
        assert!(!p.reset_passed);
        assert_eq!(counting_from(&hist[2], &p), (3.0, hour(22)));
        // After the reset at hour 100: the next window, counting from zero at its start.
        let p = window_of(&hist[2], &hist, hour(120));
        assert!(p.reset_passed);
        assert_eq!(p.manual_reset, None);
        assert_eq!((p.window.start, p.window.end), (hour(100), hour(268)));
        assert_eq!(counting_from(&hist[2], &p), (0.0, hour(100)));
    }

    #[test]
    fn calibration_points_and_own_ratio_by_hand() {
        // Stored 10.0, 10.3, 11.0; the current 12.0 is not stored yet. Newest first: 12.0, 11.0,
        // 10.3; 10.0 is within half a point of 10.3 and is skipped. Each span runs from the
        // window start (hour 100 − 168 = hour −68) to its reading.
        let hist = [wk(1, 10.0), wk(2, 10.3), wk(3, 11.0)];
        let pts = calibration_points(&hist, &wk(4, 12.0));
        let start = hour(-68);
        let want: Vec<(Span, f64)> = [(4, 12.0), (3, 11.0), (2, 10.3)]
            .iter()
            .map(|&(h, u)| {
                (
                    Span {
                        start,
                        end: hour(h),
                    },
                    u,
                )
            })
            .collect();
        assert_eq!(pts, want);
        // At most 12 readings: 20 readings one point apart give the 12 newest.
        let many: Vec<Reading> = (1..=20).map(|h| wk(h, h as f64)).collect();
        let pts = calibration_points(&many, &many[19]);
        assert_eq!(pts.len(), 12);
        assert_eq!(pts[11].1, 9.0);
        // 10% after $4 → $0.40/pt; 30% after $14 → $14/30; 0.5% is too little to trust.
        // Median of the two: (12/30 + 14/30) / 2 = 13/30.
        let r = own_ratio(&[(10.0, 4.0), (30.0, 14.0), (0.5, 10.0)]).unwrap();
        assert!((r - 13.0 / 30.0).abs() < 1e-12);
        assert_eq!(own_ratio(&[]), None);
    }

    #[test]
    fn curve_step_keeps_about_170_points() {
        let span = |h: i64| Span {
            start: hour(0),
            end: hour(h),
        };
        // A week: 10 080 min / 168 = 60 min. Five hours: 300 / 168 = 1 min.
        assert_eq!(curve_step(span(168)), Duration::minutes(60));
        assert_eq!(curve_step(span(5)), Duration::minutes(1));
    }

    /// 30% read at hour 10; the window ends at hour 100 and now is hour 76 (24 h left).
    fn inputs() -> WindowInputs {
        let r = wk(10, 30.0);
        WindowInputs {
            reading: r,
            place: window_of(&r, &[r], hour(76)),
            own_usd_per_point: Some(0.5),
            spent_since: 5.0,
            cost_in_window: 15.0,
            cost_burn: 2.0,
            by_project: vec![("b".into(), 5.0), ("a".into(), 10.0)],
            series: vec![5.0, 0.0, 10.0],
            series_step: Duration::hours(1),
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn window_estimate_with_its_own_ratio() {
        let e = window_estimate(&inputs(), None, hour(76));
        // $0.50 per point: 30 + $5 / 0.5 = 40%.
        assert_eq!(e.usd_per_point, Some(0.5));
        assert!(near(e.estimate_now, 40.0));
        // $15 in the window explain 30 points; the other 10 come from elsewhere.
        assert!(near(e.visible_points.unwrap(), 30.0));
        assert!(near(e.outside_points.unwrap(), 10.0));
        // $2 in the last hour = 4 points/h: (100 − 40) / 4 = 15 h, before the reset in 24 h.
        assert!(near(e.burn_usd_per_hour, 2.0));
        assert!(near(e.points_per_hour.unwrap(), 4.0));
        assert_eq!(e.eta, Some(Duration::hours(15)));
        // The visible 30 points split $10 : $5 → a 20, b 10 (largest first).
        assert_eq!(e.shares[0].key, "a");
        assert!(near(e.shares[0].limit_points, 20.0));
        assert!(near(e.shares[1].limit_points, 10.0));
        // A full window ≈ 100 × $0.50 = $50.
        assert!(near(e.window_usd.unwrap(), 50.0));
        // Cumulative 5, 5, 15 scaled by 40/15: 13.33, 13.33, 40.
        assert!(near(e.curve[0], 40.0 / 3.0));
        assert!(near(e.curve[1], 40.0 / 3.0));
        assert!(near(e.curve[2], 40.0));
    }

    #[test]
    fn window_estimate_with_a_tier_reference() {
        // The reference $1.00 per point replaces the own $0.50: 30 + 5 / 1 = 35%; the $15 seen
        // here explain 15 points, 20 come from elsewhere; 2 points/h → (100 − 35) / 2 = 32.5 h,
        // after the reset in 24 h, so no runs-out time.
        let e = window_estimate(&inputs(), Some(1.0), hour(76));
        assert_eq!(e.usd_per_point, Some(1.0));
        assert!(near(e.estimate_now, 35.0));
        assert!(near(e.visible_points.unwrap(), 15.0));
        assert!(near(e.outside_points.unwrap(), 20.0));
        assert!(near(e.points_per_hour.unwrap(), 2.0));
        assert_eq!(e.eta, None);
        assert!(near(e.window_usd.unwrap(), 100.0));
    }

    #[test]
    fn window_estimate_after_the_reset() {
        // Rolled over at hour 100; now hour 120. Counting from 0 at hour 100: $6 at $2/pt = 3%.
        let r = wk(10, 30.0);
        let i = WindowInputs {
            place: window_of(&r, &[r], hour(120)),
            own_usd_per_point: Some(2.0),
            spent_since: 6.0,
            cost_in_window: 6.0,
            series: Vec::new(),
            ..inputs()
        };
        let e = window_estimate(&i, None, hour(120));
        assert!(near(e.estimate_now, 3.0));
        assert!(near(e.visible_points.unwrap(), 3.0));
        assert!(near(e.outside_points.unwrap(), 0.0));
        assert!(e.curve.is_empty());
    }

    #[test]
    fn window_estimate_with_nothing_known() {
        // No ratio and nothing spent: the reading itself, nothing derived.
        let i = WindowInputs {
            own_usd_per_point: None,
            spent_since: 0.0,
            cost_in_window: 0.0,
            cost_burn: 0.0,
            by_project: Vec::new(),
            series: Vec::new(),
            ..inputs()
        };
        let e = window_estimate(&i, None, hour(76));
        assert!(near(e.estimate_now, 30.0));
        assert_eq!(
            (e.usd_per_point, e.visible_points, e.outside_points),
            (None, None, None)
        );
        assert_eq!((e.points_per_hour, e.eta, e.window_usd), (None, None, None));
        assert!(e.shares.is_empty() && e.curve.is_empty());
        assert!(near(e.burn_usd_per_hour, 0.0));
    }
}
