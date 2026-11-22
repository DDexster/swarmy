//! `swarmy limits`: where each account's limit went, and when it runs out.
//!
//! The utilization Anthropic reports covers everything the account did: Claude Code on this
//! machine, but also claude.ai, the desktop app and other computers, none of which leave
//! transcripts here. When two accounts share a plan tier, the one with the most API-equivalent
//! spend per point of utilization shows the least hidden usage; its ratio is used as the
//! reference for the others, and the gap is reported as usage outside this machine.

use std::collections::HashMap;

use chrono::{Duration, Local};
use serde::Serialize;

use crate::core::limits::{self, Reading, Share, WindowKind};
use crate::core::period::Span;
use crate::index::Index;
use crate::index::query::{Dim, Filter};
use crate::ui::fmt;
use crate::ui::style::Tone;
use crate::ui::table::{Align, Cell, Table};

use super::Format;
use super::context::Ctx;

#[derive(Debug, Clone, Serialize)]
pub struct WindowReport {
    pub reading: Reading,
    pub reset_passed: bool,
    pub window: Span,
    /// When the limit was reset by hand inside this window (Claude Code's limit reset).
    pub manual_reset: Option<chrono::DateTime<chrono::Utc>>,
    /// USD per point measured on this account alone.
    pub own_usd_per_point: Option<f64>,
    /// USD per point used for the estimates (own, or the tier reference).
    pub usd_per_point: Option<f64>,
    /// The account whose ratio served as the reference, when not this one.
    pub reference: Option<String>,
    pub cost_in_window: f64,
    pub estimate_now: f64,
    /// ≈ points explained by spend visible on this machine.
    pub visible_points: Option<f64>,
    /// ≈ points from claude.ai, the desktop app, other computers…
    pub outside_points: Option<f64>,
    pub burn_usd_per_hour: f64,
    pub points_per_hour: Option<f64>,
    #[serde(skip)]
    pub eta: Option<Duration>,
    pub eta_seconds: Option<i64>,
    pub shares: Vec<Share>,
    /// ≈ API-equivalent dollars of a full window.
    pub window_usd: Option<f64>,
    /// Estimated utilization through the window: cumulative spend, scaled to end at
    /// `estimate_now`; one point per `curve_step` from `window.start`.
    #[serde(skip)]
    pub curve: Vec<f64>,
    #[serde(skip)]
    pub curve_step: Duration,
    /// Utilization readings inside the window (what Claude Code really fetched).
    #[serde(skip)]
    pub marks: Vec<(chrono::DateTime<chrono::Utc>, f64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountReport {
    pub account_uuid: String,
    pub name: String,
    pub tier: Option<String>,
    pub windows: Vec<WindowReport>,
}

const BURN_MINUTES: i64 = 60;

fn current_window(r: &Reading, now: chrono::DateTime<chrono::Utc>) -> Span {
    let mut w = r.window();
    while w.end <= now {
        w = Span {
            start: w.end,
            end: w.end + r.kind.length(),
        };
    }
    w
}

/// Spend of an account over a span. Spans that ended more than five minutes ago do not change,
/// so they are remembered for the life of the process (the dashboard asks every few seconds),
/// keyed by the index file and the price fingerprint the costs were computed with: a reprice
/// (here or in another process) is never cached away, and a total with requests still waiting
/// for a price is not remembered at all.
fn total_cached(idx: &Index, account: &str, span: Span) -> anyhow::Result<f64> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Key = (String, String, String, i64, i64);
    static MEMO: OnceLock<Mutex<HashMap<Key, f64>>> = OnceLock::new();
    let settled = span.end < chrono::Utc::now() - Duration::minutes(5);
    let key = |fp: &str| {
        (
            idx.path().unwrap_or(":memory:").to_string(),
            fp.to_string(),
            account.to_string(),
            span.start.timestamp_millis(),
            span.end.timestamp_millis(),
        )
    };
    let memo = MEMO.get_or_init(Default::default);
    if settled
        && let Some(fp) = idx.meta("price_fp")?
        && let Some(v) = memo.lock().unwrap().get(&key(&fp))
    {
        return Ok(*v);
    }
    let filter = Filter {
        account: Some(account.to_string()),
        ..Default::default()
    };
    let (row, fp) = idx.total_priced(span, &filter)?;
    if settled
        && row.pending == 0
        && let Some(fp) = fp
    {
        let mut m = memo.lock().unwrap();
        if m.len() > 4096 {
            m.clear();
        }
        m.insert(key(&fp), row.cost);
    }
    Ok(row.cost)
}

/// USD per point from every stored reading of this account and window kind (median).
pub fn own_usd_per_point(idx: &Index, account: &str, r: &Reading) -> anyhow::Result<Option<f64>> {
    let mut samples = Vec::new();
    let mut history = idx.readings(account, r.kind)?;
    if !history.iter().any(|h| h.fetched_at == r.fetched_at) {
        history.push(*r);
    }
    // The most recent readings with distinct values are enough, and keep this cheap.
    let mut recent: Vec<Reading> = Vec::new();
    for h in history.iter().rev() {
        if recent
            .last()
            .is_none_or(|l: &Reading| (l.utilization - h.utilization).abs() >= 0.5)
        {
            recent.push(*h);
        }
        if recent.len() >= 12 {
            break;
        }
    }
    for h in recent {
        let (start, _) = limits::effective_start(&history, &h);
        let c = total_cached(
            idx,
            account,
            Span {
                start,
                end: h.fetched_at,
            },
        )?;
        if let Some(x) = limits::usd_per_point(h.utilization, c) {
            samples.push(x);
        }
    }
    Ok(limits::calibrate(&samples))
}

pub struct Reference {
    pub usd_per_point: f64,
    pub account: String,
}

pub fn analyze(
    ctx: &Ctx,
    idx: &Index,
    account: &str,
    tier: Option<String>,
    readings: &[Reading],
    refs: &HashMap<(String, WindowKind), Reference>,
) -> anyhow::Result<AccountReport> {
    let filter = Filter {
        account: Some(account.to_string()),
        ..Default::default()
    };
    let now_end = ctx.now + Duration::seconds(1);
    let mut windows = Vec::new();
    for r in readings {
        let reset_passed = r.is_reset(ctx.now);
        let mut window = current_window(r, ctx.now);
        let mut manual_reset = None;
        if !reset_passed {
            let history = idx.readings(account, r.kind)?;
            let (start, reset) = limits::effective_start(&history, r);
            window.start = start;
            manual_reset = reset;
        }
        let own = own_usd_per_point(idx, account, r)?;
        let reference = tier
            .as_ref()
            .and_then(|t| refs.get(&(t.clone(), r.kind)))
            .filter(|x| x.account != account && own.is_none_or(|o| x.usd_per_point > o * 1.2));
        let usd_per_point = reference.map(|x| x.usd_per_point).or(own);
        let (base, since) = if reset_passed {
            (0.0, window.start)
        } else {
            (r.utilization, r.fetched_at)
        };
        let spent_since = idx
            .total(
                Span {
                    start: since,
                    end: now_end,
                },
                &filter,
            )?
            .cost;
        let estimate_now = limits::estimate_now(base, spent_since, usd_per_point);
        let cost_in_window = idx
            .total(
                Span {
                    start: window.start,
                    end: now_end,
                },
                &filter,
            )?
            .cost;
        let visible_points = usd_per_point.map(|u| (cost_in_window / u).min(estimate_now));
        let outside_points = visible_points.map(|v| (estimate_now - v).max(0.0));
        let burn = idx
            .total(
                Span {
                    start: ctx.now - Duration::minutes(BURN_MINUTES),
                    end: now_end,
                },
                &filter,
            )?
            .cost
            * 60.0
            / BURN_MINUTES as f64;
        let points_per_hour = usd_per_point.map(|u| burn / u);
        let eta = points_per_hour
            .and_then(|pph| limits::eta(estimate_now, pph))
            .filter(|d| ctx.now + *d < window.end);
        let spend = idx.spend_by(
            Dim::Project,
            Span {
                start: window.start,
                end: now_end,
            },
            &filter,
        )?;
        let costs: Vec<(String, f64)> = spend
            .into_iter()
            .map(|row| {
                (
                    if ctx.mask {
                        ctx.show_path(&row.label)
                    } else {
                        row.label
                    },
                    row.cost,
                )
            })
            .collect();
        let shares = limits::attribute(visible_points.unwrap_or(estimate_now), &costs);
        // The curve of the window: about 170 steps whatever its length.
        let (curve, curve_step, marks) = if reset_passed {
            (Vec::new(), Duration::hours(1), Vec::new())
        } else {
            let step = Duration::minutes(((window.end - window.start).num_minutes() / 168).max(1));
            let series = idx.spend_series(
                Span {
                    start: window.start,
                    end: now_end,
                },
                step.num_minutes(),
                &filter,
            )?;
            let total: f64 = series.iter().sum();
            let scale = if total > 0.0 {
                estimate_now / total
            } else {
                0.0
            };
            let mut acc = 0.0;
            let curve = series
                .iter()
                .map(|c| {
                    acc += c;
                    acc * scale
                })
                .collect();
            let marks = idx
                .readings(account, r.kind)?
                .into_iter()
                .filter(|x| x.fetched_at >= window.start && x.fetched_at <= ctx.now)
                .map(|x| (x.fetched_at, x.utilization))
                .collect();
            (curve, step, marks)
        };
        windows.push(WindowReport {
            reading: *r,
            reset_passed,
            window,
            manual_reset,
            own_usd_per_point: own,
            usd_per_point,
            reference: reference.map(|x| ctx.account_name(&x.account)),
            cost_in_window,
            estimate_now,
            visible_points,
            outside_points,
            burn_usd_per_hour: burn,
            points_per_hour,
            eta,
            eta_seconds: eta.map(|d| d.num_seconds()),
            shares,
            window_usd: usd_per_point.map(|u| u * 100.0),
            curve,
            curve_step,
            marks,
        });
    }
    Ok(AccountReport {
        account_uuid: account.to_string(),
        name: ctx.account_name(account),
        tier,
        windows,
    })
}

fn tier_of(ctx: &Ctx, account: &str) -> Option<String> {
    if let Some(t) = ctx.login_tiers.get(account) {
        return Some(t.clone());
    }
    ctx.seat_info
        .iter()
        .filter_map(|s| s.account.as_ref())
        .find(|a| a.account_uuid == account)
        .and_then(|a| a.rate_limit_tier.clone())
}

/// Analyses every account with a cached reading, using cross-account references per tier.
pub fn all_reports(ctx: &Ctx, idx: &Index) -> anyhow::Result<Vec<AccountReport>> {
    // Every account ever read, not only those in a seat now: a login parked in the vault keeps
    // its last known limit and reset time.
    let mut merged: std::collections::BTreeMap<String, Vec<Reading>> =
        idx.latest_readings()?.into_iter().collect();
    for (acct, rs) in ctx.latest_readings() {
        let e = merged.entry(acct).or_default();
        for r in rs {
            match e.iter_mut().find(|x| x.kind == r.kind) {
                Some(x) if x.fetched_at < r.fetched_at => *x = r,
                Some(_) => {}
                None => e.push(r),
            }
        }
    }
    let readings: Vec<(String, Vec<Reading>)> = merged.into_iter().collect();
    let mut refs: HashMap<(String, WindowKind), Reference> = HashMap::new();
    for (acct, rs) in &readings {
        let Some(tier) = tier_of(ctx, acct) else {
            continue;
        };
        for r in rs {
            if let Some(u) = own_usd_per_point(idx, acct, r)? {
                let e = refs.entry((tier.clone(), r.kind)).or_insert(Reference {
                    usd_per_point: u,
                    account: acct.clone(),
                });
                if u > e.usd_per_point {
                    *e = Reference {
                        usd_per_point: u,
                        account: acct.clone(),
                    };
                }
            }
        }
    }
    let mut out = Vec::new();
    for (acct, rs) in readings {
        if !rs.is_empty() {
            out.push(analyze(ctx, idx, &acct, tier_of(ctx, &acct), &rs, &refs)?);
        }
    }
    Ok(out)
}

pub fn run(ctx: &Ctx, format: Format, top: usize) -> anyhow::Result<()> {
    let (idx, _) = ctx.index()?;
    let reports = all_reports(ctx, &idx)?;
    if format == Format::Json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }
    let p = &ctx.painter;
    if reports.is_empty() {
        println!("{}", p.paint("No limit readings yet: Claude Code caches them after it shows usage. Run /usage in Claude Code once.", Tone::Dim));
        return Ok(());
    }
    for (i, rep) in reports.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let email = ctx
            .email(&rep.account_uuid)
            .map(|e| format!(" · {e}"))
            .unwrap_or_default();
        let tier = rep
            .tier
            .as_deref()
            .map(|t| format!(" · {}", t.replace("default_claude_", "")))
            .unwrap_or_default();
        println!(
            "{} {}{}",
            p.paint("ACCOUNT", Tone::Header),
            p.paint(&rep.name, Tone::Bold),
            p.paint(&format!("{email}{tier}"), Tone::Dim)
        );
        for w in &rep.windows {
            print_window(ctx, w, top);
        }
    }
    println!();
    println!(
        "{}",
        p.paint(
            "≈ estimates: limit usage is assumed proportional to API-equivalent cost.",
            Tone::Dim
        )
    );
    Ok(())
}

