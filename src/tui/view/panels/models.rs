//! MODELS: spend per model with ▲▼ trends, effort mix, and project × model.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{dim, faint, prev_label, trend_w};
use crate::core::models::{Family, short_name};
use crate::tui::app::{App, Panel};
use crate::tui::view::kit::*;
use crate::ui::fmt;

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
