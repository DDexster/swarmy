//! The panels. Each one fills the rectangle it is given: columns, bar widths and row counts come
//! from the size, so the same panel works in a 40-column strip and on a 4K wall.

use chrono::{Local, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CLine};

use super::kit::*;
use crate::core::limits::WindowKind;
use crate::core::models::{Family, context_window, short_name};
use crate::live::State;
use crate::tui::app::{App, ManaRow, Panel};
use crate::tui::bigdigits;
use crate::ui::fmt;

fn dim(app: &App) -> Style {
    Style::new().fg(app.theme.dim)
}

fn faint(app: &App) -> Style {
    Style::new().fg(app.theme.faint)
}

// ───────────────────────────── SWARM ─────────────────────────────

pub fn swarm(f: &mut Frame, app: &mut App, area: Rect) {
    let n = app.visible_agents().len();
    let suffix = match &app.filter {
        Some(fl) if !fl.is_empty() => format!("{n} agents · filter \"{fl}\""),
        _ => format!("{n} agents"),
    };
    let inner = frame(f, app, Panel::Swarm, area, &suffix, false);
    let app: &App = app;
    let th = app.theme;
    let agents = app.visible_agents();
    if agents.is_empty() {
        let msg = if app.snapshot.taken_at.is_none() {
            "linking…"
        } else {
            "no agents active in the last 30 minutes"
        };
        put(f, inner, vec![Line::from(Span::styled(msg, dim(app)))]);
        return;
    }
    let width = inner.width as usize;
    let tree_w = |d: u32| if d == 0 { 0 } else { 2 * d as usize };
    let longest = agents
        .iter()
        .map(|a| {
            tree_w(a.depth)
                + w(&if a.is_main() {
                    format!("{} · {}", a.name, a.project)
                } else {
                    a.name.clone()
                })
        })
        .max()
        .unwrap_or(16);
    let (model_w, t_w, cost_w) = (13usize, 8usize, 10usize);
    let ctx_w = if width >= 120 { 16 } else { 0 };
    let spark_w = if width >= 150 { 16 } else { 0 };
    let extra = width >= 210;
    let (reqs_w, age_w) = if extra { (7usize, 8usize) } else { (0, 0) };
    let fixed = 3
        + 1
        + model_w
        + 1
        + t_w
        + ctx_w
        + spark_w
        + if extra { reqs_w + age_w + 2 } else { 0 }
        + 1
        + cost_w
        + 1;
    let mut name_w = (longest + 2).clamp(16, 52);
    let mut act_w = width.saturating_sub(fixed + name_w);
    if act_w < 24 {
        name_w = name_w.saturating_sub(24 - act_w).max(12);
        act_w = width.saturating_sub(fixed + name_w).max(8);
    }
    let mut header = format!(
        "   {} {} {} {}",
        fit("AGENT", name_w),
        fit("MODEL", model_w),
        fit("ACTIVITY", act_w),
        fit_right("TIME", t_w)
    );
    if ctx_w > 0 {
        header.push_str(&format!(" {}", fit("CONTEXT", ctx_w - 1)));
    }
    if spark_w > 0 {
        header.push_str(&format!(" {}", fit("$ · 15 min", spark_w - 1)));
    }
    if extra {
        header.push_str(&format!(
            " {} {}",
            fit_right("REQS", reqs_w),
            fit_right("AGE", age_w)
        ));
    }
    header.push_str(&format!(" {}", fit_right("COST", cost_w)));
    let mut lines = vec![Line::from(Span::styled(header, faint(app)))];
    let rows_h = inner.height.saturating_sub(1) as usize;
    let sel = app.selected_index();
    let offset = app.swarm_offset(rows_h);
    let now = Utc::now();
    for (i, a) in agents.iter().enumerate().skip(offset).take(rows_h) {
        let (g, gc) = glyph(app, &a.state);
        let last_in_branch = agents
            .get(i + 1)
            .is_none_or(|nx| nx.depth < a.depth || nx.is_main() || nx.session_id != a.session_id);
        let tree = if a.depth == 0 {
            String::new()
        } else {
            format!(
                "{}{}",
                "│ ".repeat((a.depth - 1) as usize),
                if last_in_branch { "└ " } else { "├ " }
            )
        };
        let name = if a.is_main() {
            format!("{} · {}", a.name, a.project)
        } else {
            a.name.clone()
        };
        let quiet = matches!(a.state, State::Done | State::Gone | State::Idle);
        let fg = if quiet { th.dim } else { th.fg };
        let elapsed = now - a.since;
        let timer = if matches!(a.state, State::Done | State::Gone) {
            fmt::duration(now - a.last_activity)
        } else if elapsed.num_hours() >= 1 {
            fmt::duration(elapsed)
        } else {
            fmt::clock(elapsed)
        };
        let mut act = activity(&a.state);
        if a.is_main() && act_w >= 48 {
            let mut extra_s = format!("{}/h", fmt::money(a.session_burn));
            if a.cpu > 0.0 || a.memory > 0 {
                extra_s.push_str(&format!(
                    " · cpu {:.0}% {}",
                    a.cpu,
                    crate::cli::doctor::human_bytes(a.memory)
                ));
            }
            let room = act_w.saturating_sub(w(&extra_s) + 2);
            act = format!("{}  {}", fit(&act, room), extra_s);
        }
        let act_style = match &a.state {
            State::Tool { .. } => Style::new().fg(th.fg),
            s => Style::new().fg(th.state(s)),
        };
        let mut spans = vec![
            Span::styled(format!("{g}  "), Style::new().fg(gc)),
            Span::styled(tree.clone(), Style::new().fg(th.faint)),
            Span::styled(
                fit(&name, name_w.saturating_sub(w(&tree))),
                if a.is_main() {
                    th.bold(fg)
                } else {
                    Style::new().fg(fg)
                },
            ),
            Span::styled(
                format!(" {}", fit(&model_chip(a), model_w)),
                Style::new().fg(if quiet { th.dim } else { th.family(a.family()) }),
            ),
            Span::styled(format!(" {}", fit(&act, act_w)), act_style),
            Span::styled(format!(" {}", fit_right(&timer, t_w)), dim(app)),
        ];
        if ctx_w > 0 {
            let win = a.model.as_deref().map(context_window).unwrap_or(1_000_000) as f64;
            let frac = a.context_tokens as f64 / win;
            spans.push(Span::raw(" "));
            if a.context_tokens > 0 {
                spans.extend(gauge(
                    frac,
                    10,
                    if frac >= 0.8 { th.warn } else { th.thinking },
                    &th,
                ));
                spans.push(Span::styled(
                    fit_right(&fmt::pct(frac * 100.0), 5),
                    dim(app),
                ));
            } else {
                spans.push(Span::styled(fit("", ctx_w - 1), dim(app)));
            }
        }
        if spark_w > 0 {
            spans.push(Span::raw(" "));
            spans.push(spark(&a.spark, th.family(a.family())));
            spans.push(Span::raw(
                " ".repeat((spark_w - 1).saturating_sub(a.spark.len())),
            ));
        }
        if extra {
            spans.push(Span::styled(
                format!(" {}", fit_right(&fmt::count(a.requests), reqs_w)),
                dim(app),
            ));
            spans.push(Span::styled(
                format!(" {}", fit_right(&fmt::duration(now - a.first_seen), age_w)),
                dim(app),
            ));
        }
        let cost = if a.is_main() {
            format!("Σ{}", fmt::money(a.session_cost))
        } else {
            fmt::money(a.cost)
        };
        spans.push(Span::styled(
            format!(" {}", fit_right(&cost, cost_w)),
            th.bold(if quiet { th.dim } else { th.fg }),
        ));
        let mut line = Line::from(spans);
        let alert = app.alerts.get(&a.key()).map(|t| t.elapsed().as_secs_f32());
        if i == sel {
            line = line.style(Style::new().bg(th.sel).add_modifier(Modifier::BOLD));
        } else if matches!(a.state, State::NeedsYou(_)) && blink(app) {
            line = line.style(Style::new().bg(th.fade(th.warn, 0.8)));
        } else if let Some(t) = alert.filter(|t| *t < 0.9 && app.animating()) {
            line = line.style(Style::new().bg(th.fade(th.accent, 0.82 + t / 0.9 * 0.18)));
        }
        lines.push(line);
    }
    // The space under the live tree shows what finished recently.
    let room = (inner.height as usize).saturating_sub(lines.len() + 1);
    if room >= 3 && !app.snapshot.recent_done.is_empty() {
        let done = &app.snapshot.recent_done;
        let total: f64 = done.iter().map(|d| d.cost).sum();
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(" RECENTLY DONE ", th.heading()),
            Span::styled(
                format!(
                    " · {} agents in the last 6 h · {}",
                    done.len(),
                    fmt::money(total)
                ),
                faint(app),
            ),
        ]));
        let proj_w = 18usize;
        let dname_w = name_w.max(24);
        for d in done.iter().take(room.saturating_sub(2)) {
            let model = d.model.as_deref().map(short_name).unwrap_or_default();
            let fam = d.model.as_deref().map(Family::of).unwrap_or(Family::Other);
            lines.push(Line::from(vec![
                Span::styled("✓  ", Style::new().fg(th.fade(th.good, 0.35))),
                Span::styled(fit(&d.name, dname_w), Style::new().fg(th.dim)),
                Span::styled(format!(" {}", fit(&d.project, proj_w)), faint(app)),
                Span::styled(
                    format!(" {}", fit(&model, 11)),
                    Style::new().fg(th.fade(th.family(fam), 0.3)),
                ),
                Span::styled(
                    format!(" took {}", fit(&fmt::duration(d.finished - d.started), 8)),
                    faint(app),
                ),
                Span::styled(
                    format!(" done {}", d.finished.with_timezone(&Local).format("%H:%M")),
                    faint(app),
                ),
                Span::styled(
                    format!(" {}", fit_right(&fmt::money(d.cost), 10)),
                    Style::new().fg(th.dim),
                ),
            ]));
        }
    }
    put(f, inner, lines);
}

