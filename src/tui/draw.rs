//! Drawing every screen.

use chrono::{Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CLine};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::app::{App, SCREENS, Screen, Tone};
use super::boot;
use super::theme::Theme;
use crate::core::models::{Family, short_name};
use crate::live::{Agent, State};
use crate::ui::fmt;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
const VBARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn w(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn fit(s: &str, width: usize) -> String {
    let t = crate::ui::table::truncate(s, width);
    let pad = width.saturating_sub(w(&t));
    format!("{t}{}", " ".repeat(pad))
}

fn fit_right(s: &str, width: usize) -> String {
    let t = crate::ui::table::truncate(s, width);
    format!("{}{t}", " ".repeat(width.saturating_sub(w(&t))))
}

fn panel<'a>(th: &Theme, title: &'a str, hot: bool) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if hot { th.warn } else { th.border }))
        .title(Line::from(vec![
            Span::styled(" ", Style::new()),
            Span::styled(title, th.bold(th.accent)),
            Span::styled(" ", Style::new()),
        ]))
        .style(th.base())
}

/// `▕█████▍   ▏`-style bar without brackets: filled part coloured, track faint.
fn gauge(frac: f64, width: usize, color: Color, th: &Theme) -> Vec<Span<'static>> {
    let frac = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let eighths = (frac * width as f64 * 8.0).round() as usize;
    let full = (eighths / 8).min(width);
    let mut s = "█".repeat(full);
    let mut used = full;
    if !eighths.is_multiple_of(8) && used < width {
        s.push(EIGHTHS[eighths % 8 - 1]);
        used += 1;
    }
    vec![
        Span::styled(s, Style::new().fg(color)),
        Span::styled("·".repeat(width - used), Style::new().fg(th.faint)),
    ]
}

fn spark(values: &[f64], color: Color) -> Span<'static> {
    let max = values.iter().copied().fold(0.0, f64::max);
    let s: String = values
        .iter()
        .map(|v| {
            if max <= 0.0 || *v <= 0.0 {
                ' '
            } else {
                VBARS[((v / max) * 7.0).round() as usize]
            }
        })
        .collect();
    Span::styled(s, Style::new().fg(color))
}

fn pulse(app: &App, c: Color, hz: f64) -> Color {
    if !app.animating() {
        return c;
    }
    let k = ((app.t() * hz * std::f64::consts::TAU).sin() * 0.5 + 0.5) as f32;
    app.theme.glow(c, k * 0.45)
}

fn blink(app: &App) -> bool {
    !app.animating() || ((app.t() * 2.0) as u64).is_multiple_of(2)
}

fn effort_abbr(e: Option<&str>) -> &'static str {
    match e.unwrap_or("") {
        "low" => "lo",
        "medium" => "md",
        "high" => "hi",
        "xhigh" => "xh",
        "max" => "mx",
        _ => "",
    }
}

fn model_chip(a: &Agent) -> String {
    let m = a
        .model
        .as_deref()
        .map(short_name)
        .unwrap_or_else(|| "-".into());
    let e = effort_abbr(a.effort.as_deref());
    if e.is_empty() { m } else { format!("{m} {e}") }
}

fn glyph(app: &App, s: &State) -> (String, Color) {
    let th = &app.theme;
    let spin = SPINNER[(app.t() * 12.0) as usize % SPINNER.len()];
    match s {
        State::NeedsYou(_) => (if blink(app) { "▲" } else { " " }.into(), th.warn),
        State::Tool { .. } => ("●".into(), pulse(app, th.good, 1.0)),
        State::Thinking => (
            if app.animating() {
                spin.to_string()
            } else {
                "◌".into()
            },
            th.thinking,
        ),
        State::WaitingAgents(_) => ("◐".into(), th.accent),
        State::Idle => ("○".into(), th.dim),
        State::Stalled => ("✖".into(), th.bad),
        State::Done => ("✓".into(), th.good),
        State::Gone => ("·".into(), th.faint),
    }
}

fn activity(s: &State) -> String {
    match s {
        State::NeedsYou(w) => format!("NEEDS YOU: {w}"),
        State::Tool { name, summary } => {
            if summary.is_empty() {
                name.clone()
            } else {
                format!("{name} {summary}")
            }
        }
        State::Thinking => "thinking".into(),
        State::WaitingAgents(n) => format!("waiting {n} agent{}", if *n == 1 { "" } else { "s" }),
        State::Idle => "idle".into(),
        State::Stalled => "STALLED".into(),
        State::Done => "done".into(),
        State::Gone => "exited".into(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if app.boot.is_some() {
        if boot::done(app) {
            app.boot = None;
        } else {
            boot::draw(f, app, area);
            return;
        }
    }
    let th = app.theme;
    f.render_widget(Block::new().style(th.base()), area);
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(area);
    header(f, app, head);
    if app.zoom {
        zoomed(f, app, body);
    } else {
        match app.screen {
            Screen::Ops => ops(f, app, body),
            Screen::Agents => agents_screen(f, app, body),
            Screen::Spend => spend(f, app, body),
            Screen::Models => models(f, app, body),
            Screen::Mana => mana_screen(f, app, body),
            Screen::Vault => vault(f, app, body),
            Screen::System => system(f, app, body),
        }
    }
    footer(f, app, foot);
    if app.help {
        help(f, app, area);
    }
    toast(f, app, area);
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let name = SCREENS
        .iter()
        .find(|(s, _)| *s == app.screen)
        .map(|(_, n)| *n)
        .unwrap_or("");
    let s = &app.snapshot;
    let fresh = s
        .taken_at
        .is_some_and(|t| (Utc::now() - t).num_milliseconds() < 2500);
    let left = vec![
        Span::styled(" AGENT·MANA ", th.bold(th.accent).bg(th.faint)),
        Span::styled(format!(" ▸ {name} "), th.bold(th.fg)),
        Span::styled(
            if fresh { "●" } else { "○" },
            Style::new().fg(if fresh { th.good } else { th.dim }),
        ),
    ];
    let active = s.count(|x| x.is_active() || matches!(x, State::NeedsYou(_)));
    let accounts = app.analytics.limits.len();
    let right = format!(
        "{} seats · {} accounts · {} agents ({} active) · today {} · burn {}/h · {} ",
        app.analytics.seats.len(),
        accounts,
        s.agents.len(),
        active,
        fmt::money(s.today_cost),
        fmt::money(s.burn_per_hour),
        app.clock()
    );
    let lw: usize = left.iter().map(|x| w(&x.content)).sum();
    let pad = (area.width as usize).saturating_sub(lw + w(&right));
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::new().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(spans)).style(th.base()), area);
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    if let Some(cmd) = &app.command {
        let line = Line::from(vec![
            Span::styled(" :", th.bold(th.accent)),
            Span::styled(format!("{cmd}▏"), Style::new().fg(th.fg)),
        ]);
        f.render_widget(Paragraph::new(line).style(th.base()), area);
        return;
    }
    if app.filter_edit {
        let line = Line::from(vec![
            Span::styled(" /", th.bold(th.accent)),
            Span::styled(
                format!("{}▏", app.filter.clone().unwrap_or_default()),
                Style::new().fg(th.fg),
            ),
        ]);
        f.render_widget(Paragraph::new(line).style(th.base()), area);
        return;
    }
    let mut spans = vec![Span::raw(" ")];
    for (i, (s, n)) in SCREENS.iter().enumerate() {
        let on = *s == app.screen;
        spans.push(Span::styled(
            format!("F{}", i + 1),
            Style::new()
                .fg(if on { th.bg } else { th.dim })
                .bg(if on { th.accent } else { th.bg }),
        ));
        spans.push(Span::styled(
            format!("{n} "),
            if on {
                th.bold(th.accent)
            } else {
                Style::new().fg(th.dim)
            },
        ));
    }
    let hints = match app.screen {
        Screen::Spend => " t period · s group · ? help · q quit ",
        Screen::Vault => " ←→ seats/logins · Enter switch · ? help · q quit ",
        _ => " ⏎ drill · / filter · : cmd · J jump · m mask · ? help · q quit ",
    };
    let used: usize = spans.iter().map(|s| w(&s.content)).sum();
    spans.push(Span::raw(
        " ".repeat((area.width as usize).saturating_sub(used + w(hints))),
    ));
    spans.push(Span::styled(hints, Style::new().fg(th.faint)));
    f.render_widget(Paragraph::new(Line::from(spans)).style(th.base()), area);
}

