//! Screen composition: the wall (large terminals), the column (medium) and tabs (small), plus
//! the header, the footer and the dialogs.

mod kit;
mod panels;

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use super::app::{App, Panel, Shape, TABS};
use super::boot;
use super::vault_ui::{Modal, PickFor};
use crate::live::State;
use crate::ui::fmt;
use kit::*;

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
    app.rects.clear();
    app.shape = Shape::pick(app.layout, area);
    if !app.visible_panels().contains(&app.focus) {
        app.focus = app.visible_panels()[0];
    }
    let head = Rect { height: 1, ..area };
    let foot = Rect {
        y: area.y + area.height.saturating_sub(1),
        height: 1,
        ..area
    };
    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    header(f, app, head);
    match app.zoom {
        Some(p) => draw_panel(f, app, p, body, true),
        None => match app.shape {
            Shape::Wall => wall(f, app, body),
            Shape::Column => column(f, app, body),
            Shape::Tabs => tabs(f, app, body),
        },
    }
    footer(f, app, foot);
    picker(f, app, area);
    modal(f, app, area);
    if app.help {
        help(f, app, area);
    }
    toast(f, app, area);
}

fn draw_panel(f: &mut Frame, app: &mut App, p: Panel, area: Rect, big: bool) {
    if area.width < 10 || area.height < 3 {
        return;
    }
    match p {
        Panel::Swarm => panels::swarm(f, app, area),
        Panel::Mana => panels::mana(f, app, area, big),
        Panel::Burn => panels::burn(f, app, area),
        Panel::Attention => panels::attention(f, app, area),
        Panel::Agent => panels::agent(f, app, area),
        Panel::Projects => panels::projects(f, app, area),
        Panel::Models => panels::models(f, app, area),
        Panel::History => panels::history(f, app, area),
        Panel::Events => panels::events(f, app, area),
        Panel::System => panels::system(f, app, area),
    }
}

/// Rows the MANA · SEATS table needs: borders, header, one per seat and parked login, a hint.
fn mana_height(app: &App) -> u16 {
    app.mana_rows().len() as u16 + 4
}

/// Everything at once (from about 250×55).
fn wall(f: &mut Frame, app: &mut App, body: Rect) {
    let band_a = mana_height(app).clamp(10, 16);
    let band_c = if body.height >= 75 {
        24
    } else if body.height >= 62 {
        19
    } else {
        15
    };
    let band_b = body.height.saturating_sub(band_a + band_c);
    let rows = vsplit_fixed(body, &[band_a, band_b, band_c]);
    let a = hsplit(rows[0], &[42, 30, 28]);
    draw_panel(f, app, Panel::Mana, a[0], false);
    draw_panel(f, app, Panel::Attention, a[1], false);
    draw_panel(f, app, Panel::Burn, a[2], false);
    let b = hsplit(rows[1], &[64, 36]);
    draw_panel(f, app, Panel::Swarm, b[0], false);
    draw_panel(f, app, Panel::Agent, b[1], false);
    let c = hsplit(rows[2], &[30, 26, 20, 24]);
    draw_panel(f, app, Panel::Projects, c[0], false);
    draw_panel(f, app, Panel::Models, c[1], false);
    draw_panel(f, app, Panel::History, c[2], false);
    draw_panel(f, app, Panel::Events, c[3], false);
}

/// A narrower stack (from about 150×48).
fn column(f: &mut Frame, app: &mut App, body: Rect) {
    let band_a = mana_height(app).clamp(8, 13);
    let rest = body.height.saturating_sub(band_a);
    let swarm = rest * 45 / 100;
    let agent = rest * 28 / 100;
    let bottom = rest.saturating_sub(swarm + agent);
    let rows = vsplit_fixed(body, &[band_a, swarm, agent, bottom]);
    let a = hsplit(rows[0], &[62, 38]);
    draw_panel(f, app, Panel::Mana, a[0], false);
    draw_panel(f, app, Panel::Attention, a[1], false);
    draw_panel(f, app, Panel::Swarm, rows[1], false);
    draw_panel(f, app, Panel::Agent, rows[2], false);
    let c = hsplit(rows[3], &[55, 45]);
    draw_panel(f, app, Panel::Projects, c[0], false);
    draw_panel(f, app, Panel::Events, c[1], false);
}

