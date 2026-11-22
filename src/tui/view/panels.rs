//! The panels. Each one fills the rectangle it is given: columns, bar widths and row counts come
//! from the size, so the same panel works in a 40-column strip and on a 4K wall.

use chrono::{Datelike, Local, Utc};
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
    let suffix = if app.lanes && area.width >= 150 {
        format!(
            "{suffix} · last 30 min: ▃ thinking ▅ tool ▆ waiting for agents █ waiting for you · v switches"
        )
    } else {
        suffix
    };
    let inner = frame(f, app, Panel::Swarm, area, &suffix, false);
    let app: &App = app;
    let th = app.theme;
    let agents = app.visible_agents();
    if agents.is_empty() {
        let lines: Vec<Line> = if app.snapshot.taken_at.is_none() {
            vec![Line::from(Span::styled(
                "reading Claude Code's files…",
                dim(app),
            ))]
        } else if app.filter.as_deref().is_some_and(|f| !f.is_empty()) {
            vec![Line::from(Span::styled(
                "no agent matches the filter · Esc clears it",
                dim(app),
            ))]
        } else {
            vec![
                Line::from(Span::styled(
                    "no agents active in the last 30 minutes",
                    dim(app),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "start `claude` in any seat and it shows up here within a second",
                    faint(app),
                )),
                Line::from(Span::styled(
                    "`swarmy doctor` lists the seats and accounts that were found · ? explains the screen",
                    faint(app),
                )),
            ]
        };
        put(f, inner, lines);
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
    let spark_w = if width >= 150 {
        if app.lanes { 31 } else { 16 }
    } else {
        0
    };
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
        fit_right("IN STATE", t_w)
    );
    if ctx_w > 0 {
        header.push_str(&format!(" {}", fit("CONTEXT", ctx_w - 1)));
    }
    if spark_w > 0 {
        header.push_str(&format!(
            " {}",
            fit(
                if app.lanes {
                    "LAST 30 MIN          now"
                } else {
                    "$ · 15 min"
                },
                spark_w - 1
            )
        ));
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
    // Sessions are separated by a rule; keep the selection visible with those extra lines.
    let mut offset = app.swarm_offset(rows_h);
    let lines_to = |from: usize, to: usize| {
        (from..=to.min(agents.len().saturating_sub(1)))
            .map(|k| 1 + usize::from(k > from && agents[k].is_main()))
            .sum::<usize>()
    };
    while offset < sel && lines_to(offset, sel) > rows_h {
        offset += 1;
    }
    let now = Utc::now();
    let rule = "─".repeat(inner.width as usize);
    for (i, a) in agents.iter().enumerate().skip(offset) {
        if a.is_main() && i > offset {
            if lines.len() > rows_h {
                break;
            }
            lines.push(Line::from(Span::styled(
                rule.clone(),
                Style::new().fg(th.track),
            )));
        }
        if lines.len() > rows_h {
            break;
        }
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
        } else if elapsed.num_minutes() >= 10 {
            fmt::duration(elapsed)
        } else {
            // Short spans to the second: `2m38s`, never `2:38` (which reads as hours).
            let sec = elapsed.num_seconds().max(0);
            if sec >= 60 {
                format!("{}m{:02}s", sec / 60, sec % 60)
            } else {
                format!("{sec}s")
            }
        };
        let mut act = activity(&a.state);
        if a.state == State::Gone {
            act = format!("exited {} ago", fmt::duration(now - a.last_activity));
        } else if a.is_main() && act_w >= 48 {
            let mut extra_s = format!("{}/h", fmt::usd(a.session_burn));
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
        ];
        // A session row: its name, then the project in the directory colour.
        let nw = name_w.saturating_sub(w(&tree));
        if a.is_main() {
            let first = fit(&a.name, nw);
            let first = first.trim_end().to_string();
            let left = nw.saturating_sub(w(&first));
            spans.push(Span::styled(first, th.bold(fg)));
            spans.push(Span::styled(
                fit(&format!(" · {}", a.project), left),
                if quiet {
                    Style::new().fg(th.fade(th.dir, 0.35))
                } else {
                    th.bold(th.dir)
                },
            ));
        } else {
            spans.push(Span::styled(fit(&name, nw), Style::new().fg(fg)));
        }
        spans.extend([
            Span::styled(
                format!(" {}", fit(&model_chip(a), model_w)),
                Style::new().fg(if quiet { th.dim } else { th.family(a.family()) }),
            ),
            Span::styled(format!(" {}", fit(&act, act_w)), act_style),
            Span::styled(format!(" {}", fit_right(&timer, t_w)), dim(app)),
        ]);
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
        if spark_w > 0 && app.lanes {
            use crate::live::Lane;
            spans.push(Span::raw(" "));
            let lane = &a.lane;
            for k in 0..spark_w - 1 {
                let v = lane
                    .get(k + lane.len().saturating_sub(spark_w - 1))
                    .copied()
                    .unwrap_or_default();
                let (ch, c) = match v {
                    Lane::Quiet => ("·", th.track),
                    Lane::Thinking => ("▃", th.thinking),
                    Lane::Tool => ("▅", th.good),
                    Lane::Agents => ("▆", th.accent),
                    Lane::You => ("█", th.warn),
                };
                spans.push(Span::styled(ch, Style::new().fg(c)));
            }
        } else if spark_w > 0 {
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
            format!("Σ{}", fmt::usd(a.session_cost))
        } else {
            fmt::usd(a.cost)
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
                    fmt::usd(total)
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
                Span::styled(
                    format!(" {}", fit(&d.project, proj_w)),
                    Style::new().fg(th.fade(th.dir, 0.3)),
                ),
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
                    format!(" {}", fit_right(&fmt::usd(d.cost), 10)),
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
    lines.push(Line::from(vec![
        Span::styled(fit("where", kw), faint(app)),
        Span::styled(a.project.clone(), th.bold(th.dir)),
        Span::styled(
            format!(
                " · {} · {}",
                a.seat,
                &a.session_id[..8.min(a.session_id.len())]
            ),
            Style::new().fg(th.fg),
        ),
    ]));
    let cost = if a.is_main() {
        format!(
            "{} · session Σ{} · {}/h",
            fmt::usd(a.cost),
            fmt::usd(a.session_cost),
            fmt::usd(a.session_burn)
        )
    } else {
        format!("{} · {} requests", fmt::usd(a.cost), fmt::count(a.requests))
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
        // The message gets what the life chart (at most 12 lines) leaves.
        let room = (card.height as usize)
            .saturating_sub(lines.len() + 13)
            .clamp(3, 14);
        for l in wrap(
            if app.mask { "•••" } else { msg },
            card.width as usize,
            room,
        ) {
            lines.push(Line::from(Span::styled(l, Style::new().fg(th.fg))));
        }
    }
    let used = lines.len() as u16;
    put(f, card, lines);
    let life_area = Rect {
        y: card.y + used + 1,
        height: card.height.saturating_sub(used + 1).min(12),
        ..card
    };
    if life_area.height >= 6 && app.life.0 == a.key() {
        agent_life(f, app, &a, life_area);
    }

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
                let eta = weekly.and_then(|w| {
                    if w.estimate_now >= 100.0 {
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
    let wins: Vec<(usize, &crate::cli::limits::WindowReport)> = app
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
            w.points_per_hour.filter(|p| *p > 0.0).map(|pph| {
                let left = (w.window.end - now).num_seconds().max(0) as f64 / 3600.0;
                (hours(w.window.end), w.estimate_now + pph * left)
            })
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
            _ if w.estimate_now >= 100.0 => " · limit reached".to_string(),
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
    let period_label = app
        .analytics
        .period
        .as_ref()
        .map(|p| p.label())
        .unwrap_or_default();
    let today = fmt::usd(app.snapshot.today_cost);
    let period_cost = app.analytics.total.as_ref().map(|t| fmt::usd(t.cost));
    // The left column fits the big number and both captions, so the chart never covers them.
    let labels_w = (6 + w(&today)).max(
        period_cost
            .as_ref()
            .map_or(0, |c| w(&period_label) + 1 + w(c)),
    );
    let big_w = (bigdigits::width(&rate).max(labels_w).max(10) + 2) as u16;
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
            Span::styled(today, th.bold(th.fg)),
        ]));
        if let Some(c) = period_cost {
            lines.push(Line::from(vec![
                Span::styled(format!("{period_label} "), faint(app)),
                Span::styled(c, th.bold(th.fg)),
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
        format!("peak {}/min · −60 min … now", fmt::usd(max / 1.1)),
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
    let wide = inner.width >= 58;
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
        Span::styled(lbl("you  "), faint(app)),
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
    let height = inner.height as usize;
    // Alerts first; the event log takes the rest (at least a few lines when there is room).
    let room_for_items = if height >= 10 {
        height - 2 - 4
    } else {
        height.saturating_sub(2)
    };
    let item_offset = if cur < items.len() && cur >= room_for_items {
        cur + 1 - room_for_items
    } else {
        0
    };
    for (i, a) in items
        .iter()
        .enumerate()
        .skip(item_offset)
        .take(room_for_items)
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
    if lines.len() + 3 <= height {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" EVENTS ", th.heading()),
            Span::styled(format!(" · {}", app.events.len()), faint(app)),
        ]));
        let start = cur.saturating_sub(items.len());
        let room = height - lines.len();
        for (k, e) in app.events.iter().enumerate().skip(start).take(room) {
            let mut c = app.tone_color(e.tone);
            let age = e.born.elapsed().as_secs_f32();
            if age < 1.0 && app.animating() {
                c = th.glow(c, (1.0 - age) * 0.6);
            }
            let mut line = Line::from(vec![
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
            ]);
            if focused && cur >= items.len() && k == start {
                line = line.style(Style::new().bg(th.sel));
            }
            lines.push(line);
        }
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
        &if period == "all time" {
            format!("{period} · Enter shows its agents")
        } else {
            format!(
                "{period} · ▲▼ vs {} · Enter shows its agents",
                prev_label(&period)
            )
        },
        false,
    );
    let app: &App = app;
    let th = app.theme;
    let width = inner.width as usize;
    let (live_w, hour_w, today_w, per_w, share_w, pts_w) =
        (5usize, 9usize, 10usize, 10usize, 6usize, 10usize);
    let name_w = 18usize.min(width / 4).max(10);
    let fixed = name_w + live_w + hour_w + today_w + per_w + 6 + share_w + pts_w + 7;
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
            fit_right(&format!("{}      ", period.to_uppercase()), per_w + 6),
            fit("", bar_w),
            fit_right("SHARE", share_w),
            fit_right("% OF WEEK", pts_w)
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
                            fmt::usd(p.hour)
                        } else {
                            "·".into()
                        },
                        hour_w
                    )
                ),
                Style::new().fg(if p.hour > 0.0 { th.accent } else { th.faint }),
            ),
            Span::styled(
                format!(" {}", fit_right(&fmt::usd(p.today), today_w)),
                Style::new().fg(th.fg),
            ),
            Span::styled(
                format!(" {}", fit_right(&fmt::usd(p.period), per_w)),
                th.bold(th.fg),
            ),
            trend_w(app, p.period, p.prev, 6),
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
                        format!("≈{pts:.1}%")
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
    let sub = if period == "all time" {
        period.clone()
    } else {
        format!("{period} · ▲▼ vs {}", prev_label(&period))
    };
    let inner = frame(f, app, Panel::Models, area, &sub, false);
    let app: &App = app;
    let th = app.theme;
    let an = &app.analytics;
    let width = inner.width as usize;
    let total: f64 = an.by_model.iter().map(|r| r.cost).sum::<f64>().max(1e-9);
    let show_mix = width >= 95;
    let fixed = 11 + 9 + 11 + 6 + 8 + 6 + 6 + 6 + if show_mix { 24 } else { 0 } + 8;
    let bar_w = width.saturating_sub(fixed).clamp(4, 40);
    let mut header = format!(
        "{} {} {} {} {} {} {} {}",
        fit("MODEL", 11),
        fit_right("REQS", 8),
        fit_right("COST      ", 16),
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
                    " {} {}",
                    fit_right(&fmt::count(r.requests), 8),
                    fit_right(&fmt::usd(r.cost), 10)
                ),
                Style::new().fg(th.fg),
            ),
            trend_w(
                app,
                r.cost,
                an.prev_models
                    .iter()
                    .find(|(k, _)| *k == r.key)
                    .map(|x| x.1)
                    .unwrap_or(0.0),
                6,
            ),
            Span::raw(" "),
        ];
        sp.extend(gauge(share, bar_w, c, &th));
        sp.push(Span::styled(
            fit_right(&fmt::pct(share * 100.0), 6),
            Style::new().fg(th.fg),
        ));
        sp.push(Span::styled(
            format!(
                " {} {} {}",
                fit_right(&fmt::usd(r.cost / r.requests.max(1) as f64), 7),
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
                        fit_right(&format!("{} ", fmt::usd(c)), cell),
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
    let heat_h: u16 = 10;
    // Bars, labels and the selected day's line.
    let bars_h = inner.height.saturating_sub(heat_h + 1).max(5);
    let col = ((inner.width as usize) / 14).max(2);
    let h = bars_h.saturating_sub(3) as usize;
    let max = days.iter().map(|d| d.1).fold(0.0, f64::max).max(1e-9);
    // Per day: (family, cost) from the bottom up, biggest first.
    let fams = [
        Family::Opus,
        Family::Fable,
        Family::Sonnet,
        Family::Haiku,
        Family::Other,
    ];
    let mut stacks: std::collections::HashMap<&str, Vec<(Family, f64)>> = Default::default();
    for (d, m, c) in &app.analytics.days_model {
        let fam = match Family::of(m) {
            Family::Mythos => Family::Fable,
            x => x,
        };
        let e = stacks.entry(d.as_str()).or_default();
        match e.iter_mut().find(|x| x.0 == fam) {
            Some(x) => x.1 += c,
            None => e.push((fam, *c)),
        }
    }
    for v in stacks.values_mut() {
        v.sort_by_key(|x| fams.iter().position(|f| *f == x.0).unwrap_or(9));
    }
    let sel = 13usize.saturating_sub(app.cur(Panel::History).min(13));
    let focused = app.focus == Panel::History;
    let today_k = today.format("%Y-%m-%d").to_string();
    let weekend = |k: &str| {
        chrono::NaiveDate::parse_from_str(k, "%Y-%m-%d")
            .map(|d| matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun))
            .unwrap_or(false)
    };
    let mut lines = Vec::new();
    for r in (0..h).rev() {
        let mut spans = Vec::new();
        for (i, (k, c)) in days.iter().enumerate() {
            let level = c / max * h as f64;
            let ch = if level >= r as f64 + 1.0 {
                '█'
            } else if level > r as f64 {
                VBARS[(((level - r as f64) * 8.0) as usize).clamp(0, 7)]
            } else {
                ' '
            };
            // The family that fills the middle of this cell.
            let mid = (r as f64 + 0.5).min(level) / h as f64 * max;
            let mut acc = 0.0;
            let mut fam = Family::Other;
            for (f, v) in stacks.get(k.as_str()).map(|v| v.as_slice()).unwrap_or(&[]) {
                fam = *f;
                acc += v;
                if acc >= mid {
                    break;
                }
            }
            let mut color = th.family(fam);
            if *k == today_k {
                color = pulse(app, color, 0.5);
            } else if focused && i != sel {
                color = th.fade(color, 0.45);
            } else if weekend(k) {
                color = th.fade(color, 0.3);
            }
            spans.push(Span::styled(
                format!("{} ", ch.to_string().repeat(col - 1)),
                Style::new().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }
    let labels: Vec<Span> = days
        .iter()
        .enumerate()
        .map(|(i, (k, _))| {
            let d = fit(k.get(8..10).unwrap_or(""), col);
            if i == sel && focused {
                Span::styled(d, Style::new().fg(th.bg).bg(th.accent))
            } else if *k == today_k {
                Span::styled(d, th.bold(th.accent))
            } else if weekend(k) {
                Span::styled(d, Style::new().fg(th.warn).add_modifier(Modifier::DIM))
            } else {
                Span::styled(d, faint(app))
            }
        })
        .collect();
    lines.push(Line::from(labels));
    // The selected day (today unless the panel is focused and moved).
    let (k, c) = &days[sel];
    let date = chrono::NaiveDate::parse_from_str(k, "%Y-%m-%d").ok();
    let mut sp = vec![
        Span::styled(
            date.map(|d| d.format("%a %d %b").to_string())
                .unwrap_or_default(),
            th.bold(th.fg),
        ),
        Span::styled(format!(" {}", fmt::usd(*c)), th.bold(th.fg)),
    ];
    if sel > 0 {
        sp.push(trend(app, *c, days[sel - 1].1));
        sp.push(Span::styled(" vs day before", faint(app)));
    }
    for (fam, v) in stacks.get(k.as_str()).map(|v| v.as_slice()).unwrap_or(&[]) {
        if *v / c.max(1e-9) >= 0.005 {
            sp.push(Span::styled(
                format!("  ■ {} {}", fam.name(), fmt::pct(v / c.max(1e-9) * 100.0)),
                Style::new().fg(th.family(*fam)),
            ));
        }
    }
    lines.push(Line::from(sp));
    lines.push(Line::from(Span::styled(
        format!(
            "max {}/day · 14 days {}{}",
            fmt::usd(max),
            fmt::usd(days.iter().map(|d| d.1).sum()),
            if focused { " · ←→ pick a day" } else { "" }
        ),
        faint(app),
    )));
    put(
        f,
        Rect {
            height: bars_h + 1,
            ..inner
        },
        lines,
    );

    // Weekday × hour heatmap.
    let heat = Rect {
        y: inner.y + bars_h + 2,
        height: inner.height.saturating_sub(bars_h + 2),
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
    // Colour key of the heat map.
    let mut key = vec![Span::styled("    less ", faint(app))];
    for k in [0.85f32, 0.6, 0.35, 0.0] {
        key.push(Span::styled("█", Style::new().fg(th.fade(th.accent, k))));
    }
    key.push(Span::styled(
        " more · spend per hour, last 4 weeks, local time",
        faint(app),
    ));
    lines.push(Line::from(key));
    put(f, heat, lines);
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

/// The agent's life: what each request cost (bars) and how the context grew (line), from its
/// first request to now.
fn agent_life(f: &mut Frame, app: &App, a: &crate::live::Agent, area: Rect) {
    let th = app.theme;
    let pts = &app.life.1;
    let (Some(first), Some(last)) = (pts.first(), pts.last()) else {
        return;
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(" LIFE ", th.heading()),
        Span::styled(
            format!(
                " · {} requests over {} · max {}/req",
                fmt::count(pts.len() as u64),
                fmt::duration(Utc::now() - first.ts),
                fmt::usd(pts.iter().map(|p| p.cost).fold(0.0, f64::max))
            ),
            faint(app),
        ),
    ])];
    put(f, Rect { height: 1, ..area }, std::mem::take(&mut lines));
    let chart = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    let legend = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    let t0 = first.ts;
    let span = ((Utc::now() - t0).num_seconds().max(60)) as f64;
    let x = |t: chrono::DateTime<Utc>| (t - t0).num_seconds() as f64 / span * 100.0;
    // Costs summed into as many columns as the chart has braille dots.
    let cols = (chart.width as usize * 2).max(2);
    let mut bins = vec![0.0f64; cols];
    for p in pts {
        let i = ((x(p.ts) / 100.0) * (cols - 1) as f64).round() as usize;
        bins[i.min(cols - 1)] += p.cost;
    }
    let bmax = bins.iter().copied().fold(0.0, f64::max).max(1e-9);
    let win = a.model.as_deref().map(context_window).unwrap_or(1_000_000) as f64;
    let ctx_pts: Vec<(f64, f64)> = pts
        .iter()
        .map(|p| (x(p.ts), p.context as f64 / win * 100.0))
        .collect();
    let (bar_c, ctx_c, track) = (th.family(a.family()), th.thinking, th.track);
    let compactions: Vec<f64> = pts
        .windows(2)
        .filter(|w| w[1].context * 2 < w[0].context && w[0].context > 50_000)
        .map(|w| x(w[1].ts))
        .collect();
    let warn = th.warn;
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, 100.0])
        .y_bounds([0.0, 100.0])
        .paint(move |ctx| {
            for (i, b) in bins.iter().enumerate() {
                if *b > 0.0 {
                    let xx = i as f64 / (cols - 1) as f64 * 100.0;
                    ctx.draw(&CLine::new(xx, 0.0, xx, b / bmax * 60.0, bar_c));
                }
            }
            for w in ctx_pts.windows(2) {
                ctx.draw(&CLine::new(w[0].0, w[0].1, w[1].0, w[1].1, ctx_c));
            }
            for c in &compactions {
                ctx.draw(&CLine::new(*c, 0.0, *c, 100.0, warn));
            }
            ctx.draw(&CLine::new(0.0, 0.0, 100.0, 0.0, track));
        });
    f.render_widget(canvas, chart);
    let now_ctx = last.context as f64 / win * 100.0;
    put(
        f,
        legend,
        vec![Line::from(vec![
            Span::styled("▮ $ per request  ", Style::new().fg(bar_c)),
            Span::styled(
                format!("─ context {}  ", fmt::pct(now_ctx)),
                Style::new().fg(ctx_c),
            ),
            Span::styled("│ compaction", Style::new().fg(th.warn)),
        ])],
    );
}

// ───────────────────────────── EFFICIENCY ─────────────────────────────

/// `245h`, `3.6h`, `12m`, `40s`.
fn hrs(h: f64) -> String {
    if h >= 100.0 {
        format!("{h:.0}h")
    } else if h >= 1.0 {
        format!("{h:.1}h")
    } else if h * 60.0 >= 1.0 {
        format!("{:.0}m", h * 60.0)
    } else {
        format!("{:.0}s", h * 3600.0)
    }
}

/// The period a trend compares with: "last 7d" → "previous 7d", "today" → "yesterday".
pub fn prev_label(period: &str) -> String {
    match period {
        "today" => "yesterday".into(),
        p => match p.strip_prefix("last ") {
            Some(rest) => format!("previous {rest}"),
            None => format!("before {p}"),
        },
    }
}

/// `▲12%` / `▼8%` against the previous period; nothing when there is no base.
pub fn trend(app: &App, cur: f64, prev: f64) -> Span<'static> {
    let th = app.theme;
    if prev <= 0.0 || !cur.is_finite() {
        return Span::raw("");
    }
    let d = (cur - prev) / prev * 100.0;
    if d.abs() < 1.0 {
        return Span::styled(" ≈", faint(app));
    }
    let (g, c) = if d > 0.0 {
        ("▲", th.warn)
    } else {
        ("▼", th.good)
    };
    let v = if d.abs() >= 1000.0 {
        ">999".to_string()
    } else {
        format!("{:.0}", d.abs())
    };
    Span::styled(format!(" {g}{v}%"), Style::new().fg(c))
}

/// `trend` right-aligned in `width` cells (blank when there is no base).
pub fn trend_w(app: &App, cur: f64, prev: f64, width: usize) -> Span<'static> {
    let t = trend(app, cur, prev);
    let style = t.style;
    Span::styled(fit_right(t.content.trim_start(), width), style)
}

