//! LIMITS · SEATS: every seat and saved login with its limits, where the week went, and the
//! limit chart.

use chrono::{Local, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CLine};

use super::{dim, faint};
use crate::core::limits::{Level, WindowKind};
use crate::tui::app::{App, ManaRow, Panel};
use crate::tui::view::kit::*;
use crate::ui::fmt;

pub fn mana(f: &mut Frame, app: &mut App, area: Rect, detail: bool) {
    let hot = app
        .alerts
        .get("vault")
        .is_some_and(|t| t.elapsed().as_secs_f32() < 2.0)
        && blink(app);
    let n_acc = app.analytics.limits.len();
    let inner = frame(
        f,
        app,
        Panel::Mana,
        area,
        &format!("{n_acc} accounts · Enter switch · S best"),
        hot,
    );
    let app: &App = app;
    let th = app.theme;
    let rows = app.mana_rows();
    let width = inner.width as usize;
    // Narrow panels drop the account column (the seat and login usually say enough).
    let acct_w = if inner.width < 120 { 0usize } else { 18 };
    let (mark_w, name_w, login_w, reset_w) = (2usize, 13usize, 15usize, 9usize);
    let fixed = mark_w
        + name_w
        + 1
        + login_w
        + if acct_w > 0 { acct_w + 1 } else { 0 }
        + 5
        + 1
        + 5
        + 1
        + reset_w
        + 1;
    let info_min = 22;
    let has_five = app.analytics.limits.iter().any(|r| {
        r.windows
            .iter()
            .any(|w| w.reading.kind == WindowKind::FiveHour)
    });
    let fixed = if has_five { fixed } else { fixed - 6 };
    let gauges = if has_five { 2 } else { 1 };
    let g = ((width.saturating_sub(fixed + info_min)) / gauges).clamp(6, 24);
    let info_w = width.saturating_sub(fixed + gauges * g);
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "{}{} {} {}{}{} {} {}",
            " ".repeat(mark_w),
            fit("SEAT", name_w),
            fit("LOGIN", login_w),
            if acct_w > 0 {
                format!("{} ", fit("ACCOUNT", acct_w))
            } else {
                String::new()
            },
            if has_five {
                format!("{} ", fit("5-HOUR", g + 5))
            } else {
                String::new()
            },
            fit("WEEK USED", g + 5),
            fit("RESETS IN", reset_w),
            "STATUS"
        ),
        faint(app),
    ))];
    let cur = app.cur(Panel::Mana);
    let focused = app.focus == Panel::Mana;
    let now = Utc::now();
    let mut first_login = true;
    for (i, r) in rows.iter().enumerate() {
        let sel = focused && i == cur;
        let mut sp: Vec<Span> = vec![Span::styled(
            if sel { "▸ " } else { "  " },
            th.bold(th.accent),
        )];
        let (account, quiet) = match r {
            ManaRow::Seat(label) => {
                let s = app.analytics.seats.iter().find(|x| &x.label == label);
                let state = s.map(|x| x.state.clone()).unwrap_or_default();
                let login = s.and_then(|x| x.login.clone());
                sp.push(Span::styled(fit(label, name_w), th.bold(th.fg)));
                sp.push(match (&login, &state.fingerprint) {
                    (Some(l), _) => Span::styled(
                        format!(" {}", fit(&format!("{l} ✓"), login_w)),
                        Style::new().fg(th.good),
                    ),
                    (None, Some(_)) => {
                        Span::styled(format!(" {}", fit("not saved", login_w)), th.bold(th.warn))
                    }
                    (None, None) => Span::styled(format!(" {}", fit("-", login_w)), dim(app)),
                });
                let acct = s.and_then(|x| x.account.clone());
                let name = match (&state.unverified_since, &acct) {
                    (Some(t), _) => {
                        format!("? new login {}", t.with_timezone(&Local).format("%d %b"))
                    }
                    (None, Some(a)) => mask_name(app, &app.account_label(a)),
                    (None, None) if state.fingerprint.is_some() => "(unknown)".into(),
                    _ => "(logged out)".into(),
                };
                if acct_w > 0 {
                    sp.push(Span::styled(
                        format!(" {}", fit(&name, acct_w)),
                        Style::new().fg(if state.unverified_since.is_some() {
                            th.warn
                        } else {
                            th.fg
                        }),
                    ));
                } else if state.unverified_since.is_some() {
                    sp.push(Span::styled("?", Style::new().fg(th.warn)));
                }
                (acct, false)
            }
            ManaRow::Login(label) => {
                if first_login {
                    first_login = false;
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(" SAVED LOGINS ", th.heading()),
                        Span::styled(
                            " · in the vault, not in a seat · Enter puts one into a seat",
                            faint(app),
                        ),
                    ]));
                }
                let l = app.vault.logins.iter().find(|x| &x.label == label);
                sp.push(Span::styled(fit("", name_w), faint(app)));
                sp.push(Span::styled(
                    format!(" {}", fit(&format!("⚿ {label}"), login_w)),
                    Style::new().fg(th.fg),
                ));
                let acct = l.and_then(|x| x.account_uuid.clone());
                let name = acct
                    .as_deref()
                    .map(|a| app.account_label(a))
                    .or_else(|| l.and_then(|x| x.email.clone()))
                    .unwrap_or_else(|| "(account unknown)".into());
                if acct_w > 0 {
                    sp.push(Span::styled(
                        format!(" {}", fit(&mask_name(app, &name), acct_w)),
                        dim(app),
                    ));
                }
                (acct, true)
            }
        };
        let (five, week, reset) = app.mana_of(account.as_deref());
        let shown: Vec<Option<f64>> = if has_five {
            vec![five, week]
        } else {
            vec![week]
        };
        for v in shown {
            sp.push(Span::raw(" "));
            match v {
                Some(p) => {
                    let c = th.utilization(p);
                    let c = if quiet {
                        th.fade(c, 0.35)
                    } else if Level::of(p) >= Level::Bad {
                        pulse(app, c, 1.0)
                    } else {
                        c
                    };
                    sp.extend(gauge(p / 100.0, g, c, &th));
                    sp.push(Span::styled(fit_right(&fmt::pct(p), 5), th.bold(c)));
                }
                None => sp.push(Span::styled(fit("  unknown", g + 5), faint(app))),
            }
        }
        sp.push(Span::styled(
            format!(
                " {}",
                fit(
                    &reset.map(|r| fmt::duration(r - now)).unwrap_or_default(),
                    reset_w
                )
            ),
            dim(app),
        ));
        // Status: what to do or expect.
        let report = account
            .as_deref()
            .and_then(|a| app.analytics.limits.iter().find(|x| x.account_uuid == a));
        let weekly = report.and_then(|r| {
            r.windows
                .iter()
                .find(|w| w.reading.kind == WindowKind::SevenDay)
        });
        let status: (String, Color) = match r {
            ManaRow::Seat(label) => {
                let live = app
                    .analytics
                    .seats
                    .iter()
                    .find(|x| &x.label == label)
                    .map(|x| x.live)
                    .unwrap_or(0);
                let eta = weekly.and_then(|w| {
                    if w.level() == Level::Reached {
                        Some("limit reached".to_string())
                    } else {
                        w.eta.map(|d| {
                            format!(
                                "runs out in {} ({})",
                                fmt::duration(d),
                                (now + d).with_timezone(&Local).format("%a %H:%M")
                            )
                        })
                    }
                });
                let burn = weekly
                    .map(|w| format!("{}/h", fmt::usd(w.burn_usd_per_hour)))
                    .unwrap_or_default();
                let live_s = if live > 0 {
                    format!("●{live} live")
                } else {
                    String::new()
                };
                // Whole parts only: drop the rate before cutting a number in half.
                let mut parts: Vec<String> = Vec::new();
                for p in [eta.clone().unwrap_or_default(), live_s, burn] {
                    let used: usize = parts.iter().map(|x| w(x) + 3).sum();
                    if !p.is_empty() && used + w(&p) < info_w {
                        parts.push(p);
                    }
                }
                (
                    parts.join(" · "),
                    if eta.is_some() { th.warn } else { th.dim },
                )
            }
            ManaRow::Login(label) => {
                let l = app.vault.logins.iter().find(|x| &x.label == label);
                let read = weekly
                    .map(|w| {
                        format!(
                            "limit read {} ago",
                            fmt::duration(now - w.reading.fetched_at)
                        )
                    })
                    .unwrap_or_default();
                match l.and_then(|x| x.meta.refresh_expires_at) {
                    Some(t) if (t - now).num_days() < 7 => (
                        format!("login expires in {} · {read}", fmt::duration(t - now)),
                        th.warn,
                    ),
                    Some(t) => (
                        format!("login valid {} · {read}", fmt::duration(t - now)),
                        th.dim,
                    ),
                    None => (read, th.dim),
                }
            }
        };
        sp.push(Span::styled(
            format!(" {}", fit(&status.0, info_w)),
            Style::new().fg(status.1),
        ));
        let mut line = Line::from(sp);
        if sel {
            line = line.style(Style::new().bg(th.sel));
        }
        lines.push(line);
    }
    if focused && (lines.len() as u16) < inner.height {
        lines.push(Line::from(Span::styled(
            "  ↑↓ select · Enter switch · S best login here · c save · a add · r rename · d forget · R re-read",
            faint(app),
        )));
    }
    // Below the rows: the week as a curve (left) and where it went (right), when there is room.
    let used = lines.len() as u16;
    put(f, inner, lines);
    let rest = Rect {
        y: inner.y + used + 1,
        height: inner.height.saturating_sub(used + 1),
        ..inner
    };
    if rest.height >= 5 {
        let (chart, side) = if rest.width >= 110 {
            let c = hsplit(rest, &[58, 42]);
            (c[0], Some(c[1]))
        } else {
            (rest, None)
        };
        mana_curve(f, app, chart);
        if let Some(side) = side {
            let side = Rect {
                x: side.x + 2,
                width: side.width.saturating_sub(2),
                ..side
            };
            put(
                f,
                side,
                mana_shares(app, side.width as usize, side.height as usize, detail),
            );
        }
    }
}

