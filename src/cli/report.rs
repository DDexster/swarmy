//! `swarmy report`: API-equivalent spend grouped by a dimension.

use crate::core::models::{Family, short_name};
use crate::index::query::{Dim, Filter, Row};
use crate::ui::fmt;
use crate::ui::style::Tone;
use crate::ui::table::{Align, Cell, Table};

use super::{Format, ReportArgs};
use crate::app::context::Ctx;

pub fn run(ctx: &Ctx, a: &ReportArgs) -> anyhow::Result<()> {
    let (idx, _) = ctx.index()?;
    let filter = a.filter.to_filter();
    let span = a
        .period
        .span(ctx.now, ctx.limit_week(&idx, filter.account.as_deref())?);
    let mut rows = idx.spend_by(a.by, span, &filter)?;
    let total = idx.total(span, &filter)?;
    if a.by == Dim::Account {
        for r in &mut rows {
            r.label = ctx.account_name(&r.key);
        }
    }
    if a.by == Dim::Project && ctx.mask {
        for r in &mut rows {
            r.label = ctx.show_path(&r.label);
        }
    }
    if a.by == Dim::Weekday {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        for r in &mut rows {
            r.label = r
                .key
                .parse::<usize>()
                .ok()
                .and_then(|d| DAYS.get(d))
                .unwrap_or(&"?")
                .to_string();
        }
    }
    match a.format {
        Format::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "period": {"label": a.period.label(), "start": span.start, "end": span.end},
                    "by": a.by, "rows": rows, "total": total,
                }))?
            );
            return Ok(());
        }
        Format::Csv => {
            println!(
                "key,label,requests,input,cache_write_5m,cache_write_1h,cache_read,output,thinking,web_search,cost_usd,unpriced"
            );
            for r in &rows {
                let u = r.usage;
                println!(
                    "{},{},{},{},{},{},{},{},{},{},{:.6},{}",
                    csv(&r.key),
                    csv(&r.label),
                    r.requests,
                    u.input,
                    u.cache_write_5m,
                    u.cache_write_1h,
                    u.cache_read,
                    u.output,
                    u.thinking,
                    u.web_search,
                    r.cost,
                    r.unpriced
                );
            }
            return Ok(());
        }
        Format::Table => {}
    }
    let p = &ctx.painter;
    let shown: Vec<&Row> = if a.by.is_time() {
        rows.iter().collect()
    } else {
        rows.iter().take(a.top).collect()
    };
    println!(
        "{} {} {}",
        p.paint("SPEND", Tone::Header),
        p.paint(
            &format!(
                "· {} · {} → {}",
                a.period.label(),
                local(span.start),
                local(span.end)
            ),
            Tone::Dim
        ),
        p.paint(&filter_label(&filter), Tone::Dim)
    );
    println!();
    if rows.is_empty() {
        println!("{}", p.paint("no requests in this period", Tone::Dim));
        return Ok(());
    }
    let max = rows.iter().map(|r| r.cost).fold(0.0, f64::max);
    let mut t = Table::new(&[
        (a.by.title(), Align::Left),
        ("REQS", Align::Right),
        ("INPUT", Align::Right),
        ("C.WRITE", Align::Right),
        ("C.READ", Align::Right),
        ("OUTPUT", Align::Right),
        ("COST", Align::Right),
        ("SHARE", Align::Left),
    ]);
    for r in &shown {
        let tone = if a.by == Dim::Model {
            Tone::of_family(Family::of(&r.key))
        } else {
            Tone::Plain
        };
        let label = if a.by == Dim::Model {
            short_name(&r.label)
        } else {
            r.label.clone()
        };
        let share = if total.cost > 0.0 {
            r.cost / total.cost
        } else {
            0.0
        };
        let bar_tone = if a.by == Dim::Model {
            tone
        } else {
            Tone::Accent
        };
        t.row(vec![
            Cell::tone(label, tone),
            Cell::new(fmt::count(r.requests)),
            Cell::new(fmt::tokens(r.usage.input)),
            Cell::new(fmt::tokens(r.usage.cache_write())),
            Cell::new(fmt::tokens(r.usage.cache_read)),
            Cell::new(fmt::tokens(r.usage.output)),
            Cell::tone(fmt::money(r.cost), Tone::Bold),
            Cell::tone(
                format!(
                    "{:<16} {}",
                    fmt::bar(if max > 0.0 { r.cost / max } else { 0.0 }, 16),
                    fmt::pct(share * 100.0)
                ),
                bar_tone,
            ),
        ]);
    }
    if shown.len() < rows.len() {
        let rest: f64 = rows.iter().skip(shown.len()).map(|r| r.cost).sum();
        t.row(vec![
            Cell::tone(format!("… {} more", rows.len() - shown.len()), Tone::Dim),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
            Cell::tone(fmt::money(rest), Tone::Dim),
            Cell::new(""),
        ]);
    }
    let u = total.usage;
    t.footer(vec![
        Cell::tone("TOTAL", Tone::Bold),
        Cell::new(fmt::count(total.requests)),
        Cell::new(fmt::tokens(u.input)),
        Cell::new(fmt::tokens(u.cache_write())),
        Cell::new(fmt::tokens(u.cache_read)),
        Cell::new(fmt::tokens(u.output)),
        Cell::tone(fmt::money(total.cost), Tone::Accent),
        Cell::tone(
            format!(
                "cache hit {}",
                u.cache_hit_ratio()
                    .map(|x| fmt::pct(x * 100.0))
                    .unwrap_or_else(|| "-".into())
            ),
            Tone::Dim,
        ),
    ]);
    println!("{}", t.render(p));
    if total.unpriced > 0 {
        println!("{}", p.paint(&format!("! {} requests use models without a known price (counted as $0); see `swarmy prices`", total.unpriced), Tone::Warn));
    }
    if total.pending > 0 {
        println!("{}", p.paint(&format!("! {} requests are being priced right now and are not in these totals yet; run again in a moment", total.pending), Tone::Warn));
    }
    println!(
        "{}",
        p.paint(
            "API-equivalent cost at list prices; subscription usage is not billed this way.",
            Tone::Dim
        )
    );
    Ok(())
}

pub fn local(t: chrono::DateTime<chrono::Utc>) -> String {
    if t.timestamp() <= 0 {
        return "the beginning".into();
    }
    t.with_timezone(&chrono::Local)
        .format("%b %d %H:%M")
        .to_string()
}

pub fn filter_label(f: &Filter) -> String {
    let mut parts = Vec::new();
    if let Some(a) = &f.account {
        parts.push(format!("account={a}"));
    }
    if let Some(s) = &f.seat {
        parts.push(format!("seat={s}"));
    }
    if let Some(p) = &f.project {
        parts.push(format!("project={p}"));
    }
    if let Some(m) = &f.model {
        parts.push(format!("model~{m}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("· {}", parts.join(" "))
    }
}

fn csv(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