// ───────────────────────────── OPS ─────────────────────────────

fn ops(f: &mut Frame, app: &App, area: Rect) {
    let accounts = app.analytics.limits.len().max(1) as u16;
    let top_h = (2 + accounts * 3).clamp(9, 11);
    let min_bottom = if area.height > 36 {
        10
    } else if area.height > 26 {
        8
    } else {
        0
    };
    // The agent list takes what it needs; the panels below get the rest.
    let want_swarm = app.visible_agents().len() as u16 + 3;
    let avail = area.height.saturating_sub(top_h);
    let bottom_h = if min_bottom == 0 {
        0
    } else {
        avail.saturating_sub(want_swarm.max(6)).max(min_bottom)
    };
    let [top, mid, bottom] = Layout::vertical([
        Constraint::Length(top_h),
        Constraint::Min(4),
        Constraint::Length(bottom_h),
    ])
    .areas(area);
    if area.width >= 110 {
        let [m, b, fl] = Layout::horizontal([
            Constraint::Percentage(44),
            Constraint::Percentage(38),
            Constraint::Percentage(18),
        ])
        .areas(top);
        mana_panel(f, app, m);
        burn_panel(f, app, b);
        fleet_panel(f, app, fl);
    } else {
        let [m, b] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(top);
        mana_panel(f, app, m);
        burn_panel(f, app, b);
    }
    swarm(f, app, mid, true);
    if bottom_h > 0 {
        if area.width >= 100 {
            let [wk, ev] =
                Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
                    .areas(bottom);
            week_panel(f, app, wk);
            events_panel(f, app, ev);
        } else {
            events_panel(f, app, bottom);
        }
    }
}

fn zoomed(f: &mut Frame, app: &App, area: Rect) {
    match app.screen {
        Screen::Ops | Screen::Agents => swarm(f, app, area, true),
        _ => events_panel(f, app, area),
    }
}

fn mana_panel(f: &mut Frame, app: &App, area: Rect) {
    use crate::core::limits::WindowKind;
    let th = &app.theme;
    let block = panel(th, "MANA", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.analytics.limits.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "no limit readings yet (open /usage in Claude Code once)",
                Style::new().fg(th.dim),
            ))
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    // Two lines per account: the weekly gauge, then reset, burn, ETA and the 5-hour window.
    let mut lines = Vec::new();
    let gw = (inner.width as usize).saturating_sub(29).max(8);
    for rep in &app.analytics.limits {
        let week = rep
            .windows
            .iter()
            .find(|w| w.reading.kind == WindowKind::SevenDay);
        let five = rep
            .windows
            .iter()
            .find(|w| w.reading.kind == WindowKind::FiveHour);
        let Some(win) = week.or(five) else { continue };
        let pct = win.estimate_now;
        let mut c = th.utilization(pct);
        if pct >= 85.0 {
            c = pulse(app, c, 1.0);
        }
        if app
            .alerts
            .get(&format!("restored:{}", rep.account_uuid))
            .is_some_and(|t| t.elapsed().as_secs_f32() < 3.0)
        {
            c = pulse(app, th.good, 3.0);
        }
        let name = if app.mask {
            mask_name(&rep.name)
        } else {
            rep.name.clone()
        };
        let mut g = vec![Span::styled(fit(&name, 12), th.bold(th.fg)), Span::raw(" ")];
        g.extend(gauge(pct / 100.0, gw, c, th));
        g.push(Span::styled(format!(" {:>4}", fmt::pct(pct)), th.bold(c)));
        g.push(Span::styled(
            format!(" ⟳ {}", fmt::duration(win.window.end - Utc::now())),
            Style::new().fg(th.dim),
        ));
        lines.push(Line::from(g));
        let mut info = vec![Span::raw(" ".repeat(13))];
        match win.eta {
            Some(d) if d.num_seconds() == 0 => {
                info.push(Span::styled("LIMIT REACHED", th.bold(th.bad)))
            }
            Some(d) => info.push(Span::styled(
                format!("empty in {} ", fmt::duration(d)),
                th.bold(if d.num_hours() < 6 {
                    pulse(app, th.warn, 0.7)
                } else {
                    th.warn
                }),
            )),
            None if win.burn_usd_per_hour > 0.0 => {
                info.push(Span::styled("lasts until reset ", Style::new().fg(th.dim)))
            }
            None => info.push(Span::styled("quiet ", Style::new().fg(th.faint))),
        }
        info.push(Span::styled(
            format!("· {}/h", fmt::money(win.burn_usd_per_hour)),
            Style::new().fg(th.dim),
        ));
        lines.push(Line::from(info));
        if let Some(fv) = five.filter(|_| week.is_some()) {
            let p5 = fv.estimate_now;
            let mut g5 = vec![
                Span::styled(fit("  5-hour", 12), Style::new().fg(th.dim)),
                Span::raw(" "),
            ];
            g5.extend(gauge(p5 / 100.0, gw, th.fade(th.utilization(p5), 0.25), th));
            g5.push(Span::styled(
                format!(" {:>4}", fmt::pct(p5)),
                Style::new().fg(th.utilization(p5)),
            ));
            g5.push(Span::styled(
                format!(" ⟳ {}", fmt::duration(fv.window.end - Utc::now())),
                Style::new().fg(th.dim),
            ));
            lines.push(Line::from(g5));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn mask_name(s: &str) -> String {
    if s.contains('@') {
        "•••".into()
    } else {
        s.to_string()
    }
}

fn burn_panel(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let block = panel(th, "BURN $/min · 60m", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let [chart, legend] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(1)]).areas(inner);
    let fams = ["fable", "mythos", "opus", "sonnet", "haiku", "other"];
    let burn = &app.snapshot.burn;
    let mut totals = vec![0.0f64; 60];
    for s in burn.values() {
        for (i, v) in s.iter().enumerate() {
            totals[i] += v;
        }
    }
    let max = totals.iter().copied().fold(0.0, f64::max).max(0.01) * 1.15;
    let th2 = *th;
    let series: Vec<(Color, Vec<f64>)> = fams
        .iter()
        .filter_map(|n| burn.get(*n).map(|v| (th2.family_name(n), v.clone())))
        .collect();
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, 60.0])
        .y_bounds([0.0, max])
        .background_color(th.bg)
        .paint(move |ctx| {
            // Stacked columns: each minute is a vertical line per family.
            for i in 0..60 {
                let mut y0 = 0.0;
                for (c, v) in &series {
                    let y1 = y0 + v[i];
                    if v[i] > 0.0 {
                        for dx in [0.0, 0.5] {
                            ctx.draw(&CLine::new(i as f64 + dx, y0, i as f64 + dx, y1, *c));
                        }
                    }
                    y0 = y1;
                }
            }
            ctx.draw(&CLine::new(0.0, 0.0, 60.0, 0.0, th2.faint));
        });
    f.render_widget(canvas, chart);
    let now = format!("{}/h ", fmt::money(app.snapshot.burn_per_hour));
    f.render_widget(
        Paragraph::new(Span::styled(now.clone(), th.bold(th.fg))).alignment(Alignment::Right),
        Rect {
            x: chart.x,
            y: chart.y,
            width: chart.width,
            height: 1,
        },
    );
    let mut spans = Vec::new();
    for n in ["fable", "opus", "sonnet", "haiku"] {
        if burn.contains_key(n) || n == "opus" || n == "fable" {
            spans.push(Span::styled(
                format!("■{n} "),
                Style::new().fg(th.family_name(n)),
            ));
        }
    }
    spans.push(Span::styled(
        format!(" peak {}/min", fmt::money(max / 1.15)),
        Style::new().fg(th.faint),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), legend);
}