/// Where each account's week went: projects with their share of the used limit.
fn mana_shares(app: &App, width: usize, height: usize, _detail: bool) -> Vec<Line<'static>> {
    let th = app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, rep) in app.analytics.limits.iter().enumerate() {
        let Some(win) = rep
            .windows
            .iter()
            .find(|w| w.reading.kind == WindowKind::SevenDay)
        else {
            continue;
        };
        if lines.len() + 2 > height {
            break;
        }
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        let mut head = vec![
            Span::styled("■ ", Style::new().fg(account_color(&th, i))),
            Span::styled(mask_name(app, &rep.name), th.bold(th.fg)),
            Span::styled(
                format!(
                    " · week used {} · 1% ≈ {}",
                    fmt::pct(win.estimate_now),
                    win.usd_per_point
                        .map(fmt::money)
                        .unwrap_or_else(|| "?".into())
                ),
                dim(app),
            ),
        ];
        if let Some(t) = win.manual_reset {
            head.push(Span::styled(
                format!(
                    " · reset by hand ≈{}",
                    t.with_timezone(&Local).format("%a %H:%M")
                ),
                Style::new().fg(th.accent),
            ));
        }
        if let Some(o) = win.outside_points.filter(|o| *o >= 2.0) {
            head.push(Span::styled(
                format!(" · ≈{o:.0}% used elsewhere (claude.ai, other machines)"),
                Style::new().fg(th.warn),
            ));
        }
        lines.push(Line::from(head));
        let max = win.shares.first().map(|s| s.share).unwrap_or(0.0).max(1e-9);
        let name_w = 16usize.min(width / 4);
        let bw = width.saturating_sub(name_w + 30).clamp(6, 40);
        for s in win.shares.iter() {
            if lines.len() >= height {
                break;
            }
            let mut sp = vec![Span::styled(
                format!("  {} ", fit(&s.key, name_w)),
                Style::new().fg(th.fg),
            )];
            sp.extend(gauge(
                s.share / max,
                bw,
                th.fade(account_color(&th, i), 0.3),
                &th,
            ));
            sp.push(Span::styled(
                format!(" {:>8} ≈{:.1}% of week", fmt::usd(s.cost), s.limit_points),
                dim(app),
            ));
            lines.push(Line::from(sp));
        }
    }
    lines
}

