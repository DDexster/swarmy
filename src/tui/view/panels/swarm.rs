//! SWARM: the agent tree of every session, with activity lanes or spend sparklines.

use chrono::{Local, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{dim, faint};
use crate::core::models::{Family, context_window, short_name};
use crate::live::State;
use crate::tui::app::{App, Panel};
use crate::tui::view::kit::*;
use crate::ui::fmt;

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
            vec![Line::from(Span::styled("reading transcripts…", dim(app)))]
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
                    "start `claude` or `agy` in any seat and it shows up here within a second",
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
    let show_har = app.multi_harness();
    let har_w: usize = if show_har { 4 } else { 0 };
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
        + har_w
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
        "   {}{} {} {} {}",
        if show_har {
            fit("HAR", har_w)
        } else {
            String::new()
        },
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
                    crate::ui::fmt::human_bytes(a.memory)
                ));
            }
            let room = act_w.saturating_sub(w(&extra_s) + 2);
            act = format!("{}  {}", fit(&act, room), extra_s);
        }
        let act_style = match &a.state {
            State::Tool { .. } => Style::new().fg(th.fg),
            s => Style::new().fg(th.state(s)),
        };
        let mut spans = vec![Span::styled(format!("{g}  "), Style::new().fg(gc))];
        if show_har {
            if a.is_main() {
                // The harness is a fact about the session, not a model: plain text, no colour.
                let hc = if quiet { th.faint } else { th.dim };
                spans.push(Span::styled(
                    fit(a.harness.short(), har_w),
                    Style::new().fg(hc),
                ));
            } else {
                spans.push(Span::raw(" ".repeat(har_w)));
            }
        }
        spans.push(Span::styled(tree.clone(), Style::new().fg(th.faint)));
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
        let unpriced = a.unpriced
            && if a.is_main() {
                a.session_cost == 0.0
            } else {
                a.cost == 0.0
            };
        let cost = if unpriced {
            "—".to_string()
        } else if a.is_main() {
            format!("Σ{}", fmt::usd(a.session_cost))
        } else {
            fmt::usd(a.cost)
        };
        spans.push(Span::styled(
            format!(" {}", fit_right(&cost, cost_w)),
            if unpriced || quiet {
                Style::new().fg(th.dim)
            } else {
                th.bold(th.fg)
            },
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