// ───────────────────────────── AGENT ─────────────────────────────

fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if w(&cur) + w(word) + 1 > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            if out.len() == max_lines {
                return out;
            }
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() && out.len() < max_lines {
        out.push(cur);
    }
    out
}

pub fn agent(f: &mut Frame, app: &mut App, area: Rect) {
    let name = app
        .selected_agent()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let inner = frame(f, app, Panel::Agent, area, &name, false);
    let app: &App = app;
    let th = app.theme;
    let Some(a) = app.selected_agent().cloned() else {
        put(
            f,
            inner,
            vec![Line::from(Span::styled(
                "select an agent in SWARM",
                dim(app),
            ))],
        );
        return;
    };
    let wide = inner.width >= 90;
    let (card, tl) = if wide {
        let cw = (inner.width / 2).clamp(38, 56);
        (
            Rect { width: cw, ..inner },
            Rect {
                x: inner.x + cw + 1,
                width: inner.width - cw - 1,
                ..inner
            },
        )
    } else {
        let ch = 13.min(inner.height);
        (
            Rect {
                height: ch,
                ..inner
            },
            Rect {
                y: inner.y + ch,
                height: inner.height - ch,
                ..inner
            },
        )
    };
    let now = Utc::now();
    let (g, gc) = glyph(app, &a.state);
    let kw = 10usize;
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(fit(k, kw), faint(app)),
            Span::styled(v, Style::new().fg(c)),
        ])
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{g} "), Style::new().fg(gc)),
        Span::styled(a.name.clone(), th.bold(th.fg)),
    ])];
    lines.push(kv("state", activity(&a.state), th.state(&a.state)));
    lines.push(kv(
        "since",
        format!(
            "{} · {}",
            a.since.with_timezone(&Local).format("%H:%M:%S"),
            fmt::duration(now - a.since)
        ),
        th.fg,
    ));
    lines.push(kv(
        "model",
        format!(
            "{} · effort {}",
            a.model.as_deref().map(short_name).unwrap_or_default(),
            a.effort.clone().unwrap_or_else(|| "-".into())
        ),
        th.family(a.family()),
    ));
    lines.push(kv(
        "type",
        a.agent_type.clone().unwrap_or_else(|| {
            if a.is_main() {
                "main thread".into()
            } else {
                "subagent".into()
            }
        }),
        th.fg,
    ));
    lines.push(kv(
        "where",
        format!(
            "{} · {} · {}",
            a.project,
            a.seat,
            &a.session_id[..8.min(a.session_id.len())]
        ),
        th.fg,
    ));
    let cost = if a.is_main() {
        format!(
            "{} · session Σ{} · {}/h",
            fmt::money(a.cost),
            fmt::money(a.session_cost),
            fmt::money(a.session_burn)
        )
    } else {
        format!(
            "{} · {} requests",
            fmt::money(a.cost),
            fmt::count(a.requests)
        )
    };
    lines.push(kv("cost", cost, th.fg));
    let win = a.model.as_deref().map(context_window).unwrap_or(1_000_000);
    let frac = a.context_tokens as f64 / win as f64;
    let mut ctx = vec![Span::styled(fit("context", kw), faint(app))];
    ctx.extend(gauge(
        frac,
        14,
        if frac >= 0.8 { th.warn } else { th.thinking },
        &th,
    ));
    ctx.push(Span::styled(
        format!(" {} of {}", fmt::tokens(a.context_tokens), fmt::tokens(win)),
        dim(app),
    ));
    lines.push(Line::from(ctx));
    if let Some(t) = &a.tmux {
        lines.push(kv("tmux", format!("{t} · J jumps there"), th.accent));
    }
    if app.timeline.0 == a.key()
        && let Some(msg) = &app.timeline.2
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(" LAST MESSAGE ", th.heading())));
        let room = (card.height as usize).saturating_sub(lines.len()).max(1);
        for l in wrap(
            if app.mask { "•••" } else { msg },
            card.width as usize,
            room,
        ) {
            lines.push(Line::from(Span::styled(l, Style::new().fg(th.fg))));
        }
    }
    put(f, card, lines);

    // Tool timeline, newest first, scrollable when the panel has focus.
    let mut lines = Vec::new();
    let total = if app.timeline.0 == a.key() {
        app.timeline.1.len()
    } else {
        0
    };
    lines.push(Line::from(vec![
        Span::styled(" TOOLS ", th.heading()),
        Span::styled(
            format!(
                " · {total} recent{}",
                if app.focus == Panel::Agent {
                    " · ↑↓ scroll"
                } else {
                    ""
                }
            ),
            dim(app),
        ),
    ]));
    if app.timeline.0 == a.key() {
        let start = app.cur(Panel::Agent).min(total.saturating_sub(1));
        let sum_w = (tl.width as usize).saturating_sub(29);
        for t in app
            .timeline
            .1
            .iter()
            .skip(start)
            .take(tl.height.saturating_sub(1) as usize)
        {
            let (mark, c, dur) = match t.done {
                Some(d) if t.is_error => ("✗", th.bad, fmt::duration(d - t.ts)),
                Some(d) => ("✓", th.good, fmt::duration(d - t.ts)),
                None => ("▶", pulse(app, th.warn, 1.0), fmt::clock(now - t.ts)),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", t.ts.with_timezone(&Local).format("%H:%M:%S")),
                    faint(app),
                ),
                Span::styled(format!("{mark} "), Style::new().fg(c)),
                Span::styled(fit_right(&dur, 6), dim(app)),
                Span::styled(format!(" {} ", fit(&t.name, 8)), th.bold(th.fg)),
                Span::styled(fit(if app.mask { "" } else { &t.summary }, sum_w), dim(app)),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled("loading…", faint(app))));
    }
    put(f, tl, lines);
}

