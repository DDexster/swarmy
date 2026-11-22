//! The boot sequence: a scanline sweep, then a swarm of braille "bees" flying in from beyond the
//! screen in spirals and settling into the SWARMY logo, a light wave over it, a few bees left
//! circling, and a POST log that reports the real indexing progress.

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
    ('S', [" ████", "█    ", " ███ ", "    █", "████ "]),
    ('W', ["█   █", "█   █", "█ █ █", "██ ██", "█   █"]),
    ('R', ["████ ", "█   █", "████ ", "█  █ ", "█   █"]),
    ('Y', ["█   █", " █ █ ", "  █  ", "  █  ", "  █  "]),
];

const NOISE: &[char] = &[
    '░', '▒', '▓', '█', '╳', '⣿', '⢿', '⡿', '⣷', '⣯', '⣟', '⡾', '▚', '▞', '◢', '◣', '╱', '╲', '0',
    '1',
];

pub const LOGO_TEXT: &str = "SWARMY";

/// Rows of the logo as strings of `█` and spaces.
pub fn logo_rows() -> Vec<String> {
    let mut rows = vec![String::new(); 5];
    for (i, ch) in LOGO_TEXT.chars().enumerate() {
        let glyph = FONT
            .iter()
            .find(|(c, _)| *c == ch)
            .map(|(_, g)| g)
            .unwrap_or(&FONT[0].1);
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
    b.skip || (t > 2.5 && ready_at.elapsed().as_secs_f32() > 0.35)
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

    // The swarm logo when there is room for it; the decrypting block logo otherwise.
    let (swarm_bottom, top) = match swarm_logo(f, app, area, t) {
        Some(bottom) => (Some(bottom), bottom),
        None => (None, area.y + area.height.saturating_sub(16) / 2),
    };
    let rows = logo_rows();
    let lw = rows[0].chars().count() as u16;
    if let Some(bottom) = swarm_bottom {
        let sub = "mission control for claude code agents";
        if t > 1.55 {
            let shown = (((t - 1.55) * 70.0) as usize).min(sub.len());
            let line = Line::from(Span::styled(&sub[..shown], Style::new().fg(th.dim)));
            let w = sub.len() as u16;
            f.render_widget(
                Paragraph::new(line),
                Rect {
                    x: area.x + (area.width.saturating_sub(w)) / 2,
                    y: bottom + 1,
                    width: w,
                    height: 1,
                },
            );
        }
    } else if area.width >= lw + 4 {
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
    let first_step = if swarm_bottom.is_some() { 1.5 } else { 0.55 };
    let step = |i: usize| t > first_step + i as f32 * 0.14;
    if step(0) {
        lines.push(Line::from(vec![
            ok.clone(),
            Span::styled(
                format!(
                    "swarmy v{} · local only, no network",
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
    let y = if swarm_bottom.is_some() {
        top + 3
    } else {
        top + 8
    };
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

/// Braille dot bits: index by (dx, dy) inside a 2×4 cell.
const DOT: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

fn ease_out(u: f32) -> f32 {
    1.0 - (1.0 - u).powi(3)
}

/// Draws the swarm assembling into the logo; returns the row under the logo, or `None` when the
/// screen is too small for it.
fn swarm_logo(f: &mut Frame, app: &App, area: Rect, t: f32) -> Option<u16> {
    let th = &app.theme;
    let rows = logo_rows();
    let (pw, ph) = (rows[0].chars().count() as u16, rows.len() as u16);
    // Logo pixels as terminal cells: square-ish pixels (cells are about twice as tall as wide).
    let sx = ((area.width as f32 * 0.72) / pw as f32)
        .floor()
        .clamp(0.0, 6.0) as u16;
    if sx < 2 || area.height < 20 {
        return None;
    }
    let sy = (sx / 2).max(1);
    let (lw, lh) = (pw * sx, ph * sy);
    let left = area.x + (area.width - lw) / 2;
    let top = area.y + area.height.saturating_sub(lh + 12) / 2;
    let (cw, ch) = (area.width as usize, area.height as usize);
    // Dot grid of the whole screen.
    let (dw, dh) = (cw * 2, ch * 4);
    let mut bits = vec![0u8; cw * ch];
    let mut settled = vec![false; cw * ch];
    let cx = (left - area.x) as f32 * 2.0 + lw as f32;
    let cy = (top - area.y) as f32 * 4.0 + lh as f32 * 2.0;
    let reach = ((dw * dw + dh * dh) as f32).sqrt() * 0.6;
    let mut plot = |x: f32, y: f32, home: bool| {
        if x < 0.0 || y < 0.0 {
            return;
        }
        let (xi, yi) = (x as usize, y as usize);
        if xi >= dw || yi >= dh {
            return;
        }
        let cell = (yi / 4) * cw + xi / 2;
        bits[cell] |= DOT[xi % 2][yi % 4];
        if home {
            settled[cell] = true;
        }
    };
    // Every dot of the logo is a bee with its own start, delay, speed and spin.
    let mut n = 0u64;
    for (r, row) in rows.iter().enumerate() {
        for (c, pix) in row.chars().enumerate() {
            if pix != '█' {
                continue;
            }
            for dy in 0..(sy as usize * 4) {
                for dx in 0..(sx as usize * 2) {
                    n += 1;
                    let tx = (left - area.x) as f32 * 2.0 + (c * sx as usize * 2 + dx) as f32;
                    let ty = (top - area.y) as f32 * 4.0 + (r * sy as usize * 4 + dy) as f32;
                    // Leave a hairline between logo pixels so the letters read as dotted blocks.
                    if dx == sx as usize * 2 - 1 && sx > 2 || dy == sy as usize * 4 - 1 && sy > 1 {
                        continue;
                    }
                    let h = hash(n, 11, 3);
                    let rnd = |k: u64| (hash(n, k, 5) % 10_000) as f32 / 10_000.0;
                    let delay = 0.18 + rnd(1) * 0.55 + (c as f32 / pw as f32) * 0.25;
                    let dur = 0.7 + rnd(2) * 0.35;
                    let u = ((t - delay) / dur).clamp(0.0, 1.0);
                    if u <= 0.0 {
                        continue;
                    }
                    let e = ease_out(u);
                    // Start beyond the screen, at a random bearing.
                    let a0 = rnd(3) * std::f32::consts::TAU;
                    let (ox, oy) = (
                        cx + a0.cos() * reach * 1.1 - tx,
                        cy + a0.sin() * reach * 0.6 - ty,
                    );
                    // Spiral in: the offset turns and shrinks; bees buzz while flying.
                    let spin = if h % 2 == 0 { 1.0 } else { -1.0 } * (1.6 + rnd(4) * 2.2);
                    let ang = (1.0 - e) * spin;
                    let k = 1.0 - e;
                    let buzz = k * 3.0;
                    let (s, co) = ang.sin_cos();
                    let x = tx + (ox * co - oy * s) * k + (t * 23.0 + rnd(5) * 40.0).sin() * buzz;
                    let y = ty + (ox * s + oy * co) * k + (t * 19.0 + rnd(6) * 40.0).cos() * buzz;
                    plot(x, y, u >= 1.0);
                }
            }
        }
    }
    // After landing, a few bees keep circling the logo.
    if t > 1.2 {
        let rx = lw as f32 * 1.25;
        let ry = lh as f32 * 4.0 * 0.9;
        for i in 0..36u64 {
            let rnd = |k: u64| (hash(i, k, 9) % 10_000) as f32 / 10_000.0;
            let w = 0.8 + rnd(1) * 1.4;
            let a = t * w * if i % 2 == 0 { 1.0 } else { -1.0 } + rnd(2) * std::f32::consts::TAU;
            let r = 0.85 + rnd(3) * 0.4;
            let x = cx + a.cos() * rx * r + (t * 31.0 + i as f32).sin() * 1.5;
            let y = cy + a.sin() * ry * r + (t * 27.0 + i as f32).cos() * 1.5;
            plot(x, y, false);
        }
    }
    // Paint: settled dots in the logo gradient (with a light wave after landing), flying ones dim.
    let wave = (t - 1.45) / 0.55;
    let buf = f.buffer_mut();
    for yi in 0..ch {
        for xi in 0..cw {
            let b = bits[yi * cw + xi];
            if b == 0 {
                continue;
            }
            let (x, y) = (area.x + xi as u16, area.y + yi as u16);
            let in_logo = x >= left && x < left + lw && y >= top && y < top + lh;
            let k = if in_logo {
                (x - left) as f32 / lw as f32
            } else {
                ((x - area.x) as f32 / cw as f32).clamp(0.0, 1.0)
            };
            let target = lerp(th.accent, th.fable, k);
            let color = if settled[yi * cw + xi] {
                let d = (k - wave).abs();
                if (0.0..=1.2).contains(&wave) && d < 0.08 {
                    th.glow(target, (0.08 - d) / 0.08 * 0.9)
                } else {
                    target
                }
            } else {
                th.fade(target, 0.45)
            };
            buf[(x, y)]
                .set_char(char::from_u32(0x2800 + b as u32).unwrap_or(' '))
                .set_fg(color);
        }
    }
    Some(top + lh)
}
