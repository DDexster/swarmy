//! Limit analysis: where each account's limit went, and when it runs out. Used by
//! `swarmy limits`, `swarmy accounts`, the dashboard and (through `fleet`) the status line.
//!
//! The utilization Anthropic reports covers everything the account did: Claude Code on this
//! machine, but also claude.ai, the desktop app and other computers, none of which leave
//! transcripts here. When two accounts share a plan tier, the one with the most API-equivalent
//! spend per point of utilization shows the least hidden usage; its ratio is used as the
//! reference for the others, and the gap is reported as usage outside this machine.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::app::context::Ctx;
use crate::core::limits::{self, Level, Reading, Share, WindowInputs, WindowKind};
use crate::core::period::Span;
use crate::core::units::{Pct, Usd, UsdPerPct};
use crate::index::Index;
use crate::index::query::{Dim, Filter};

#[derive(Debug, Clone, Serialize)]
pub struct WindowReport {
    pub reading: Reading,
    pub reset_passed: bool,
    pub window: Span,
    /// When the limit was reset by hand inside this window (Claude Code's limit reset).
    pub manual_reset: Option<chrono::DateTime<chrono::Utc>>,
    /// USD per point measured on this account alone.
    pub own_usd_per_point: Option<UsdPerPct>,
    /// USD per point used for the estimates (own, or the tier reference).
    pub usd_per_point: Option<UsdPerPct>,
    /// The account whose ratio served as the reference, when not this one.
    pub reference: Option<String>,
    pub cost_in_window: Usd,
    pub estimate_now: Pct,
    /// ≈ points explained by spend visible on this machine.
    pub visible_points: Option<Pct>,
    /// ≈ points from claude.ai, the desktop app, other computers…
    pub outside_points: Option<Pct>,
    /// Dollars per hour.
    pub burn_usd_per_hour: Usd,
    /// Points per hour.
    pub points_per_hour: Option<Pct>,
    #[serde(skip)]
    pub eta: Option<Duration>,
    pub eta_seconds: Option<i64>,
    pub shares: Vec<Share>,
    /// ≈ API-equivalent dollars of a full window.
    pub window_usd: Option<Usd>,
    /// Estimated utilization through the window: cumulative spend, scaled to end at
    /// `estimate_now`; one point per `curve_step` from `window.start`.
    #[serde(skip)]
    pub curve: Vec<Pct>,
    #[serde(skip)]
    pub curve_step: Duration,
    /// Utilization readings inside the window (what Claude Code really fetched).
    #[serde(skip)]
    pub marks: Vec<(chrono::DateTime<chrono::Utc>, f64)>,
}

impl WindowReport {
    /// How full the window is now (by the estimate).
    pub fn level(&self) -> Level {
        Level::of(self.estimate_now.get())
    }