fn section_head(app: &App, title: &str, note: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {title} "), app.theme.heading()),
        Span::styled(
            if note.is_empty() {
                note
            } else {
                format!(" · {note}")
            },
            faint(app),
        ),
    ])
}

fn time_widths(cw: usize) -> (usize, usize) {
    let name_w = 16.min(cw / 3);
    (name_w, cw.saturating_sub(name_w + 26).clamp(4, 30))
}

fn time_header(app: &App, first: &str, cw: usize) -> Line<'static> {
    let (name_w, bar_w) = time_widths(cw);
    Line::from(Span::styled(
        format!(
            "{} {} {:>6} {:>7} {:>5}",
            fit(first, name_w),
            " ".repeat(bar_w),
            "TIME",
            "CALLS",
            "ERR"
        ),
        faint(app),
    ))
}

/// Rows of `name bar value…` for tool-like stats.
fn time_rows(
    app: &App,
    rows: &[crate::index::insights::ToolStat],
    n: usize,
    cw: usize,
) -> Vec<Line<'static>> {
    let th = app.theme;
    let max = rows.iter().map(|t| t.hours).fold(0.0, f64::max).max(1e-9);
    let (name_w, bar_w) = time_widths(cw);
    rows.iter()
        .filter(|t| t.hours > 0.0 || t.calls > 0)
        .take(n)
        .map(|t| {
            let err = if t.calls > 0 {
                t.errors as f64 / t.calls as f64 * 100.0
            } else {
                0.0
            };
            let mut sp = vec![Span::styled(
                format!("{} ", fit(&t.name, name_w)),
                Style::new().fg(th.fg),
            )];
            sp.extend(gauge(t.hours / max, bar_w, th.accent, &th));
            sp.push(Span::styled(
                format!(" {:>6}", hrs(t.hours)),
                th.bold(th.fg),
            ));
            sp.push(Span::styled(
                format!(" {:>7}", fmt::count(t.calls)),
                dim(app),
            ));
            sp.push(Span::styled(
                format!(
                    " {:>5}",
                    if err >= 0.05 {
                        fmt::pct(err)
                    } else {
                        "0".to_string()
                    }
                ),
                Style::new().fg(if err >= 5.0 { th.bad } else { th.faint }),
            ));
            Line::from(sp)
        })
        .collect()
}

