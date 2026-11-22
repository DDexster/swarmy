//! Architecture checks over the source text: which top-level modules may name which, and
//! counters that may only go down (ratchets). When a check fails because something got
//! *better*, lower the number in `tests/arch/` — that is the point.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn src_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            src_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The top-level module a file belongs to (`src/index/query.rs` → `index`, `src/fleet.rs` →
/// `fleet`, `src/main.rs` → `main`).
fn module_of(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap();
    let first = rel
        .components()
        .next()
        .unwrap()
        .as_os_str()
        .to_string_lossy()
        .to_string();
    first.trim_end_matches(".rs").to_string()
}

/// The file without its `#[cfg(test)] mod tests` (by convention at the end).
fn production(text: &str) -> &str {
    match text.find("#[cfg(test)]\nmod tests") {
        Some(i) => &text[..i],
        None => text,
    }
}

fn files() -> (PathBuf, Vec<(PathBuf, String)>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut v = Vec::new();
    src_files(&root, &mut v);
    v.sort();
    let texts = v.into_iter().map(|p| {
        let t = std::fs::read_to_string(&p).unwrap();
        (p, t)
    });
    (root.clone(), texts.collect())
}

fn read_list(name: &str) -> BTreeSet<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/arch")
        .join(name);
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

#[test]
fn module_edges_only_shrink() {
    let (root, files) = files();
    let modules: BTreeSet<String> = files.iter().map(|(p, _)| module_of(&root, p)).collect();
    let mut edges: BTreeSet<String> = BTreeSet::new();
    for (p, text) in &files {
        let from = module_of(&root, p);
        for m in &modules {
            if *m != from && m != "main" && production(text).contains(&format!("crate::{m}::")) {
                edges.insert(format!("{from} -> {m}"));
            }
        }
    }
    let allowed = read_list("edges.txt");
    let new: Vec<&String> = edges.difference(&allowed).collect();
    let gone: Vec<&String> = allowed.difference(&edges).collect();
    assert!(
        new.is_empty(),
        "new dependencies between modules (not in tests/arch/edges.txt): {new:?}"
    );
    assert!(
        gone.is_empty(),
        "these dependencies are gone, remove them from tests/arch/edges.txt: {gone:?}"
    );
}

/// What the ratchet counts, in production code only.
fn counts() -> BTreeMap<&'static str, usize> {
    let (root, files) = files();
    let mut c: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (p, text) in &files {
        let m = module_of(&root, p);
        let prod = production(text);
        let n = |pat: &str| prod.matches(pat).count();
        // Silent fallbacks where errors decide who pays and what is saved.
        if matches!(m.as_str(), "index" | "vault" | "app" | "cli" | "limits") {
            *c.entry("fallbacks_index_vault_app_cli_limits").or_default() +=
                n("unwrap_or_default()") + n(".ok()") + n("let _ =") + n("unwrap_or(0");
        }
        // Limit thresholds written again outside `core::limits::Level` (review M1).
        if m != "core" {
            *c.entry("limit_threshold_copies").or_default() +=
                n(">= 85.0") + n(">= 60.0") + n("estimate_now >= 100.0") + n("pct >= 100.0");
        }
        // Intervals belong in the dashboard's job table (`tui/jobs.rs`, review M10).
        if m == "tui" && !p.ends_with("tui/jobs.rs") {
            *c.entry("tui_duration_literals_outside_jobs").or_default() +=
                n("Duration::from_secs(") + n("Duration::from_millis(") + n(".as_secs() <");
        }
        *c.entry("unwraps").or_default() += n(".unwrap()") + n(".expect(");
        *c.entry("wildcard_arms").or_default() += n("_ =>");
        if prod.lines().count() > 900 {
            *c.entry("files_over_900_lines").or_default() += 1;
        }
    }
    c
}

#[test]
fn ratchets_only_go_down() {
    let limits: BTreeMap<String, usize> = read_list("ratchets.txt")
        .into_iter()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().parse().ok()?))
        })
        .collect();
    let now = counts();
    let mut errors = Vec::new();
    for (k, v) in &now {
        match limits.get(*k) {
            None => errors.push(format!("{k} = {v} is not in tests/arch/ratchets.txt")),
            Some(max) if v > max => errors.push(format!("{k}: {v} > {max} (it may only go down)")),
            Some(max) if v < max => errors.push(format!(
                "{k}: {v} < {max} — good, lower it to {v} in tests/arch/ratchets.txt"
            )),
            Some(_) => {}
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// `core` is the pure domain: no clock, no files, no database (review M2). Time comes in as an
/// argument, so every rule there is testable with fixed numbers.
#[test]
fn core_is_pure() {
    let (root, files) = files();
    let mut found = Vec::new();
    for (p, text) in &files {
        if module_of(&root, p) != "core" {
            continue;
        }
        for pat in ["Utc::now", "Local::now", "std::fs", "rusqlite", "std::env"] {
            if production(text).contains(pat) {
                found.push(format!(
                    "{}: {pat}",
                    p.strip_prefix(&root).unwrap().display()
                ));
            }
        }
    }
    assert!(found.is_empty(), "core must stay pure: {found:?}");
}

/// The index owns its schema: no SQL and no `rusqlite` outside `src/index/` (review M4). Other
/// modules read through typed methods (`index::live`, `index::query`, …).
#[test]
fn sql_stays_in_the_index() {
    let (root, files) = files();
    let found: Vec<String> = files
        .iter()
        .filter(|(p, _)| module_of(&root, p) != "index")
        .filter(|(_, t)| t.contains("SELECT ") || t.contains("rusqlite"))
        .map(|(p, _)| p.strip_prefix(&root).unwrap().display().to_string())
        .collect();
    assert!(found.is_empty(), "SQL outside src/index/: {found:?}");
}
