//! Colour themes for the dashboard.

use ratatui::style::{Color, Modifier, Style};

use crate::core::models::Family;
use crate::live::State;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    /// Secondary text (timestamps, hints): readable, at least 3:1 on the background.
    pub faint: Color,
    /// Gauge tracks and separators: barely visible on purpose.
    pub track: Color,
    /// Selected row background.
    pub sel: Color,
    pub border: Color,
    /// Panel titles and section headings: bright text on a plate, so they stand out from data.
    pub title: Color,
    pub title_bg: Color,
    /// Project and directory names, so they stand apart from agent names.
    pub dir: Color,
    pub accent: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    pub thinking: Color,
    pub fable: Color,
    pub opus: Color,
    pub sonnet: Color,
    pub haiku: Color,
    pub other: Color,
    pub animate: bool,
}

const OPS: Theme = Theme {
    name: "ops",
    bg: Color::Rgb(10, 13, 18),
    fg: Color::Rgb(205, 214, 224),
    dim: Color::Rgb(140, 150, 164),
    faint: Color::Rgb(104, 114, 130),
    track: Color::Rgb(44, 52, 64),
    sel: Color::Rgb(24, 56, 76),
    border: Color::Rgb(40, 66, 82),
    title: Color::Rgb(240, 244, 250),
    title_bg: Color::Rgb(30, 64, 98),
    dir: Color::Rgb(230, 196, 128),
    accent: Color::Rgb(0, 215, 255),
    good: Color::Rgb(80, 250, 123),
    warn: Color::Rgb(255, 184, 0),
    bad: Color::Rgb(255, 85, 85),
    thinking: Color::Rgb(0, 190, 230),
    fable: Color::Rgb(189, 147, 249),
    opus: Color::Rgb(255, 140, 66),
    sonnet: Color::Rgb(98, 160, 234),
    haiku: Color::Rgb(64, 224, 208),
    other: Color::Rgb(160, 160, 160),
    animate: true,
};

const PHOSPHOR: Theme = Theme {
    name: "phosphor",
    bg: Color::Rgb(2, 10, 4),
    fg: Color::Rgb(120, 255, 140),
    dim: Color::Rgb(40, 140, 60),
    faint: Color::Rgb(40, 130, 60),
    track: Color::Rgb(16, 60, 26),
    sel: Color::Rgb(14, 70, 30),
    border: Color::Rgb(24, 100, 40),
    title: Color::Rgb(210, 255, 215),
    title_bg: Color::Rgb(18, 84, 36),
    dir: Color::Rgb(190, 255, 120),
    accent: Color::Rgb(170, 255, 180),
    good: Color::Rgb(120, 255, 140),
    warn: Color::Rgb(230, 255, 120),
    bad: Color::Rgb(255, 120, 100),
    thinking: Color::Rgb(90, 220, 120),
    fable: Color::Rgb(200, 255, 200),
    opus: Color::Rgb(150, 255, 150),
    sonnet: Color::Rgb(100, 220, 120),
    haiku: Color::Rgb(70, 190, 100),
    other: Color::Rgb(60, 160, 80),
    animate: true,
};

const AMBER: Theme = Theme {
    name: "amber",
    bg: Color::Rgb(12, 8, 2),
    fg: Color::Rgb(255, 190, 90),
    dim: Color::Rgb(160, 110, 40),
    faint: Color::Rgb(150, 100, 40),
    track: Color::Rgb(70, 46, 14),
    sel: Color::Rgb(70, 46, 10),
    border: Color::Rgb(110, 72, 20),
    title: Color::Rgb(255, 238, 200),
    title_bg: Color::Rgb(96, 60, 12),
    dir: Color::Rgb(255, 225, 160),
    accent: Color::Rgb(255, 214, 140),
    good: Color::Rgb(255, 200, 100),
    warn: Color::Rgb(255, 240, 160),
    bad: Color::Rgb(255, 110, 60),
    thinking: Color::Rgb(230, 170, 80),
    fable: Color::Rgb(255, 230, 170),
    opus: Color::Rgb(255, 180, 80),
    sonnet: Color::Rgb(220, 150, 60),
    haiku: Color::Rgb(190, 130, 50),
    other: Color::Rgb(150, 100, 40),
    animate: true,
};