// ───────────────────────────── MANA · SEATS ─────────────────────────────

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
            fit("WEEK", g + 5),
            fit("RESETS", reset_w),
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
                }
                let l = app.vault.logins.iter().find(|x| &x.label == label);
                sp.push(Span::styled(
                    fit(&format!("⚿ {label}"), name_w),
                    Style::new().fg(th.fg),
                ));
                sp.push(Span::styled(
                    format!(" {}", fit("parked", login_w)),
                    dim(app),
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
                    } else if p >= 85.0 {
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
                let eta = weekly
                    .and_then(|w| w.eta)
                    .map(|d| format!("empty in {}", fmt::duration(d)));
                let burn = weekly
                    .map(|w| format!("{}/h", fmt::money(w.burn_usd_per_hour)))
                    .unwrap_or_default();
                let live_s = if live > 0 {
                    format!("●{live} live")
                } else {
                    String::new()
                };
                let parts: Vec<String> = [eta.clone().unwrap_or_default(), burn, live_s]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect();
                (
                    parts.join(" · "),
                    if eta.is_some() { th.warn } else { th.dim },
                )
            }
            ManaRow::Login(label) => {
                let l = app.vault.logins.iter().find(|x| &x.label == label);
                let read = weekly
                    .map(|w| format!("read {} ago", fmt::duration(now - w.reading.fetched_at)))
                    .unwrap_or_default();
                match l.and_then(|x| x.meta.refresh_expires_at) {
                    Some(t) if (t - now).num_days() < 7 => (
                        format!("login expires in {} · {read}", fmt::duration(t - now)),
                        th.warn,
                    ),
                    Some(t) => (
                        format!("login ok {} · {read}", fmt::duration(t - now)),
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
            "  ↑↓ select · Enter switch (seat: pick a login · login: pick a seat) · S best login here · c save seat's login · a add · r rename · d forget · R re-read",
            faint(app),
        )));
    }
    // Where each account's week went, when there is room.
    if detail || inner.height as usize > lines.len() + 4 {
        for rep in &app.analytics.limits {
            let Some(win) = rep
                .windows
                .iter()
                .find(|w| w.reading.kind == WindowKind::SevenDay)
            else {
                continue;
            };
            if (lines.len() as u16) + 2 >= inner.height {
                break;
            }
            lines.push(Line::from(""));
            let mut head = vec![
                Span::styled(mask_name(app, &rep.name), th.bold(th.fg)),
                Span::styled(
                    format!(
                        " · week {} · ≈{}/pt",
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
                    format!(" · ≈{o:.0}pt used elsewhere (claude.ai, other devices)"),
                    Style::new().fg(th.warn),
                ));
            }
            lines.push(Line::from(head));
            let max = win.shares.first().map(|s| s.share).unwrap_or(0.0).max(1e-9);
            let bw = width.saturating_sub(48).clamp(10, 60);
            for s in win.shares.iter() {
                if (lines.len() as u16) >= inner.height {
                    break;
                }
                let mut sp = vec![Span::styled(
                    format!("  {} ", fit(&s.key, 18)),
                    Style::new().fg(th.fg),
                )];
                sp.extend(gauge(s.share / max, bw, th.fade(th.accent, 0.3), &th));
                sp.push(Span::styled(
                    format!(
                        " {:>10} {:>5} ≈{:.1}pt",
                        fmt::money(s.cost),
                        fmt::pct(s.share * 100.0),
                        s.limit_points
                    ),
                    dim(app),
                ));
                lines.push(Line::from(sp));
            }
        }
    }
    put(f, inner, lines);
}