fn fleet_panel(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let s = &app.snapshot;
    let needs = s.count(|x| matches!(x, State::NeedsYou(_)));
    let block = panel(th, "FLEET", needs > 0 && blink(app));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let row = |g: &str, n: usize, label: &str, c: Color| {
        Line::from(vec![
            Span::styled(format!(" {g} "), Style::new().fg(c)),
            Span::styled(
                format!("{n:>3} "),
                th.bold(if n > 0 { c } else { th.faint }),
            ),
            Span::styled(label.to_string(), Style::new().fg(th.dim)),
        ])
    };
    let lines = vec![
        row(
            "●",
            s.count(|x| matches!(x, State::Tool { .. })),
            "working",
            th.good,
        ),
        row(
            "◌",
            s.count(|x| matches!(x, State::Thinking)),
            "thinking",
            th.thinking,
        ),
        row(
            "◐",
            s.count(|x| matches!(x, State::WaitingAgents(_))),
            "waiting",
            th.accent,
        ),
        row(
            "▲",
            needs,
            "NEEDS YOU",
            if needs > 0 {
                pulse(app, th.warn, 2.0)
            } else {
                th.warn
            },
        ),
        row(
            "✖",
            s.count(|x| matches!(x, State::Stalled)),
            "stalled",
            th.bad,
        ),
        row("○", s.count(|x| matches!(x, State::Idle)), "idle", th.dim),
        row("✓", s.count(|x| matches!(x, State::Done)), "done", th.good),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn swarm(f: &mut Frame, app: &App, area: Rect, full: bool) {
    let th = &app.theme;
    let agents = app.visible_agents();
    let title = match &app.filter {
        Some(fl) if !fl.is_empty() => format!("SWARM · filter \"{fl}\" · {}", agents.len()),
        _ => format!("SWARM · {}", agents.len()),
    };
    let block = panel(th, &title, false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if agents.is_empty() {
        let msg = if app.snapshot.taken_at.is_none() {
            "linking…"
        } else {
            "no agents active in the last 30 minutes"
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::new().fg(th.dim))),
            inner,
        );
        return;
    }
    let width = inner.width as usize;
    let (cm, ct, cc, cs, ccost) = (13usize, 7usize, 10usize, 15usize, 10usize);
    let show_spark = full && width > 110;
    let show_ctx = full && width > 90;
    // In a narrow panel (the AGENTS screen) the activity is in the detail card: names get the room.
    let show_act = full || width > 100;
    let fixed = 2
        + cm
        + if show_act { ct } else { 0 }
        + ccost
        + if show_spark { cs + 1 } else { 0 }
        + if show_ctx { cc + 1 } else { 0 }
        + if show_act { 5 } else { 3 };
    let flex = width.saturating_sub(fixed);
    let name_w = if show_act {
        (flex * 2 / 5).max(14)
    } else {
        flex.max(14)
    };
    let act_w = flex.saturating_sub(name_w).max(10);
    let mut header = vec![
        Span::styled(format!("{} ", fit("ST", 2)), Style::new().fg(th.dim)),
        Span::styled(fit("AGENT", name_w), Style::new().fg(th.dim)),
        Span::styled(format!(" {}", fit("MODEL", cm)), Style::new().fg(th.dim)),
    ];
    if show_act {
        header.push(Span::styled(
            format!(" {}", fit("ACTIVITY", act_w)),
            Style::new().fg(th.dim),
        ));
        header.push(Span::styled(
            format!(" {}", fit_right("T", ct)),
            Style::new().fg(th.dim),
        ));
    }
    if show_ctx {
        header.push(Span::styled(
            format!(" {}", fit("CONTEXT", cc)),
            Style::new().fg(th.dim),
        ));
    }
    if show_spark {
        header.push(Span::styled(
            format!(" {}", fit("$ · 15m", cs)),
            Style::new().fg(th.dim),
        ));
    }
    header.push(Span::styled(
        format!(" {}", fit_right("COST", ccost)),
        Style::new().fg(th.dim),
    ));
    let rows_h = inner.height.saturating_sub(1) as usize;
    let sel = app.selected.min(agents.len() - 1);
    let offset = if sel >= rows_h { sel + 1 - rows_h } else { 0 };
    let mut lines = vec![Line::from(header)];
    let now = Utc::now();
    for (i, a) in agents.iter().enumerate().skip(offset).take(rows_h) {
        let (g, gc) = glyph(app, &a.state);
        let last_in_branch = agents
            .get(i + 1)
            .is_none_or(|n| n.depth < a.depth || n.is_main() || n.session_id != a.session_id);
        let tree = if a.depth == 0 {
            String::new()
        } else {
            format!(
                "{}{}",
                "│ ".repeat((a.depth - 1) as usize),
                if last_in_branch { "└ " } else { "├ " }
            )
        };
        let name = if app.mask && a.is_main() {
            format!("{} · {}", a.name, a.project)
        } else if a.is_main() {
            format!("{} · {}", a.name, a.project)
        } else {
            a.name.clone()
        };
        let state_c = th.state(&a.state);
        let dim_row = matches!(a.state, State::Done | State::Gone | State::Idle);
        let fg = if dim_row { th.dim } else { th.fg };
        let elapsed = now - a.since;
        let timer = if matches!(a.state, State::Done | State::Gone) || elapsed.num_hours() >= 1 {
            fmt::duration(if matches!(a.state, State::Done | State::Gone) {
                now - a.last_activity
            } else {
                elapsed
            })
        } else {
            fmt::clock(elapsed)
        };
        // Share of a 1M-token context window.
        let ctx_frac = a.context_tokens as f64 / 1_000_000.0;
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
                format!(" {}", fit(&model_chip(a), cm)),
                Style::new().fg(if dim_row {
                    th.dim
                } else {
                    th.family(a.family())
                }),
            ),
        ];
        if show_act {
            spans.push(Span::styled(
                format!(" {}", fit(&activity(&a.state), act_w)),
                Style::new().fg(if matches!(a.state, State::Tool { .. }) {
                    th.fg
                } else {
                    state_c
                }),
            ));
            spans.push(Span::styled(
                format!(" {}", fit_right(&timer, ct)),
                Style::new().fg(th.dim),
            ));
        }
        if show_ctx {
            spans.push(Span::raw(" "));
            let tw = 4;
            spans.extend(gauge(
                ctx_frac,
                tw,
                if ctx_frac > 0.6 { th.warn } else { th.accent },
                th,
            ));
            spans.push(Span::styled(
                fit_right(&fmt::tokens(a.context_tokens), cc - tw),
                Style::new().fg(th.dim),
            ));
        }
        if show_spark {
            spans.push(Span::raw(" "));
            spans.push(spark(&a.spark, th.family(a.family())));
        }
        spans.push(Span::styled(
            format!(" {}", fit_right(&fmt::money(a.cost), ccost)),
            th.bold(if dim_row { th.dim } else { th.fg }),
        ));
        let mut line = Line::from(spans);
        let alert = app.alerts.get(&a.key()).map(|t| t.elapsed().as_secs_f32());
        if i == sel {
            line = line.style(Style::new().bg(th.faint));
        } else if matches!(a.state, State::NeedsYou(_)) && blink(app) {
            line = line.style(Style::new().bg(th.fade(th.warn, 0.82)));
        } else if let Some(t) = alert.filter(|t| *t < 0.9 && app.animating()) {
            line = line.style(Style::new().bg(th.fade(th.accent, 0.8 + t / 0.9 * 0.2)));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn week_panel(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let block = panel(th, "WEEK · where the mana went", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    let bw = (inner.width as usize).saturating_sub(30).max(6);
    for rep in &app.analytics.limits {
        let Some(win) = rep
            .windows
            .iter()
            .find(|w| w.reading.kind == crate::core::limits::WindowKind::SevenDay)
        else {
            continue;
        };
        let name = if app.mask {
            mask_name(&rep.name)
        } else {
            rep.name.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(name, th.bold(th.fg)),
            Span::styled(
                format!(" · {} used", fmt::pct(win.estimate_now)),
                Style::new().fg(th.utilization(win.estimate_now)),
            ),
        ]));
        let max = win.shares.first().map(|s| s.share).unwrap_or(0.0).max(1e-9);
        let per = ((inner.height as usize).saturating_sub(app.analytics.limits.len())
            / app.analytics.limits.len().max(1))
        .clamp(1, 4);
        for s in win.shares.iter().take(per) {
            let mut sp = vec![Span::styled(
                format!(" {} ", fit(&s.key, 13)),
                Style::new().fg(th.fg),
            )];
            sp.extend(gauge(s.share / max, bw, th.accent, th));
            sp.push(Span::styled(
                format!(" {:>4} ≈{:.1}pt", fmt::pct(s.share * 100.0), s.limit_points),
                Style::new().fg(th.dim),
            ));
            lines.push(Line::from(sp));
        }
        if let Some(o) = win.outside_points.filter(|o| *o >= 2.0) {
            lines.push(Line::from(Span::styled(
                format!(
                    " {} ≈{o:.1}pt · claude.ai, other devices",
                    fit("elsewhere", 12)
                ),
                Style::new().fg(th.warn),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "waiting for limit data…",
            Style::new().fg(th.dim),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn events_panel(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let block = panel(th, "EVENTS", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    for e in app.events.iter().take(inner.height as usize) {
        let mut c = app.tone_color(e.tone);
        let age = e.born.elapsed().as_secs_f32();
        if age < 1.0 && app.animating() {
            c = th.glow(c, (1.0 - age) * 0.6);
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", e.at.with_timezone(&Local).format("%H:%M:%S")),
                Style::new().fg(th.faint),
            ),
            Span::styled(format!("{} ", e.glyph), th.bold(c)),
            Span::styled(
                e.text.clone(),
                Style::new().fg(if e.tone == Tone::Info { th.dim } else { th.fg }),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "listening…",
            Style::new().fg(th.faint),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────────── AGENTS ─────────────────────────────

fn agents_screen(f: &mut Frame, app: &App, area: Rect) {
    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    swarm(f, app, list, false);
    let th = &app.theme;
    let block = panel(th, "AGENT", false);
    let inner = block.inner(detail);
    f.render_widget(block, detail);
    let Some(a) = app.selected_agent() else {
        f.render_widget(
            Paragraph::new(Span::styled("select an agent", Style::new().fg(th.dim))),
            inner,
        );
        return;
    };
    let (g, gc) = glyph(app, &a.state);
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(format!("{:<10}", k), Style::new().fg(th.dim)),
            Span::styled(v, Style::new().fg(c)),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{g} "), Style::new().fg(gc)),
            Span::styled(a.name.clone(), th.bold(th.fg)),
        ]),
        kv("state", activity(&a.state), th.state(&a.state)),
        kv(
            "since",
            format!(
                "{} ({})",
                a.since.with_timezone(&Local).format("%H:%M:%S"),
                fmt::duration(Utc::now() - a.since)
            ),
            th.fg,
        ),
        kv(
            "model",
            format!(
                "{} · effort {}",
                a.model.clone().unwrap_or_default(),
                a.effort.clone().unwrap_or_else(|| "-".into())
            ),
            th.family(a.family()),
        ),
        kv(
            "type",
            a.agent_type.clone().unwrap_or_else(|| {
                if a.is_main() {
                    "main thread".into()
                } else {
                    "subagent".into()
                }
            }),
            th.fg,
        ),
        kv("project", a.project.clone(), th.fg),
        kv(
            "seat",
            format!(
                "{} · session {}",
                a.seat,
                &a.session_id[..8.min(a.session_id.len())]
            ),
            th.fg,
        ),
        kv(
            "cost",
            format!(
                "{} · {} requests",
                fmt::money(a.cost),
                fmt::count(a.requests)
            ),
            th.fg,
        ),
        kv(
            "context",
            format!("{} tokens", fmt::tokens(a.context_tokens)),
            th.fg,
        ),
    ];
    if let Some(t) = &a.tmux {
        lines.push(kv("tmux", format!("{t} · press J to jump"), th.accent));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "TOOL TIMELINE",
        th.bold(th.accent),
    )));
    let now = Utc::now();
    if app.timeline.0 == a.key() {
        for t in app
            .timeline
            .1
            .iter()
            .take((inner.height as usize).saturating_sub(lines.len()))
        {
            let (mark, c, dur) = match t.done {
                Some(d) if t.is_error => ("✗", th.bad, fmt::duration(d - t.ts)),
                Some(d) => ("✓", th.good, fmt::duration(d - t.ts)),
                None => ("▶", pulse(app, th.warn, 1.0), fmt::clock(now - t.ts)),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", t.ts.with_timezone(&Local).format("%H:%M:%S")),
                    Style::new().fg(th.faint),
                ),
                Span::styled(format!("{mark} "), Style::new().fg(c)),
                Span::styled(format!("{:>6} ", dur), Style::new().fg(th.dim)),
                Span::styled(format!("{:<8} ", t.name), th.bold(th.fg)),
                Span::styled(
                    if app.mask {
                        String::new()
                    } else {
                        t.summary.clone()
                    },
                    Style::new().fg(th.dim),
                ),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "loading…",
            Style::new().fg(th.faint),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────────── SPEND ─────────────────────────────

fn spend(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let an = &app.analytics;
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(area);
    let title = format!(
        "SPEND · {} · by {} · {}",
        an.period.as_ref().map(|p| p.label()).unwrap_or_default(),
        an.group
            .map(|g| g.title().to_lowercase())
            .unwrap_or_default(),
        an.total
            .as_ref()
            .map(|t| fmt::money(t.cost))
            .unwrap_or_default()
    );
    let block = panel(th, &title, false);
    let inner = block.inner(left);
    f.render_widget(block, left);
    let total = an.total.as_ref().map(|t| t.cost).unwrap_or(0.0).max(1e-9);
    let max = an.groups.first().map(|r| r.cost).unwrap_or(0.0).max(1e-9);
    let lw = 18usize;
    let bw = (inner.width as usize).saturating_sub(lw + 22).max(6);
    let mut lines = Vec::new();
    for r in an.groups.iter().take(inner.height as usize) {
        let is_model = an.group == Some(crate::index::query::Dim::Model);
        let label = if is_model {
            short_name(&r.label)
        } else if app.mask && r.label.contains('@') {
            "•••".into()
        } else {
            r.label.clone()
        };
        let c = if is_model {
            th.family_name(&r.key)
        } else {
            th.accent
        };
        let mut sp = vec![Span::styled(
            fit(&label, lw),
            Style::new().fg(if is_model { c } else { th.fg }),
        )];
        sp.push(Span::raw(" "));
        sp.extend(gauge(r.cost / max, bw, c, th));
        sp.push(Span::styled(
            format!(
                " {:>10} {:>5}",
                fmt::money(r.cost),
                fmt::pct(r.cost / total * 100.0)
            ),
            Style::new().fg(th.fg),
        ));
        lines.push(Line::from(sp));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no spend in this period",
            Style::new().fg(th.dim),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);

    let [days, heat] =
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(right);
    days_panel(f, app, days);
    heat_panel(f, app, heat);
}

fn days_panel(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let block = panel(th, "DAILY · 14 days", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Every one of the last 14 days, including the ones without spend.
    let by_day: std::collections::HashMap<&str, f64> = app
        .analytics
        .days
        .iter()
        .map(|d| (d.key.as_str(), d.cost))
        .collect();
    let today = Local::now().date_naive();
    let days: Vec<crate::index::query::Row> = (0..14)
        .rev()
        .map(|i| {
            let key = (today - chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string();
            let cost = by_day.get(key.as_str()).copied().unwrap_or(0.0);
            crate::index::query::Row {
                key: key.clone(),
                label: key,
                requests: 0,
                usage: Default::default(),
                cost,
                unpriced: 0,
            }
        })
        .collect();
    let days = &days;
    if app.analytics.days.is_empty() || inner.height < 3 {
        return;
    }
    let h = inner.height.saturating_sub(2) as usize;
    let max = days.iter().map(|d| d.cost).fold(0.0, f64::max).max(1e-9);
    let col = ((inner.width as usize) / days.len().max(1)).clamp(2, 6);
    let mut lines = Vec::new();
    for row in (0..h).rev() {
        let mut spans = Vec::new();
        for d in days {
            let level = d.cost / max * h as f64;
            let ch = if level >= row as f64 + 1.0 {
                '█'
            } else if level > row as f64 {
                VBARS[(((level - row as f64) * 8.0) as usize).clamp(0, 7)]
            } else {
                ' '
            };
            let c = if d.key == Local::now().format("%Y-%m-%d").to_string() {
                pulse(app, th.accent, 0.5)
            } else {
                th.accent
            };
            spans.push(Span::styled(
                format!("{}{}", ch.to_string().repeat(col - 1), " "),
                Style::new().fg(c),
            ));
        }
        lines.push(Line::from(spans));
    }
    let labels: String = days
        .iter()
        .map(|d| fit(d.key.get(8..10).unwrap_or(""), col))
        .collect();
    lines.push(Line::from(Span::styled(labels, Style::new().fg(th.dim))));
    lines.push(Line::from(Span::styled(
        format!(
            "max {}/day · total {}",
            fmt::money(max),
            fmt::money(days.iter().map(|d| d.cost).sum())
        ),
        Style::new().fg(th.faint),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

fn heat_panel(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let block = panel(th, "WHEN · weekday × hour · 28 days", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut grid = [[0.0f64; 24]; 7];
    for (wd, hr, c) in &app.analytics.heat {
        if let (Ok(d), Ok(h)) = (wd.parse::<usize>(), hr.parse::<usize>())
            && d < 7
            && h < 24
        {
            grid[d][h] += c;
        }
    }
    let max = grid.iter().flatten().copied().fold(0.0, f64::max).max(1e-9);
    let cw = (((inner.width as usize).saturating_sub(4)) / 24).clamp(1, 3);
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mut lines = Vec::new();
    for d in [1, 2, 3, 4, 5, 6, 0] {
        let mut spans = vec![Span::styled(
            format!("{} ", names[d]),
            Style::new().fg(th.dim),
        )];
        for h in 0..24 {
            let k = (grid[d][h] / max) as f32;
            let c = if grid[d][h] <= 0.0 {
                th.faint
            } else {
                th.fade(th.accent, 0.85 - k * 0.85)
            };
            spans.push(Span::styled(
                "■".repeat(cw.min(1)) + &" ".repeat(cw.saturating_sub(1)),
                Style::new().fg(c),
            ));
        }
        lines.push(Line::from(spans));
    }
    let mut hours = String::from("    ");
    for h in 0..24 {
        let label = if h % 6 == 0 {
            h.to_string()
        } else {
            String::new()
        };
        hours.push_str(&fit(&label, cw));
    }
    lines.push(Line::from(Span::styled(hours, Style::new().fg(th.faint))));
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────────── MODELS ─────────────────────────────

fn models(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let an = &app.analytics;
    let title = format!(
        "MODELS · {}",
        an.period.as_ref().map(|p| p.label()).unwrap_or_default()
    );
    let block = panel(th, &title, false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let total: f64 = an.by_model.iter().map(|r| r.cost).sum::<f64>().max(1e-9);
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "{} {:>8} {:>11} {} {:>5}  {:>7} {:>6} {:>6}  {}",
            fit("MODEL", 12),
            "REQS",
            "COST",
            fit("SHARE", 17),
            "",
            "$/REQ",
            "HIT",
            "THINK",
            "EFFORT MIX"
        ),
        Style::new().fg(th.dim),
    ))];
    for r in &an.by_model {
        let c = th.family_name(&r.key);
        let share = r.cost / total;
        let u = r.usage;
        let mut mix: Vec<(String, f64)> = an
            .effort
            .iter()
            .filter(|(m, _, _)| *m == r.key)
            .map(|(_, e, c)| (e.clone(), *c))
            .collect();
        mix.sort_by(|a, b| b.1.total_cmp(&a.1));
        let msum: f64 = mix.iter().map(|x| x.1).sum::<f64>().max(1e-9);
        let mix_s = mix
            .iter()
            .filter(|x| x.1 / msum >= 0.01)
            .map(|(e, c)| format!("{e} {:.0}%", c / msum * 100.0))
            .collect::<Vec<_>>()
            .join(" · ");
        let mut sp = vec![
            Span::styled(fit(&short_name(&r.key), 12), th.bold(c)),
            Span::styled(
                format!(" {:>8} {:>11} ", fmt::count(r.requests), fmt::money(r.cost)),
                Style::new().fg(th.fg),
            ),
        ];
        sp.extend(gauge(share, 17, c, th));
        sp.push(Span::styled(
            format!(" {:>5}", fmt::pct(share * 100.0)),
            Style::new().fg(th.fg),
        ));
        sp.push(Span::styled(
            format!(
                "  {:>7} {:>6} {:>6}  ",
                fmt::money(r.cost / r.requests.max(1) as f64),
                u.cache_hit_ratio()
                    .map(|x| fmt::pct(x * 100.0))
                    .unwrap_or_else(|| "-".into()),
                if u.output > 0 {
                    fmt::pct(u.thinking as f64 / u.output as f64 * 100.0)
                } else {
                    "-".into()
                }
            ),
            Style::new().fg(th.dim),
        ));
        sp.push(Span::styled(mix_s, Style::new().fg(th.dim)));
        lines.push(Line::from(sp));
    }
    lines.push(Line::from(""));
    // Family split.
    let mut fam: std::collections::BTreeMap<Family, f64> = Default::default();
    for r in &an.by_model {
        *fam.entry(Family::of(&r.key)).or_default() += r.cost;
    }
    let width = (inner.width as usize).saturating_sub(2);
    let mut bar = vec![Span::raw(" ")];
    let mut legend = vec![Span::raw(" ")];
    for (fm, c) in &fam {
        let cells = ((c / total) * width as f64).round() as usize;
        bar.push(Span::styled(
            "█".repeat(cells),
            Style::new().fg(th.family(*fm)),
        ));
        legend.push(Span::styled(
            format!("■ {} {}  ", fm.name(), fmt::pct(c / total * 100.0)),
            Style::new().fg(th.family(*fm)),
        ));
    }
    lines.push(Line::from(bar));
    lines.push(Line::from(legend));
    if !an.substitutions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "REQUESTED ≠ SERVED",
            th.bold(th.accent),
        )));
        for (req, got, n) in an.substitutions.iter().take(5) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} → ", short_name(req)), Style::new().fg(th.fg)),
                Span::styled(short_name(got), Style::new().fg(th.warn)),
                Span::styled(
                    format!("  {} requests", fmt::count(*n)),
                    Style::new().fg(th.dim),
                ),
            ]));
        }
    }
    // Project × model: where each model's money went, as a heat table.
    if !an.project_model.is_empty() {
        let mut models: Vec<String> = an.by_model.iter().map(|r| r.key.clone()).collect();
        models.truncate(5);
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
        lines.push(Line::from(Span::styled(
            "PROJECT × MODEL",
            th.bold(th.accent),
        )));
        let mut head = vec![Span::styled(fit("", 18), Style::new())];
        for m in &models {
            head.push(Span::styled(
                fit_right(&short_name(m), 13),
                th.bold(th.family_name(m)),
            ));
        }
        head.push(Span::styled(
            fit_right("TOTAL", 12),
            Style::new().fg(th.dim),
        ));
        lines.push(Line::from(head));
        let room = (inner.height as usize).saturating_sub(lines.len() + 1);
        for (p, total) in projects.iter().take(room) {
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
                let cell = if c <= 0.0 {
                    "·".to_string()
                } else {
                    fmt::money(c)
                };
                let style = if c <= 0.0 {
                    Style::new().fg(th.faint)
                } else {
                    Style::new()
                        .fg(if k > 0.45 { th.bg } else { th.fg })
                        .bg(th.fade(th.family_name(m), 0.75 - k * 0.75))
                };
                row.push(Span::raw("  "));
                row.push(Span::styled(fit_right(&format!("{cell} "), 11), style));
            }
            row.push(Span::styled(
                fit_right(&fmt::money(*total), 12),
                th.bold(th.fg),
            ));
            lines.push(Line::from(row));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────────── MANA ─────────────────────────────

fn mana_screen(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let block = panel(th, "MANA · limits per account", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    let gw = (inner.width as usize).saturating_sub(24).clamp(10, 70);
    for rep in &app.analytics.limits {
        let name = if app.mask {
            mask_name(&rep.name)
        } else {
            rep.name.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(name, th.bold(th.fg)),
            Span::styled(
                format!(
                    "  {}",
                    rep.tier
                        .clone()
                        .unwrap_or_default()
                        .replace("default_claude_", "")
                ),
                Style::new().fg(th.dim),
            ),
        ]));
        for win in &rep.windows {
            let pct = win.estimate_now;
            let c = if pct >= 85.0 {
                pulse(app, th.utilization(pct), 1.0)
            } else {
                th.utilization(pct)
            };
            let mut g = vec![Span::styled(
                format!("  {:<14}", win.reading.kind.label()),
                Style::new().fg(th.fg),
            )];
            g.extend(gauge(pct / 100.0, gw, c, th));
            g.push(Span::styled(format!(" ≈{}", fmt::pct(pct)), th.bold(c)));
            lines.push(Line::from(g));
            let mut info = format!(
                "  {:<14}reading {} ({} old) · resets {} (in {})",
                "",
                fmt::pct(win.reading.utilization),
                fmt::duration(Utc::now() - win.reading.fetched_at),
                win.window.end.with_timezone(&Local).format("%a %H:%M"),
                fmt::duration(win.window.end - Utc::now())
            );
            if let Some(u) = win.usd_per_point {
                info.push_str(&format!(" · ≈{}/pt", fmt::money(u)));
            }
            if let Some(t) = win.manual_reset {
                info.push_str(&format!(
                    " · reset by hand ≈{}",
                    t.with_timezone(&Local).format("%a %H:%M")
                ));
            }
            lines.push(Line::from(Span::styled(info, Style::new().fg(th.dim))));
            let burn = match win.eta {
                Some(d) if d.num_seconds() == 0 => "LIMIT REACHED".to_string(),
                Some(d) => format!(
                    "burn {}/h → empty {} (in {})",
                    fmt::money(win.burn_usd_per_hour),
                    (Utc::now() + d).with_timezone(&Local).format("%a %H:%M"),
                    fmt::duration(d)
                ),
                None => format!(
                    "burn {}/h · lasts until reset",
                    fmt::money(win.burn_usd_per_hour)
                ),
            };
            lines.push(Line::from(Span::styled(
                format!("  {:<14}{burn}", ""),
                Style::new().fg(if win.eta.is_some() { th.warn } else { th.dim }),
            )));
            let max = win.shares.first().map(|s| s.share).unwrap_or(0.0).max(1e-9);
            for s in win.shares.iter().take(6) {
                let mut sp = vec![
                    Span::styled(format!("  {:<14}", ""), Style::new()),
                    Span::styled(fit(&s.key, 16), Style::new().fg(th.fg)),
                ];
                sp.extend(gauge(s.share / max, 24, th.accent, th));
                sp.push(Span::styled(
                    format!(
                        " {:>9} {:>5} ≈{:.1}pt",
                        fmt::money(s.cost),
                        fmt::pct(s.share * 100.0),
                        s.limit_points
                    ),
                    Style::new().fg(th.dim),
                ));
                lines.push(Line::from(sp));
            }
            if let Some(o) = win.outside_points.filter(|o| *o >= 2.0) {
                lines.push(Line::from(Span::styled(
                    format!("  {:<14}{} ≈{o:.1}pt not on this machine (claude.ai, desktop, other computers){}", "", fit("elsewhere", 16), win.reference.as_ref().map(|r| format!(" · ref {r}")).unwrap_or_default()),
                    Style::new().fg(th.warn),
                )));
            }
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no limit readings yet",
            Style::new().fg(th.dim),
        )));
    }
    lines.push(Line::from(Span::styled(
        "≈ limit usage assumed proportional to API-equivalent cost",
        Style::new().fg(th.faint),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────────── VAULT ─────────────────────────────

fn vault(f: &mut Frame, app: &App, area: Rect) {
    use super::vault_ui::{Focus, Modal, Pick};
    let th = &app.theme;
    let v = &app.vault;
    let hot = app
        .alerts
        .get("vault")
        .is_some_and(|t| t.elapsed().as_secs_f32() < 2.0);
    let an = &app.analytics;
    let week = |acct: Option<&str>| -> Option<f64> {
        let acct = acct?;
        an.limits
            .iter()
            .find(|r| r.account_uuid == acct)?
            .windows
            .iter()
            .find(|w| w.reading.kind == crate::core::limits::WindowKind::SevenDay)
            .map(|w| w.estimate_now)
    };
    let banner_h = if v.pick.is_some() { 3 } else { 0 };
    let [banner, seats_area, logins_area, keys] = Layout::vertical([
        Constraint::Length(banner_h),
        Constraint::Length(app.seats.len() as u16 + 3),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(area);
    if let Some(p) = &v.pick {
        let text = match p {
            Pick::LoginFor(seat) => {
                format!("Choose the login to put into seat '{seat}' · Enter switch · Esc cancel")
            }
            Pick::SeatFor(login) => {
                format!("Choose the seat for login '{login}' · Enter switch · Esc cancel")
            }
        };
        let b = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(pulse(app, th.accent, 1.0)))
            .style(th.base());
        let inner = b.inner(banner);
        f.render_widget(b, banner);
        f.render_widget(
            Paragraph::new(Span::styled(text, th.bold(th.accent))),
            inner,
        );
    }

    // Seats.
    let focus_seats = v.focus == Focus::Seats;
    let block = panel(th, "SEATS · Claude Code config dirs", hot && blink(app));
    let block = if focus_seats {
        block.border_style(Style::new().fg(th.accent))
    } else {
        block
    };
    let inner = block.inner(seats_area);
    f.render_widget(block, seats_area);
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "  {} {} {} {} {}",
            fit("SEAT", 14),
            fit("LOGIN", 16),
            fit("ACCOUNT", 24),
            fit("WEEK", 26),
            "LIVE"
        ),
        Style::new().fg(th.dim),
    ))];
    for (i, seat) in app.seats.iter().enumerate() {
        let row = an.seats.iter().find(|r| r.label == seat.label);
        let login = v
            .logins
            .iter()
            .find(|l| l.mounted.as_deref() == Some(&seat.label))
            .map(|l| l.label.clone());
        let account = row.and_then(|r| r.account.clone());
        let acct_name = account
            .as_deref()
            .map(|a| {
                an.limits
                    .iter()
                    .find(|r| r.account_uuid == a)
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| format!("acct-{}", &a[..8.min(a.len())]))
            })
            .unwrap_or_else(|| "(logged out)".into());
        let sel = focus_seats && i == v.seat_sel;
        let mut sp = vec![
            Span::styled(if sel { "▸ " } else { "  " }, th.bold(th.accent)),
            Span::styled(fit(&seat.label, 14), th.bold(th.fg)),
            Span::styled(
                format!(" {}", fit(login.as_deref().unwrap_or("(not saved)"), 16)),
                Style::new().fg(if login.is_some() { th.fg } else { th.dim }),
            ),
            Span::styled(
                format!(
                    " {}",
                    fit(
                        &if app.mask {
                            mask_name(&acct_name)
                        } else {
                            acct_name
                        },
                        24
                    )
                ),
                Style::new().fg(th.dim),
            ),
            Span::raw(" "),
        ];
        match week(account.as_deref()) {
            Some(p) => {
                sp.extend(gauge(p / 100.0, 18, th.utilization(p), th));
                sp.push(Span::styled(
                    format!(" {:>5}  ", fmt::pct(p)),
                    Style::new().fg(th.utilization(p)),
                ));
            }
            None => sp.push(Span::styled(fit("unknown", 26), Style::new().fg(th.dim))),
        }
        let live = row.map(|r| r.live).unwrap_or(0);
        sp.push(Span::styled(
            live.to_string(),
            Style::new().fg(if live > 0 { th.good } else { th.dim }),
        ));
        let mut line = Line::from(sp);
        if sel {
            line = line.style(Style::new().bg(th.faint));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);

    // Saved logins.
    let focus_logins = v.focus == Focus::Logins;
    let block = panel(th, "SAVED LOGINS", false);
    let block = if focus_logins {
        block.border_style(Style::new().fg(th.accent))
    } else {
        block
    };
    let inner = block.inner(logins_area);
    f.render_widget(block, logins_area);
    let mut lines = Vec::new();
    if v.logins.is_empty() {
        lines.push(Line::from(Span::styled(
            "none yet — select a seat and press c to save its login, or a to log into a new account",
            Style::new().fg(th.dim),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} {} {} {} {}",
                fit("LOGIN", 16),
                fit("EMAIL", 26),
                fit("IN SEAT", 12),
                fit("WEEK", 20),
                "LOGIN EXPIRES"
            ),
            Style::new().fg(th.dim),
        )));
    }
    let mut best: Option<(f64, &str)> = None;
    for (i, l) in v.logins.iter().enumerate() {
        let sel = focus_logins && i == v.login_sel;
        let mut sp = vec![
            Span::styled(if sel { "▸ " } else { "  " }, th.bold(th.accent)),
            Span::styled(fit(&l.label, 16), th.bold(th.fg)),
            Span::styled(
                format!(
                    " {}",
                    fit(
                        &if app.mask {
                            "•••".into()
                        } else {
                            l.email.clone().unwrap_or_default()
                        },
                        26
                    )
                ),
                Style::new().fg(th.dim),
            ),
            Span::styled(
                format!(" {}", fit(l.mounted.as_deref().unwrap_or("-"), 12)),
                Style::new().fg(if l.mounted.is_some() { th.good } else { th.dim }),
            ),
            Span::raw(" "),
        ];
        match week(l.account_uuid.as_deref()) {
            Some(p) => {
                if l.mounted.is_none() && best.is_none_or(|(b, _)| p < b) {
                    best = Some((p, &l.label));
                }
                sp.extend(gauge(p / 100.0, 12, th.utilization(p), th));
                sp.push(Span::styled(
                    format!(" {:>5}  ", fmt::pct(p)),
                    Style::new().fg(th.utilization(p)),
                ));
            }
            None => sp.push(Span::styled(fit("unknown", 20), Style::new().fg(th.dim))),
        }
        if let Some(t) = l.meta.refresh_expires_at {
            let left = t - Utc::now();
            sp.push(Span::styled(
                format!("in {}", fmt::duration(left)),
                Style::new().fg(if left.num_days() < 7 {
                    th.warn
                } else {
                    th.faint
                }),
            ));
        }
        let mut line = Line::from(sp);
        if sel {
            line = line.style(Style::new().bg(th.faint));
        }
        lines.push(line);
    }
    if let Some((p, label)) = best {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "tip: '{label}' has the most limit left ({} used)",
                fmt::pct(p)
            ),
            Style::new().fg(th.accent),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);

    let hints = match v.focus {
        Focus::Seats => {
            " ↑↓ seat · → logins · Enter put a login here · c save this seat's login · a add login"
        }
        Focus::Logins => {
            " ↑↓ login · ← seats · Enter put into a seat · r rename · d forget · a add login"
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(hints, Style::new().fg(th.dim))).style(th.base()),
        keys,
    );

    // Modal.
    if let Some(m) = &v.modal {
        let (title, body, input) = match m {
            Modal::Input { title, value, .. } => ("INPUT", title.clone(), Some(value.clone())),
            Modal::Confirm { text, .. } => ("CONFIRM", text.clone(), None),
        };
        let wdt = 76.min(area.width);
        let body_lines: Vec<&str> = body.lines().collect();
        let wrapped: usize = body_lines
            .iter()
            .map(|l| (w(l) / (wdt as usize - 4)).max(0) + 1)
            .sum();
        let h = (wrapped as u16 + if input.is_some() { 4 } else { 2 }).min(area.height);
        let r = Rect {
            x: area.x + (area.width - wdt) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: wdt,
            height: h,
        };
        f.render_widget(Clear, r);
        let block = panel(th, title, false).border_style(Style::new().fg(th.accent));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let mut lines: Vec<Line> = body_lines
            .iter()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(th.fg))))
            .collect();
        if let Some(val) = input {
            lines.push(Line::from(""));
            let cursor = if blink(app) { "▏" } else { " " };
            lines.push(Line::from(vec![
                Span::styled("› ", th.bold(th.accent)),
                Span::styled(val, th.bold(th.fg)),
                Span::styled(cursor, Style::new().fg(th.accent)),
                Span::styled("   Enter ok · Esc cancel", Style::new().fg(th.faint)),
            ]));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

// ───────────────────────────── SYSTEM ─────────────────────────────

fn system(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let [procs, res] =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(area);
    let block = panel(th, "PROCESSES · claude and children", false);
    let inner = block.inner(procs);
    f.render_widget(block, procs);
    let mut lines = Vec::new();
    for (seat, name, pid, cpu, mem, kids) in &app.system.instances {
        lines.push(Line::from(vec![
            Span::styled(format!("● {name}"), th.bold(th.fg)),
            Span::styled(format!("  {seat} · pid {pid}"), Style::new().fg(th.dim)),
            Span::styled(
                format!(
                    "  cpu {cpu:.0}% · {}",
                    crate::cli::doctor::human_bytes(*mem)
                ),
                Style::new().fg(if *cpu > 200.0 { th.warn } else { th.accent }),
            ),
        ]));
        for (cmd, c, m) in kids {
            let cw = (inner.width as usize).saturating_sub(26);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "   {:>5.0}% {:>8}  ",
                        c,
                        crate::cli::doctor::human_bytes(*m)
                    ),
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
            "no live Claude Code sessions",
            Style::new().fg(th.dim),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);

    let block = panel(th, "RESOURCES", false);
    let inner = block.inner(res);
    f.render_widget(block, res);
    let s = &app.system;
    let gw = (inner.width as usize).saturating_sub(18).max(6);
    let mut lines = Vec::new();
    if s.mem_total > 0 {
        let used = s.mem_total.saturating_sub(s.mem_available) as f64 / s.mem_total as f64;
        let mut sp = vec![Span::styled("memory ", Style::new().fg(th.dim))];
        sp.extend(gauge(used, gw, th.utilization(used * 100.0), th));
        lines.push(Line::from(sp));
        lines.push(Line::from(Span::styled(
            format!(
                "       {} free of {}",
                crate::cli::doctor::human_bytes(s.mem_available),
                crate::cli::doctor::human_bytes(s.mem_total)
            ),
            Style::new().fg(th.dim),
        )));
    }
    if let Some(free) = s.disk_free {
        lines.push(Line::from(vec![
            Span::styled("disk   ", Style::new().fg(th.dim)),
            Span::styled(
                format!("{} free in home", crate::cli::doctor::human_bytes(free)),
                Style::new().fg(if free < 60 << 30 { th.warn } else { th.fg }),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "TRANSCRIPTS ON DISK",
        th.bold(th.accent),
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
            th.accent,
            th,
        ));
        sp.push(Span::styled(
            format!(" {}", crate::cli::doctor::human_bytes(*b)),
            Style::new().fg(th.dim),
        ));
        lines.push(Line::from(sp));
    }
    lines.push(Line::from(Span::styled(
        format!(
            "index      {}",
            crate::cli::doctor::human_bytes(app.analytics.index_bytes)
        ),
        Style::new().fg(th.dim),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────────── overlays ─────────────────────────────

fn help(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let rows = [
        ("F1-F7 · 1-7 · Tab", "switch screen"),
        ("↑↓ · j k · PgUp PgDn", "select agent"),
        ("Enter", "agent details"),
        ("J", "jump to the agent's tmux pane"),
        ("/", "filter agents (Esc clears)"),
        (
            ":",
            "command: switch <seat> <login> · capture <seat> · period 7d · theme amber",
        ),
        ("t", "cycle period (today · week · 7d · 30d · all)"),
        ("s", "cycle grouping on SPEND"),
        ("z", "zoom the agent list"),
        ("m", "mask names, emails and paths"),
        ("p", "pause animations"),
        ("T", "cycle theme (ops · phosphor · amber · plain)"),
        ("q · Ctrl-C", "quit"),
    ];
    let h = rows.len() as u16 + 4;
    let wdt = 86.min(area.width);
    let r = Rect {
        x: area.x + (area.width - wdt) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: wdt,
        height: h.min(area.height),
    };
    f.render_widget(Clear, r);
    let block = panel(th, "KEYS", false);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(fit(k, 24), th.bold(th.accent)),
                Span::styled(*d, Style::new().fg(th.fg)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn toast(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let msg = if let Some(e) = &app.error {
        Some((format!("error: {e}"), th.bad))
    } else {
        app.toast
            .as_ref()
            .filter(|(_, _, t)| t.elapsed().as_secs_f32() < 4.0)
            .map(|(m, tone, _)| (m.clone(), app.tone_color(*tone)))
    };
    let Some((m, c)) = msg else { return };
    let wdt = (w(&m) as u16 + 4).min(area.width);
    let r = Rect {
        x: area.x + area.width - wdt,
        y: area.y + area.height.saturating_sub(4),
        width: wdt,
        height: 3,
    };
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(c))
        .style(th.base());
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(
        Paragraph::new(Span::styled(
            m,
            Style::new().fg(th.fg).add_modifier(Modifier::BOLD),
        )),
        inner,
    );
}