/// One account in the limit chart: colour, the curve, the readings, the projection
/// (from x, from y, to x, to y).
type CurveSeries = (
    Color,
    Vec<(f64, f64)>,
    Vec<(f64, f64)>,
    Option<(f64, f64, f64, f64)>,
);

/// Colours of accounts in the limit chart and its legend.
pub fn account_color(th: &crate::tui::theme::Theme, i: usize) -> Color {
    [th.accent, Color::Rgb(255, 121, 198), th.good, th.fable][i % 4]
}

/// The limit window as a curve per account: estimated utilization from the window start to now
/// (solid), the readings Claude Code fetched (dots), the projection to the reset at the current
/// pace (dotted), the 100% line and "now".
fn mana_curve(f: &mut Frame, app: &App, area: Rect) {
    use chrono::Duration;
    use ratatui::widgets::canvas::Points;
    let th = app.theme;
    let now = Utc::now();
    let wins: Vec<(usize, &crate::limits::WindowReport)> = app
        .analytics
        .limits
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            r.windows
                .iter()
                .find(|w| w.reading.kind == WindowKind::SevenDay && !w.curve.is_empty())
                .map(|w| (i, w))
        })
        .collect();
    if wins.is_empty() || area.height < 4 || area.width < 30 {
        return;
    }
    let [chart, legend] = [
        Rect {
            height: area.height - 1,
            ..area
        },
        Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        },
    ];
    let x0 = wins
        .iter()
        .map(|(_, w)| w.window.start)
        .min()
        .unwrap_or(now);
    let x1 = wins.iter().map(|(_, w)| w.window.end).max().unwrap_or(now);
    let hours = |t: chrono::DateTime<Utc>| (t - x0).num_seconds() as f64 / 3600.0;
    let span = hours(x1).max(1.0);
    // Projections: the current pace until the reset.
    let proj: Vec<Option<(f64, f64)>> = wins
        .iter()
        .map(|(_, w)| {
            w.projection_at_reset(now)
                .map(|at_reset| (hours(w.window.end), at_reset))
        })
        .collect();
    let peak = wins
        .iter()
        .map(|(_, w)| w.estimate_now)
        .chain(proj.iter().flatten().map(|p| p.1.min(100.0)))
        .fold(100.0f64, f64::max)
        .min(400.0);
    let ymax = peak * 1.08;
    let (bad, faint_c, track) = (th.bad, th.faint, th.track);
    let series: Vec<CurveSeries> = wins
        .iter()
        .zip(&proj)
        .map(|((i, w), p)| {
            let step = w.curve_step.num_seconds() as f64 / 3600.0;
            let start = hours(w.window.start);
            let mut pts = vec![(start, 0.0)];
            pts.extend(
                w.curve
                    .iter()
                    .enumerate()
                    .map(|(k, v)| ((start + (k as f64 + 1.0) * step).min(hours(now)), *v)),
            );
            let marks = w.marks.iter().map(|(t, u)| (hours(*t), *u)).collect();
            let pr = p.map(|(xe, ye)| (hours(now), w.estimate_now, xe, ye));
            (account_color(&th, *i), pts, marks, pr)
        })
        .collect();
    let xn = hours(now);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, span])
        .y_bounds([0.0, ymax])
        .paint(move |ctx| {
            // Day boundaries, then 100% and now.
            let mut d = (x0 + Duration::days(1)).with_timezone(&Local).date_naive();
            while let Some(t) = d
                .and_hms_opt(0, 0, 0)
                .and_then(|t| t.and_local_timezone(Local).single())
            {
                let x = hours(t.with_timezone(&Utc));
                if x >= span {
                    break;
                }
                ctx.draw(&CLine::new(x, 0.0, x, ymax * 0.04, track));
                ctx.print(
                    x,
                    0.0,
                    Line::styled(d.format(" %a").to_string(), Style::new().fg(faint_c)),
                );
                d = d.succ_opt().unwrap_or(d);
            }
            let mut x = 0.0;
            while x < span {
                ctx.draw(&CLine::new(
                    x,
                    100.0,
                    (x + span / 120.0).min(span),
                    100.0,
                    bad,
                ));
                x += span / 60.0;
            }
            ctx.draw(&CLine::new(xn, 0.0, xn, ymax, faint_c));
            for (k, (c, pts, marks, pr)) in series.iter().enumerate() {
                let k = &k;
                for w in pts.windows(2) {
                    ctx.draw(&CLine::new(w[0].0, w[0].1, w[1].0, w[1].1, *c));
                }
                if let Some((xa, ya, xe, ye)) = pr.filter(|p| p.1 < 100.0) {
                    // Dotted, and only up to the 100% line: where it crosses is the news.
                    let stop = if ye > 100.0 {
                        (100.0 - ya) / (ye - ya)
                    } else {
                        1.0
                    };
                    let n = 40;
                    for k in (0..n).step_by(2) {
                        let (t0, t1) = (k as f64 / n as f64, ((k + 1) as f64 / n as f64).min(stop));
                        if t0 >= stop {
                            break;
                        }
                        ctx.draw(&CLine::new(
                            xa + (xe - xa) * t0,
                            ya + (ye - ya) * t0,
                            xa + (xe - xa) * t1,
                            ya + (ye - ya) * t1,
                            *c,
                        ));
                    }
                    if stop < 1.0 {
                        let xc = xa + (xe - xa) * stop;
                        let t = x0 + Duration::seconds((xc * 3600.0) as i64);
                        ctx.print(
                            xc,
                            100.0 - ymax * 0.09 * (*k as f64 + 1.0),
                            Line::styled(
                                format!("⊘{}", t.with_timezone(&Local).format("%a %H:%M")),
                                Style::new().fg(*c),
                            ),
                        );
                    }
                }
                ctx.draw(&Points {
                    coords: marks,
                    color: Color::White,
                });
            }
        });
    f.render_widget(canvas, chart);
    // Legend: per account where it is and where it is heading.
    let mut spans = vec![
        Span::styled("100% ", Style::new().fg(th.bad)),
        Span::styled("╌ ", Style::new().fg(th.bad)),
    ];
    for ((i, w), p) in wins.iter().zip(&proj) {
        let name = mask_name(app, &app.analytics.limits[*i].name);
        let ahead = match (w.eta, p) {
            _ if w.level() == Level::Reached => " · limit reached".to_string(),
            (Some(d), _) => format!(
                " → 100% {}",
                (now + d).with_timezone(&Local).format("%a %H:%M")
            ),
            (None, Some((_, ye))) => format!(" → ≈{} at reset", fmt::pct(*ye)),
            _ => String::new(),
        };
        spans.push(Span::styled("■ ", Style::new().fg(account_color(&th, *i))));
        spans.push(Span::styled(
            format!("{name} {}{ahead}   ", fmt::pct(w.estimate_now)),
            dim(app),
        ));
    }
    let range = format!(
        "{} … {} · dots: readings",
        x0.with_timezone(&Local).format("%a %d"),
        x1.with_timezone(&Local).format("%a %d")
    );
    let used: usize = spans.iter().map(|s| w(&s.content)).sum();
    if used + w(&range) <= legend.width as usize {
        spans.push(Span::styled(range, faint(app)));
    }
    put(f, legend, vec![Line::from(spans)]);
}