    /// Utilization at the reset at the current pace (see `core::limits::projection_at_reset`).
    pub fn projection_at_reset(&self, now: chrono::DateTime<chrono::Utc>) -> Option<Pct> {
        limits::projection_at_reset(
            self.estimate_now.get(),
            self.points_per_hour.map(Pct::get),
            now,
            self.window.end,
        )
        .map(Pct::new)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountReport {
    pub account_uuid: String,
    pub name: String,
    pub tier: Option<String>,
    pub windows: Vec<WindowReport>,
}

/// Utilization readings inside a window: what Claude Code really fetched, for the chart.
type Marks = Vec<(DateTime<Utc>, f64)>;

/// Queries what [`limits::window_estimate`] needs for one reading of an account.
/// `history` holds the stored readings of the reading's kind, oldest first.
fn load_window(
    ctx: &Ctx,
    idx: &Index,
    account: &str,
    r: &Reading,
    history: &[Reading],
) -> anyhow::Result<(WindowInputs, Marks)> {
    let filter = Filter {
        account: Some(crate::core::ids::AccountId::new(account)),
        ..Default::default()
    };
    let now_end = ctx.now + Duration::seconds(1);
    let until_now = |start| Span {
        start,
        end: now_end,
    };
    let place = limits::window_of(r, history, ctx.now);
    let window = place.window;
    let mut points = Vec::new();
    for (span, utilization) in limits::calibration_points(history, r) {
        let spent = Usd::new(idx.settled_total(account, span, ctx.now)?);
        points.push((Pct::new(utilization), spent));
    }
    let (_, since) = limits::counting_from(r, &place);
    // The index sums dollars as numbers; they become `Usd` here, at the edge of the rules.
    let spent_since = Usd::new(idx.total(until_now(since), &filter)?.cost);
    let cost_in_window = Usd::new(idx.total(until_now(window.start), &filter)?.cost);
    let cost_burn = Usd::new(
        idx.total(
            until_now(ctx.now - Duration::minutes(limits::BURN_MINUTES)),
            &filter,
        )?
        .cost,
    );
    let by_project = idx
        .spend_by(Dim::Project, until_now(window.start), &filter)?
        .into_iter()
        .map(|row| {
            (
                if ctx.mask {
                    ctx.show_path(&row.label)
                } else {
                    row.label
                },
                Usd::new(row.cost),
            )
        })
        .collect();
    let (series, series_step, marks) = if place.reset_passed {
        (Vec::new(), Duration::hours(1), Vec::new())
    } else {
        let step = limits::curve_step(window);
        let series = idx
            .spend_series(until_now(window.start), step.num_minutes(), &filter)?
            .into_iter()
            .map(Usd::new)
            .collect();
        let marks = history
            .iter()
            .filter(|x| x.fetched_at >= window.start && x.fetched_at <= ctx.now)
            .map(|x| (x.fetched_at, x.utilization))
            .collect();
        (series, step, marks)
    };
    let inputs = WindowInputs {
        reading: *r,
        place,
        own_usd_per_point: limits::own_ratio(&points),
        spent_since,
        cost_in_window,
        cost_burn,
        by_project,
        series,
        series_step,
    };
    Ok((inputs, marks))
}

/// The best own ratio per plan tier and window kind: the account with the least hidden usage.
pub struct Reference {
    pub usd_per_point: UsdPerPct,
    pub account: String,
}

fn report_window(
    ctx: &Ctx,
    i: WindowInputs,
    marks: Marks,
    reference: Option<&Reference>,
) -> WindowReport {
    let e = limits::window_estimate(&i, reference.map(|x| x.usd_per_point), ctx.now);
    WindowReport {
        reading: i.reading,
        reset_passed: i.place.reset_passed,
        window: i.place.window,
        manual_reset: i.place.manual_reset,
        own_usd_per_point: i.own_usd_per_point,
        usd_per_point: e.usd_per_point,
        reference: reference.map(|x| ctx.account_name(&x.account)),
        cost_in_window: i.cost_in_window,
        estimate_now: e.estimate_now,
        visible_points: e.visible_points,
        outside_points: e.outside_points,
        burn_usd_per_hour: e.burn_usd_per_hour,
        points_per_hour: e.points_per_hour,
        eta: e.eta,
        eta_seconds: e.eta.map(|d| d.num_seconds()),
        shares: e.shares,
        window_usd: e.window_usd,
        curve: e.curve,
        curve_step: i.series_step,
        marks,
    }
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

/// Analyses every account with a reading ([`Ctx::latest_readings`]), using cross-account
/// references per tier: first the queries for every window, then the pure estimates.
pub fn all_reports(ctx: &Ctx, idx: &Index) -> anyhow::Result<Vec<AccountReport>> {
    type Loaded = (
        String,
        Option<String>,
        Vec<(WindowInputs, Vec<(DateTime<Utc>, f64)>)>,
    );
    let mut loaded: Vec<Loaded> = Vec::new();
    for (acct, rs) in ctx.latest_readings(idx)? {
        if rs.is_empty() {
            continue;
        }
        let mut windows = Vec::new();
        for r in &rs {
            let history = idx.readings(&acct, r.kind)?;
            windows.push(load_window(ctx, idx, &acct, r, &history)?);
        }
        let tier = tier_of(ctx, &acct);
        loaded.push((acct, tier, windows));
    }
    let mut refs: HashMap<(String, WindowKind), Reference> = HashMap::new();
    for (acct, tier, windows) in &loaded {
        let Some(tier) = tier else { continue };
        for (i, _) in windows {
            let Some(u) = i.own_usd_per_point else {
                continue;
            };
            let e = refs
                .entry((tier.clone(), i.reading.kind))
                .or_insert(Reference {
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
    Ok(loaded
        .into_iter()
        .map(|(acct, tier, windows)| {
            let windows = windows
                .into_iter()
                .map(|(i, marks)| {
                    let reference = tier
                        .as_ref()
                        .and_then(|t| refs.get(&(t.clone(), i.reading.kind)))
                        .filter(|x| {
                            x.account != acct
                                && limits::reference_wins(i.own_usd_per_point, x.usd_per_point)
                        });
                    report_window(ctx, i, marks, reference)
                })
                .collect();
            AccountReport {
                name: ctx.account_name(&acct),
                account_uuid: acct,
                tier,
                windows,
            }
        })
        .collect())
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

    fn close(a: impl Into<f64>, b: impl Into<f64>) -> bool {
        let (a, b): (f64, f64) = (a.into(), b.into());
        (a - b).abs() < 1e-9
    }

    /// A context over the world's seats with these seat readings, at `now`.
    fn ctx_of(w: &World, seat_info: Vec<SeatInfo>, now: &str) -> Ctx {
        Ctx {
            dirs: Dirs {
                data: w.dir.path().join("data"),
                config: w.dir.path().join("config"),
                home: w.dir.path().to_path_buf(),
            },
            config: Config::default(),
            seats: w.seats.clone(),
            seat_info,
            prices: PriceBook::builtin(),
            procs: ProcTable::empty(),
            painter: Painter::new(ColorChoice::Never),
            mask: false,
            now: at(now),
            login_labels: Default::default(),
            login_tiers: Default::default(),
            problems: Vec::new(),
        }
    }

    #[test]
    fn every_surface_starts_from_the_latest_reading_of_an_account() {
        // Seen on real data: the seat still caches an old 82% reading of its account, while a
        // newer 8% reading (after a reset by hand) is in the index, recorded earlier from another
        // seat or the status line. `swarmy accounts` used the seat's 82%, `swarmy limits` and the
        // dashboard the 8%. Now all of them read the 8%.
        let w = World::new(&["work"]);
        let resets = "2026-11-24T00:00:00Z";
        let seat = |used: f64, fetched: &str| SeatInfo {
            seat: Default::default(),
            label: "work".into(),
            account: Some(account("acc-a", "2026-11-01T00:00:00Z")),
            utilization: Some(weekly("acc-a", fetched, used, resets)),
        };
        let mut idx = w.index("i.sqlite");
        w.refresh(
            &mut idx,
            &[seat(82.0, "2026-11-18T00:00:00Z")],
            &PriceBook::builtin(),
        );
        w.refresh(
            &mut idx,
            &[seat(8.0, "2026-11-20T00:00:00Z")],
            &PriceBook::builtin(),
        );
        let ctx = ctx_of(
            &w,
            vec![seat(82.0, "2026-11-18T00:00:00Z")],
            "2026-11-20T12:00:00Z",
        );
        let latest = ctx.latest_readings(&idx).unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].0, "acc-a");
        assert_eq!(latest[0].1[0].utilization, 8.0);
        assert_eq!(latest[0].1[0].fetched_at, at("2026-11-20T00:00:00Z"));
        // The analysis (what every surface shows): no spend recorded, so the estimate is the
        // reading itself, 8%, in the window that resets 11-24.
        let reports = all_reports(&ctx, &idx).unwrap();
        let week = &reports[0].windows[0];
        assert_eq!(week.reading.utilization, 8.0);
        assert!(close(week.estimate_now, 8.0), "{}", week.estimate_now);
        assert_eq!(week.window.end, at(resets));
        // A seat reading newer than the stored ones wins.
        let ctx = ctx_of(
            &w,
            vec![seat(11.0, "2026-11-20T06:00:00Z")],
            "2026-11-20T12:00:00Z",
        );
        assert_eq!(ctx.latest_readings(&idx).unwrap()[0].1[0].utilization, 11.0);
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
            seat: Default::default(),
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
        let ctx = ctx_of(&w, vec![now_seat], "2026-11-20T12:00:00Z");
        let rep = all_reports(&ctx, &idx).unwrap().remove(0);
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

    #[test]
    fn settled_spend_follows_late_requests_reprices_and_reattribution() {
        // The weekly window of `a_weekly_window_by_hand`, calibrated on two readings: 10% at
        // 11-18 ($4 before it) and 30% at 11-20 ($14 before it).
        let w = World::new(&["work"]);
        let m = "claude-sonnet-5-5";
        let req =
            |id: &str, ts: &str, usd: u64| assistant(id, ts, m, "/w/shop", (usd * 500_000, 0), &[]);
        w.append(
            "work",
            "s1",
            &[
                owner("acc-a"),
                req("q1", "2026-11-17T06:00:00Z", 4),
                req("q2", "2026-11-19T06:00:00Z", 6),
                req("q3", "2026-11-19T08:00:00Z", 4),
            ],
        );
        let resets = "2026-11-24T00:00:00Z";
        let seat = |used: f64, fetched: &str| SeatInfo {
            seat: Default::default(),
            label: "work".into(),
            account: Some(account("acc-a", "2026-11-01T00:00:00Z")),
            utilization: Some(weekly("acc-a", fetched, used, resets)),
        };
        let mut idx = w.index("i.sqlite");
        let book = PriceBook::builtin();
        w.refresh(&mut idx, &[seat(10.0, "2026-11-18T00:00:00Z")], &book);
        let now_seat = seat(30.0, "2026-11-20T00:00:00Z");
        w.refresh(&mut idx, std::slice::from_ref(&now_seat), &book);
        let ctx = ctx_of(&w, vec![now_seat.clone()], "2026-11-20T12:00:00Z");
        let own = |idx: &Index| {
            all_reports(&ctx, idx).unwrap()[0].windows[0]
                .own_usd_per_point
                .unwrap()
        };
        // median(4/10, 14/30) = 13/30.
        assert!(close(own(&idx), 13.0 / 30.0));
        // The same again comes from the memo (and agrees).
        assert!(close(own(&idx), 13.0 / 30.0));

        // A request of 11-17 indexed late (a file found by the full scan): +$2 before both
        // readings → median(6/10, 16/30) = (18/30 + 16/30) / 2 = 17/30.
        w.append(
            "work",
            "s2",
            &[owner("acc-a"), req("late", "2026-11-17T09:00:00Z", 2)],
        );
        w.refresh(&mut idx, std::slice::from_ref(&now_seat), &book);
        assert!(close(own(&idx), 17.0 / 30.0), "{}", own(&idx));

        // Sonnet 5.5 repriced at $4 per MTok of input: every dollar doubles → 34/30.
        let mut dear = PriceBook::builtin();
        dear.push_override(
            m,
            None,
            4.0,
            20.0,
            &crate::core::pricing::RateOverrides::default(),
            at("2026-11-20T00:00:00Z").date_naive(),
        )
        .unwrap();
        idx.price_all(&dear).unwrap();
        assert!(close(own(&idx), 34.0 / 30.0), "{}", own(&idx));

        // The seat turns out to have had acc-b logged in since 11-19 00:00: q2, q3 move to acc-b.
        // acc-a keeps q1 + late = $6 × 2 = $12 before both readings → median(12/10, 12/30) =
        // (36/30 + 12/30) / 2 = 24/30.
        let swapped = SeatInfo {
            account: Some(account("acc-b", "2026-11-19T00:00:00Z")),
            utilization: None,
            ..now_seat
        };
        w.refresh(&mut idx, &[swapped], &dear);
        assert!(close(own(&idx), 24.0 / 30.0), "{}", own(&idx));
    }

    #[test]
    fn report_week_limits_and_the_dashboard_share_the_window_in_force() {
        // 70% at 11-19 00:00, reset by hand, 3% at 11-19 02:00: `swarmy limits` counts from the
        // reset; `report --period week` (and the dashboard's period) must count from there too.
        let w = World::new(&["work"]);
        let resets = "2026-11-24T00:00:00Z";
        let seat = |used: f64, fetched: &str| SeatInfo {
            seat: Default::default(),
            label: "work".into(),
            account: Some(account("acc-a", "2026-11-01T00:00:00Z")),
            utilization: Some(weekly("acc-a", fetched, used, resets)),
        };
        let mut idx = w.index("i.sqlite");
        let book = PriceBook::builtin();
        for (used, fetched) in [
            (40.0, "2026-11-18T00:00:00Z"),
            (70.0, "2026-11-19T00:00:00Z"),
            (3.0, "2026-11-19T02:00:00Z"),
        ] {
            w.refresh(&mut idx, &[seat(used, fetched)], &book);
        }
        let ctx = ctx_of(
            &w,
            vec![seat(3.0, "2026-11-19T02:00:00Z")],
            "2026-11-20T12:00:00Z",
        );
        let limits_window = all_reports(&ctx, &idx).unwrap()[0].windows[0].window;
        assert_eq!(limits_window.start, at("2026-11-19T00:00:00Z"));
        for account in [Some("acc-a".into()), None] {
            let week = ctx.limit_week(&idx, account.as_ref()).unwrap().unwrap();
            assert_eq!(week.start, limits_window.start, "{account:?}");
            assert_eq!(week.end, ctx.now + Duration::seconds(1));
        }
        let span = crate::core::period::Period::LimitWeek
            .span(ctx.now, ctx.limit_week(&idx, None).unwrap());
        assert_eq!(span.start, at("2026-11-19T00:00:00Z"));
    }

    /// Review H7: `--account` takes a uuid, an email or a saved login's label and resolves it once,
    /// so `report --account <email> --period week` counts the same window and spend as the uuid.
    /// A name that fits two accounts is an error listing both; an unknown one says what to give.
    #[test]
    fn an_account_named_any_way_gives_the_same_week_and_totals() {
        let w = World::new(&["work", "home"]);
        let resets = "2026-11-24T00:00:00Z";
        let acct = |uuid: &str, email: &str| crate::claude::config_json::Account {
            email: Some(email.into()),
            ..account(uuid, "2026-11-01T00:00:00Z")
        };
        let infos = |used: f64, fetched: &str| {
            vec![
                SeatInfo {
                    seat: w.id("work"),
                    label: "work".into(),
                    account: Some(acct("acc-a", "a@example.com")),
                    utilization: Some(weekly("acc-a", fetched, used, resets)),
                },
                SeatInfo {
                    seat: w.id("home"),
                    label: "home".into(),
                    account: Some(acct("acc-b", "b@example.com")),
                    utilization: Some(weekly("acc-b", fetched, 10.0, resets)),
                },
            ]
        };
        let m = "claude-sonnet-5-5";
        w.append(
            "work",
            "s1",
            &[
                owner("acc-a"),
                assistant(
                    "q1",
                    "2026-11-18T12:00:00Z",
                    m,
                    "/w/shop",
                    (1_000_000, 0),
                    &[],
                ),
                assistant(
                    "q2",
                    "2026-11-19T12:00:00Z",
                    m,
                    "/w/shop",
                    (1_000_000, 0),
                    &[],
                ),
            ],
        );
        let mut idx = w.index("i.sqlite");
        let book = PriceBook::builtin();
        for (used, fetched) in [
            (70.0, "2026-11-19T00:00:00Z"),
            (3.0, "2026-11-19T02:00:00Z"),
        ] {
            w.refresh(&mut idx, &infos(used, fetched), &book);
        }
        let mut ctx = ctx_of(
            &w,
            infos(3.0, "2026-11-19T02:00:00Z"),
            "2026-11-20T12:00:00Z",
        );
        // A saved login names acc-a too.
        {
            let mut v = crate::vault::Vault::open(&ctx.dirs.vault_dir()).unwrap();
            let secret = crate::claude::credentials::Secret::new(
                serde_json::json!({"accessToken": "a", "refreshToken": "r-a"}),
            );
            v.edit(|tx| {
                tx.put(
                    &secret,
                    None,
                    Some(&acct("acc-a", "a@example.com")),
                    Some("main"),
                    None,
                )
            })
            .unwrap();
        }
        let span_and_total = |name: &str| {
            let id = ctx.resolve_account(&idx, name).unwrap();
            let week = ctx.limit_week(&idx, Some(&id)).unwrap();
            let span = crate::core::period::Period::LimitWeek.span(ctx.now, week);
            let f = Filter {
                account: Some(id.clone()),
                ..Default::default()
            };
            (id, span, idx.total(span, &f).unwrap().cost)
        };
        let by_uuid = span_and_total("acc-a");
        assert_eq!(by_uuid.0.as_str(), "acc-a");
        // The window in force starts at the reset by hand (11-19 00:00): only q2, 1M input at
        // Sonnet 5.5's $2 per MTok = $2.
        assert_eq!(by_uuid.1.start, at("2026-11-19T00:00:00Z"));
        assert!(close(by_uuid.2, 2.0), "{}", by_uuid.2);
        for name in ["a@example.com", "A@Example.com", "main", " acc-a "] {
            assert_eq!(span_and_total(name), by_uuid, "{name}");
        }
        // One name, two accounts: an error that lists both.
        ctx.config.accounts.insert("acc-b".into(), "main".into());
        let e = ctx.resolve_account(&idx, "main").unwrap_err();
        assert_eq!(crate::errors::code_of(&e), Some("ambiguous-account"));
        let msg = e.to_string();
        assert!(msg.contains("(acc-a)") && msg.contains("(acc-b)"), "{msg}");
        let e = ctx.resolve_account(&idx, "nobody@example.com").unwrap_err();
        assert_eq!(crate::errors::code_of(&e), Some("no-account"));
    }
}
