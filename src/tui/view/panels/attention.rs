//! ATTENTION: what needs a look, then the event log.

use chrono::{Local, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::faint;
use crate::live::State;
use crate::tui::app::{App, Panel};
use crate::tui::view::kit::*;
use crate::ui::fmt;

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
