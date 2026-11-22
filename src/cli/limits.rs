//! `swarmy limits`: where each account's limit went, and when it runs out (the analysis itself
//! lives in `crate::limits`).

use chrono::Local;

use crate::app::context::Ctx;
use crate::core::limits::Share;
use crate::limits::{WindowReport, all_reports};
use crate::ui::fmt;
use crate::ui::style::Tone;
use crate::ui::table::{Align, Cell, Table};

use super::Format;

pub fn run(ctx: &Ctx, format: Format, top: usize) -> anyhow::Result<()> {
    let (idx, _) = ctx.index()?;
    let reports = all_reports(ctx, &idx)?;
    if format == Format::Json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }
    let p = &ctx.painter;
    if reports.is_empty() {
        println!("{}", p.paint("No limit readings yet: Claude Code caches them after it shows usage. Run /usage in Claude Code once.", Tone::Dim));
        return Ok(());
    }
    for (i, rep) in reports.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let email = ctx
            .email(&rep.account_uuid)
            .map(|e| format!(" · {e}"))
            .unwrap_or_default();
        let tier = rep
            .tier
            .as_deref()
            .map(|t| format!(" · {}", t.replace("default_claude_", "")))
            .unwrap_or_default();
        println!(
            "{} {}{}",
            p.paint("ACCOUNT", Tone::Header),
            p.paint(&rep.name, Tone::Bold),
            p.paint(&format!("{email}{tier}"), Tone::Dim)
        );
        for w in &rep.windows {
            print_window(ctx, w, top);
        }
    }
    println!();
    println!(
        "{}",
        p.paint(
            "≈ estimates: limit usage is assumed proportional to API-equivalent cost.",
            Tone::Dim
        )
    );
    Ok(())
}

fn print_window(ctx: &Ctx, w: &WindowReport, top: usize) {
    let p = &ctx.painter;
    let r = &w.reading;
    let shown = w.estimate_now;
    let tone = Tone::of_utilization(shown);
    let label = format!("{:<13}", r.kind.label());
    let main = if w.reset_passed {
        format!(
            "{} ≈{} (new window since {}, no reading yet)",
            fmt::gauge(shown / 100.0, 24),
            fmt::pct(shown),
            r.resets_at.with_timezone(&Local).format("%a %H:%M")
        )
    } else {
        format!(
            "{} {}  ≈{} now",
            fmt::gauge(shown / 100.0, 24),
            fmt::pct(r.utilization),
            fmt::pct(shown)
        )
    };
    println!("  {} {}", p.paint(&label, Tone::Bold), p.paint(&main, tone));
    let pad = " ".repeat(16);
    println!(
        "{pad}{}",
        p.paint(
            &format!(
                "resets {} (in {}) · reading {} old",
                w.window.end.with_timezone(&Local).format("%a %H:%M"),
                fmt::duration(w.window.end - ctx.now),
                fmt::duration(ctx.now - r.fetched_at)
            ),
            Tone::Dim
        )
    );
    if let Some(t) = w.manual_reset {
        println!(
            "{pad}{}",
            p.paint(
                &format!(
                    "limit reset by hand ≈{}; counting from there",
                    t.with_timezone(&Local).format("%a %H:%M")
                ),
                Tone::Accent
            )
        );
    }
    let mut burn = format!("burn {}/h", fmt::money(w.burn_usd_per_hour));
    if let Some(pph) = w.points_per_hour {
        burn.push_str(&format!(" ≈ {pph:.1} pts/h"));
    }
    match w.eta {
        Some(d) if d.num_seconds() == 0 => burn.push_str(" · LIMIT REACHED"),
        Some(d) => burn.push_str(&format!(
            " → empty {} (in {})",
            (ctx.now + d).with_timezone(&Local).format("%a %H:%M"),
            fmt::duration(d)
        )),
        None if w.burn_usd_per_hour > 0.0 => burn.push_str(" · lasts until reset"),
        None => {}
    }
    println!(
        "{pad}{}",
        p.paint(
            &burn,
            if w.eta.is_some() {
                Tone::Warn
            } else {
                Tone::Dim
            }
        )
    );
    if let (Some(u), Some(full)) = (w.usd_per_point, w.window_usd) {
        let src = match &w.reference {
            Some(r) => format!(" (reference: {r}, same plan)"),
            None => String::new(),
        };
        println!(
            "{pad}{}",
            p.paint(
                &format!(
                    "≈{}/pt · a full window ≈ {} API-equivalent{src}",
                    fmt::money(u),
                    fmt::money(full)
                ),
                Tone::Dim
            )
        );
    }
    if let (Some(v), Some(o)) = (w.visible_points, w.outside_points)
        && o >= 2.0
    {
        println!(
            "{pad}{}",
            p.paint(&format!("on this machine ≈{v:.1} pts · elsewhere (claude.ai, desktop, other computers) ≈{o:.1} pts"), Tone::Warn)
        );
    }
    if w.shares.is_empty() {
        return;
    }
    let mut t = Table::new(&[
        ("PROJECT", Align::Left),
        ("COST", Align::Right),
        ("SHARE", Align::Left),
        ("≈ LIMIT", Align::Right),
    ]);
    t.max_width = t.max_width.map(|m| m.saturating_sub(4));
    let max = w.shares.first().map(|s| s.share).unwrap_or(0.0);
    for s in w.shares.iter().take(top) {
        t.row(vec![
            Cell::new(s.key.clone()),
            Cell::new(fmt::money(s.cost)),
            Cell::tone(
                format!(
                    "{:<20} {}",
                    fmt::bar(if max > 0.0 { s.share / max } else { 0.0 }, 20),
                    fmt::pct(s.share * 100.0)
                ),
                Tone::Accent,
            ),
            Cell::tone(format!("≈{:.1} pts", s.limit_points), Tone::Bold),
        ]);
    }
    if w.shares.len() > top {
        let rest: Vec<&Share> = w.shares.iter().skip(top).collect();
        t.row(vec![
            Cell::tone(format!("… {} more", rest.len()), Tone::Dim),
            Cell::tone(fmt::money(rest.iter().map(|s| s.cost).sum()), Tone::Dim),
            Cell::new(""),
            Cell::tone(
                format!(
                    "≈{:.1} pts",
                    rest.iter().map(|s| s.limit_points).sum::<f64>()
                ),
                Tone::Dim,
            ),
        ]);
    }
    if let Some(o) = w.outside_points.filter(|o| *o >= 2.0) {
        t.row(vec![
            Cell::tone("(elsewhere)", Tone::Warn),
            Cell::tone("?", Tone::Dim),
            Cell::new(""),
            Cell::tone(format!("≈{o:.1} pts"), Tone::Warn),
        ]);
    }
    for line in t.render(p).lines() {
        println!("    {line}");
    }
}
