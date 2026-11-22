//! Number, money and time formats.

use chrono::Duration;

fn group(int: u64) -> String {
    let s = int.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `$1,234.56`; small amounts keep a meaningful digit: `$0.042`.
pub fn money(x: f64) -> String {
    if !x.is_finite() {
        return "-".into();
    }
    let neg = x < 0.0;
    let a = x.abs();
    let body = if a >= 1.0 || a == 0.0 {
        let cents = (a * 100.0).round() as u64;
        format!("{}.{:02}", group(cents / 100), cents % 100)
    } else if a >= 0.01 {
        format!("{a:.2}")
    } else {
        format!("{a:.4}")
    };
    format!("{}${body}", if neg { "-" } else { "" })
}

/// Money for the dashboard: cents only below $1,000 (`$8,675`, `$12.40`).
pub fn usd(x: f64) -> String {
    if x.is_finite() && x.abs() >= 1000.0 {
        let neg = x < 0.0;
        format!(
            "{}${}",
            if neg { "-" } else { "" },
            group(x.abs().round() as u64)
        )
    } else {
        money(x)
    }
}

/// `950`, `12.3k`, `4.56M`, `1.20B`.
pub fn tokens(n: u64) -> String {
    let f = n as f64;
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", f / 1e3)
    } else if n < 1_000_000_000 {
        format!("{:.2}M", f / 1e6)
    } else {
        format!("{:.2}B", f / 1e9)
    }
}

pub fn count(n: u64) -> String {
    group(n)
}

/// `3d 15h`, `2h 05m`, `14m`, `45s`.
pub fn duration(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    let (days, hours, mins) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        format!("{s}s")
    }
}

/// `m:ss` under an hour, then `h:mm:ss` — for running timers.
pub fn clock(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn pct(x: f64) -> String {
    if x >= 9.95 || x == 0.0 {
        format!("{x:.0}%")
    } else {
        format!("{x:.1}%")
    }
}

const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// A horizontal bar with 1/8-cell precision; `frac` is clamped to 0..=1.
pub fn bar(frac: f64, width: usize) -> String {
    let frac = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let eighths = (frac * width as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let rem = eighths % 8;
    let mut s = "█".repeat(full);
    if rem > 0 {
        s.push(EIGHTHS[rem - 1]);
    }
    s
}

/// A gauge `▕██████▍    ▏` exactly `width` cells inside the brackets.
pub fn gauge(frac: f64, width: usize) -> String {
    let b = bar(frac, width);
    let used = unicode_width::UnicodeWidthStr::width(b.as_str());
    format!("▕{b}{}▏", " ".repeat(width.saturating_sub(used)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(money(1234.567), "$1,234.57");
        assert_eq!(money(0.0382254), "$0.04");
        assert_eq!(money(0.004), "$0.0040");
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_345), "12.3k");
        assert_eq!(tokens(4_560_000), "4.56M");
        assert_eq!(
            duration(Duration::seconds(3 * 86_400 + 15 * 3600 + 59)),
            "3d 15h"
        );
        assert_eq!(duration(Duration::seconds(2 * 3600 + 5 * 60)), "2h 05m");
        assert_eq!(clock(Duration::seconds(134)), "2:14");
        assert_eq!(pct(77.0), "77%");
        assert_eq!(pct(5.39), "5.4%");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0.5, 4), "██");
        assert_eq!(bar(1.0 / 32.0, 4), "▏");
        assert_eq!(bar(2.0, 2), "██");
        assert_eq!(gauge(0.25, 4), "▕█   ▏");
    }
}