/// Small terminals: one tab at a time.
fn tabs(f: &mut Frame, app: &mut App, body: Rect) {
    use super::app::Tab;
    match app.tab {
        Tab::Ops => {
            let mana = mana_height(app).clamp(6, 10).min(body.height / 3);
            let strip = if body.height > 28 { 8 } else { 0 };
            let swarm = body.height.saturating_sub(mana + strip);
            let rows = vsplit_fixed(body, &[mana, swarm, strip]);
            draw_panel(f, app, Panel::Mana, rows[0], false);
            draw_panel(f, app, Panel::Swarm, rows[1], false);
            if strip > 0 {
                let c = hsplit(rows[2], &[50, 50]);
                draw_panel(f, app, Panel::Attention, c[0], false);
                draw_panel(f, app, Panel::Events, c[1], false);
            }
        }
        Tab::Agent => {
            let c = hsplit(body, &[45, 55]);
            draw_panel(f, app, Panel::Swarm, c[0], false);
            draw_panel(f, app, Panel::Agent, c[1], false);
        }
        Tab::Mana => draw_panel(f, app, Panel::Mana, body, true),
        Tab::Spend => {
            let c = hsplit(body, &[52, 48]);
            draw_panel(f, app, Panel::Projects, c[0], false);
            let r = vsplit_fixed(c[1], &[c[1].height / 2, c[1].height - c[1].height / 2]);
            draw_panel(f, app, Panel::History, r[0], false);
            draw_panel(f, app, Panel::Models, r[1], false);
        }
        Tab::System => draw_panel(f, app, Panel::System, body, true),
    }
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let s = &app.snapshot;
    let fresh = s
        .taken_at
        .is_some_and(|t| (Utc::now() - t).num_milliseconds() < 2500);
    let total_burn: Vec<f64> = (0..60)
        .map(|i| s.burn.values().map(|v| v[i]).sum())
        .collect();
    let n = |p: &dyn Fn(&State) -> bool| s.count(p);
    let needs = n(&|x| matches!(x, State::NeedsYou(_)));
    let mut spans = vec![
        Span::styled(" AGENT·MANA ", th.bold(th.bg).bg(th.accent)),
        Span::styled(
            if fresh { " ● " } else { " ○ " },
            Style::new().fg(if fresh { th.good } else { th.dim }),
        ),
        Span::styled("today ", Style::new().fg(th.faint)),
        Span::styled(fmt::money(s.today_cost), th.bold(th.fg)),
    ];
    if let (Some(t), Some(p)) = (&app.analytics.total, &app.analytics.period) {
        spans.push(Span::styled(
            format!("  {} ", p.label()),
            Style::new().fg(th.faint),
        ));
        spans.push(Span::styled(fmt::money(t.cost), th.bold(th.fg)));
    }
    spans.push(Span::styled("  burn ", Style::new().fg(th.faint)));
    spans.push(Span::styled(
        format!("{}/h ", fmt::money(s.burn_per_hour)),
        th.bold(th.accent),
    ));
    if area.width >= 170 {
        spans.push(spark(&total_burn[36..], th.accent));
    }
    spans.push(Span::styled(
        format!("   {} agents ", s.agents.len()),
        Style::new().fg(th.dim),
    ));
    spans.push(Span::styled(
        format!(
            "●{} ",
            n(&|x| matches!(x, State::Tool { .. })) + n(&|x| matches!(x, State::Thinking))
        ),
        Style::new().fg(th.good),
    ));
    if needs > 0 {
        spans.push(Span::styled(
            format!("▲{needs} needs you "),
            th.bold(pulse(app, th.warn, 1.5)),
        ));
    }
    let stalled = n(&|x| matches!(x, State::Stalled));
    if stalled > 0 {
        spans.push(Span::styled(
            format!("✖{stalled} "),
            Style::new().fg(th.bad),
        ));
    }
    // The tightest account.
    if let Some((name, pct)) = app
        .analytics
        .limits
        .iter()
        .filter_map(|r| {
            r.windows
                .iter()
                .filter(|w| !w.reset_passed)
                .map(|w| (r.name.clone(), w.estimate_now))
                .max_by(|a, b| a.1.total_cmp(&b.1))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        spans.push(Span::styled("  mana ", Style::new().fg(th.faint)));
        spans.push(Span::styled(
            format!("{} {}", mask_name(app, &name), fmt::pct(pct)),
            th.bold(th.utilization(pct)),
        ));
    }
    if app.eco {
        spans.push(Span::styled("  ECO", Style::new().fg(th.good)));
    }
    let right = if area.width >= 170 {
        format!("layout {:?} · {} ", app.shape, app.clock()).to_lowercase()
    } else {
        format!("{} ", app.clock())
    };
    // Drop from the middle when it does not fit.
    while spans.iter().map(|s| w(&s.content)).sum::<usize>() + w(&right) + 1 > area.width as usize
        && spans.len() > 4
    {
        spans.remove(4);
    }
    let used: usize = spans.iter().map(|s| w(&s.content)).sum();
    spans.push(Span::raw(
        " ".repeat((area.width as usize).saturating_sub(used + w(&right))),
    ));
    spans.push(Span::styled(right, Style::new().fg(th.faint)));
    f.render_widget(Paragraph::new(Line::from(spans)).style(th.base()), area);
}

fn panel_keys(p: Panel) -> &'static str {
    match p {
        Panel::Swarm => "↑↓ select · Enter details · J tmux · / filter",
        Panel::Agent => "↑↓ scroll tools · J tmux",
        Panel::Mana => {
            "↑↓ · Enter switch · S best · c save · a add · r rename · d forget · R re-read"
        }
        Panel::Attention => "↑↓ · Enter go there · n/N next",
        Panel::Projects => "↑↓ · Enter its agents · t period · g group",
        Panel::Models | Panel::Burn => "t period",
        Panel::History => "",
        Panel::Events => "↑↓ scroll",
        Panel::System => "",
    }
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    if let Some(cmd) = &app.command {
        let line = Line::from(vec![
            Span::styled(" :", th.bold(th.accent)),
            Span::styled(format!("{cmd}▏"), Style::new().fg(th.fg)),
            Span::styled(
                "   switch <seat> <login> · capture <seat> [label] · period 7d · filter p:shop · layout wall|column|tabs · theme amber",
                Style::new().fg(th.faint),
            ),
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
            Span::styled(
                "   words match names; p:project m:model s:seat state:needs · Enter keep · Esc clear",
                Style::new().fg(th.faint),
            ),
        ]);
        f.render_widget(Paragraph::new(line).style(th.base()), area);
        return;
    }
    let mut spans = vec![Span::raw(" ")];
    if app.shape == Shape::Tabs && app.zoom.is_none() {
        for (i, (t, name, _)) in TABS.iter().enumerate() {
            let on = *t == app.tab;
            spans.push(Span::styled(
                format!(" {} {name} ", i + 1),
                if on {
                    th.bold(th.bg).bg(th.accent)
                } else {
                    Style::new().fg(th.dim)
                },
            ));
        }
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        format!("{} ", app.focus.title()),
        th.bold(th.accent),
    ));
    spans.push(Span::styled(panel_keys(app.focus), Style::new().fg(th.dim)));
    let global = if app.zoom.is_some() {
        "  │  Esc back · z unzoom · ? help · q quit "
    } else {
        "  │  Tab next panel · 1-9 focus (again: zoom) · n next alert · t period · : cmd · ? help · q quit "
    };
    let used: usize = spans.iter().map(|s| w(&s.content)).sum();
    let g = crate::ui::table::truncate(global, (area.width as usize).saturating_sub(used));
    spans.push(Span::raw(
        " ".repeat((area.width as usize).saturating_sub(used + w(&g))),
    ));
    spans.push(Span::styled(g, Style::new().fg(th.faint)));
    f.render_widget(Paragraph::new(Line::from(spans)).style(th.base()), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let wdt = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - wdt) / 2,
        y: area.y + (area.height - h) / 2,
        width: wdt,
        height: h,
    }
}