const PLAIN: Theme = Theme {
    name: "plain",
    bg: Color::Reset,
    fg: Color::Reset,
    dim: Color::DarkGray,
    faint: Color::DarkGray,
    track: Color::DarkGray,
    sel: Color::DarkGray,
    border: Color::DarkGray,
    title: Color::White,
    title_bg: Color::Blue,
    dir: Color::Yellow,
    accent: Color::Cyan,
    good: Color::Green,
    warn: Color::Yellow,
    bad: Color::Red,
    thinking: Color::Cyan,
    fable: Color::Magenta,
    opus: Color::Yellow,
    sonnet: Color::Blue,
    haiku: Color::Cyan,
    other: Color::Gray,
    animate: false,
};

pub const NAMES: [&str; 4] = ["ops", "phosphor", "amber", "plain"];

impl Theme {
    pub fn named(name: Option<&str>) -> Theme {
        match name.unwrap_or("ops") {
            "phosphor" => PHOSPHOR,
            "amber" => AMBER,
            "plain" => PLAIN,
            _ => OPS,
        }
    }

    pub fn next(self) -> Theme {
        let i = NAMES.iter().position(|n| *n == self.name).unwrap_or(0);
        Theme::named(Some(NAMES[(i + 1) % NAMES.len()]))
    }

    pub fn s(&self, c: Color) -> Style {
        Style::new().fg(c)
    }

    pub fn base(&self) -> Style {
        Style::new().fg(self.fg).bg(self.bg)
    }

    /// A section heading inside a panel.
    pub fn heading(&self) -> Style {
        Style::new()
            .fg(self.title)
            .bg(self.title_bg)
            .add_modifier(ratatui::style::Modifier::BOLD)
    }

    pub fn bold(&self, c: Color) -> Style {
        Style::new().fg(c).add_modifier(Modifier::BOLD)
    }

    pub fn family(&self, f: Family) -> Color {
        match f {
            Family::Fable | Family::Mythos => self.fable,
            Family::Opus => self.opus,
            Family::Sonnet => self.sonnet,
            Family::Haiku => self.haiku,
            Family::Other => self.other,
        }
    }

    pub fn family_name(&self, name: &str) -> Color {
        self.family(Family::of(name))
    }

    pub fn utilization(&self, pct: f64) -> Color {
        if pct >= 85.0 {
            self.bad
        } else if pct >= 60.0 {
            self.warn
        } else {
            self.good
        }
    }

    pub fn state(&self, s: &State) -> Color {
        match s {
            State::NeedsYou(_) => self.warn,
            State::Stalled => self.bad,
            State::Tool { .. } => self.good,
            State::Thinking => self.thinking,
            State::WaitingAgents(_) => self.accent,
            State::Idle => self.dim,
            State::Done => self.faint,
            State::Gone => self.faint,
        }
    }

    /// `c` dimmed towards the background by `k` (0 = unchanged, 1 = background).
    pub fn fade(&self, c: Color, k: f32) -> Color {
        match (c, self.bg) {
            (Color::Rgb(r, g, b), Color::Rgb(br, bgc, bb)) => {
                let mix =
                    |a: u8, z: u8| (a as f32 + (z as f32 - a as f32) * k.clamp(0.0, 1.0)) as u8;
                Color::Rgb(mix(r, br), mix(g, bgc), mix(b, bb))
            }
            _ => c,
        }
    }

    /// Brightens towards white by `k`.
    pub fn glow(&self, c: Color, k: f32) -> Color {
        match c {
            Color::Rgb(r, g, b) => {
                let mix = |a: u8| (a as f32 + (255.0 - a as f32) * k.clamp(0.0, 1.0)) as u8;
                Color::Rgb(mix(r), mix(g), mix(b))
            }
            _ => c,
        }
    }
}
