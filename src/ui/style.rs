//! Semantic colours. Colour is used only where it carries meaning: state, model family, limit
//! thresholds. Truecolor when the terminal says so, the 16 ANSI colours otherwise, none when
//! output is not a terminal or `NO_COLOR` is set.

use std::io::IsTerminal;

use owo_colors::{AnsiColors, OwoColorize, Style};

use crate::core::limits::Level;
use crate::core::models::Family;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Dim,
    Bold,
    Accent,
    Good,
    Warn,
    Bad,
    Header,
    Fable,
    Opus,
    Sonnet,
    Haiku,
    Gemini,
    Gpt,
    Other,
}

impl Tone {
    pub fn of_family(f: Family) -> Tone {
        match f {
            Family::Fable | Family::Mythos => Tone::Fable,
            Family::Opus => Tone::Opus,
            Family::Sonnet => Tone::Sonnet,
            Family::Haiku => Tone::Haiku,
            Family::Gemini => Tone::Gemini,
            Family::Gpt => Tone::Gpt,
            Family::Other => Tone::Other,
        }
    }

    /// The tone of a limit's level (`core::limits::Level`): green, amber, red.
    pub fn of_utilization(pct: f64) -> Tone {
        match Level::of(pct) {
            Level::Ok => Tone::Good,
            Level::Warn => Tone::Warn,
            Level::Bad | Level::Reached => Tone::Bad,
        }
    }

    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Tone::Plain | Tone::Bold => (220, 223, 228),
            Tone::Dim => (110, 118, 129),
            Tone::Accent | Tone::Header => (0, 215, 255),
            Tone::Good => (80, 250, 123),
            Tone::Warn => (255, 184, 0),
            Tone::Bad => (255, 85, 85),
            Tone::Fable => (189, 147, 249),
            Tone::Opus => (255, 140, 66),
            Tone::Sonnet => (98, 160, 234),
            Tone::Haiku => (64, 224, 208),
            Tone::Gemini => (236, 112, 200),
            Tone::Gpt => (245, 169, 184),
            Tone::Other => (160, 160, 160),
        }
    }

    fn ansi(self) -> AnsiColors {
        match self {
            Tone::Plain | Tone::Bold => AnsiColors::Default,
            Tone::Dim | Tone::Other => AnsiColors::BrightBlack,
            Tone::Accent | Tone::Header => AnsiColors::BrightCyan,
            Tone::Good => AnsiColors::BrightGreen,
            Tone::Warn => AnsiColors::Yellow,
            Tone::Bad => AnsiColors::BrightRed,
            Tone::Fable => AnsiColors::BrightMagenta,
            Tone::Opus => AnsiColors::BrightYellow,
            Tone::Sonnet => AnsiColors::BrightBlue,
            Tone::Haiku => AnsiColors::Cyan,
            Tone::Gemini => AnsiColors::BrightMagenta,
            Tone::Gpt => AnsiColors::Magenta,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    None,
    Ansi,
    True,
}

#[derive(Debug, Clone, Copy)]
pub struct Painter {
    pub depth: Depth,
}

impl Painter {
    pub fn new(choice: ColorChoice) -> Painter {
        let truecolor = std::env::var("COLORTERM")
            .map(|v| v == "truecolor" || v == "24bit")
            .unwrap_or(false)
            || std::env::var("WT_SESSION").is_ok();
        let depth = match choice {
            ColorChoice::Never => Depth::None,
            ColorChoice::Always => {
                if truecolor {
                    Depth::True
                } else {
                    Depth::Ansi
                }
            }
            ColorChoice::Auto => {
                if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
                    || !std::io::stdout().is_terminal()
                {
                    Depth::None
                } else if truecolor {
                    Depth::True
                } else {
                    Depth::Ansi
                }
            }
        };
        Painter { depth }
    }

    #[cfg(test)]
    pub fn plain() -> Painter {
        Painter { depth: Depth::None }
    }

    pub fn paint(&self, s: &str, tone: Tone) -> String {
        if self.depth == Depth::None || tone == Tone::Plain {
            return s.to_string();
        }
        let mut st = match self.depth {
            Depth::True => {
                let (r, g, b) = tone.rgb();
                Style::new().truecolor(r, g, b)
            }
            _ => Style::new().color(tone.ansi()),
        };
        if matches!(tone, Tone::Bold | Tone::Header) {
            st = st.bold();
        }
        s.style(st).to_string()
    }
}
