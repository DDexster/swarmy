# Backend Architecture Review

> Analyzed on: 2026-11-22
> Project: swarmy (repository root)
> Stack: Rust 2024, single binary crate; clap CLI + ratatui TUI; SQLite via rusqlite (WAL); JSON files (vault, status line, Claude Code's own files)
> Modules analyzed: 12 (`core`, `claude`, `index`, `live`, `vault`, `cli`, `tui`, `ui`, `fleet`, `config`, `demo`, `power`) · Findings: 21 (High 5, Medium 11, Low 5)

## Summary

The domain kernel is the best part of the codebase: `src/core` is pure, its enums are closed, and its money and limit rules are tested with numbers computed by hand in comments. The problems sit around it. The highest-risk one is the vault. It promises never to lose a token, but only `switch` takes the lock. Two threads of the same dashboard, and every other `swarmy` process, do unlocked read-modify-write of `logins.json` and `secrets.json`, and threads of one process share a temp file name. Next comes integrity of the index: facts that cannot be rebuilt are dropped on every schema bump, and a `-1` cost sentinel is silently summed as $0. Structurally, the limit estimation that the dashboard, the CLI and the status line all depend on lives in `src/cli/limits.rs` and has no tests. That gives a `tui ↔ cli` cycle, and its rules are copied, with drift, into the views. Nothing is enforced by tooling: there is no CI, no lint configuration, and `cargo clippy` already reports 40 warnings.

## System map

| Area | Role (layer) | Size (lines) | Notes |
|---|---|---|---|
| `core/` | domain kernel: usage, pricing, periods, limits math | 1 107 | pure; reads `Local` time zone only |
| `claude/` | anti-corruption layer: readers of Claude Code files; also the status line renderer/writer | 3 297 | `statusline.rs` (928) renders ANSI and writes files |
| `index/` | persistence: SQLite cache, incremental refresh, queries, insights | 1 975 | `pub conn` |
| `live/` | live agent states (application/read model) | 996 | 20 raw SQL `SELECT`s on index tables |
| `vault/` | login vault: the only writer into Claude Code dirs | 1 015 | |
| `cli/` | commands **and** shared application services (`Ctx`, limits analysis) | 2 887 | |
| `tui/` | worker thread (use cases) + UI | 7 827 | 38% of the code |
| `ui/` | text formatting, tables, ANSI | 512 | |
| `fleet.rs`, `config.rs`, `demo.rs`, `power.rs` | status-line contract, config, demo world, battery | 901 | |

There is no HTTP layer. The entry points are the clap subcommands in `cli/mod.rs`, plus the dashboard worker loop (`tui/mod.rs::worker`, 368 lines) and the `statusline` command that Claude Code runs after each response.

**Dependency matrix** (row imports column; counts of `crate::` references, measured with `grep -oE 'crate::[a-z_]+'`):

| from \ to | core | claude | index | live | vault | cli | tui | ui | fleet | config | demo |
|---|---|---|---|---|---|---|---|---|---|---|---|
| core | | | | | | | | | | | |
| claude | 3 | | | | | | | | **6** | **3** | |
| index | 7 | 5 | | | | | | | | | |
| live | 2 | 5 | 1 | | | | | **2** | | | |
| vault | | 5 | | | | | | | | | |
| cli | 11 | 21 | 7 | | 4 | | 3 | 22 | 2 | 6 | 1 |
| tui | 10 | 13 | 6 | 10 | 6 | **6** | | 10 | 6 | | |
| ui | 1 | | | | | | | | | | |
| config | 1 | **1** | | | | | | | | | |
| demo | | 2 | | | | | **1** | | | | |

Forbidden edges (bold): `tui → cli` closes the cycle `cli ↔ tui` (and `cli → demo → tui → cli`). `claude ↔ config` is a second cycle. The reader layer also points upward: `claude → fleet` and `claude → config`. `live → ui` puts presentation into a read model, and `demo → tui` exists only to borrow a hash function.

## Scorecard

| Requirement | Score (1-5) | Evidence |
|---|---|---|
| Cohesion and coupling | 2 | two cycles; limits domain logic in `cli/`; limit rules copied into 5 files (H4, M1) |
| Modularity | 3 | areas grouped by concept; but binary-only crate, no facades, `pub` everywhere |
| Layers and ownership | 2 | reader layer renders and writes; read model formats money; no ownership data (L2) |
| DDD (boundaries, language, pure domain, thin use cases) | 3 | pure `core`, ACL in `claude/`; "mana" vs "limits" drift; fat worker and `analyze` (M2, M10, L1) |
| Value objects | 2 | 99 id-like `String`/`&str` fields and params, 23 money `f64` fields, ambiguous account keys (M6) |
| Invariants and failure modes | 2 | `-1` sentinel, no `CHECK`s, 233 silent-fallback sites in non-test code (H3, H5, M8) |
| API surface and integrity | 3 | SQL fragments are all static and every value is bound; but `pub conn`, `count(&str)` (M4, L4) |
| State-change pipeline | 2 | vault writes unlocked except `switch`; reprice split across two transactions (H1, H3) |
| Declarations and duplication | 2 | rate defaults ×3, window roll-forward ×3, "today" ×3, atomic write ×4, tool names ×3 (M1, M5, M7, L5) |
| Tests as specification | 2 | 54 tests; `core` exemplary; 0 tests for index refresh, limits analysis, attention, vault concurrency (M9) |
| Enforcement by mechanism | 1 | no CI, no `[lints]`, 40 clippy warnings, no dependency check (M11) |
| **Overall** | **2** | strong kernel, weak guarantees around it |

## Findings

### H1: Vault writes are not serialized; only `switch` holds the lock

**Severity**: High
**Requirement**: 9. One pipeline for every state change (also 4: capabilities by type)
**Location**: `src/vault/mod.rs:135` (`save`) and `:383`, `:440-452`, `:266-267` (+ unlocked writers: `sync`, `put`, `capture`, `observe`, `rename`, `remove`). Callers: `src/tui/mod.rs:863-864` (worker thread, every analytics pass), `src/tui/mod.rs:437-444` (worker, on every login-file change and every 20 s), `src/tui/vault_ui.rs:70,145,189,358,403,446` and `src/tui/app.rs:1378` (UI thread), `src/cli/context.rs:87-103` (every CLI command). `src/vault/store.rs:47-59,70`.

`Vault::open` loads `logins.json` into memory, and every `save()` writes that whole copy back. The only `Lock::acquire` call is in `switch`. In the dashboard, the worker thread reloads seat info as soon as a login file changes. It does exactly that while the UI thread is in the middle of a switch (the credentials file was written in step 3, the bookkeeping is saved in step 5). The worker then saves its stale copy, which undoes the switch's `mounted` bookkeeping or drops the auto-saved login. `FileStore::put` is also an unlocked load-modify-write of `secrets.json`, so two writers can drop each other's secret. Both threads write through the same temp path (`.{name}.swarmy-{pid}.tmp`, with `truncate(true)`), so concurrent writes can interleave bytes and leave `secrets.json` unparseable. Nothing backs up the vault's own files, so a damaged `secrets.json` loses every saved token.

#### Current

```rust
// src/tui/mod.rs:863 — worker thread, no lock; seat_states → sync/observe → save()
if let Ok(mut v) = Vault::open(&ctx.dirs.vault_dir()) {
    states = v.seat_states(&ctx.seats).unwrap_or_default();
    vault_logins = v.logins().to_vec();
}
// src/vault/store.rs:47 — read-modify-write of every secret, unlocked
fn put(&self, id: &str, secret: &Secret) -> anyhow::Result<()> {
    let mut m = self.load()?;
    m.insert(id.to_string(), secret.expose().clone());
    atomic_write(&self.path, serde_json::to_string(&m)?.as_bytes())
}
// src/vault/store.rs:70 — same temp name for every thread of the process
let tmp = dir.join(format!(".{name}.swarmy-{}.tmp", std::process::id()));
```

#### Recommended

```rust
/// The only way to change the vault: lock, re-read under the lock, change, save, unlock.
pub fn edit<T>(dir: &Path, f: impl FnOnce(&mut VaultTx<'_>) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let lock = std::fs::File::create(dir.join("lock"))?;
    lock.lock()?;                                  // std flock (Rust 1.89): released by the kernel on death
    let mut v = Vault::open_unlocked(dir)?;        // never a copy read before the lock
    let out = f(&mut VaultTx { v: &mut v, _lock: &lock })?;
    v.backup_own_files()?;                         // keep the last N logins/secrets.json
    v.save()?;
    Ok(out)
}
// `sync`, `put`, `capture`, `switch`, `rename`, `remove`, `observe` move to `impl VaultTx`;
// `Vault` itself keeps only reads. Temp names: `.{name}.{pid}.{seq}.tmp` with an AtomicU64 seq.
```

**Lock it in**: the type system. Every write method lives on `VaultTx`, which only `edit` can construct (private field). Add a test that runs `capture`/`sync` from 8 threads for 500 iterations against temp seats and asserts that `logins.json` and `secrets.json` parse and that every login has its secret.

---

### H2: Facts that cannot be rebuilt are dropped on every schema bump

**Severity**: High
**Requirement**: 1. Committed facts are never rewritten (also 3: invariants)
**Location**: `src/index/schema.rs:174-188` (`TABLES`), `src/index/mod.rs:130-137`; facts fed only from live snapshots at `src/index/mod.rs:396-452`.

According to `schema.rs`, the index is "a cache of what the transcripts say". But `seat_accounts`, which records which account each seat held and since when, is observed only from the current `.claude.json` and cannot be rebuilt from transcripts. The same holds for `accounts` (emails and tiers of parked logins). Both are in `TABLES` and are dropped when `VERSION` changes. After the next bump, every request made before a login swap falls back to the stale `session-owner` records. Those records are wrong after a swap, by the project's own CLAUDE.md. Past spend therefore moves to the wrong account, and the per-account `$/point` calibration and the limit attribution change with it. `readings` was correctly kept out of `TABLES`, which shows the rule is known but only kept by hand.

#### Current

```rust
// src/index/schema.rs:174
pub const TABLES: &[&str] = &[
    "files", "requests", "tool_calls", "sessions", "agents", "owners",
    "seat_accounts", "projects", "accounts", "prompts", "api_errors", "cost_states", "record_types",
];
// src/index/mod.rs:130
if idx.meta("schema_version")?.as_deref() != Some(schema::VERSION) {
    for t in schema::TABLES {
        idx.conn.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
    }
```

#### Recommended

```rust
/// Derived from transcripts: dropped and rebuilt on a schema bump.
pub const CACHE_TABLES: &[&str] = &["files", "requests", "tool_calls", "sessions", "agents",
    "owners", "projects", "prompts", "api_errors", "cost_states", "record_types"];
/// Observed over time from `.claude.json`: migrated with `ALTER TABLE`, never dropped.
pub const FACT_TABLES: &[&str] = &["meta", "readings", "seat_accounts", "accounts"];
```

**Lock it in**: a test that parses every `CREATE TABLE` name in `schema::SQL` and asserts that each one is in exactly one of the two lists. A second test fills `seat_accounts`, opens the index with a different `VERSION`, and asserts the rows survive.

---

### H3: The `cost = -1` sentinel is summed as $0, unflagged, and memoized for the life of the process

**Severity**: High
**Requirement**: 3. Invariants; nothing fails silently
**Location**: `src/index/schema.rs:43`, `src/index/mod.rs:592-596` (reset outside the pricing transaction), `src/index/query.rs:186-187` (`SUM(MAX(r.cost, 0))`, + 9 copies at `query.rs:305,422` and `live/mod.rs:283,292,530,554,584,599,673`), `src/cli/limits.rs:87-114` (memo).

`price_all` sets every row to `-1` in an autocommit statement and reprices in a later transaction. Until that commit, any other process (the dashboard, `swarmy report`) sums the unpriced rows as $0. `priced` stays `1` during that time, so the "unpriced" counter does not show the gap. The dashboard memoizes "settled" spans in `total_cached`. A partial total read during a reprice therefore becomes a `$/point` calibration sample for the rest of the session. The memo key also ignores the price fingerprint, so a legitimate reprice never reaches it.

#### Current

```rust
// src/index/mod.rs:592
pub fn price_all(&mut self, prices: &PriceBook) -> anyhow::Result<()> {
    let fp = prices.fingerprint();
    if self.meta("price_fp")?.as_deref() != Some(fp.as_str()) {
        self.conn.execute("UPDATE requests SET cost = -1", [])?;   // committed on its own
    }
    let tx = self.conn.transaction()?;
// src/index/query.rs:186
const SUMS: &str = "COUNT(*), SUM(r.input), ..., SUM(MAX(r.cost, 0)), SUM(1 - r.priced)";
```

#### Recommended

```rust
// schema: NULL = not priced yet; the database refuses anything else below zero
// cost REAL CHECK (cost IS NULL OR cost >= 0),
const SUMS: &str = "COUNT(*), ..., SUM(r.cost), SUM(1 - r.priced), SUM(r.cost IS NULL)";

pub fn price_all(&mut self, prices: &PriceBook) -> anyhow::Result<()> {
    let fp = prices.fingerprint();
    let tx = self.conn.transaction()?;                 // reset + reprice + fp: one commit
    if meta_in(&tx, "price_fp")?.as_deref() != Some(fp.as_str()) {
        tx.execute("UPDATE requests SET cost = NULL", [])?;
    }
    /* price rows WHERE cost IS NULL */
    set_meta_in(&tx, "price_fp", &fp)?;
    tx.commit()?;
    Ok(())
}
```

Key the `total_cached` memo by `(price_fp, account, span)`, or drop it, and show `pending > 0` in the totals as "≈ repricing".

**Lock it in**: the `CHECK` constraint, plus a test that opens two connections to one file, reprices on one, and asserts the other never sees a total below the last committed total.

---

### H4: Limit estimation lives in the CLI, and the dashboard depends on it (cycle `tui ↔ cli`)

**Severity**: High
**Requirement**: 5. Maximum cohesion, minimum coupling (also 7: layers)
**Location**: `src/tui/mod.rs:27-28,442`, `src/tui/view/panels.rs:220,1010,2102` (+ `cli/mod.rs:189,228,230,245` for the other direction; `demo.rs:20 → tui::boot::hash`; `config.rs:11 ↔ claude/statusline.rs:664-685`).

`cli/limits.rs` holds the core of the product's "where did my week go": the reference account per tier, visible vs elsewhere points, burn, ETA, and the window curve. `cli/context.rs` holds the composition root `Ctx`. The TUI imports both, plus `cli::doctor::human_bytes`, while `cli` imports `tui` to launch it. A change to how a command prints can now break the dashboard, and the estimation cannot be reused or tested without the presentation module.

#### Current

```rust
// src/tui/mod.rs:27
use crate::cli::context::Ctx;
use crate::cli::limits::{self, AccountReport};
// src/tui/view/panels.rs:1010
let wins: Vec<(usize, &crate::cli::limits::WindowReport)> = app
// src/tui/view/panels.rs:220
crate::cli::doctor::human_bytes(a.memory)
```

#### Recommended

```text
src/app/context.rs      Ctx (composition root; used by cli and tui)
src/limits/report.rs    AccountReport, WindowReport, all_reports(), analyze()   (index + core::limits)
src/core/limits.rs      + the pure decisions now inside analyze() (see M2)
src/cli/limits.rs       print_window() and run() only
src/ui/fmt.rs           + human_bytes
```

```rust
// src/tui/mod.rs
use crate::app::context::Ctx;
use crate::limits::report::{self, AccountReport};
```

**Lock it in**: a test (`tests/deps.rs` once there is a lib target, or a `#[test]` that scans `src/`) that reads every `crate::<area>` reference and fails on any edge outside the allowed matrix. In particular: `tui`/`cli` may not import each other, `core` and `claude` import nothing above them, and `live` does not import `ui`. Start with today's edges as a ratchet.

---

### H5: Silent fallbacks on the account and limit path turn errors into wrong attribution

**Severity**: High
**Requirement**: 3. Nothing fails silently
**Location**: 233 sites in non-test code: 80 `unwrap_or_default()`, 51 `.ok()`, 36 `let _ =`, 66 `unwrap_or(0…/false)`. By area: `tui` 60, `claude` 56, `live` 27, `index` 26, `cli` 24, `vault` 16, `ui` 3, `core` 2. Worst: `src/cli/context.rs:87,103,128,306`, `src/vault/mod.rs:106,383`, `src/index/mod.rs:433-438,463-470,502`, `src/tui/mod.rs:864`.

These are not cosmetic fallbacks. If the vault cannot be opened, `drop_unconfirmed` returns without dropping the stale profile of an unconfirmed seat. The seat is then merged into the wrong account and borrows its limit reading, which is the exact case the function exists to prevent. A damaged `.claude.json` becomes an empty one, which silently removes the seat's account, readings and Claude Code prices. In `post_pass`, `.ok()` treats any SQL error as "no previous account" and inserts a login change, which then re-attributes requests.

#### Current

```rust
// src/cli/context.rs:86
pub fn drop_unconfirmed(&mut self) {
    let Ok(mut v) = crate::vault::Vault::open(&self.dirs.vault_dir()) else {
        return;
    };
    // ...
    let Ok(states) = v.seat_states(&self.seats) else {
        return;
    };
// src/index/mod.rs:433
let before: Option<String> = tx
    .query_row("SELECT account_uuid FROM seat_accounts WHERE seat = ?1 AND ts <= ?2 ORDER BY ts DESC LIMIT 1",
        params![si.label, ts], |r| r.get(0))
    .ok();
```

#### Recommended

```rust
pub fn drop_unconfirmed(&mut self) -> anyhow::Result<()> {
    let mut v = crate::vault::Vault::open(&self.dirs.vault_dir())
        .context("cannot read the vault: seats with a new login cannot be confirmed")?;
    let states = v.seat_states(&self.seats)?;
    // ...
    Ok(())
}
// index: `.optional()?` keeps "no row" apart from "SQL failed"
let before: Option<String> = tx.query_row(SQL_SEAT_ACCOUNT_AT, params![si.label, ts], |r| r.get(0))
    .optional()?;
```

The dashboard shows a failed read as an ATTENTION line ("vault unreadable"), and the CLI reports it in `doctor`. It never falls back to an empty value.

**Lock it in**: workspace lints `clippy::unwrap_used`, `clippy::let_underscore_must_use`, `clippy::map_unwrap_or`, plus a counted ratchet for `unwrap_or_default()`/`.ok()` in `src/{index,vault,cli,limits}` that may only go down. Each remaining site carries `#[expect(..., reason = "...")]` or a comment saying why the default is correct.

---

### M1: Limit rules are copied, with drift, into five files

**Severity**: Medium
**Requirement**: 5. One business rule = one place (also 8)
**Location**: "limit reached" (`>= 100.0`) ×6: `core/limits.rs:177`, `cli/limits.rs` via `eta`, `tui/app.rs:527`, `tui/view/panels.rs:819,1175`, `tui/view/mod.rs:354`, `claude/statusline.rs:443`. Utilization level 85/60 ×3: `ui/style.rs:40-48`, `tui/theme.rs:189-197`, `claude/statusline.rs:270-278` (+ `panels.rs:779`). Window roll-forward ×3: `cli/limits.rs:73-82`, `cli/context.rs:273-280`, `cli/accounts.rs:111-116`. Reset detection ×2: `core/limits.rs:88-112` and `tui/app.rs:879`. Projection to reset only in the view: `panels.rs:1045-1052`.

The copies already disagree. `core::limits::effective_start` calls a fall of more than 2 points a reset, while the event log calls it a reset only when the reading goes from ≥50% to <5%, so a reset from 40% is never logged. `swarmy accounts` shows the last raw reading (`accounts.rs::weekly`), but the dashboard and the status line show the estimate. The "≈X at reset" projection exists only in the chart.

#### Current

```rust
// src/claude/statusline.rs:270            // src/tui/theme.rs:189
fn level(pct: f64) -> &'static str {        pub fn utilization(&self, pct: f64) -> Color {
    if pct >= 85.0 { "1;31" }                   if pct >= 85.0 { self.bad }
    else if pct >= 60.0 { "33" }                else if pct >= 60.0 { self.warn }
    else { "32" }                               else { self.good }
}                                           }
// src/tui/app.rs:879 — a second, different reset rule
if p >= 50.0 && w.estimate_now < 5.0 {
    self.log_event("◆", format!("LIMIT RESET · {} {}", rep.name, w.reading.kind.label()), Tone::Good);
```

#### Recommended

```rust
// src/core/limits.rs — declared once, mapped to colours by each front end
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level { Ok, Warn, Bad, Reached }
impl Level {
    pub fn of(pct: f64) -> Level {
        match pct { p if p >= 100.0 => Level::Reached, p if p >= 85.0 => Level::Bad,
                    p if p >= 60.0 => Level::Warn, _ => Level::Ok }
    }
}
pub fn current_window(r: &Reading, now: DateTime<Utc>) -> Span { /* the one roll-forward */ }
pub fn projection_at_reset(w: &WindowReport, now: DateTime<Utc>) -> Option<f64> { /* ... */ }
// tui/app.rs: a reset is `WindowReport::manual_reset` changing, not a second threshold.
```

**Lock it in**: a duplicate-predicate check in the test suite: `grep -rE '>= ?(85|60|100)\.0'` outside `core/limits.rs` must return nothing (with a ratchet), plus hand-computed tests for `Level::of` boundaries.

---

### M2: `analyze` interleaves six queries with untested estimation rules

**Severity**: Medium
**Requirement**: 1. Domain logic is pure; use cases are thin (also 10)
**Location**: `src/cli/limits.rs:157-314` (159 lines; + `own_usd_per_point` `:116-150`, `total_cached` `:87-114` reading `Utc::now()`, masking applied inside attribution at `:240-244`).

The tier reference rule (another account counts as the reference only if its ratio is more than 1.2 times this account's own), the visible/outside split, the ETA cut-off at the window end and the curve scaling are all business decisions. They sit between `idx.total(...)` calls, so the only way to test them is with a full index, and none of them is tested. `readings(account, kind)` is loaded three times per window. Masking (`ctx.show_path`) changes the attribution keys, so two projects with the same last path component merge into one share when `--mask` is on.

#### Current

```rust
// src/cli/limits.rs:181
let own = own_usd_per_point(idx, account, r)?;
let reference = tier
    .as_ref()
    .and_then(|t| refs.get(&(t.clone(), r.kind)))
    .filter(|x| x.account != account && own.is_none_or(|o| x.usd_per_point > o * 1.2));
let usd_per_point = reference.map(|x| x.usd_per_point).or(own);
let (base, since) = if reset_passed { (0.0, window.start) } else { (r.utilization, r.fetched_at) };
let spent_since = idx.total(Span { start: since, end: now_end }, &filter)?.cost;
let estimate_now = limits::estimate_now(base, spent_since, usd_per_point);
```

#### Recommended

```rust
// src/limits/report.rs — loader: queries only
let inputs = WindowInputs::load(idx, account, r, ctx.now)?;   // history, spend since, in window, burn, by project
// src/core/limits.rs — decision: pure, testable with numbers
pub fn window_estimate(i: &WindowInputs, reference: Option<&Reference>, now: DateTime<Utc>) -> WindowEstimate {
    let usd_per_point = choose_ratio(i.own_usd_per_point, reference);   // the 1.2 rule, named
    let estimate_now = estimate_now(i.base(), i.spent_since, usd_per_point);
    /* visible / outside / eta / shares */
}
// masking happens when printing, never on attribution keys
```

**Lock it in**: hand-computed tests for `choose_ratio` (own only, reference wins, reference within 20% loses) and for `window_estimate` (reset passed, manual reset, nothing spent). Add a purity check that `core/` contains no `Utc::now`, `Local::now`, `std::fs` or `rusqlite`.

---

### M3: `panels.rs` (2 772 lines) and `app.rs` (1 396) mix drawing, state and business rules

**Severity**: Medium
**Requirement**: 5. Cohesion (also 6: modularity)
**Location**: `src/tui/view/panels.rs` (26 functions; `swarm` 358 lines, `mana` 304, `efficiency_sections` 289, `history` 227, `agent` 210, `models` 203, `mana_curve` 198), `src/tui/app.rs` (`App` with 36 `pub` fields; `attention` 178 lines at `:454-631`, `limit_alerts` `:851-894`).

One file holds ten unrelated panels, and every panel takes `&mut App`. `App::attention` decides severities, thresholds (6 h, 85%), parked-login exceptions and texts, and it is not tested. When the alert rules change, the person changing them has to work inside the key-handling god object.

#### Current

```rust
// src/tui/app.rs:527
if w.estimate_now >= 100.0 && !w.reset_passed {
    out.push(Attn { severity: if parked { 8 } else { 1 }, glyph: "⊘",
        tone: if parked { Tone::Info } else { Tone::Bad }, /* text … */ });
} else if let Some(eta) = w.eta.filter(|d| d.num_hours() < 6) {
    out.push(Attn { severity: 1, glyph: "⊘", tone: Tone::Warn, /* … */ });
} else if w.estimate_now >= 85.0 {
    out.push(Attn { severity: 6, glyph: "⊘", tone: Tone::Warn, /* … */ });
}
```

#### Recommended

```text
src/alerts.rs                     pub fn alerts(a: &Analytics, seats: &[SeatRow], now) -> Vec<Alert>  (pure, tested)
src/tui/view/panels/mod.rs        dispatch only
src/tui/view/panels/{swarm,limits,burn,attention,agent,projects,models,history,efficiency,system}.rs
src/tui/app/{state,keys,mouse}.rs
```

```rust
pub fn attention(&self) -> Vec<Attn> {
    crate::alerts::alerts(&self.analytics, &self.analytics.seats, self.now).into_iter().map(Attn::from).collect()
}
```

**Lock it in**: a file-size test (no `.rs` over 900 lines, with today's offenders listed as a ratchet) and table tests for `alerts()` with hand-written readings.

---

### M4: The index leaks its connection; `live` queries index tables directly

**Severity**: Medium
**Requirement**: 4. Minimal API surface; reads cannot write
**Location**: `src/index/mod.rs:35` (`pub conn: Connection`); `src/live/mod.rs` (20 SQL literals, 5 `.conn` uses; `build` is 472 lines at `:190`).

Any module can `execute` on the cache through `idx.conn`, and the index schema is known by two modules. In `live::build`, the agent-state rule (main thread Gone/NeedsYou/Tool/Thinking/Idle; subagent Done/Stalled/Tool/Thinking) is buried among SQL. It is the most user-visible rule of the dashboard, and it has no direct test.

#### Current

```rust
// src/index/mod.rs:34
pub struct Index {
    pub conn: Connection,
// src/live/mod.rs:202
let conn = &idx.conn;
// ...
let mut st = conn.prepare("SELECT a.seat, a.session_id, a.agent_id, a.kind, a.agent_type, ... FROM agents a LEFT JOIN sessions s ...")?;
```

#### Recommended

```rust
pub struct Index { conn: Connection, /* … */ }
impl Index {
    /// Rows the live view needs, typed; the SQL stays in `index/`.
    pub fn live_agents(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<AgentRow>> { /* … */ }
}
// src/live/state.rs — pure
pub fn agent_state(row: &AgentRow, session: Option<&LiveSession>, tool: Option<&ToolRow>,
                   now: DateTime<Utc>, opts: &Options) -> (State, DateTime<Utc>) { /* … */ }
```

**Lock it in**: make `conn` private (the compiler enforces the rest), add a test that `"SELECT "` appears only under `src/index/`, and add table tests for `agent_state`.

---

### M5: Prices are "versioned data" only on paper: every built-in and cached price is backdated to 2023

**Severity**: Medium
**Requirement**: 1. Rules that change over time are dated data (also 8)
**Location**: `src/core/pricing.rs:80,86-141,156-191`; defaults `input × 1.25 / × 2.0 / × 0.1`, `0.01` and the date `2023-01-01` repeated in `pricing.rs:124-127,169-172` and `config.rs:131-140`.

`BUILTIN` has no `from` column, and `merge_claude_code_cache` stamps the cache with `EPOCH`. When Anthropic changes a price, Claude Code's cache updates, the fingerprint changes, and every historical request is repriced at today's rate, including past limit windows and their calibration. The derived-rate defaults are written three times. The user-override copy forgets `fast_multiplier` and `long_context` (see Defects).

#### Current

```rust
// src/core/pricing.rs:185
self.entries.push(PriceEntry { model: model.clone(), from: EPOCH, rates, source });
// src/config.rs:135
cache_write_5m: p.cache_write_5m.unwrap_or(p.input * 1.25),
cache_write_1h: p.cache_write_1h.unwrap_or(p.input * 2.0),
cache_read: p.cache_read.unwrap_or(p.input * 0.1),
web_search: p.web_search.unwrap_or(0.01),
fast_multiplier: p.fast_multiplier,
long_context: None,
```

#### Recommended

```rust
impl Rates {
    /// The one place the derived rates are defined; anything not given inherits from `base`.
    pub fn derive(input: f64, output: f64, over: RateOverrides, base: Option<&Rates>) -> Rates { /* … */ }
}
// Claude Code's cache: store each distinct (model, rates) in the index's FACT tables with the
// date it was first seen, and price a request with the version in force on its day.
const BUILTIN: &[(&str, NaiveDate, f64, f64, f64, Option<f64>)] = &[ /* model, from, … */ ];
```

**Lock it in**: a test where a cache price changes "today" and requests from yesterday keep their cost, plus a test that a user override of `claude-opus-5-5` with only `input`/`output` still doubles in fast mode.

---

### M6: Primitive obsession: ids, account keys and money are raw `String`/`f64`

**Severity**: Medium
**Requirement**: 2. Value objects
**Location**: 99 id-like `String`/`&str` fields and parameters (`account_uuid`, `seat`, `session_id`, `agent_id`, `login`, `id`, `label`; worst: `claude/transcript.rs` 13, `vault/mod.rs` 11, `live/mod.rs` 10, `cli/accounts.rs` 9). 23 money `f64` fields (`live/mod.rs` 6, `cli/limits.rs` 6, `core/limits.rs` 3). 15 percent/points `f64` fields. 44 stringly `kind/tier/plan/effort/model/theme` fields. Ambiguous keys: `src/index/query.rs:161-169`, `src/vault/mod.rs:146-151`, `config.rs` (`subscription` "by account label or uuid").

The account parameter means a uuid in one place, an email in another (switched on `contains('@')`), and a login label elsewhere. A seat is a label or a path depending on the caller. On money, an honest assessment: `f64` is adequate for an API-equivalent estimate that is never billed. Summing about 10⁶ requests keeps an error far below one cent, and the comparisons that matter (the 1.2 ratio, 85%, ETA) are not sensitive at that scale. The real costs of raw `f64` are elsewhere. USD, points and percent mix freely (`estimate_now + spent / usd_per_point`). A sentinel can hide in the column (H3). Negative, `nan` or `inf` rates from `config.toml` (TOML allows them) are accepted without a parse. So this is graded Medium, not the High that floats used for billed money would get.

#### Current

```rust
// src/index/query.rs:161
if let Some(a) = &f.account {
    // A uuid uses the (account_uuid, ts) index; an email is looked up once.
    if a.contains('@') {
        w.push("r.account_uuid IN (SELECT account_uuid FROM accounts WHERE email = ?)".into());
    } else {
        w.push("r.account_uuid = ?".into());
    }
// src/vault/mod.rs:146
pub fn find(&self, key: &str) -> Option<&Login> {
    self.index.logins.iter().find(|l| l.label == key || l.id == key || l.email.as_deref() == Some(key))
}
```

#### Recommended

```rust
#[repr(transparent)] #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)] pub struct AccountId(String);
#[repr(transparent)] #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)] pub struct SeatLabel(String);
pub enum AccountRef { Id(AccountId), Email(String) }          // parsed once at the CLI edge
#[repr(transparent)] #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize)]
pub struct Usd(f64);                                           // finite and >= 0, via Usd::new
impl Usd { pub fn new(v: f64) -> Result<Usd, ValueError> { /* reject NaN, inf, < 0 */ } }
impl std::iter::Sum for Usd { /* … */ }
// Points / Usd = no impl: converting needs a UsdPerPoint, by name.
```

**Lock it in**: a compile-fail doctest showing that `Usd + Points` does not compile, `Usd::new` as the only constructor (private field), and `PriceConf` deserializing through `Usd::new`.

---

### M7: Claude Code's tool names leak past the reader into four modules

**Severity**: Medium
**Requirement**: 1. Foreign models stay at the edge
**Location**: `"AskUserQuestion" | "ExitPlanMode"` in `src/live/mod.rs:888`, `src/index/insights.rs:166-169`, `src/tui/view/panels.rs:2432`. `"Agent" | "Task"` in `src/claude/transcript.rs:286`, `src/live/mod.rs:889`. Effort strings in `src/tui/view/kit.rs:142-148`. File kind round-trip in `src/index/mod.rs:194-198`.

Claude Code renaming a tool, or adding a new question tool, has to be found in four modules. If one is missed, a wait for the human is silently counted as tool time in EFFICIENCY while the lanes show it correctly.

#### Current

```rust
// src/live/mod.rs:887
let kind = match name.as_str() {
    "AskUserQuestion" | "ExitPlanMode" => Lane::You,
    "Agent" | "Task" => Lane::Agents,
    _ => Lane::Tool,
};
// src/tui/view/panels.rs:2432
.filter(|t| !matches!(t.name.as_str(), "AskUserQuestion" | "ExitPlanMode"))
```

#### Recommended

```rust
// src/claude/transcript.rs — translated once, stored as a column
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind { AsksHuman, SpawnsAgent, Shell, File, Search, Other }
impl ToolKind {
    pub fn of(name: &str) -> ToolKind {
        match name { "AskUserQuestion" | "ExitPlanMode" => ToolKind::AsksHuman,
                     "Agent" | "Task" => ToolKind::SpawnsAgent, /* … */ _ => ToolKind::Other }
    }
}
// tool_calls.kind TEXT NOT NULL CHECK (kind IN ('asks_human','spawns_agent',...))
```

**Lock it in**: a test that the literal `"AskUserQuestion"` appears only in `src/claude/`, and an exhaustive `match` on `ToolKind` everywhere else (no `_ =>`).

---

### M8: The index schema carries no constraints; its rules live only in Rust

**Severity**: Medium
**Requirement**: 3. Invariants at the lowest level
**Location**: `src/index/schema.rs:23-171`.

No column has a `CHECK`. `fast`, `priced` and `finished` accept any integer, `kind`, `effort` and `readings.kind` accept any text, and token counts and `utilization` can be negative. There are no foreign keys between `requests`, `sessions` and `agents`. Every invariant is therefore enforced only by whichever code path writes it, and a future path (or a migration bug) goes unnoticed.

#### Current

```sql
CREATE TABLE IF NOT EXISTS requests (
  msg_id TEXT PRIMARY KEY,
  ...
  fast INTEGER NOT NULL DEFAULT 0,
  input INTEGER NOT NULL DEFAULT 0,
  ...
  cost REAL NOT NULL DEFAULT -1,
  priced INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS readings ( account_uuid TEXT NOT NULL, kind TEXT NOT NULL, ... utilization REAL NOT NULL, ...
```

#### Recommended

```sql
  fast INTEGER NOT NULL DEFAULT 0 CHECK (fast IN (0, 1)),
  input INTEGER NOT NULL DEFAULT 0 CHECK (input >= 0),      -- same for cw5, cw1h, cread, output, thinking, web
  cost REAL CHECK (cost IS NULL OR cost >= 0),
  priced INTEGER NOT NULL DEFAULT 1 CHECK (priced IN (0, 1))
-- readings
  kind TEXT NOT NULL CHECK (kind IN ('five_hour','seven_day','seven_day_opus','seven_day_sonnet')),
  utilization REAL NOT NULL CHECK (utilization >= 0),
```

Generate the `kind IN (...)` lists from `WindowKind::key()` so they are declared once.

**Lock it in**: the constraints themselves, plus a test that the generated `CHECK` list equals the enum's keys.

---

### M9: The tests cover the kernel; the index, the limit analysis and the live state have none

**Severity**: Medium
**Requirement**: 10. Tests as executable specification
**Location**: 54 tests total. `core` 17 (exemplary), `claude` 22, `vault` 5, `index` 2 (open/reopen, command grouping), `live` 1, `cli` 1, `tui` 1. Zero tests for `Index::refresh`, the dedupe upsert (`src/index/mod.rs:665-673`), re-attribution after a login swap (`:425-470`), project folding, `price_all`, `cli/limits.rs` (568 lines), `App::attention`, agent states, and vault concurrency or crash mid-switch.

The documented dedupe rule, `Usage::merge_max`, is tested, but it never runs. `cargo clippy` reports "method `merge_max` is never used". The rule that runs is a second copy in SQL (`input = MAX(input, excluded.input)`), and that copy is untested. The crate is binary-only, so no scenario test can drive it through its public API from `tests/`. The `demo` world, with its synthetic seats and transcripts, would make a ready-made fixture.

#### Current

```rust
// src/core/usage.rs:37 — tested, dead
pub fn merge_max(&mut self, o: &Usage) { self.input = self.input.max(o.input); /* … */ }
// src/index/mod.rs:668 — runs, untested
ON CONFLICT(msg_id) DO UPDATE SET
  input = MAX(input, excluded.input), cw5 = MAX(cw5, excluded.cw5), cw1h = MAX(cw1h, excluded.cw1h),
  cread = MAX(cread, excluded.cread), output = MAX(output, excluded.output), thinking = MAX(thinking, excluded.thinking),
  web = MAX(web, excluded.web), tools = tools + excluded.tools,
```

#### Recommended

```rust
// Cargo: src/lib.rs (all modules) + src/main.rs (cli::run only)
// tests/index_scenarios.rs
#[test]
fn incremental_equals_full_rebuild() {
    let w = fixture::world(&[("work", SESSIONS_WITH_DUPLICATE_BLOCKS)]);
    let inc = w.refresh_in_steps(&[100, 250, usize::MAX]);   // append in chunks
    let full = w.refresh_once();
    assert_eq!(inc.dump("requests"), full.dump("requests"));
    // opus-5-5: 2 in, 409 1h-write, 155 527 read, 192 out = $0.0382254 (hand-computed)
    assert_eq!(full.total(Period::All).cost_micros(), 38_225);
}
```

**Lock it in**: a lib target, scenario files under `tests/fixtures/`, a rule that every bug fix lands with a reproducing scenario, and a test that fails while dead domain functions exist (`#![deny(dead_code)]` in `core`).

---

### M10: The dashboard worker is a 368-line use case with ~15 hard-coded cadences

**Severity**: Medium
**Requirement**: 9. One pipeline, explicit order (also 8)
**Location**: `src/tui/mod.rs:456-823` (`worker`), `:824-1085` (`analytics`, 262 lines); interval literals at `:542,545,561,569,586,614,637,647,700,704,721,742,747,786,848,850`.

The battery budget is a hard requirement: about 2 CPU-seconds per minute. It is kept by an informal table in CLAUDE.md and by elapsed-time checks scattered through one loop. Moving a job, or adding one, means reading the whole worker, and nothing checks that per-second work only touches running sessions.

#### Current

```rust
// src/tui/mod.rs:561
if sl != sl_stamp && last_sl.elapsed() > Duration::from_secs(5) { /* … */ reload_seat_info(&mut ctx); }
let stamp = login_files_stamp(&ctx);
if stamp != login_stamp || last_info.elapsed() > Duration::from_secs(20) { /* … */ reload_seat_info(&mut ctx); }
// …
let due = last_analytics.is_none_or(|t| t.elapsed() > Duration::from_secs(30))
// …
|| last_limits.is_none_or(|t| t.elapsed() > Duration::from_secs(55));
```

#### Recommended

```rust
/// Declared once; CLAUDE.md's budget table and SWARMY_DEBUG timings are generated from it.
const JOBS: &[Job] = &[
    Job { name: "hot refresh",  every: secs(1),   trigger: Trigger::Tick,             cost: Cost::RunningOnly },
    Job { name: "seat info",    every: secs(20),  trigger: Trigger::LoginFileChanged, cost: Cost::PerSeat },
    Job { name: "limits",       every: secs(55),  trigger: Trigger::SeatsChanged,     cost: Cost::Heavy },
    Job { name: "efficiency",   every: secs(300), trigger: Trigger::Tick,             cost: Cost::Heavy },
    // …
];
```

**Lock it in**: a test that no `Cost::Heavy` job runs more often than every 30 s, plus a freshness test for the generated budget table.

---

### M11: Nothing is enforced by tooling

**Severity**: Medium
**Requirement**: Enforcement by mechanism
**Location**: `Cargo.toml` (no `[lints]`), no CI configuration in the repository, no `clippy.toml`. `cargo clippy --all-targets` reports 40 warnings today, including 17 dead-code items (`merge_max`, `Span::contains`, `Span::hours`, `account_labels`, `active_accounts`, `open_in_memory`, `keychain_service`, …). 2 `#[allow]`.

Every rule in CLAUDE.md (no personal data, the battery budget, keeping the vocabulary, "never test the vault on real dirs") is kept by agreement only. A lint that already warns is a lint nobody reads.

#### Current

```toml
[dependencies]
anyhow = "1.0.104"
# … no [lints] table
```

#### Recommended

```toml
[lints.rust]
unsafe_code = "forbid"
dead_code = "deny"
[lints.clippy]
unwrap_used = "deny"            # tests: #![cfg_attr(test, allow(clippy::unwrap_used))]
wildcard_enum_match_arm = "deny"
allow_attributes_without_reason = "deny"
```

Add a `tools/check.sh` (`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --no-fail-fast`) as a pre-push hook or CI job, together with the ratchets from H4, H5 and M3.

**Lock it in**: the check script in CI or as a pre-push hook; ratchet files under version control.

---

### L1: "mana" in code, "limits" on screen; two `Tone` enums

**Severity**: Low
**Requirement**: 1. One name per concept
**Location**: "mana" 93 times in 5 files (`Panel::Mana`, `ManaRow`, `mana_curve`, `AttnTarget::Mana`, the alert key `"mana:{uuid}"` at `src/tui/app.rs:875`). `ui::style::Tone` (`src/ui/style.rs:12`) and `tui::app::Tone` (`src/tui/app.rs:157`).

CLAUDE.md bans "mana" on screen, but the code still thinks in it, so every newcomer has to learn that `Mana` means LIMITS · SEATS.

#### Current

```rust
pub enum Panel { Swarm, Mana, Burn, Attention, Agent, Projects, Models, History, Efficiency, System }
// …
Panel::Mana => "LIMITS · SEATS",
```

#### Recommended

```rust
pub enum Panel { Swarm, Limits, Burn, Attention, Agent, Projects, Models, History, Efficiency, System }
pub enum LimitsRow { Seat(String), Parked(String) }
```

**Lock it in**: a glossary test that fails on `mana` in identifiers (excluding the old-dir migration in `config.rs`).

---

### L2: Layer leaks around the reader and the read model

**Severity**: Low
**Requirement**: 7. Layers point one way
**Location**: `src/live/mod.rs:704,711` (`live → ui::fmt`: `PastEvent.text` is a pre-formatted string), `src/demo.rs:20` (`→ tui::boot::hash`), `src/claude/statusline.rs:227,664-685` (`claude → fleet, config`; the reader layer renders ANSI and writes files), `src/config.rs:11` (`→ claude::project::Alias`).

Each leak is small, but together they mean the reader layer cannot be built or reused without the front ends, and the event texts cannot be re-rendered (for example masked, or in JSON).

#### Current

```rust
// src/live/mod.rs:707
text: format!("{name} done · {took} · {}", crate::ui::fmt::money(cost.unwrap_or(0.0))),
```

#### Recommended

```rust
pub enum PastKind { Spawned { model: String, effort: Option<String> }, Done { took: Duration, cost: Option<Usd> }, /* … */ }
// the TUI formats; `statusline.rs` moves to `src/statusline/` (reader of stdin + renderer), `Alias` to `config`.
```

**Lock it in**: the dependency test from H4 (`claude` and `live` may not import `ui`, `fleet`, `config` or `tui`).

---

### L3: Wildcards on own enums and a silent theme fallback

**Severity**: Low
**Requirement**: 3. Exhaustive handling
**Location**: `src/core/limits.rs:52` (`WindowKind::length`, `_ => 7 days`), `src/tui/app.rs:1191` (`Panel` → list length `_ => 0`), `src/tui/app.rs:968` (`Panel` → `Tab` `_ => Ops`), `src/tui/theme.rs:147` (`theme = "ambr"` silently becomes `ops`, while the rest of `config.toml` uses `deny_unknown_fields`). 58 `_ =>` arms in total; most match foreign strings and are legitimate.

#### Current

```rust
pub fn length(self) -> Duration {
    match self {
        WindowKind::FiveHour => Duration::hours(5),
        _ => Duration::days(7),
    }
}
```

#### Recommended

```rust
pub fn length(self) -> Duration {
    match self {
        WindowKind::FiveHour => Duration::hours(5),
        WindowKind::SevenDay | WindowKind::SevenDayOpus | WindowKind::SevenDaySonnet => Duration::days(7),
    }
}
// config: `theme: Option<ThemeName>` with `#[serde(rename_all = "lowercase")] enum ThemeName { Ops, Phosphor, Amber, Plain }`
```

**Lock it in**: `clippy::wildcard_enum_match_arm = "deny"` (M11).

---

### L4: SQL is safe today, but one helper accepts a table name as `&str`

**Severity**: Low
**Requirement**: 4. Queries are never built from data
**Location**: `src/index/query.rs:443-448` (`count(table: &str)`), `src/index/query.rs:179-181` (`LIKE '%{m}%'` without escaping `%`/`_`). All 9 `format!` SQL builders were checked: they interpolate only `&'static str` fragments (`Dim::exprs`, `SUMS`, `SEAT_ACCOUNT_AT`, `schema::TABLES`), and every value is bound.

#### Current

```rust
pub fn count(&self, table: &str) -> anyhow::Result<u64> {
    Ok(self.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0))? as u64)
}
```

#### Recommended

```rust
pub enum Table { Requests, ToolCalls, Sessions, Agents /* … */ }
pub fn count(&self, t: Table) -> anyhow::Result<u64> { /* t.name() is &'static str */ }
// model filter: `r.model LIKE ? ESCAPE '\'` with % and _ escaped in the bound value
```

**Lock it in**: a test that greps `format!(` calls containing SQL keywords and asserts that every `{…}` placeholder names a `const` or a `&'static str` helper.

---

### L5: Four implementations of "write a file atomically"

**Severity**: Low
**Requirement**: 4. One concept, one mechanism
**Location**: `src/vault/store.rs:64-94` (fsync, 0600, dir fsync), `src/claude/statusline.rs:160-167`, `src/fleet.rs:72-84` (fixed temp name `fleet.json.tmp`), `src/cli/setup.rs:174-197` (Claude Code's `settings.json`, no fsync).

`setup` writes a Claude Code file without the guarantees the vault gets. All three non-vault copies `unwrap_or_default()` a serialization result, so a failure would write an empty file.

#### Current

```rust
// src/cli/setup.rs:193
let tmp = path.with_extension("json.swarmy-tmp");
std::fs::write(&tmp, text)?;
std::fs::rename(&tmp, path)?;
```

#### Recommended

```rust
// one helper in a small `fsio` module, used by vault, setup, statusline and fleet
fsio::atomic_write(path, text.as_bytes(), fsio::Mode::KeepExisting)?;   // or Mode::Private (0600)
```

**Lock it in**: a test that `fs::rename` appears only in `fsio`.

---

## Roadmap

| # | Action | Findings | Effort | Impact |
|---|---|---|---|---|
| **1. Guardrails first** | | | | |
| 1.1 | `[lints]` table, fix the 40 clippy warnings, `tools/check.sh` as a pre-push hook or CI | M11, L3 | S | stops new drift |
| 1.2 | Dependency test over `crate::` edges with today's edges as a ratchet | H4, L2 | S | blocks new cycles |
| 1.3 | Ratchet counters: fallbacks in `index/vault/cli`, files over 900 lines, threshold literals | H5, M1, M3 | S | numbers only go down |
| **2. Structural fixes** | | | | |
| 2.1 | `vault::edit` + `VaultTx`, std `File::lock`, unique temp names, backup of `logins/secrets.json`, concurrency test | H1, defects 1, 3, 5, 6 | M | removes the token-loss path |
| 2.2 | Split `TABLES` into cache and fact tables; survive-a-bump test | H2 | S | keeps account history |
| 2.3 | `cost` nullable + `CHECK`, reprice in one transaction, memo keyed by fingerprint, dashboard reloads prices | H3, defect 8 | S | no silent $0 |
| 2.4 | Lib + bin split; move `Ctx` to `app/`, limits analysis to `limits/`, `human_bytes` to `ui/` | H4, M2, M9 | M | breaks the cycle, enables scenario tests |
| 2.5 | Private `Index::conn`; typed read methods for `live`; pure `agent_state` | M4 | M | one owner of the schema |
| 2.6 | `alerts.rs` out of `App`; split `panels.rs` per panel; declared job table for the worker | M3, M10 | M | smaller change surface |
| **3. Model upgrades** | | | | |
| 3.1 | `Level`, `current_window`, `projection_at_reset` in `core::limits`; delete the copies | M1 | S | one limit vocabulary |
| 3.2 | Scenario tests: incremental equals full, dedupe, re-attribution, repricing, limits analysis with hand numbers | M9, M2 | M | specification for the index |
| 3.3 | Dated built-in and observed prices; `Rates::derive` | M5, defect 9 | M | history keeps its prices |
| 3.4 | `AccountId`, `SeatLabel`, `AccountRef`, `Usd`, `ToolKind`, `ThemeName` | M6, M7, L3 | M | type-checked keys |
| 3.5 | `CHECK` constraints generated from enums | M8 | S | the database guards every path |

## Defects found along the way

| Severity | Location | Defect | Prevented by |
|---|---|---|---|
| High | `src/vault/mod.rs:135,383,440`, `src/vault/store.rs:47,70`, `src/tui/mod.rs:444,863` | Unlocked read-modify-write of `logins.json`/`secrets.json` from the dashboard worker, the UI thread and other processes. Same temp path for every thread of a process, so writes can interleave bytes. No backup of the vault's own files (H1). | a pipeline that owns the lock (`VaultTx`) |
| High | `src/index/mod.rs:130-137`, `src/index/schema.rs:174` | A schema bump drops `seat_accounts`/`accounts`, which cannot be rebuilt; past spend moves to the wrong account (H2). | fact/cache split with a test |
| High | `src/index/mod.rs:595`, `src/index/query.rs:186`, `src/cli/limits.rs:87-114` | During a reprice, other processes sum unpriced rows as $0 with `priced = 1`. The dashboard memoizes such totals for its lifetime and never invalidates them on a price change (H3). | `CHECK`, one transaction |
| Medium | `src/vault/store.rs:128-158` | Stale-lock takeover is a TOCTOU race. A reader can see the file before its pid is written, treat it as dead and delete it, or two takers can both delete and recreate it, and then both "hold" the lock. A reused pid blocks the lock forever. | `std::fs::File::lock` (flock) |
| Medium | `src/vault/mod.rs:527-552` | `switch` reads `.claude.json`, then backs up and fsyncs several files, then writes `.claude.json` back. Claude Code rewrites that file every few seconds, so whatever it wrote in between is lost. | re-read and compare mtime immediately before the write; retry |
| Medium | `src/vault/mod.rs:223-225` | `put` on an existing login sets `label` without a uniqueness check (`accounts capture <seat> --label <taken>`). `find` then returns the first match, so `switch` can mount the wrong login. | `LoginLabel` unique by construction; uniqueness checked in one place |
| Medium | `src/tui/mod.rs:479,593` vs `src/index/mod.rs:592-596` | The dashboard keeps the `PriceBook` from launch. A CLI run with a newer book reprices the whole table, and the dashboard's next refresh reprices it back, so the two disagree in turn and both rewrite every row. | price book reloaded on `.claude.json`/config change; fingerprint in the memo key |
| Medium | `src/config.rs:139-140` | A `[[price]]` override of a model drops its fast-mode multiplier and long-context rule, because the user entry fully replaces the built-in one. Fast-mode Opus 5.5 is then priced at 1× instead of 2×. Negative, `nan` or `inf` rates are accepted. | `Rates::derive` with inheritance; `Usd::new` parser |
| Low | `src/core/pricing.rs:171`, `src/config.rs:137` | A missing cache-read price falls back to 10% of input instead of the model's built-in rate (Opus 5.5: 5%, Fable 5.2: 2.5%), so cache-heavy usage is overpriced 2× to 4× when Claude Code's cache omits `promptCacheReadTokens` or a user override omits `cache_read`. | `Rates::derive` (one default) |
| Low | `src/index/mod.rs:672,800` | Re-reading a replaced or truncated transcript from the start adds `tools` and `compactions` a second time. The rows themselves are deduplicated by `MAX`. | "incremental equals full rebuild" scenario test |
| Low | `src/vault/mod.rs:609-611` | `remove` deletes the secret before saving the index. If the save fails, a listed login has no secret. | write the index first, then delete; or a tombstone |
| Low | `src/vault/mod.rs:524-541` | Backups are pruned to 40 files across all seats (about 20 switches), so a rarely switched seat loses its only backup. Second-resolution stamps make two switches in the same second overwrite each other's backup. | per-seat retention, unique names |
| Low | `src/vault/mod.rs:235-238`, `src/tui/view/kit.rs:205` | Auto-generated login labels are the email's local part, and `--mask` masks only strings that contain `@`, so screenshots taken with `--mask` show it. | masking by type (`Email`, `Label`) at render time |
| Low (verify intent) | `src/live/mod.rs:395-398` | An unfinished subagent of an exited Claude Code shows as STALLED (red) for 24 h, while its main thread shows as exited. | table tests for a pure `agent_state` (M4) |

No access-control, network or SQL-injection defects were found. The tool makes no network calls, every SQL value is bound, the vault directory is 0700 and its files are 0600, and `Secret` never prints a token.

## What is already good

- **A pure, well-tested kernel.** `src/core` has no I/O, and its tests compute money by hand in comments (`src/core/pricing.rs:298-310`, `src/core/limits.rs:188-207`). Keep that style as the template for every new rule.
- **Unknown is never $0.** `Cost::priced` flags models without a price (`src/core/pricing.rs:68-73,226-237`), and unknown transcript record types are counted and reported by `doctor`.
- **Price precedence is data.** The user, Claude Code cache and built-in layers merge by an explicit key `(source, specificity, from)` (`src/core/pricing.rs:203-224`), and the merge is tested.
- **The anti-corruption layer exists.** `src/claude/*` translates Claude Code's JSON into `core` types (`Reading`, `WindowKind`, `Usage`). `WindowKind::from_key` rejects unknown windows instead of guessing.
- **Safe SQL.** `where_clause` (`src/index/query.rs:150-184`) binds every value, and the dynamic parts are `&'static str`. `from_for` joins only the tables a query refers to.
- **Incremental indexing done carefully.** It tracks inode, size and offset, re-reads files that shrank or were replaced, leaves a partial last line for later (tested in `src/claude/transcript.rs`), and uses WAL with a 10 s busy timeout.
- **Switch safety basics are right.** The seat's current login is saved first, both files are backed up, writes use fsync+rename at 0600, `mcpOAuth` is kept (tested in `src/vault/tests.rs:35-108`), a grant is refused in two seats without `--share`, and a different refresh token is treated as a new login, never overwritten (`src/vault/tests.rs:135`).
- **Secrets hygiene.** `Secret`'s `Debug` shows only the fingerprint, and its token strings are wiped on drop (`src/claude/credentials.rs:12-54`).
- **Strict config.** `deny_unknown_fields` on every `config.toml` struct (`src/config.rs:63,83,90,235`).
- **Explicit rule revisions.** `PROJECTS_REV` recomputes project folding once when the rules change (`src/index/mod.rs:31`), and `readings` is deliberately kept out of the drop list.
- **Synthetic test data.** The vault tests run on throwaway seats under a temp dir and never touch a real config dir.