fn print_window(ctx: &Ctx, w: &WindowReport, top: usize) {
    let p = &ctx.painter;
    let r = &w.reading;
    let shown = w.estimate_now;
    let tone = Tone::of_utilization(shown);
    let label = format!("{:<13}", r.kind.label());
    let main = if w.reset_passed {
        format!(
            "{} ≈{} (new window since {}, no reading yet)",
            fmt::gauge(shown / 100.0, 24),
            fmt::pct(shown),
            r.resets_at.with_timezone(&Local).format("%a %H:%M")
        )
    } else {
        format!(
            "{} {}  ≈{} now",
            fmt::gauge(shown / 100.0, 24),
            fmt::pct(r.utilization),
            fmt::pct(shown)
        )
    };
    println!("  {} {}", p.paint(&label, Tone::Bold), p.paint(&main, tone));
    let pad = " ".repeat(16);
    println!(
        "{pad}{}",
        p.paint(
            &format!(
                "resets {} (in {}) · reading {} old",
                w.window.end.with_timezone(&Local).format("%a %H:%M"),
                fmt::duration(w.window.end - ctx.now),
                fmt::duration(ctx.now - r.fetched_at)
            ),
            Tone::Dim
        )
    );
    if let Some(t) = w.manual_reset {
        println!(
            "{pad}{}",
            p.paint(
                &format!(
                    "limit reset by hand ≈{}; counting from there",
                    t.with_timezone(&Local).format("%a %H:%M")
                ),
                Tone::Accent
            )
        );
    }
    let mut burn = format!("burn {}/h", fmt::money(w.burn_usd_per_hour));
    if let Some(pph) = w.points_per_hour {
        burn.push_str(&format!(" ≈ {pph:.1} pts/h"));
    }
    match w.eta {
        Some(d) if d.num_seconds() == 0 => burn.push_str(" · LIMIT REACHED"),
        Some(d) => burn.push_str(&format!(
            " → empty {} (in {})",
            (ctx.now + d).with_timezone(&Local).format("%a %H:%M"),
            fmt::duration(d)
        )),
        None if w.burn_usd_per_hour > 0.0 => burn.push_str(" · lasts until reset"),
        None => {}
    }
    println!(
        "{pad}{}",
        p.paint(
            &burn,
            if w.eta.is_some() {
                Tone::Warn
            } else {
                Tone::Dim
            }
        )
    );
    if let (Some(u), Some(full)) = (w.usd_per_point, w.window_usd) {
        let src = match &w.reference {
            Some(r) => format!(" (reference: {r}, same plan)"),
            None => String::new(),
        };
        println!(
            "{pad}{}",
            p.paint(
                &format!(
                    "≈{}/pt · a full window ≈ {} API-equivalent{src}",
                    fmt::money(u),
                    fmt::money(full)
                ),
                Tone::Dim
            )
        );
    }
    if let (Some(v), Some(o)) = (w.visible_points, w.outside_points)
        && o >= 2.0
    {
        println!(
            "{pad}{}",
            p.paint(&format!("on this machine ≈{v:.1} pts · elsewhere (claude.ai, desktop, other computers) ≈{o:.1} pts"), Tone::Warn)
        );
    }
    if w.shares.is_empty() {
        return;
    }
    let mut t = Table::new(&[
        ("PROJECT", Align::Left),
        ("COST", Align::Right),
        ("SHARE", Align::Left),
        ("≈ LIMIT", Align::Right),
    ]);
    t.max_width = t.max_width.map(|m| m.saturating_sub(4));
    let max = w.shares.first().map(|s| s.share).unwrap_or(0.0);
    for s in w.shares.iter().take(top) {
        t.row(vec![
            Cell::new(s.key.clone()),
            Cell::new(fmt::money(s.cost)),
            Cell::tone(
                format!(
                    "{:<20} {}",
                    fmt::bar(if max > 0.0 { s.share / max } else { 0.0 }, 20),
                    fmt::pct(s.share * 100.0)
                ),
                Tone::Accent,
            ),
            Cell::tone(format!("≈{:.1} pts", s.limit_points), Tone::Bold),
        ]);
    }
    if w.shares.len() > top {
        let rest: Vec<&Share> = w.shares.iter().skip(top).collect();
        t.row(vec![
            Cell::tone(format!("… {} more", rest.len()), Tone::Dim),
            Cell::tone(fmt::money(rest.iter().map(|s| s.cost).sum()), Tone::Dim),
            Cell::new(""),
            Cell::tone(
                format!(
                    "≈{:.1} pts",
                    rest.iter().map(|s| s.limit_points).sum::<f64>()
                ),
                Tone::Dim,
            ),
        ]);
    }
    if let Some(o) = w.outside_points.filter(|o| *o >= 2.0) {
        t.row(vec![
            Cell::tone("(elsewhere)", Tone::Warn),
            Cell::tone("?", Tone::Dim),
            Cell::new(""),
            Cell::tone(format!("≈{o:.1} pts"), Tone::Warn),
        ]);
    }
    for line in t.render(p).lines() {
        println!("    {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::procs::ProcTable;
    use crate::config::{Config, Dirs};
    use crate::core::pricing::PriceBook;
    use crate::index::SeatInfo;
    use crate::index::scenarios::{World, account, assistant, at, owner, weekly};
    use crate::ui::style::{ColorChoice, Painter};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_weekly_window_by_hand() {
        // Account acc-a on seat "work", Claude Sonnet 5.5 ($2 per MTok of input), so 500 000
        // input tokens cost $1. The week runs 11-17 00:00 → 11-24 00:00 (UTC).
        let w = World::new(&["work"]);
        let m = "claude-sonnet-5-5";
        let req = |id: &str, ts: &str, cwd: &str, usd: u64| {
            assistant(id, ts, m, cwd, (usd * 500_000, 0), &[])
        };
        w.append(
            "work",
            "s1",
            &[
                owner("acc-a"),
                req("q1", "2026-11-17T06:00:00Z", "/w/shop", 4),
                req("q2", "2026-11-19T06:00:00Z", "/w/shop", 6),
                req("q3", "2026-11-19T08:00:00Z", "/w/docs", 4),
                req("q4", "2026-11-20T06:00:00Z", "/w/docs", 2),
                req("q5", "2026-11-20T11:30:00Z", "/w/shop", 3),
            ],
        );
        let resets = "2026-11-24T00:00:00Z";
        let seat = |used: f64, fetched: &str| SeatInfo {
            label: "work".into(),
            account: Some(account("acc-a", "2026-11-01T00:00:00Z")),
            utilization: Some(weekly("acc-a", fetched, used, resets)),
        };
        // Two readings stored over time: 10% at 11-18 00:00, 30% at 11-20 00:00.
        let mut idx = w.index("i.sqlite");
        w.refresh(
            &mut idx,
            &[seat(10.0, "2026-11-18T00:00:00Z")],
            &PriceBook::builtin(),
        );
        let now_seat = seat(30.0, "2026-11-20T00:00:00Z");
        w.refresh(
            &mut idx,
            std::slice::from_ref(&now_seat),
            &PriceBook::builtin(),
        );
        let reading = now_seat.utilization.as_ref().unwrap().readings[0];
        let ctx = Ctx {
            dirs: Dirs {
                data: w.dir.path().join("data"),
                config: w.dir.path().join("config"),
                home: w.dir.path().to_path_buf(),
            },
            config: Config::default(),
            seats: w.seats.clone(),
            seat_info: vec![now_seat],
            prices: PriceBook::builtin(),
            procs: ProcTable::empty(),
            painter: Painter::new(ColorChoice::Never),
            mask: false,
            now: at("2026-11-20T12:00:00Z"),
            login_labels: Default::default(),
            login_tiers: Default::default(),
        };
        let rep = analyze(&ctx, &idx, "acc-a", None, &[reading], &HashMap::new()).unwrap();
        let win = &rep.windows[0];
        assert!(!win.reset_passed);
        assert_eq!(win.window.start, at("2026-11-17T00:00:00Z"));
        assert_eq!(win.manual_reset, None);
        // $/point per reading, from the window start to the reading:
        //   10% at 11-18: q1 = $4            → 0.4
        //   30% at 11-20: q1 + q2 + q3 = $14 → 14/30
        // median of the two = (12/30 + 14/30) / 2 = 13/30.
        let upp = 13.0 / 30.0;
        assert!(close(win.own_usd_per_point.unwrap(), upp));
        assert!(close(win.usd_per_point.unwrap(), upp));
        assert_eq!(win.reference, None);
        // Now: 30 + (q4 + q5 = $5) / (13/30) = 30 + 150/13 = 540/13 ≈ 41.54%.
        assert!(
            close(win.estimate_now, 540.0 / 13.0),
            "{}",
            win.estimate_now
        );
        // In the window: 4 + 6 + 4 + 2 + 3 = $19; that explains 570/13 points, more than the
        // estimate, so everything is visible here and nothing comes from elsewhere.
        assert!(close(win.cost_in_window, 19.0));
        assert!(close(win.visible_points.unwrap(), 540.0 / 13.0));
        assert!(close(win.outside_points.unwrap(), 0.0));
        // Burn over the last hour: q5 = $3/h → 3 / (13/30) = 90/13 points per hour.
        assert!(close(win.burn_usd_per_hour, 3.0));
        assert!(close(win.points_per_hour.unwrap(), 90.0 / 13.0));
        // Empty in (100 − 540/13) / (90/13) = 760/90 h = 30 400 s, before the reset.
        let eta = win.eta_seconds.unwrap();
        assert!((30_399..=30_400).contains(&eta), "{eta}");
        // A full window ≈ 100 × 13/30 = $43.33.
        assert!(close(win.window_usd.unwrap(), 1300.0 / 30.0));
        // Shares of the visible 540/13 points: shop $13 of $19, docs $6 of $19.
        assert_eq!(win.shares.len(), 2);
        assert_eq!(win.shares[0].key, "shop");
        assert!(close(win.shares[0].cost, 13.0));
        assert!(close(win.shares[0].limit_points, 540.0 / 19.0));
        assert_eq!(win.shares[1].key, "docs");
        assert!(close(win.shares[1].limit_points, 3240.0 / 247.0));
    }
}
