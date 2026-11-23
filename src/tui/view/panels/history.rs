//! HISTORY: daily bars by model and the weekday × hour heat map.

use chrono::{Datelike, Local};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{faint, trend};
use crate::core::models::Family;
use crate::tui::app::{App, Panel};
use crate::tui::view::kit::*;
use crate::ui::fmt;

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
            fmt::usd(days.iter().map(|d| d.1).sum::<f64>()),
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