fn efficiency_sections(app: &App, cw: usize) -> Vec<Vec<Line<'static>>> {
    let th = app.theme;
    let Some(e) = &app.analytics.efficiency else {
        return vec![vec![Line::from(Span::styled("counting…", faint(app)))]];
    };
    let mut out = Vec::new();

    // Where agent time goes.
    let tools: Vec<_> = e
        .tools
        .iter()
        .filter(|t| !matches!(t.name.as_str(), "AskUserQuestion" | "ExitPlanMode"))
        .cloned()
        .collect();
    let total_h: f64 = tools.iter().map(|t| t.hours).sum();
    let mut s = vec![section_head(
        app,
        "WHERE AGENT TIME GOES",
        format!("{} in tools", hrs(total_h)),
    )];
    s.push(time_header(app, "TOOL", cw));
    s.extend(time_rows(app, &tools, 7, cw));
    out.push(s);

    let mut s = vec![section_head(
        app,
        "SLOWEST COMMANDS",
        "Bash, by time".into(),
    )];
    s.push(time_header(app, "COMMAND", cw));
    s.extend(time_rows(app, &e.commands, 7, cw));
    out.push(s);

    // Waiting on the user, and what a prompt buys.
    let mut s = vec![section_head(app, "WAITING ON YOU", String::new())];
    for (label, (n, h)) in [("questions", e.questions), ("plan approvals", e.plans)] {
        if n == 0 {
            continue;
        }
        s.push(Line::from(vec![
            Span::styled(fit(label, 15), Style::new().fg(th.fg)),
            Span::styled(format!("{:>5}", fmt::count(n)), th.bold(th.fg)),
            Span::styled(format!(" · avg {:>4}", hrs(h / n as f64)), dim(app)),
            Span::styled(
                format!(" · {} blocked", hrs(h)),
                Style::new().fg(if h >= 1.0 { th.warn } else { th.dim }),
            ),
        ]));
    }
    if e.prompts > 0 {
        s.push(Line::from(vec![
            Span::styled(fit("your prompts", 15), Style::new().fg(th.fg)),
            Span::styled(format!("{:>5}", fmt::count(e.prompts)), th.bold(th.fg)),
            Span::styled(
                format!(" → {} +", fmt::usd(e.cost / e.prompts as f64)),
                dim(app),
            ),
            Span::styled(
                format!(" {} agent work each", hrs(e.agent_hours / e.prompts as f64)),
                dim(app),
            ),
        ]));
    }
    if s.len() == 1 {
        s.push(Line::from(Span::styled(
            "nothing waited for you",
            faint(app),
        )));
    }
    out.push(s);

    // Cache and what the money buys.
    let sp = e.split;
    let tot = sp.total().max(1e-9);
    let mut s = vec![section_head(
        app,
        "CACHE & TOKENS",
        format!(
            "hit {} · saved ≈{} (vs none)",
            fmt::pct(e.cache_hit * 100.0),
            fmt::usd(e.cache_saved)
        ),
    )];
    let parts = [
        ("input", sp.input, th.sonnet),
        ("cache write", sp.cache_write, th.warn),
        ("cache read", sp.cache_read, th.haiku),
        ("output", sp.output, th.fable),
    ];
    let bw = cw.saturating_sub(1).max(10);
    let mut bar = Vec::new();
    let mut used = 0usize;
    for (k, (_, v, c)) in parts.iter().enumerate() {
        let n = if k == parts.len() - 1 {
            bw - used.min(bw)
        } else {
            ((v / tot) * bw as f64).round() as usize
        };
        let n = n.min(bw - used.min(bw));
        used += n;
        bar.push(Span::styled("█".repeat(n), Style::new().fg(*c)));
    }
    s.push(Line::from(bar));
    for chunk in parts.chunks(2) {
        let mut l = Vec::new();
        for (name, v, c) in chunk {
            l.push(Span::styled("■ ", Style::new().fg(*c)));
            l.push(Span::styled(
                fit(
                    &format!("{name} {} {}", fmt::usd(*v), fmt::pct(v / tot * 100.0)),
                    cw / 2 - 2,
                ),
                dim(app),
            ));
        }
        s.push(Line::from(l));
    }
    out.push(s);

    // Agent types.
    let mut s = vec![section_head(
        app,
        "AGENT TYPES",
        "cost · lifetime · unfinished".into(),
    )];
    let name_w = 16.min(cw / 3);
    s.push(Line::from(Span::styled(
        format!(
            "{} {:>5} {:>9} {:>8} {:>7} {:>5}",
            fit("TYPE", name_w),
            "N",
            "COST",
            "AVG $",
            "AVG T",
            "UNF"
        ),
        faint(app),
    )));
    for t in e.types.iter().take(7) {
        s.push(Line::from(vec![
            Span::styled(format!("{} ", fit(&t.name, name_w)), Style::new().fg(th.fg)),
            Span::styled(format!("{:>5}", fmt::count(t.agents)), dim(app)),
            Span::styled(format!(" {:>9}", fmt::usd(t.cost)), th.bold(th.fg)),
            Span::styled(
                format!(" {:>8}", fmt::usd(t.cost / t.agents.max(1) as f64)),
                dim(app),
            ),
            Span::styled(format!(" {:>7}", hrs(t.avg_minutes / 60.0)), dim(app)),
            Span::styled(
                format!(
                    " {:>5}",
                    if t.unfinished > 0 {
                        t.unfinished.to_string()
                    } else {
                        String::new()
                    }
                ),
                Style::new().fg(th.warn),
            ),
        ]));
    }
    out.push(s);

    // The fleet and its troubles.
    let peak = e.fleet.iter().copied().max().unwrap_or(0);
    let peak_at = e.fleet.iter().rposition(|n| *n == peak).unwrap_or(0);
    let peak_time = (Utc::now() - chrono::Duration::hours(24 - peak_at as i64))
        .with_timezone(&Local)
        .format("%H:00");
    let mut s = vec![section_head(
        app,
        "FLEET · 24 h",
        format!("peak {peak} agents at {peak_time}"),
    )];
    let vals: Vec<f64> = e.fleet.iter().map(|n| *n as f64).collect();
    let reps = (cw.saturating_sub(2) / vals.len().max(1)).clamp(1, 3);
    let wide: Vec<f64> = vals
        .iter()
        .flat_map(|v| std::iter::repeat_n(*v, reps))
        .collect();
    s.push(Line::from(spark(&wide, th.accent)));
    s.push(Line::from(Span::styled(
        fit(
            &format!("{:<w$}now", "−24 h", w = (wide.len()).saturating_sub(3)),
            cw,
        ),
        faint(app),
    )));
    let mut l = vec![
        Span::styled(
            format!("rate limits {}", e.rate_limits),
            Style::new().fg(if e.rate_limits > 0 { th.bad } else { th.dim }),
        ),
        Span::styled(format!(" · API errors {}", e.other_errors), dim(app)),
        Span::styled(format!(" · compactions {}", e.compactions), dim(app)),
    ];
    if e.rate_limits == 0 {
        l[0] = Span::styled("rate limits 0", dim(app));
    }
    s.push(Line::from(l));
    s.push(Line::from(vec![
        Span::styled(
            format!("files edited {}", fmt::count(e.files_edited)),
            dim(app),
        ),
        Span::styled(format!(" ({} edits)", fmt::count(e.edits)), faint(app)),
        Span::styled(
            format!(
                " · {} conflict{}",
                e.conflicts.len(),
                if e.conflicts.len() == 1 { "" } else { "s" }
            ),
            Style::new().fg(if e.conflicts.is_empty() {
                th.dim
            } else {
                th.warn
            }),
        ),
    ]));
    for (file, n) in e.conflicts.iter().take(3) {
        let name = if app.mask {
            "•••".to_string()
        } else {
            file.rsplit('/')
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("/")
        };
        s.push(Line::from(Span::styled(
            format!(
                "  ⚠ {} · {n} agents in one hour",
                fit(&name, cw.saturating_sub(24))
            ),
            Style::new().fg(th.warn),
        )));
    }
    out.push(s);

    // What the subscription is worth, when its price is configured.
    if !app.analytics.subscriptions.is_empty() {
        let mut s = vec![section_head(
            app,
            "SUBSCRIPTION VALUE",
            "API-equivalent per week".into(),
        )];
        for rep in &app.analytics.limits {
            let Some(price) = app
                .analytics
                .subscriptions
                .iter()
                .find(|(k, _)| *k == rep.name || *k == rep.account_uuid)
                .map(|x| x.1)
            else {
                continue;
            };
            let Some(win) = rep
                .windows
                .iter()
                .find(|w| w.reading.kind == WindowKind::SevenDay)
            else {
                continue;
            };
            let week_price = price * 12.0 / 52.0;
            s.push(Line::from(vec![
                Span::styled(fit(&mask_name(app, &rep.name), 18), Style::new().fg(th.fg)),
                Span::styled(
                    format!(
                        " {} vs {}/wk",
                        fmt::usd(win.cost_in_window),
                        fmt::usd(week_price)
                    ),
                    dim(app),
                ),
                Span::styled(
                    format!("  ×{:.1}", win.cost_in_window / week_price.max(1e-9)),
                    th.bold(th.good),
                ),
            ]));
        }
        if s.len() > 1 {
            out.push(s);
        }
    }
    out
}

