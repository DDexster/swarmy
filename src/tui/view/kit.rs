//! Drawing primitives shared by the panels.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::core::models::short_name;
use crate::live::{Agent, State};
use crate::tui::app::{App, Panel};
use crate::tui::theme::Theme;

pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
pub const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
pub const VBARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub fn w(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// `s` cut or padded to exactly `width` cells.
pub fn fit(s: &str, width: usize) -> String {
    let t = crate::ui::table::truncate(s, width);
    let pad = width.saturating_sub(w(&t));
    format!("{t}{}", " ".repeat(pad))
}

pub fn fit_right(s: &str, width: usize) -> String {
    let t = crate::ui::table::truncate(s, width);
    format!("{}{t}", " ".repeat(width.saturating_sub(w(&t))))
}

/// A panel frame: its number, title and an optional suffix; the focused one is drawn bright.
/// Returns the inner area.
pub fn frame(f: &mut Frame, app: &mut App, p: Panel, area: Rect, suffix: &str, hot: bool) -> Rect {
    let th = app.theme;
    let focused = app.focus == p;
    let border = if hot {
        th.warn
    } else if focused {
        th.accent
    } else {
        th.border
    };
    // In tabs the digits switch tabs, so panel numbers would mislead.
    let tabs = app.shape == crate::tui::app::Shape::Tabs;
    let mut title = vec![
        Span::raw(" "),
        Span::styled(
            if tabs {
                String::new()
            } else {
                format!(" {} ", p.number())
            },
            Style::new()
                .fg(if focused { th.bg } else { th.dim })
                .bg(if focused { th.accent } else { th.track })
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", p.title()),
            if focused {
                th.bold(th.bg).bg(th.accent)
            } else {
                th.heading()
            },
        ),
    ];
    if !suffix.is_empty() {
        title.push(Span::styled(
            format!(" · {suffix}"),
            Style::new().fg(th.dim),
        ));
    }
    title.push(Span::raw(" "));
    let block = Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(Style::new().fg(border))
        .title(Line::from(title))
        .style(th.base());
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.rects.push((p, area));
    inner
}

/// A bar `width` cells wide: filled part in `color` with 1/8-cell precision, the rest a faint track.
pub fn gauge(frac: f64, width: usize, color: Color, th: &Theme) -> Vec<Span<'static>> {
    let frac = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let eighths = (frac * width as f64 * 8.0).round() as usize;
    let full = (eighths / 8).min(width);
    let mut s = "█".repeat(full);
    let mut used = full;
    if eighths % 8 > 0 && used < width {
        s.push(EIGHTHS[eighths % 8 - 1]);
        used += 1;
    }
    vec![
        Span::styled(s, Style::new().fg(color)),
        Span::styled("▁".repeat(width - used), Style::new().fg(th.track)),
    ]
}

pub fn spark(values: &[f64], color: Color) -> Span<'static> {
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

pub fn pulse(app: &App, c: Color, hz: f64) -> Color {
    if !app.animating() {
        return c;
    }
    let k = ((app.t() * hz * std::f64::consts::TAU).sin() * 0.5 + 0.5) as f32;
    app.theme.glow(c, k * 0.45)
}

pub fn blink(app: &App) -> bool {
    !app.animating() || (app.t() * 2.0) as u64 % 2 == 0
}

pub fn effort_abbr(e: Option<&str>) -> &'static str {
    match e.unwrap_or("") {
        "low" => "lo",
        "medium" => "md",
        "high" => "hi",
        "xhigh" => "xh",
        "max" => "mx",
        _ => "",
    }
}

pub fn model_chip(a: &Agent) -> String {
    let m = a
        .model
        .as_deref()
        .map(short_name)
        .unwrap_or_else(|| "-".into());
    let e = effort_abbr(a.effort.as_deref());
    if e.is_empty() { m } else { format!("{m} {e}") }
}

pub fn glyph(app: &App, s: &State) -> (String, Color) {
    let th = &app.theme;
    let spin = SPINNER[(app.t() * 12.0) as usize % SPINNER.len()];
    match s {
        State::NeedsYou(_) => (if blink(app) { "▲" } else { "△" }.into(), th.warn),
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

pub fn activity(s: &State) -> String {
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
        State::WaitingAgents(n) => {
            format!("waiting for {n} agent{}", if *n == 1 { "" } else { "s" })
        }
        State::Idle => "idle".into(),
        State::Stalled => "STALLED".into(),
        State::Done => "done".into(),
        State::Gone => "exited".into(),
    }
}

pub fn mask_name(app: &App, s: &str) -> String {
    if app.mask && s.contains('@') {
        "•••".into()
    } else {
        s.to_string()
    }
}

/// Renders lines into `area`, clipped.
pub fn put(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    f.render_widget(Paragraph::new(lines), area);
}

/// A single row `y` of `area` (or `None` beyond its bottom).
pub fn row(area: Rect, y: u16) -> Option<Rect> {
    (y < area.height).then_some(Rect {
        x: area.x,
        y: area.y + y,
        width: area.width,
        height: 1,
    })
}

/// Splits `total` into parts proportional to `weights`, the last taking the remainder.
pub fn split(total: u16, weights: &[u16]) -> Vec<u16> {
    let sum: u16 = weights.iter().sum::<u16>().max(1);
    let mut out: Vec<u16> = weights
        .iter()
        .map(|w| (total as u32 * *w as u32 / sum as u32) as u16)
        .collect();
    let used: u16 = out.iter().sum();
    if let Some(last) = out.last_mut() {
        *last += total.saturating_sub(used);
    }
    out
}

pub fn hsplit(area: Rect, weights: &[u16]) -> Vec<Rect> {
    let mut x = area.x;
    split(area.width, weights)
        .into_iter()
        .map(|wd| {
            let r = Rect {
                x,
                y: area.y,
                width: wd,
                height: area.height,
            };
            x += wd;
            r
        })
        .collect()
}

pub fn vsplit_fixed(area: Rect, heights: &[u16]) -> Vec<Rect> {
    let mut y = area.y;
    heights
        .iter()
        .map(|h| {
            let h = (*h).min(area.y + area.height - y.min(area.y + area.height));
            let r = Rect {
                x: area.x,
                y,
                width: area.width,
                height: h,
            };
            y += h;
            r
        })
        .collect()
}