fn dialog<'a>(app: &App, title: &'a str) -> Block<'a> {
    let th = &app.theme;
    Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(th.accent))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            th.bold(th.accent),
        )))
        .style(th.base())
}

fn picker(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.vault.picker else { return };
    let th = &app.theme;
    let title = match &p.what {
        PickFor::Seat(s) => format!("put a login into seat '{s}' · most limit left first"),
        PickFor::Login(l) => format!("put login '{l}' into a seat"),
    };
    let r = centered(area, 96, p.items.len() as u16 + 6);
    f.render_widget(Clear, r);
    let block = dialog(app, &title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let now = Utc::now();
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "  {} {} {} {} {}",
            fit("LOGIN / SEAT", 20),
            fit("ACCOUNT", 22),
            fit("5-HOUR", 13),
            fit("WEEK", 13),
            "WEEK RESETS · LOGIN EXPIRES"
        ),
        Style::new().fg(th.faint),
    ))];
    for (i, item) in p.items.iter().enumerate() {
        let account = match &p.what {
            PickFor::Seat(_) => app
                .vault
                .logins
                .iter()
                .find(|l| &l.label == item)
                .and_then(|l| l.account_uuid.clone()),
            PickFor::Login(_) => app
                .analytics
                .seats
                .iter()
                .find(|s| &s.label == item)
                .and_then(|s| s.account.clone()),
        };
        let (five, week, reset) = app.mana_of(account.as_deref());
        let acct = account
            .as_deref()
            .map(|a| mask_name(app, &app.account_label(a)))
            .unwrap_or_else(|| "(unknown)".into());
        let pc = |v: Option<f64>| -> Vec<Span<'static>> {
            match v {
                Some(x) => {
                    let mut g = gauge(x / 100.0, 7, th.utilization(x), th);
                    g.push(Span::styled(
                        fit_right(&fmt::pct(x), 5),
                        th.bold(th.utilization(x)),
                    ));
                    g.push(Span::raw(" "));
                    g
                }
                None => vec![Span::styled(
                    fit("  unknown", 13),
                    Style::new().fg(th.faint),
                )],
            }
        };
        let expires = app
            .vault
            .logins
            .iter()
            .find(|l| &l.label == item)
            .and_then(|l| l.meta.refresh_expires_at)
            .map(|t| format!(" · exp. {}", fmt::duration(t - now)))
            .unwrap_or_default();
        let mut sp = vec![
            Span::styled(if i == p.sel { "▸ " } else { "  " }, th.bold(th.accent)),
            Span::styled(fit(item, 20), th.bold(th.fg)),
            Span::styled(format!(" {}", fit(&acct, 22)), Style::new().fg(th.dim)),
            Span::raw(" "),
        ];
        sp.extend(pc(five));
        sp.extend(pc(week));
        sp.push(Span::styled(
            format!(
                "{}{expires}",
                reset.map(|r| fmt::duration(r - now)).unwrap_or_default()
            ),
            Style::new().fg(th.dim),
        ));
        let mut line = Line::from(sp);
        if i == p.sel {
            line = line.style(Style::new().bg(th.sel));
        }
        lines.push(line);
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ choose · Enter switch (asks first) · Esc cancel",
        Style::new().fg(th.faint),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

