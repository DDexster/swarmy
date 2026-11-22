//! The boot sequence: a scanline sweep, the logo decrypting out of glyph noise, and a POST log
//! that reports the real indexing progress.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::App;
use crate::ui::fmt;

const FONT: &[(char, [&str; 5])] = &[
    ('A', [" ███ ", "█   █", "█████", "█   █", "█   █"]),
    ('G', [" ████", "█    ", "█  ██", "█   █", " ████"]),
    ('E', ["█████", "█    ", "████ ", "█    ", "█████"]),
    ('N', ["█   █", "██  █", "█ █ █", "█  ██", "█   █"]),
    ('T', ["█████", "  █  ", "  █  ", "  █  ", "  █  "]),
    ('M', ["█   █", "██ ██", "█ █ █", "█   █", "█   █"]),
    ('·', ["     ", "     ", "  █  ", "     ", "     "]),
];

const NOISE: &[char] = &[
    '░', '▒', '▓', '█', '╳', '⣿', '⢿', '⡿', '⣷', '⣯', '⣟', '⡾', '▚', '▞', '◢', '◣', '╱', '╲', '0',
    '1',
];

pub const LOGO_TEXT: &str = "AGENT·MANA";

/// Rows of the logo as strings of `█` and spaces.
pub fn logo_rows() -> Vec<String> {
    let mut rows = vec![String::new(); 5];
    for (i, ch) in LOGO_TEXT.chars().enumerate() {
        let glyph = FONT
            .iter()
            .find(|(c, _)| *c == ch)
            .map(|(_, g)| g)
            .unwrap_or(&FONT[6].1);
        for (r, line) in glyph.iter().enumerate() {
            if i > 0 {
                rows[r].push(' ');
            }
            rows[r].push_str(line);
        }
    }
    rows
}

/// A cheap deterministic hash for per-cell randomness.
pub fn hash(a: u64, b: u64, c: u64) -> u64 {
    let mut x = a.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ c.wrapping_mul(0x1656_67B1_9E37_79F9);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x
}

fn lerp(a: Color, b: Color, k: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k.clamp(0.0, 1.0)) as u8;
            Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
        }
        _ => a,
    }
}

