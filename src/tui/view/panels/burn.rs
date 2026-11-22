//! BURN: the spend rate in big digits ($ per hour), today and the period, and $ per minute over
//! the last 60 minutes.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CLine};

use super::{dim, faint};
use crate::tui::app::{App, Panel};
use crate::tui::bigdigits;
use crate::tui::view::kit::*;
use crate::ui::fmt;

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
