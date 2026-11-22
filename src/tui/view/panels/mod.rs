//! The panels. Each one fills the rectangle it is given: columns, bar widths and row counts come
//! from the size, so the same panel works in a 40-column strip and on a 4K wall. One file per
//! panel; this one holds what several of them share.

mod agent;
mod attention;
mod burn;
mod efficiency;
mod history;
mod limits;
mod models;
mod projects;
mod swarm;
mod system;

pub use agent::agent;
pub use attention::attention;
pub use burn::burn;
pub use efficiency::efficiency;
pub use history::history;
pub use limits::mana;
pub use models::models;
pub use projects::projects;
pub use swarm::swarm;
pub use system::system;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::tui::app::App;
use crate::tui::view::kit::fit_right;

fn dim(app: &App) -> Style {
    Style::new().fg(app.theme.dim)
}

fn faint(app: &App) -> Style {
    Style::new().fg(app.theme.faint)
}

/// The period a trend compares with: "last 7d" → "previous 7d", "today" → "yesterday".
pub fn prev_label(period: &str) -> String {
    match period {
        "today" => "yesterday".into(),
        p => match p.strip_prefix("last ") {
            Some(rest) => format!("previous {rest}"),
            None => format!("before {p}"),
        },
    }
}

/// `▲12%` / `▼8%` against the previous period; nothing when there is no base.
pub fn trend(app: &App, cur: f64, prev: f64) -> Span<'static> {
    let th = app.theme;
    if prev <= 0.0 || !cur.is_finite() {
        return Span::raw("");
    }
    let d = (cur - prev) / prev * 100.0;
    if d.abs() < 1.0 {
        return Span::styled(" ≈", faint(app));
    }
    let (g, c) = if d > 0.0 {
        ("▲", th.warn)
    } else {
        ("▼", th.good)
    };
    let v = if d.abs() >= 1000.0 {
        ">999".to_string()
    } else {
        format!("{:.0}", d.abs())
    };
    Span::styled(format!(" {g}{v}%"), Style::new().fg(c))
}

/// `trend` right-aligned in `width` cells (blank when there is no base).
pub fn trend_w(app: &App, cur: f64, prev: f64, width: usize) -> Span<'static> {
    let t = trend(app, cur, prev);
    let style = t.style;
    Span::styled(fit_right(t.content.trim_start(), width), style)
}