/// Whether the boot screen is finished.
pub fn done(app: &App) -> bool {
    if app.error.is_some() {
        return true;
    }
    let Some(b) = &app.boot else { return true };
    let Some(ready_at) = b.ready_at else {
        return false;
    };
    let t = b.started.elapsed().as_secs_f32();
    b.skip || (t > 1.9 && ready_at.elapsed().as_secs_f32() > 0.35)
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let Some(b) = &app.boot else { return };
    let t = b.started.elapsed().as_secs_f32();
    let frame = (t * 30.0) as u64;
    f.render_widget(Paragraph::new("").style(th.base()), area);

    // Scanline sweep.
    if t < 0.45 {
        let y = area.y + ((t / 0.45) * area.height as f32) as u16;
        for dy in 0..3u16 {
            if y >= area.y + dy && y - dy < area.y + area.height {
                let c = th.fade(th.accent, 0.35 * dy as f32);
                let line = Line::from(Span::styled(
                    "▀".repeat(area.width as usize),
                    Style::new().fg(c),
                ));
                f.render_widget(
                    Paragraph::new(line),
                    Rect {
                        x: area.x,
                        y: y - dy,
                        width: area.width,
                        height: 1,
                    },
                );
            }
        }
    }

    let rows = logo_rows();
    let lw = rows[0].chars().count() as u16;
    let top = area.y + area.height.saturating_sub(16) / 2;
    if area.width >= lw + 4 {
        let left = area.x + (area.width - lw) / 2;
        for (r, row) in rows.iter().enumerate() {
            let mut spans = Vec::new();
            for (c, ch) in row.chars().enumerate() {
                let resolve = 0.25 + (hash(r as u64, c as u64, 7) % 1000) as f32 / 1000.0 * 0.95;
                let k = c as f32 / lw as f32;
                let target = lerp(th.accent, th.fable, k);
                if ch == ' ' {
                    // Occasional noise in the gaps before resolve.
                    if t < resolve && hash(r as u64, c as u64, frame).is_multiple_of(9) && t > 0.2 {
                        let g =
                            NOISE[(hash(c as u64, r as u64, frame) % NOISE.len() as u64) as usize];
                        spans.push(Span::styled(g.to_string(), Style::new().fg(th.faint)));
                    } else {
                        spans.push(Span::raw(" "));
                    }
                } else if t < 0.2 {
                    spans.push(Span::raw(" "));
                } else if t < resolve {
                    let g = NOISE[(hash(r as u64, c as u64, frame) % NOISE.len() as u64) as usize];
                    spans.push(Span::styled(
                        g.to_string(),
                        Style::new().fg(th.fade(target, 0.55)),
                    ));
                } else {
                    let since = t - resolve;
                    let c = if since < 0.12 {
                        th.glow(target, 0.8 - since * 6.0)
                    } else {
                        target
                    };
                    spans.push(Span::styled("█", Style::new().fg(c)));
                }
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect {
                    x: left,
                    y: top + r as u16,
                    width: lw,
                    height: 1,
                },
            );
        }
        let sub = "mission control for claude code agents";
        if t > 1.0 {
            let shown = (((t - 1.0) * 60.0) as usize).min(sub.len());
            let line = Line::from(Span::styled(&sub[..shown], Style::new().fg(th.dim)));
            let w = sub.len() as u16;
            f.render_widget(
                Paragraph::new(line),
                Rect {
                    x: area.x + (area.width.saturating_sub(w)) / 2,
                    y: top + 6,
                    width: w,
                    height: 1,
                },
            );
        }
    } else {
        let line = Line::from(Span::styled(LOGO_TEXT, th.bold(th.accent)));
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: area.x + 2,
                y: top,
                width: area.width.saturating_sub(2),
                height: 1,
            },
        );
    }

    // POST log.
    let ok = Span::styled("[ OK ] ", Style::new().fg(th.good));
    let wait = Span::styled("[ .. ] ", Style::new().fg(th.dim));
    let mut lines: Vec<Line> = Vec::new();
    let step = |i: usize| t > 0.55 + i as f32 * 0.14;
    if step(0) {
        lines.push(Line::from(vec![
            ok.clone(),
            Span::styled(
                format!(
                    "agentmana v{} · local only, no network",
                    env!("CARGO_PKG_VERSION")
                ),
                Style::new().fg(th.fg),
            ),
        ]));
    }
    if step(1) {
        match b.ready {
            Some((seats, accounts, _)) => lines.push(Line::from(vec![
                ok.clone(),
                Span::styled(
                    format!("seats {seats} · accounts {accounts}"),
                    Style::new().fg(th.fg),
                ),
            ])),
            None => lines.push(Line::from(vec![
                wait.clone(),
                Span::styled("scanning config dirs", Style::new().fg(th.dim)),
            ])),
        }
    }
    if step(2) {
        let p = b.progress;
        match b.ready {
            Some((_, _, files)) => lines.push(Line::from(vec![
                ok.clone(),
                Span::styled(
                    format!("index {} transcripts", fmt::count(files as u64)),
                    Style::new().fg(th.fg),
                ),
            ])),
            None => {
                let frac = if p.bytes_total > 0 {
                    p.bytes_done as f64 / p.bytes_total as f64
                } else {
                    0.0
                };
                lines.push(Line::from(vec![
                    wait.clone(),
                    Span::styled(
                        format!("indexing {} transcripts ", fmt::count(p.files_total as u64)),
                        Style::new().fg(th.fg),
                    ),
                    Span::styled(fmt::gauge(frac, 20), Style::new().fg(th.accent)),
                    Span::styled(
                        format!(
                            " {:>3.0}%  {:.1} GB",
                            frac * 100.0,
                            p.bytes_done as f64 / 1e9
                        ),
                        Style::new().fg(th.dim),
                    ),
                ]));
            }
        }
    }
    if step(3) && b.ready.is_some() {
        lines.push(Line::from(vec![
            ok,
            Span::styled("live link established", Style::new().fg(th.fg)),
        ]));
    }
    let w = 64u16.min(area.width);
    let y = top + 8;
    if y < area.y + area.height {
        f.render_widget(
            Paragraph::new(lines),
            Rect {
                x: area.x + (area.width - w) / 2,
                y,
                width: w,
                height: (area.y + area.height - y).min(6),
            },
        );
    }
    let hint = Line::from(Span::styled(
        "press any key to skip",
        Style::new().fg(th.faint),
    ));
    f.render_widget(
        Paragraph::new(hint).alignment(ratatui::layout::Alignment::Center),
        Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        },
    );
}