// ───────────────────────────── BURN ─────────────────────────────

pub fn burn(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = frame(f, app, Panel::Burn, area, "$ per minute · 60 min", false);
    let app: &App = app;
    let th = app.theme;
    if inner.height < 3 {
        return;
    }
    // Headline figures on the left when there is room.
    // Big digits carry the number only; the unit is written under it.
    let b = app.snapshot.burn_per_hour;
    let rate = if b >= 100.0 {
        format!("{b:.0}")
    } else if b >= 10.0 {
        format!("{b:.1}")
    } else {
        format!("{b:.2}")
    };
    let big_w = bigdigits::width(&rate) as u16 + 2;
    let chart_area = if inner.width >= big_w + 30 && inner.height >= 6 {
        let rows = bigdigits::render(&rate);
        let c = if app.snapshot.burn_per_hour > 0.0 {
            th.accent
        } else {
            th.dim
        };
        let mut lines: Vec<Line> = rows
            .into_iter()
            .map(|r| Line::from(Span::styled(r, th.bold(c))))
            .collect();
        lines.push(Line::from(Span::styled("$ per hour", dim(app))));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("today ", faint(app)),
            Span::styled(fmt::money(app.snapshot.today_cost), th.bold(th.fg)),
        ]));
        if let Some(t) = &app.analytics.total {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{} ",
                        app.analytics
                            .period
                            .as_ref()
                            .map(|p| p.label())
                            .unwrap_or_default()
                    ),
                    faint(app),
                ),
                Span::styled(fmt::money(t.cost), th.bold(th.fg)),
            ]));
        }
        put(
            f,
            Rect {
                width: big_w,
                ..inner
            },
            lines,
        );
        Rect {
            x: inner.x + big_w,
            width: inner.width - big_w,
            ..inner
        }
    } else {
        inner
    };
    let [chart, legend] = [
        Rect {
            height: chart_area.height - 1,
            ..chart_area
        },
        Rect {
            y: chart_area.y + chart_area.height - 1,
            height: 1,
            ..chart_area
        },
    ];
    let fams = ["fable", "mythos", "opus", "sonnet", "haiku", "other"];
    let burn = &app.snapshot.burn;
    let mut totals = vec![0.0f64; 60];
    for s in burn.values() {
        for (i, v) in s.iter().enumerate() {
            totals[i] += v;
        }
    }
    let max = totals.iter().copied().fold(0.0, f64::max).max(0.01) * 1.1;
    let series: Vec<(Color, Vec<f64>)> = fams
        .iter()
        .filter_map(|n| burn.get(*n).map(|v| (th.family_name(n), v.clone())))
        .collect();
    let track = th.track;
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, 60.0])
        .y_bounds([0.0, max])
        .background_color(th.bg)
        .paint(move |ctx| {
            for i in 0..60 {
                let mut y0 = 0.0;
                for (c, v) in &series {
                    let y1 = y0 + v[i];
                    if v[i] > 0.0 {
                        for dx in [0.0, 0.33, 0.66] {
                            ctx.draw(&CLine::new(i as f64 + dx, y0, i as f64 + dx, y1, *c));
                        }
                    }
                    y0 = y1;
                }
            }
            ctx.draw(&CLine::new(0.0, 0.0, 60.0, 0.0, track));
        });
    f.render_widget(canvas, chart);
    let mut spans = Vec::new();
    for n in ["fable", "opus", "sonnet", "haiku"] {
        if burn.contains_key(n) {
            spans.push(Span::styled(
                format!("■ {n}  "),
                Style::new().fg(th.family_name(n)),
            ));
        }
    }
    spans.push(Span::styled(
        format!("peak {}/min · −60 min … now", fmt::money(max / 1.1)),
        faint(app),
    ));
    put(f, legend, vec![Line::from(spans)]);
}

