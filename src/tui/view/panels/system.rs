//! SYSTEM: Claude Code processes with their heaviest children, memory and disk.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::dim;
use crate::tui::app::{App, Panel};
use crate::tui::view::kit::*;

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
    let hb = crate::ui::fmt::human_bytes;
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
