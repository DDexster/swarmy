//! `agentmana doctor`: what was found, what is readable, what looks off.

use chrono::Duration;

use crate::claude::registry::{self, SessionStatus};
use crate::claude::transcript::ParseStats;
use crate::core::period::Span;
use crate::index::query::Filter;
use crate::ui::fmt;
use crate::ui::style::Tone;
use crate::ui::table::{Align, Cell, Table};

use super::context::Ctx;

pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let p = &ctx.painter;
    let ok = |s: &str| format!("{} {s}", p.paint("[ OK ]", Tone::Good));
    let warn = |s: &str| format!("{} {s}", p.paint("[WARN]", Tone::Warn));
    let info = |s: &str| format!("{} {s}", p.paint("[ .. ]", Tone::Dim));

    println!(
        "{}",
        p.paint("SEATS (Claude Code config dirs)", Tone::Header)
    );
    if ctx.seats.is_empty() {
        println!(
            "{}",
            warn(
                "no Claude Code config dir found (looked for ~/.claude and ~/.claude-*; add [[seat]] to config.toml)"
            )
        );
    }
    let mut t = Table::new(&[
        ("SEAT", Align::Left),
        ("DIR", Align::Left),
        ("ACCOUNT", Align::Left),
        ("PLAN", Align::Left),
        ("LIVE", Align::Right),
        ("FOUND BY", Align::Left),
    ]);
    t.flex_col = 1;
    for (s, si) in ctx.seats.iter().zip(&ctx.seat_info) {
        let live = registry::live_sessions(&s.label, &s.sessions_dir(), &ctx.procs);
        let acct = si
            .account
            .as_ref()
            .map(|a| ctx.account_name(&a.account_uuid))
            .unwrap_or_else(|| "(not logged in)".into());
        let plan = si
            .account
            .as_ref()
            .and_then(|a| a.rate_limit_tier.clone().or(a.plan.clone()))
            .unwrap_or_default();
        t.row(vec![
            Cell::tone(&s.label, Tone::Bold),
            Cell::new(ctx.show_path(&s.dir.to_string_lossy())),
            Cell::new(acct),
            Cell::tone(plan.replace("default_claude_", ""), Tone::Dim),
            Cell::new(live.len().to_string()),
            Cell::tone(s.found_by.join(", "), Tone::Dim),
        ]);
    }
    println!("{}", t.render(p));

    // Accounts shared by several seats share one limit.
    let mut by_acct: std::collections::BTreeMap<String, Vec<&str>> = Default::default();
    for si in &ctx.seat_info {
        if let Some(a) = &si.account {
            by_acct
                .entry(a.account_uuid.clone())
                .or_default()
                .push(&si.label);
        }
    }
    println!();
    println!(
        "{}",
        ok(&format!(
            "{} seats, {} accounts",
            ctx.seats.len(),
            by_acct.len()
        ))
    );
    for (acct, seats) in &by_acct {
        if seats.len() > 1 {
            println!(
                "{}",
                info(&format!(
                    "seats {} are logged into the same account ({}): they share one limit",
                    seats.join(" + "),
                    ctx.account_name(acct)
                ))
            );
        }
    }

    println!();
    println!("{}", p.paint("LIVE SESSIONS", Tone::Header));
    let mut any = false;
    for s in &ctx.seats {
        for l in registry::live_sessions(&s.label, &s.sessions_dir(), &ctx.procs) {
            any = true;
            let (st, tone) = match &l.status {
                SessionStatus::Busy => ("busy".to_string(), Tone::Good),
                SessionStatus::Idle => ("idle".to_string(), Tone::Dim),
                SessionStatus::Waiting(w) => (format!("NEEDS YOU: {w}"), Tone::Warn),
                SessionStatus::Other(o) => (o.clone(), Tone::Plain),
            };
            println!(
                "  {} {:<16} {:<10} {} {}",
                p.paint("●", tone),
                l.name
                    .clone()
                    .unwrap_or_else(|| l.session_id[..8].to_string()),
                s.label,
                p.paint(&st, tone),
                p.paint(
                    &format!(
                        "pid {} · {} · v{}",
                        l.pid,
                        ctx.show_path(&l.cwd),
                        l.version.clone().unwrap_or_default()
                    ),
                    Tone::Dim
                )
            );
        }
    }
    if !any {
        println!("  {}", p.paint("none", Tone::Dim));
    }
    let unregistered = ctx
        .procs
        .claude
        .iter()
        .filter(|c| c.config_dir.is_none() && !ctx.seats.iter().any(|s| s.is_default))
        .count();
    if unregistered > 0 {
        println!(
            "{}",
            warn(&format!(
                "{unregistered} claude processes use the default dir, which was not found"
            ))
        );
    }

    println!();
    println!("{}", p.paint("INDEX", Tone::Header));
    let (idx, stats) = ctx.index()?;
    println!(
        "{}",
        ok(&format!(
            "{} ({})",
            ctx.show_path(&ctx.dirs.index_file().to_string_lossy()),
            human_bytes(
                std::fs::metadata(ctx.dirs.index_file())
                    .map(|m| m.len())
                    .unwrap_or(0)
            )
        ))
    );
    println!(
        "{}",
        ok(&format!(
            "{} transcripts · {} requests · {} tool calls · {} sessions · {} agents",
            fmt::count(stats.files_total as u64),
            fmt::count(idx.count("requests")?),
            fmt::count(idx.count("tool_calls")?),
            fmt::count(idx.count("sessions")?),
            fmt::count(idx.count("agents")?)
        ))
    );
    println!(
        "{}",
        info(&format!(
            "this run read {} files, {:.1} MB in {:.2}s",
            stats.files_read,
            stats.bytes_read as f64 / 1e6,
            stats.elapsed.as_secs_f64()
        ))
    );
    let mut ps = ParseStats::default();
    for (t, n) in idx.record_types()? {
        ps.by_type.insert(t, n);
    }
    let unknown = ps.unknown_types();
    if unknown.is_empty() {
        println!("{}", ok("every transcript record type is recognised"));
    } else {
        let list: Vec<String> = unknown.iter().map(|(t, n)| format!("{t} ({n})")).collect();
        println!(
            "{}",
            warn(&format!(
                "unrecognised record types (Claude Code format moved on?): {}",
                list.join(", ")
            ))
        );
    }
    let week = Span {
        start: ctx.now - Duration::days(7),
        end: ctx.now,
    };
    let total = idx.total(week, &Filter::default())?;
    if total.unpriced > 0 {
        println!(
            "{}",
            warn(&format!(
                "{} requests in the last 7 days have no price (see `agentmana prices`)",
                total.unpriced
            ))
        );
    }
    let (n, theirs, ours) = idx.cost_crosscheck(week)?;
    if n > 0 && theirs > 0.0 {
        let diff = (ours - theirs) / theirs * 100.0;
        let line = format!(
            "cost cross-check, {n} sessions of the last 7 days: Claude Code counted {}, transcripts give {} ({:+.1}%; background calls such as titles are not in transcripts)",
            fmt::money(theirs),
            fmt::money(ours),
            diff
        );
        println!(
            "{}",
            if diff.abs() <= 5.0 {
                ok(&line)
            } else {
                warn(&line)
            }
        );
    }
    let errs = idx.api_errors(week)?;
    if !errs.is_empty() {
        let list: Vec<String> = errs.iter().map(|(k, n)| format!("{k} ×{n}")).collect();
        println!(
            "{}",
            info(&format!(
                "API errors in the last 7 days: {}",
                list.join(", ")
            ))
        );
    }

    println!();
    println!("{}", p.paint("LIMIT READINGS", Tone::Header));
    let readings = ctx.latest_readings();
    if readings.is_empty() {
        println!(
            "{}",
            warn("no cached utilization in any .claude.json (open /usage in Claude Code once)")
        );
    }
    for (acct, rs) in readings {
        for r in rs {
            println!(
                "{}",
                ok(&format!(
                    "{} {} {} · resets {} · reading {} old",
                    ctx.account_name(&acct),
                    r.kind.label(),
                    fmt::pct(r.utilization),
                    r.resets_at
                        .with_timezone(&chrono::Local)
                        .format("%a %d %b %H:%M"),
                    fmt::duration(ctx.now - r.fetched_at)
                ))
            );
        }
    }
    println!();
    println!("{}", p.paint("CONFIG", Tone::Header));
    let cfg = ctx.dirs.config_file();
    println!(
        "{}",
        if cfg.exists() {
            ok(&ctx.show_path(&cfg.to_string_lossy()))
        } else {
            info(&format!(
                "{} (not present; defaults in use)",
                ctx.show_path(&cfg.to_string_lossy())
            ))
        }
    );
    Ok(())
}

pub fn human_bytes(b: u64) -> String {
    let f = b as f64;
    if b >= 1 << 30 {
        format!("{:.1} GB", f / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", f / (1u64 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.0} KB", f / 1024.0)
    } else {
        format!("{b} B")
    }
}