// ───────────────────────────── ATTENTION ─────────────────────────────

pub fn attention(f: &mut Frame, app: &mut App, area: Rect) {
    let items = app.attention();
    let hot = items.iter().any(|a| a.severity == 0) && blink(app);
    let inner = frame(
        f,
        app,
        Panel::Attention,
        area,
        &format!("{} · n next", items.len()),
        hot,
    );
    let app: &App = app;
    let th = app.theme;
    let s = &app.snapshot;
    let count = |p: &dyn Fn(&State) -> bool| s.count(p);
    let wide = inner.width >= 90;
    let lbl = |t: &'static str| if wide { t } else { " " };
    let fleet = vec![
        Span::styled(
            format!("●{} ", count(&|x| matches!(x, State::Tool { .. }))),
            Style::new().fg(th.good),
        ),
        Span::styled(lbl("working  "), faint(app)),
        Span::styled(
            format!("◌{} ", count(&|x| matches!(x, State::Thinking))),
            Style::new().fg(th.thinking),
        ),
        Span::styled(lbl("thinking  "), faint(app)),
        Span::styled(
            format!("◐{} ", count(&|x| matches!(x, State::WaitingAgents(_)))),
            Style::new().fg(th.accent),
        ),
        Span::styled(lbl("waiting  "), faint(app)),
        Span::styled(
            format!("▲{} ", count(&|x| matches!(x, State::NeedsYou(_)))),
            Style::new().fg(th.warn),
        ),
        Span::styled(lbl("needs you  "), faint(app)),
        Span::styled(
            format!("✖{} ", count(&|x| matches!(x, State::Stalled))),
            Style::new().fg(th.bad),
        ),
        Span::styled(lbl("stalled  "), faint(app)),
        Span::styled(
            format!("✓{} ", count(&|x| matches!(x, State::Done))),
            Style::new().fg(th.good),
        ),
        Span::styled(lbl("done"), faint(app)),
    ];
    let mut lines = vec![Line::from(fleet), Line::from("")];
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "✓ all clear: nothing waits for you",
            Style::new().fg(th.good),
        )));
    }
    let cur = app.cur(Panel::Attention);
    let focused = app.focus == Panel::Attention;
    let now = Utc::now();
    let tw = (inner.width as usize).saturating_sub(10);
    for (i, a) in items
        .iter()
        .enumerate()
        .take(inner.height.saturating_sub(2) as usize)
    {
        let c = app.tone_color(a.tone);
        let c = if a.severity == 0 {
            pulse(app, c, 1.5)
        } else {
            c
        };
        let age = a.since.map(|t| fmt::duration(now - t)).unwrap_or_default();
        let mut line = Line::from(vec![
            Span::styled(format!("{} ", a.glyph), th.bold(c)),
            Span::styled(
                fit(&a.text, tw.saturating_sub(1)),
                Style::new().fg(if a.severity <= 2 { th.fg } else { th.dim }),
            ),
            Span::styled(fit_right(&age, 7), faint(app)),
        ]);
        if focused && i == cur {
            line = line.style(Style::new().bg(th.sel));
        }
        lines.push(line);
    }
    put(f, inner, lines);
}

// ───────────────────────────── PROJECTS ─────────────────────────────