fn modal(f: &mut Frame, app: &App, area: Rect) {
    let Some(m) = &app.vault.modal else { return };
    let th = &app.theme;
    let (title, body, input) = match m {
        Modal::Input { title, value, .. } => ("INPUT", title.clone(), Some(value.clone())),
        Modal::Confirm { text, .. } => ("CONFIRM", text.clone(), None),
    };
    let wdt = 84u16;
    let lines_n: usize = body.lines().map(|l| w(l) / (wdt as usize - 4) + 1).sum();
    let r = centered(
        area,
        wdt,
        lines_n as u16 + if input.is_some() { 5 } else { 3 },
    );
    f.render_widget(Clear, r);
    let block = dialog(app, title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines: Vec<Line> = body
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(th.fg))))
        .collect();
    if let Some(val) = input {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("› ", th.bold(th.accent)),
            Span::styled(val, th.bold(th.fg)),
            Span::styled(
                if blink(app) { "▏" } else { " " },
                Style::new().fg(th.accent),
            ),
            Span::styled("   Enter ok · Esc cancel", Style::new().fg(th.faint)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn help(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let mut rows: Vec<(String, String)> =
        vec![(format!("— {} —", app.focus.title()), String::new())];
    for part in panel_keys(app.focus).split(" · ") {
        if let Some((k, d)) = part.split_once(' ') {
            rows.push((k.to_string(), d.to_string()));
        }
    }
    rows.push(("— everywhere —".into(), String::new()));
    for (k, d) in [
        (
            "Tab · Shift-Tab",
            "next / previous panel (the focused one has a bright border)",
        ),
        (
            "1-9, 0",
            "focus panel N (numbers are in the titles); press again to zoom; 0 = SYSTEM",
        ),
        (
            "z · Esc",
            "zoom the focused panel · back (closes dialogs, zoom, filter)",
        ),
        (
            "n · N",
            "next / previous thing that needs attention (selects the agent)",
        ),
        (
            "/",
            "filter agents: words, p:project m:model s:seat state:needs",
        ),
        (
            ":",
            "switch <seat> <login> · capture <seat> · period 7d · layout wall|column|tabs",
        ),
        ("t · g", "period (today · 7d · 30d · all) · spend grouping"),
        ("J", "jump to the selected agent's Claude Code in tmux"),
        ("L", "layout: auto · wall · column · tabs"),
        ("m · p · T", "mask names · pause animations · theme"),
        (
            "mouse",
            "click focuses a panel / selects an agent; wheel scrolls the panel under it",
        ),
        ("? · q", "close help · quit"),
    ] {
        rows.push((k.into(), d.into()));
    }
    let r = centered(area, 100, rows.len() as u16 + 2);
    f.render_widget(Clear, r);
    let block = dialog(app, "KEYS");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(k, d)| {
            if d.is_empty() {
                Line::from(Span::styled(k, th.bold(th.dim)))
            } else {
                Line::from(vec![
                    Span::styled(fit(&k, 20), th.bold(th.accent)),
                    Span::styled(d, Style::new().fg(th.fg)),
                ])
            }
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