pub fn efficiency(f: &mut Frame, app: &mut App, area: Rect) {
    let period = app
        .analytics
        .period
        .as_ref()
        .map(|p| p.label())
        .unwrap_or_default();
    // Lay the sections out in columns, page by page.
    let probe = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let ncols = ((probe.width as usize + 2) / 42).max(1);
    let cw = (probe.width as usize / ncols).saturating_sub(2).max(20);
    let height = probe.height as usize;
    let sections = efficiency_sections(app, cw);
    let ncols = ncols.min(sections.len()).max(1);
    // Each section goes to the shortest column it fits in; a new page when none has room.
    let mut pages: Vec<Vec<Vec<Line<'static>>>> = vec![vec![Vec::new(); ncols]];
    for mut sec in sections {
        sec.truncate(height);
        let page = pages.last_mut().unwrap();
        let fits = |c: &Vec<Line>| c.len() + sec.len() + usize::from(!c.is_empty()) <= height;
        match (0..ncols)
            .filter(|i| fits(&page[*i]))
            .min_by_key(|i| page[*i].len())
        {
            Some(i) => {
                if !page[i].is_empty() {
                    page[i].push(Line::from(""));
                }
                page[i].extend(sec);
            }
            None => {
                let mut p = vec![Vec::new(); ncols];
                p[0] = sec;
                pages.push(p);
            }
        }
    }
    let n = pages.len();
    let page_i = app.cur(Panel::Efficiency).min(n - 1);
    let suffix = if n > 1 {
        format!("{period} · page {}/{n} · ↑↓", page_i + 1)
    } else {
        period
    };
    let inner = frame(f, app, Panel::Efficiency, area, &suffix, false);
    let cols = hsplit(inner, &vec![1; ncols]);
    for (rect, col) in cols.iter().zip(pages.swap_remove(page_i)) {
        put(
            f,
            Rect {
                x: rect.x + 1,
                width: rect.width.saturating_sub(2),
                ..*rect
            },
            col,
        );
    }
}