pub fn projects(f: &mut Frame, app: &mut App, area: Rect) {
    let period = app
        .analytics
        .period
        .as_ref()
        .map(|p| p.label())
        .unwrap_or_default();
    let inner = frame(
        f,
        app,
        Panel::Projects,
        area,
        &format!("{period} · Enter shows its agents"),
        false,
    );
    let app: &App = app;
    let th = app.theme;
    let width = inner.width as usize;
    let (live_w, hour_w, today_w, per_w, share_w, pts_w) =
        (5usize, 9usize, 10usize, 10usize, 6usize, 8usize);
    let name_w = 18usize.min(width / 4).max(10);
    let fixed = name_w + live_w + hour_w + today_w + per_w + share_w + pts_w + 7;
    let bar_w = width.saturating_sub(fixed).min(40);
    let total: f64 = app
        .analytics
        .projects
        .iter()
        .map(|p| p.period)
        .sum::<f64>()
        .max(1e-9);
    let max = app
        .analytics
        .projects
        .iter()
        .map(|p| p.period)
        .fold(0.0, f64::max)
        .max(1e-9);
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "{} {} {} {} {} {} {} {}",
            fit("PROJECT", name_w),
            fit_right("LIVE", live_w),
            fit_right("$/HOUR", hour_w),
            fit_right("TODAY", today_w),
            fit_right(&period.to_uppercase(), per_w),
            fit("", bar_w),
            fit_right("SHARE", share_w),
            fit_right("≈WEEK", pts_w)
        ),
        faint(app),
    ))];
    let cur = app.cur(Panel::Projects);
    let focused = app.focus == Panel::Projects;
    let rows = inner.height.saturating_sub(1) as usize;
    let offset = if cur >= rows { cur + 1 - rows } else { 0 };
    for (i, p) in app
        .analytics
        .projects
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows)
    {
        let live = app
            .snapshot
            .agents
            .iter()
            .filter(|a| {
                a.project == p.name
                    && (a.state.is_active() || matches!(a.state, State::NeedsYou(_)))
            })
            .count();
        let pts: f64 = app
            .analytics
            .limits
            .iter()
            .filter_map(|r| {
                r.windows
                    .iter()
                    .find(|w| w.reading.kind == WindowKind::SevenDay)
            })
            .flat_map(|w| {
                w.shares
                    .iter()
                    .filter(|s| s.key == p.name)
                    .map(|s| s.limit_points)
            })
            .sum();
        let name = if app.mask {
            crate::ui::table::truncate(&p.name, 12)
        } else {
            p.name.clone()
        };
        let mut sp = vec![
            Span::styled(
                fit(&name, name_w),
                th.bold(if p.hour > 0.0 { th.fg } else { th.dim }),
            ),
            Span::styled(
                format!(
                    " {}",
                    fit_right(
                        &if live > 0 {
                            format!("●{live}")
                        } else {
                            "·".into()
                        },
                        live_w
                    )
                ),
                Style::new().fg(if live > 0 { th.good } else { th.faint }),
            ),
            Span::styled(
                format!(
                    " {}",
                    fit_right(
                        &if p.hour > 0.0 {
                            fmt::money(p.hour)
                        } else {
                            "·".into()
                        },
                        hour_w
                    )
                ),
                Style::new().fg(if p.hour > 0.0 { th.accent } else { th.faint }),
            ),
            Span::styled(
                format!(" {}", fit_right(&fmt::money(p.today), today_w)),
                Style::new().fg(th.fg),
            ),
            Span::styled(
                format!(" {}", fit_right(&fmt::money(p.period), per_w)),
                th.bold(th.fg),
            ),
            Span::raw(" "),
        ];
        sp.extend(gauge(p.period / max, bar_w, th.fade(th.accent, 0.25), &th));
        sp.push(Span::styled(
            format!(
                " {}",
                fit_right(&fmt::pct(p.period / total * 100.0), share_w)
            ),
            dim(app),
        ));
        sp.push(Span::styled(
            format!(
                " {}",
                fit_right(
                    &if pts > 0.05 {
                        format!("≈{pts:.1}pt")
                    } else {
                        String::new()
                    },
                    pts_w
                )
            ),
            dim(app),
        ));
        let mut line = Line::from(sp);
        if focused && i == cur {
            line = line.style(Style::new().bg(th.sel));
        }
        lines.push(line);
    }
    if app.analytics.projects.is_empty() {
        lines.push(Line::from(Span::styled(
            "no spend in this period",
            dim(app),
        )));
    }
    put(f, inner, lines);
}

// ───────────────────────────── MODELS ─────────────────────────────

pub fn models(f: &mut Frame, app: &mut App, area: Rect) {
    let period = app
        .analytics
        .period
        .as_ref()
        .map(|p| p.label())
        .unwrap_or_default();
    let inner = frame(f, app, Panel::Models, area, &period, false);
    let app: &App = app;
    let th = app.theme;
    let an = &app.analytics;
    let width = inner.width as usize;
    let total: f64 = an.by_model.iter().map(|r| r.cost).sum::<f64>().max(1e-9);
    let show_mix = width >= 95;
    let fixed = 11 + 9 + 11 + 6 + 8 + 6 + 6 + if show_mix { 24 } else { 0 } + 8;
    let bar_w = width.saturating_sub(fixed).clamp(4, 40);
    let mut header = format!(
        "{} {} {} {} {} {} {} {}",
        fit("MODEL", 11),
        fit_right("REQS", 8),
        fit_right("COST", 10),
        fit("SHARE", bar_w),
        fit_right("", 5),
        fit_right("$/REQ", 7),
        fit_right("CACHE", 6),
        fit_right("THINK", 5)
    );
    if show_mix {
        header.push_str("  EFFORT");
    }
    let mut lines = vec![Line::from(Span::styled(header, faint(app)))];
    for r in &an.by_model {
        let c = th.family_name(&r.key);
        let share = r.cost / total;
        let u = r.usage;
        let mut sp = vec![
            Span::styled(fit(&short_name(&r.key), 11), th.bold(c)),
            Span::styled(
                format!(
                    " {} {} ",
                    fit_right(&fmt::count(r.requests), 8),
                    fit_right(&fmt::money(r.cost), 10)
                ),
                Style::new().fg(th.fg),
            ),
        ];
        sp.extend(gauge(share, bar_w, c, &th));
        sp.push(Span::styled(
            fit_right(&fmt::pct(share * 100.0), 6),
            Style::new().fg(th.fg),
        ));
        sp.push(Span::styled(
            format!(
                " {} {} {}",
                fit_right(&fmt::money(r.cost / r.requests.max(1) as f64), 7),
                fit_right(
                    &u.cache_hit_ratio()
                        .map(|x| fmt::pct(x * 100.0))
                        .unwrap_or_else(|| "-".into()),
                    6
                ),
                fit_right(
                    &if u.output > 0 {
                        fmt::pct(u.thinking as f64 / u.output as f64 * 100.0)
                    } else {
                        "-".into()
                    },
                    5
                )
            ),
            dim(app),
        ));
        if show_mix {
            let mut mix: Vec<(String, f64)> = an
                .effort
                .iter()
                .filter(|(m, _, _)| *m == r.key)
                .map(|(_, e, c)| (e.clone(), *c))
                .collect();
            mix.sort_by(|a, b| b.1.total_cmp(&a.1));
            let msum: f64 = mix.iter().map(|x| x.1).sum::<f64>().max(1e-9);
            let s = mix
                .iter()
                .filter(|x| x.1 / msum >= 0.02)
                .map(|(e, c)| format!("{} {:.0}%", effort_abbr(Some(e)).trim(), c / msum * 100.0))
                .collect::<Vec<_>>()
                .join(" ");
            sp.push(Span::styled(format!("  {s}"), faint(app)));
        }
        lines.push(Line::from(sp));
    }
    if !an.by_model.is_empty() && (lines.len() as u16) + 2 < inner.height {
        // One bar split by model family.
        let mut fam: std::collections::BTreeMap<Family, f64> = Default::default();
        for r in &an.by_model {
            *fam.entry(Family::of(&r.key)).or_default() += r.cost;
        }
        let mut bar = Vec::new();
        let mut legend = Vec::new();
        for (fm, c) in &fam {
            let cells = ((c / total) * width.saturating_sub(1) as f64).round() as usize;
            bar.push(Span::styled(
                "█".repeat(cells),
                Style::new().fg(th.family(*fm)),
            ));
            legend.push(Span::styled(
                format!("■ {} {}  ", fm.name(), fmt::pct(c / total * 100.0)),
                Style::new().fg(th.family(*fm)),
            ));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(bar));
        lines.push(Line::from(legend));
    }
    // Project × model, when tall enough.
    if !an.project_model.is_empty() && (lines.len() as u16) + 4 < inner.height {
        let mut models: Vec<String> = an.by_model.iter().map(|r| r.key.clone()).collect();
        let cell = 11usize;
        models.truncate(((width.saturating_sub(30)) / (cell + 2)).max(1));
        let mut projects: Vec<(String, f64)> = Vec::new();
        for (p, _, c) in &an.project_model {
            match projects.iter_mut().find(|(k, _)| k == p) {
                Some(e) => e.1 += c,
                None => projects.push((p.clone(), *c)),
            }
        }
        projects.sort_by(|a, b| b.1.total_cmp(&a.1));
        let max = an
            .project_model
            .iter()
            .map(|x| x.2)
            .fold(0.0, f64::max)
            .max(1e-9);
        lines.push(Line::from(""));
        let mut head = vec![
            Span::styled(" PROJECT × MODEL ", th.heading()),
            Span::raw(" "),
        ];
        for m in &models {
            head.push(Span::styled(
                fit_right(&short_name(m), cell + 2),
                th.bold(th.family_name(m)),
            ));
        }
        lines.push(Line::from(head));
        let room = (inner.height as usize).saturating_sub(lines.len());
        for (p, _) in projects.iter().take(room) {
            let label = if app.mask {
                crate::ui::table::truncate(p, 10)
            } else {
                p.clone()
            };
            let mut row = vec![Span::styled(fit(&label, 18), Style::new().fg(th.fg))];
            for m in &models {
                let c = an
                    .project_model
                    .iter()
                    .find(|(pp, mm, _)| pp == p && mm == m)
                    .map(|x| x.2)
                    .unwrap_or(0.0);
                let k = (c / max) as f32;
                row.push(Span::raw("  "));
                if c <= 0.0 {
                    row.push(Span::styled(fit_right("·", cell), faint(app)));
                } else {
                    row.push(Span::styled(
                        fit_right(&format!("{} ", fmt::money(c)), cell),
                        Style::new()
                            .fg(if k > 0.45 { th.bg } else { th.fg })
                            .bg(th.fade(th.family_name(m), 0.78 - k * 0.78)),
                    ));
                }
            }
            lines.push(Line::from(row));
        }
    }
    if an.by_model.is_empty() {
        lines.push(Line::from(Span::styled(
            "no requests in this period",
            dim(app),
        )));
    }
    put(f, inner, lines);
}

