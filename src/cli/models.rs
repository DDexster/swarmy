//! `agentmana models`: usage statistics per model.

use std::collections::BTreeMap;

use crate::core::models::{Family, short_name};
use crate::index::query::Dim;
use crate::ui::fmt;
use crate::ui::style::Tone;
use crate::ui::table::{Align, Cell, Table};

use super::context::Ctx;
use super::report::{filter_label, local};
use super::{Format, PeriodFilterArgs};

pub fn run(ctx: &Ctx, a: &PeriodFilterArgs) -> anyhow::Result<()> {
    let (idx, _) = ctx.index()?;
    let filter = a.filter.to_filter();
    let span = a
        .period
        .span(ctx.now, ctx.limit_week(filter.account.as_deref()));
    let rows = idx.spend_by(Dim::Model, span, &filter)?;
    let total = idx.total(span, &filter)?;
    let subs = idx.model_substitutions(span, &filter)?;
    let efforts: Vec<(String, String, f64)> = idx.matrix(Dim::Model, Dim::Effort, span, &filter)?;
    if a.format == Format::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "models": rows, "total": total, "substitutions": subs, "effort_cost": efforts,
            }))?
        );
        return Ok(());
    }
    let p = &ctx.painter;
    println!(
        "{} {} {}",
        p.paint("MODELS", Tone::Header),
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
    // Effort mix per model, by cost.
    let mut mix: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for (model, effort, cost) in efforts {
        mix.entry(model).or_default().push((effort, cost));
    }
    let mut t = Table::new(&[
        ("MODEL", Align::Left),
        ("REQS", Align::Right),
        ("COST", Align::Right),
        ("SHARE", Align::Left),
        ("$/REQ", Align::Right),
        ("OUT/REQ", Align::Right),
        ("THINK", Align::Right),
        ("CACHE HIT", Align::Right),
        ("1H WRITES", Align::Right),
        ("EFFORT MIX", Align::Left),
    ]);
    t.flex_col = 9;
    for r in &rows {
        let tone = Tone::of_family(Family::of(&r.key));
        let share = if total.cost > 0.0 {
            r.cost / total.cost
        } else {
            0.0
        };
        let u = r.usage;
        let reqs = r.requests.max(1) as f64;
        let mut m = mix.remove(&r.key).unwrap_or_default();
        m.sort_by(|a, b| b.1.total_cmp(&a.1));
        let msum: f64 = m.iter().map(|x| x.1).sum();
        let mix_s = m
            .iter()
            .filter(|x| msum > 0.0 && x.1 / msum >= 0.01)
            .map(|(e, c)| format!("{e} {:.0}%", c / msum * 100.0))
            .collect::<Vec<_>>()
            .join(" · ");
        t.row(vec![
            Cell::tone(short_name(&r.key), tone),
            Cell::new(fmt::count(r.requests)),
            Cell::tone(fmt::money(r.cost), Tone::Bold),
            Cell::tone(
                format!("{:<12} {}", fmt::bar(share, 12), fmt::pct(share * 100.0)),
                tone,
            ),
            Cell::new(fmt::money(r.cost / reqs)),
            Cell::new(fmt::tokens((u.output as f64 / reqs) as u64)),
            Cell::new(if u.output > 0 {
                fmt::pct(u.thinking as f64 / u.output as f64 * 100.0)
            } else {
                "-".into()
            }),
            Cell::new(
                u.cache_hit_ratio()
                    .map(|x| fmt::pct(x * 100.0))
                    .unwrap_or_else(|| "-".into()),
            ),
            Cell::new(if u.cache_write() > 0 {
                fmt::pct(u.cache_write_1h as f64 / u.cache_write() as f64 * 100.0)
            } else {
                "-".into()
            }),
            Cell::tone(mix_s, Tone::Dim),
        ]);
    }
    t.footer(vec![
        Cell::tone("TOTAL", Tone::Bold),
        Cell::new(fmt::count(total.requests)),
        Cell::tone(fmt::money(total.cost), Tone::Accent),
    ]);
    println!("{}", t.render(p));

    // Family split as one stacked bar.
    let mut fam: BTreeMap<Family, f64> = BTreeMap::new();
    for r in &rows {
        *fam.entry(Family::of(&r.key)).or_default() += r.cost;
    }
    if total.cost > 0.0 {
        let width = 60usize;
        let mut line = String::new();
        let mut legend = Vec::new();
        for (f, c) in &fam {
            let cells = ((c / total.cost) * width as f64).round() as usize;
            line.push_str(&p.paint(&"█".repeat(cells), Tone::of_family(*f)));
            legend.push(p.paint(
                &format!("■ {} {}", f.name(), fmt::pct(c / total.cost * 100.0)),
                Tone::of_family(*f),
            ));
        }
        println!();
        println!("{line}");
        println!("{}", legend.join("  "));
    }

    if !subs.is_empty() {
        println!();
        println!("{}", p.paint("REQUESTED ≠ SERVED", Tone::Header));
        for (req, got, n) in subs.iter().take(10) {
            println!(
                "  {} → {}  {}",
                short_name(req),
                p.paint(&short_name(got), Tone::Warn),
                p.paint(&format!("{} requests", fmt::count(*n)), Tone::Dim)
            );
        }
    }
    Ok(())
}