// ───────────────────────────── HISTORY ─────────────────────────────

pub fn history(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = frame(
        f,
        app,
        Panel::History,
        area,
        "14 days · weekday × hour",
        false,
    );
    let app: &App = app;
    let th = app.theme;
    if inner.height < 6 {
        return;
    }
    let by_day: std::collections::HashMap<&str, f64> = app
        .analytics
        .days
        .iter()
        .map(|d| (d.key.as_str(), d.cost))
        .collect();
    let today = Local::now().date_naive();
    let days: Vec<(String, f64)> = (0..14)
        .rev()
        .map(|i| {
            let k = (today - chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string();
            let c = by_day.get(k.as_str()).copied().unwrap_or(0.0);
            (k, c)
        })
        .collect();
    let heat_h: u16 = 9;
    let bars_h = inner.height.saturating_sub(heat_h + 1).max(4);
    let col = ((inner.width as usize) / 14).max(2);
    let h = bars_h.saturating_sub(2) as usize;
    let max = days.iter().map(|d| d.1).fold(0.0, f64::max).max(1e-9);
    let mut lines = Vec::new();
    for r in (0..h).rev() {
        let mut spans = Vec::new();
        for (k, c) in &days {
            let level = c / max * h as f64;
            let ch = if level >= r as f64 + 1.0 {
                '█'
            } else if level > r as f64 {
                VBARS[(((level - r as f64) * 8.0) as usize).clamp(0, 7)]
            } else {
                ' '
            };
            let color = if *k == today.format("%Y-%m-%d").to_string() {
                pulse(app, th.accent, 0.5)
            } else {
                th.fade(th.accent, 0.2)
            };
            spans.push(Span::styled(
                format!("{} ", ch.to_string().repeat(col - 1)),
                Style::new().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }
    let labels: String = days
        .iter()
        .map(|(k, _)| fit(k.get(8..10).unwrap_or(""), col))
        .collect();
    lines.push(Line::from(Span::styled(labels, faint(app))));
    lines.push(Line::from(Span::styled(
        format!(
            "max {}/day · 14 days {}",
            fmt::money(max),
            fmt::money(days.iter().map(|d| d.1).sum())
        ),
        faint(app),
    )));
    put(
        f,
        Rect {
            height: bars_h,
            ..inner
        },
        lines,
    );

    // Weekday × hour heatmap.
    let heat = Rect {
        y: inner.y + bars_h + 1,
        height: inner.height.saturating_sub(bars_h + 1),
        ..inner
    };
    let mut grid = [[0.0f64; 24]; 7];
    for (wd, hr, c) in &app.analytics.heat {
        if let (Ok(d), Ok(h)) = (wd.parse::<usize>(), hr.parse::<usize>())
            && d < 7
            && h < 24
        {
            grid[d][h] += c;
        }
    }
    let hmax = grid.iter().flatten().copied().fold(0.0, f64::max).max(1e-9);
    let cw = (((heat.width as usize).saturating_sub(4)) / 24).max(1);
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mut lines = Vec::new();
    for d in [1, 2, 3, 4, 5, 6, 0] {
        let mut spans = vec![Span::styled(format!("{} ", names[d]), faint(app))];
        for hh in grid[d] {
            let k = (hh / hmax) as f32;
            let c = if hh <= 0.0 {
                th.track
            } else {
                th.fade(th.accent, 0.85 - k * 0.85)
            };
            let cell = if cw >= 2 {
                format!("{} ", "█".repeat(cw - 1))
            } else {
                "■".into()
            };
            spans.push(Span::styled(cell, Style::new().fg(c)));
        }
        lines.push(Line::from(spans));
    }
    let mut hours = String::from("    ");
    for hh in 0..24 {
        let l = if hh % 6 == 0 {
            hh.to_string()
        } else {
            String::new()
        };
        hours.push_str(&fit(&l, cw));
    }
    lines.push(Line::from(Span::styled(hours, faint(app))));
    put(f, heat, lines);
}

// ───────────────────────────── EVENTS ─────────────────────────────

pub fn events(f: &mut Frame, app: &mut App, area: Rect) {
    let n = app.events.len();
    let inner = frame(f, app, Panel::Events, area, &format!("{n}"), false);
    let app: &App = app;
    let th = app.theme;
    let start = app.cur(Panel::Events).min(n.saturating_sub(1));
    let mut lines = Vec::new();
    for e in app.events.iter().skip(start).take(inner.height as usize) {
        let mut c = app.tone_color(e.tone);
        let age = e.born.elapsed().as_secs_f32();
        if age < 1.0 && app.animating() {
            c = th.glow(c, (1.0 - age) * 0.6);
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", e.at.with_timezone(&Local).format("%H:%M:%S")),
                faint(app),
            ),
            Span::styled(format!("{} ", e.glyph), th.bold(c)),
            Span::styled(
                e.text.clone(),
                Style::new().fg(if e.tone == crate::tui::app::Tone::Info {
                    th.dim
                } else {
                    th.fg
                }),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("listening…", faint(app))));
    }
    put(f, inner, lines);
}

// ───────────────────────────── SYSTEM ─────────────────────────────

pub fn system(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = frame(
        f,
        app,
        Panel::System,
        area,
        "Claude Code processes and resources",
        false,
    );
    let app: &App = app;
    let th = app.theme;
    let parts = hsplit(inner, &[62, 38]);
    let (procs, res) = (parts[0], parts[1]);
    let hb = crate::cli::doctor::human_bytes;
    let mut lines = Vec::new();
    for (seat, name, pid, cpu, mem, kids) in &app.system.instances {
        lines.push(Line::from(vec![
            Span::styled(format!("● {name}"), th.bold(th.fg)),
            Span::styled(format!("  {seat} · pid {pid}"), dim(app)),
            Span::styled(
                format!("  cpu {cpu:.0}% · {}", hb(*mem)),
                Style::new().fg(if *cpu > 200.0 { th.warn } else { th.accent }),
            ),
        ]));
        for (cmd, c, m) in kids {
            let cw = (procs.width as usize).saturating_sub(22);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {:>5.0}% {:>8}  ", c, hb(*m)),
                    Style::new().fg(if *c > 50.0 { th.warn } else { th.dim }),
                ),
                Span::styled(
                    fit(
                        if app.mask {
                            cmd.split_whitespace().next().unwrap_or("")
                        } else {
                            cmd
                        },
                        cw,
                    ),
                    Style::new().fg(th.fg),
                ),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "reading the process table…",
            dim(app),
        )));
    }
    put(f, procs, lines);
    let s = &app.system;
    let gw = (res.width as usize).saturating_sub(20).max(6);
    let mut lines = Vec::new();
    if s.mem_total > 0 {
        let used = s.mem_total.saturating_sub(s.mem_available) as f64 / s.mem_total as f64;
        let mut sp = vec![Span::styled("memory ", dim(app))];
        sp.extend(gauge(used, gw, th.utilization(used * 100.0), &th));
        lines.push(Line::from(sp));
        lines.push(Line::from(Span::styled(
            format!("       {} free of {}", hb(s.mem_available), hb(s.mem_total)),
            dim(app),
        )));
    }
    if let Some(free) = s.disk_free {
        lines.push(Line::from(vec![
            Span::styled("disk   ", dim(app)),
            Span::styled(
                format!("{} free in home", hb(free)),
                Style::new().fg(if free < 60 << 30 { th.warn } else { th.fg }),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " TRANSCRIPTS ON DISK ",
        th.heading(),
    )));
    let max = app
        .analytics
        .transcripts_bytes
        .iter()
        .map(|x| x.1)
        .max()
        .unwrap_or(1)
        .max(1);
    for (seat, b) in &app.analytics.transcripts_bytes {
        let mut sp = vec![Span::styled(fit(seat, 10), Style::new().fg(th.fg))];
        sp.extend(gauge(
            *b as f64 / max as f64,
            gw.saturating_sub(4),
            th.fade(th.accent, 0.3),
            &th,
        ));
        sp.push(Span::styled(format!(" {}", hb(*b)), dim(app)));
        lines.push(Line::from(sp));
    }
    lines.push(Line::from(Span::styled(
        format!("index      {}", hb(app.analytics.index_bytes)),
        dim(app),
    )));
    put(f, res, lines);
}
