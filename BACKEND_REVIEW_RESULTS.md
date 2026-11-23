# Backend Architecture Review

> Analyzed on: 2026-11-23 (third review, at `44c2b8e`)
> Project: swarmy (repository root)
> Stack: Rust 2024, one library crate plus a thin binary; clap CLI and ratatui TUI; SQLite through rusqlite (WAL); JSON files (vault, status-line readings, fleet summary, Claude Code's own files)
> Modules analyzed: 17 top-level modules (`app`, `claude`, `cli`, `config`, `core`, `demo`, `diag`, `errors`, `fleet`, `fsutil`, `index`, `limits`, `live`, `power`, `tui`, `ui`, `vault`) · Findings: 23 (High 4, Medium 12, Low 7)

## Summary

The biggest correctness risk is now **who pays**. Two paths write permanent account attributions from timestamps that do not say when a login entered a seat. Tests confirm both:

- After a vault switch, the saved `oauthAccount`'s old `profileFetchedAt` is taken as the switch time. That moves days of the previous account's requests to the new account, overriding the transcripts' own `session-owner` records (H1).
- A status-line reading stores no account. The next refresh books it, and the requests since it was taken, to whatever account the seat holds now (H2).

Both end up in fact tables that are never rebuilt. Since the last review, the rollup re-key carries H1 into the archive as well.

The new storage tier mostly holds its promise that a compacted index reads the same as one never compacted. It breaks in four ways, all confirmed by tests:

- A response whose content blocks straddle `rolled_until` is counted twice when its file is read again (H3).
- The settled-sum cache cuts off the newest minutes of the limit curve (M1).
- The rollup re-key can double count.
- `MAX(priced)` hides unpriced requests (both M2).

The vault's single lock works. However, a switch that fails between the credentials write and the `.claude.json` write leaves the seat and the vault out of step, so the shared-grant guard can be bypassed (H4).

Structurally the codebase is much better than two reviews ago:

- `core` is pure and guarded by a test.
- Module edges, fallbacks, unwraps and wildcards are ratcheted.
- The schema carries generated `CHECK`s.
- Every cadence lives in one job table.

What remains: a strict-lint gap of 2,157 warnings, mostly casts and indexing in the TUI. Value objects are half adopted (28 raw account strings next to `AccountId`). The status line, 932 lines that render, execute and read the fleet summary, still sits inside the reader layer.

## System map

| Area | Role (layer) | Lines (prod + tests) | Notes |
|---|---|---|---|
| `core/` | domain kernel: usage, pricing (dated), periods, limits math, storage rules, ids, units | 3 266 | pure (`tests/architecture.rs::core_is_pure`) |
| `claude/` | anti-corruption layer over Claude Code's files; **also** the status-line command (renders ANSI, runs the user's previous command, reads `fleet.json`) | 3 459 | `statusline.rs` 932 |
| `index/` | persistence: SQLite cache, incremental refresh, read marks, compactor, archive, settled parts, queries, insights | 9 721 (3 119 of them scenario tests) | private connection; SQL only here (architecture test) |
| `limits/` | limit analysis per account (queries feeding `core::limits`) | 682 | depends on `app::Ctx` |
| `live/` | live agent read model (`state.rs` pure) | 1 172 | per-agent reads kept between ticks |
| `vault/` | login vault: the only writer into Claude Code dirs | 1 919 | one OS lock per change (`Vault::edit`) |
| `app/` | composition root `Ctx` | 581 | takes the vault lock on every load |
| `cli/` | commands | 3 009 | |
| `tui/` | worker, job table, analytics, alerts, view | 9 500 | 26% of the code |
| `fleet.rs`, `fsutil.rs`, `errors.rs`, `diag.rs`, `config.rs`, `power.rs`, `ui/`, `demo.rs` | contract for the status line, atomic writes, error catalogue, debug channel, config, battery, formatting, demo world | 2 288 | |

Entry points: the clap subcommands (`cli/mod.rs`), the dashboard worker loop (`tui/worker.rs::worker`), and `swarmy statusline`, which Claude Code runs after every response and on `refreshInterval`.

**Dependency matrix** (row imports column; `crate::<module>` references in production code, measured):

| from \ to | app | claude | cli | config | core | diag | errors | fleet | fsutil | index | limits | live | tui | ui | vault |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| app | | 7 | | 1 | 8 | 3 | 1 | | | 3 | | | | 3 | 2 |
| claude | | | | **2** | 7 | 1 | | **3** | 2 | | | | | | |
| cli | 11 | 14 | | 6 | 12 | 1 | 1 | 2 | 2 | 11 | 3 | | 3 | 23 | 3 |
| config | | **1** | | | 2 | | 1 | | | | | | | | |
| fleet | | | | | 1 | | | | 2 | | | | | | |
| index | | 13 | | | 28 | | 2 | | | | | | | | |
| limits | **1** | | | | 4 | | | | | 2 | | | | | |
| live | | 6 | | | 4 | | | | | 6 | | | | | |
| tui | 3 | 13 | | 1 | 29 | 1 | | 6 | | 11 | 5 | 13 | | 23 | 8 |
| vault | | 4 | | | | 3 | 3 | | 2 | | | | | | |

Forbidden or questionable edges are in bold:

- `claude ↔ config` is still a cycle: `config.rs` names `claude::project::Alias`, and `claude/statusline.rs` names `config::Dirs` and `StatuslineConf`.
- `claude → fleet`: the reader layer depends on a contract written by the dashboard.
- `limits → app`: a domain analysis depends on the composition root, and masks paths for display.

The `tui → cli` cycle and `live → ui` from the first review are gone. `tests/arch/edges.txt` records every remaining edge with a reason.

**Context map**:

- Claude Code → `claude/`: an anti-corruption layer, conformist to Claude Code's undocumented formats. Format drift is counted (`record_types`) and shown by `doctor`.
- `index`, `live`, `limits` and `vault` read through `claude/` types: a customer–supplier relationship.
- dashboard → status line: an open host service. `fleet.json` is versioned (`v: 2`) and checked on read.
- status line → dashboard: `statusline/<seat>.json` (`LiveLimits`) is a published language with **no version field and no account**. That is the root of H2.

**Subdomains**:

- Core: spend at API prices and weekly-limit attribution (`core::pricing`, `core::limits`, `index`, `limits`).
- Supporting: live swarm view, vault, status line.
- Generic: storage compaction, atomic files, process table, themes.

## Scorecard

| Requirement | Score (1-5) | Evidence |
|---|---|---|
| Cohesion and coupling | 3 | no cross-feature cycles left except `claude ↔ config`; edges ratcheted (`tests/arch/edges.txt`); but `claude/statusline.rs` mixes reading, rendering, process spawning and the fleet contract, and `limits → app` (M10) |
| Modularity | 3 | areas grouped by concept (`index/compact.rs`, `index/marks.rs`, `tui/view/panels/*`); every module `pub`, with 465 `pub fn` against 70 restricted ones (L5) |
| Layers and ownership | 3 | table classes are data (`RAW_TABLES`, `DIMENSION_TABLES`, `ARCHIVE_TABLES`, `FACT_TABLES`) and SQL stays in `index/` by test; reader layer still points up (M10); file ownership only in the `CLAUDE.md` layout table |
| Strategic DDD (contexts, subdomains, ubiquitous language, ACL) | 3 | seat / login / account defined and the "mana" test enforces one word; ACL over Claude Code; but "the seat's account at time t" is inferred from timestamps that mean something else (H1, H2); status-line file unversioned; `Pct` vs `points` (L6) |
| Tactical DDD (aggregates, pure domain, thin use cases) | 3 | pure `core` with a test; the vault is a real aggregate with one transaction; but `worker` is 352 lines, `post_pass` mixes attribution, projects and pricing, and `switch` saves halfway (M11, H4) |
| Value objects (zero-cost) | 3 | `SeatId`, `SessionId`, `AccountId`, `Usd`, `Pct`, `UsdPerPct` exist, `repr(transparent)`, with a compile-fail doctest; but `SeatId::default()` and `from_stored(label)` make invalid ids, 28 account fields are still `String`, and 18 money fields are `f64` (M9) |
| Static typing and analysis | 3 | `[lints]` denies unwrap/expect/panic, unsafe, dead code and reasonless allows; clippy clean under the project config; strict run: 2 157 warnings (445 `as_conversions`, 394 `indexing_slicing`, 258 `float_arithmetic`, 239 lossy casts) (M8) |
| Domain invariants and failure modes | 2 | `CHECK`s generated from enums; facts survive bumps; but attribution (H1, H2), count-once across raw and archive (H3, M2), one-grant-one-seat (H4) and settled = fresh (M1) are each broken on a path, and records can be lost silently (M3, M4) |
| API surface and integrity | 3 | private connection, typed reads for `live`, every SQL value bound; `Index::count(&str)` and `pub mod vault::store` (raw writers that bypass `Vault::edit`) remain (L5); every command takes the vault write lock (L4) |
| State-change pipeline | 3 | `Vault::edit` (lock, re-read, save on change, backups) and immediate index transactions; the error catalogue has stable codes; but `switch` is not atomic and the index errors are untyped (H4, M12) |
| Declarations and duplication | 4 | `KEYS`/`SUMS` derive the rollup DDL and its merge and re-key SQL; `ToolClass`, `FileKind::TAGS` and `WindowKind` generate `CHECK` lists; job table; limit thresholds once, guarded by a ratchet |
| Tests as specification | 3 | 186 tests; scenario tests on synthetic seats with money computed by hand; "compacted = never compacted" proved on every surface; but the fixtures write one record per response (H3 slipped through), no property tests, and the vault concurrency test flaked once in six runs (L4) |
| Enforcement by mechanism | 3 | `tests/architecture.rs` (edges, six ratchets, purity, SQL placement, renames, vocabulary, id slicing), `tools/check.sh`, `tools/perf.sh`; no CI and no installed hook, so all of it runs only when someone remembers |
| **Overall** | **3** | solid guardrails and kernel; the new storage and attribution paths carry the remaining correctness bugs |

## Findings

### H1: A vault switch re-attributes days of the previous account's requests to the new login

**Severity**: High
**Requirement**: 3. Invariants (a request is attributed to the account that made it)
**Location**: `src/index/mod.rs:700-748` (event from `profile_fetched_at`, the `UPDATE … SET account_uuid = SEAT_ACCOUNT_AT`, the rollup re-key); `src/index/mod.rs:48` (`SEAT_ACCOUNT_AT` prefers `seat_accounts` over the owner-derived account); `src/vault/mod.rs:693-698` (the saved `oauthAccount` is written back verbatim, `profileFetchedAt` included)

`post_pass` treats `oauthAccount.profileFetchedAt` as "the seat has had this account since". A vault switch restores a saved `oauthAccount` verbatim, so `profileFetchedAt` is the capture time, possibly days earlier and in another seat.

The next refresh inserts `seat_accounts(seat, capture time, new account)`. It then rewrites every request of the seat since that time. That includes requests whose transcripts carry a `session-owner` record naming the old account: `SEAT_ACCOUNT_AT` wins over it.

When the capture time is older than `rolled_until`, `archive::rekey_sql` moves the rollups too. There the original attribution cannot be recovered.

Confirmed with a scenario test in a scratch copy:

- Seat on acc-a (profile read 11-08), requests r1 on 11-09 and r2 on 11-10, both owned by acc-a.
- Switch to acc-b, whose saved profile is from 11-09.
- Result: `["r1|acc-b", "r2|acc-b"]`; `seat_accounts = [11-08 acc-a, 11-09 acc-b]`.

The limit calibration, per-account spend and "% of week" per project for both accounts are then wrong, and the result is stored as a fact.

#### Current

```rust
// src/index/mod.rs:702-711
let mut events: Vec<(i64, &str)> = Vec::new();
if let Some(t) = a.profile_fetched_at {
    events.push((ms(t), a.account_uuid.as_str()));
}
if let Some(u) = si.utilization.as_ref().filter(|u| u.account_uuid == a.account_uuid) {
    events.push((ms(u.fetched_at), u.account_uuid.as_str()));
}
// … later, for each new event:
tx.execute(&format!("UPDATE requests SET account_uuid = {SEAT_ACCOUNT_AT} WHERE seat = ?1 AND ts >= ?2"), …)?;
```

#### Recommended

```rust
// A seat's login change is an event with provenance, never a timestamp borrowed from a profile.
pub enum LoginSeen {
    /// The vault switched the seat at this instant (journal / logins.json `switched_at`).
    Switched { at: DateTime<Utc>, account: AccountId },
    /// First time swarmy sees this account in the seat: valid from now, not from the profile.
    Observed { at: DateTime<Utc>, account: AccountId },
}
// post_pass: events come only from LoginSeen; `profile_fetched_at` is used only when it is newer
// than the seat's last recorded change. Requests whose transcript names an owner keep it:
const SEAT_ACCOUNT_AT: &str = "COALESCE(requests.owner_account, (SELECT sa.account_uuid …), requests.account_uuid)";
```

**Lock it in**: a scenario test in `index/seat_scenarios.rs` where a vault switch restores a login captured earlier. The old account's requests and rollups must keep their account. A second test: requests with a `session-owner` record are never re-keyed by seat inference. Also a `CHECK`-like guard that rejects a `seat_accounts` row older than the seat's newest one unless its source is `Switched`.

---

### H2: A status-line reading is booked to whatever account the seat holds now

**Severity**: High
**Requirement**: 2. Value objects (a reading without its account), 3. Invariants
**Location**: `src/claude/statusline.rs:24-35` (`LiveLimits` has no account), `src/claude/statusline.rs:172-212` (`merge`), `src/app/context.rs:539-542`, `src/index/mod.rs:678-694` and `:707-711`

`swarmy statusline` stores `{at, config_dir, five_hour, seven_day}`. `seat_info_of` hands it to `merge` together with the seat's *current* account, and `merge` relabels the reading with that account.

Example: the seat ran acc-a when the status line wrote 80% at 09:00, and was switched to acc-b at 12:00. Claude Code's cached reading is older (still acc-a's). Then:

- the merged reading says "acc-b, 80%, 09:00";
- `post_pass` stores it in `readings`, a fact table;
- `post_pass` adds `seat_accounts(seat, 09:00, acc-b)`, which moves acc-a's requests from 09:00 to 12:00 to acc-b.

Confirmed by two independent scenario tests. One gives `requests = ["r1|acc-a", "r2|acc-b"]` and `readings = ["acc-b|80"]`. This lasts until acc-b's first response runs the status line again.

#### Current

```rust
// src/claude/statusline.rs:24-35, 206-208
pub struct LiveLimits {
    pub at: DateTime<Utc>,
    pub config_dir: String,
    pub session_id: Option<String>,
    pub five_hour: Option<(f64, DateTime<Utc>)>,
    pub seven_day: Option<(f64, DateTime<Utc>)>,
}
// merge(…)
Some(Utilization { account_uuid: account.to_string(), fetched_at: live.at, readings, … })
```

#### Recommended

```rust
#[derive(Serialize, Deserialize)]
pub struct LiveLimits {
    pub v: u32,                       // published language: versioned like fleet.json
    pub at: DateTime<Utc>,
    pub account: Option<AccountId>,   // oauthAccount.accountUuid read when the line ran
    pub session_id: Option<SessionId>,
    pub five_hour: Option<(Pct, DateTime<Utc>)>,
    pub seven_day: Option<(Pct, DateTime<Utc>)>,
}
pub fn merge(cached: Option<Utilization>, account: &AccountId, live: Option<LiveLimits>) -> Option<Utilization> {
    let live = live.filter(|l| l.account.as_ref() == Some(account)); // another account's reading is not ours
    …
}
```

**Lock it in**: the account is a required field of the type, so a reading cannot be built without one. Add a scenario test that switches a seat between a status-line write and the next refresh, and one that checks a reading from another account is ignored. Make `merge` take `&AccountId`, so the label cannot come from elsewhere.

---

### H3: A response whose blocks straddle `rolled_until` is counted twice when its file is read again

**Severity**: High
**Requirement**: 3. Invariants (each API response is counted once; compacted = never compacted)
**Location**: `src/index/write.rs:106-128` (per-record skip), `src/index/marks.rs:74-76` (`rolled_up`); introduced by 7949013 (read marks) on top of ba90725 (compactor)

Claude Code writes one response as several `assistant` records, one per content block, each with its own timestamp. The raw row keeps the first block's time, so the compactor rolls the whole response up when that time is older than the cut.

When the file is later read from its start, the skip is decided per record:

- the first block (older than `rolled_until`) is skipped;
- a later block of the same response, at or after `rolled_until`, is inserted as a new raw row;
- its `msg_id` is no longer in `requests`, so nothing deduplicates it;
- its tool calls are inserted again.

Files are read from the start on every `CACHE_VERSION` bump, a replaced or truncated transcript, and a fork. With 15–20 agents, a few responses are always mid-stream at a cut. Confirmed by test: request rows 3 → 3 after compaction → 4 after the re-read, cost 0.0031 → 0.0056.

#### Current

```rust
// src/index/write.rs:106-128
Event::Request(r) => {
    let t = ms(r.ts);
    …
    if rolled(t) {
        skipped += 1;
        continue;
    }
    late |= t < at.settled_before;
    *rowid += 1;
    ins_req.execute(params![r.msg_id, f.seat, f.session_id, f.agent_id, …, t, …])?;
```

#### Recommended

```rust
// Decide once per response, by the time the raw row would carry (its first block):
let first_ts = *first_seen.entry(r.msg_id.clone()).or_insert(t);
if rolled(first_ts) {
    skipped += 1;
    continue;
}
// …and across chunks: a msg_id already rolled up is looked up in the archive's msg set, or the
// compactor keeps a `rolled_msgs(msg_id)` table for the last two days of the cut.
```

**Lock it in**: give the scenario fixture (`index/scenarios.rs::assistant`) a multi-block form, as Claude Code really writes it. In `compact_scenarios.rs`, add a response straddling the cut, re-read from the start, and assert every surface still reads the same. A property test over random block splits and cut times would catch the whole class.

---

### H4: A switch that fails halfway leaves the seat on the new login while the vault records the old one

**Severity**: High
**Requirement**: 9. One pipeline (the transaction is all or nothing), 3. Invariants (one grant is never in two seats)
**Location**: `src/vault/mod.rs:648-721`; the `edit` doc (`src/vault/mod.rs:186-206`) promises that nothing staged is saved when the closure fails

`switch` calls `self.save()` in step 1, then writes `.credentials.json` in step 3, then `.claude.json` in step 4, and records `mounted` only in step 5.

If step 4 fails (disk full, or `$HOME` not writable for the default seat), the error is reported. But:

- the seat now holds login B's refresh token while `.claude.json` names A;
- the vault still says A is mounted there and B is free.

`sync` never re-mounts a login just because its fingerprint matches, so B is offered for other seats. A second `switch(other, "B")` then succeeds without `--share`, which is exactly the state `SharedGrant` exists to prevent. Confirmed by test in a scratch copy, with the default seat's dir set to 0500. No token is lost (step 1 saved the old one).

#### Current

```rust
// src/vault/mod.rs:674-699
self.save()?;                                   // 1. persisted
…
atomic_write(&seat.credentials_file(), new_creds.as_bytes())?;   // 3. seat changed
if let Some(acct) = &target.oauth_account {
    store::rewrite_merging(&seat.claude_json, |t| config_json::with_oauth_account(t, Some(acct)))?; // 4. may fail
}
// 5. Bookkeeping (lost when 4 fails)
```

#### Recommended

```rust
// Stage both seat files first, then commit them; on any failure put the backups back.
let creds = fsutil::stage(&seat.credentials_file(), new_creds.as_bytes(), Mode::Private)?;
let cj = store::stage_merging(&seat.claude_json, |t| config_json::with_oauth_account(t, Some(acct)))?;
creds.commit(&seat.credentials_file())?;
if let Err(e) = cj.commit(&seat.claude_json) {
    restore_backup(&bdir, seat)?;               // the seat is as it was before the switch
    return Err(e);
}
// and in `sync`: a seat whose fingerprint equals a saved login's marks that login mounted there.
```

**Lock it in**: a vault test that makes the `.claude.json` write fail and asserts the seat's files equal the backup and the vault still says A is mounted. A `sync` test: a seat holding B's fingerprint makes B `mounted` there, so `SharedGrant` fires.

---

### M1: The settled-sum cache cuts the newest minutes off the limit curve

**Severity**: Medium
**Requirement**: 3. Invariants (a kept part plus a fresh part equals a fresh sum)
**Location**: `src/index/query.rs:532-556` (`spend_series`); introduced by 6ea15cb ("settled parts of long sums kept")

`n` (the number of buckets) depends on `span.end`, but the kept part's key is `series|{bucket}|{filter}` plus the span start. `limits::load_window` calls it with a fixed window start, 1-minute steps over a 5-hour window, and `end = now + 1s`.

Within one settled horizon (up to 15 minutes), the kept vector stays at the first call's length, and `zip` cuts the sum to it. So the curve and its projection lose up to about 14 minutes. Confirmed by test: lengths 180, then 180 again five minutes later (185 expected), 185 on a fresh index.

#### Current

```rust
let n = (((ms(span.end) - origin) as f64) / bucket as f64).ceil().max(1.0) as usize;
let parts = self.settled_read(&format!("series|{bucket}|{f:?}"), span, |c, s| series_on(c, s, origin, bucket, n, f))?;
Ok(match parts {
    (Some(mut a), b) => { for (x, y) in a.iter_mut().zip(b) { *x += y; } a }
    (None, b) => b,
})
```

#### Recommended

```rust
Ok(match parts {
    (Some(a), b) => {
        let mut out = vec![0.0; n];                        // length from this call's span
        for (o, x) in out.iter_mut().zip(a.iter().chain(std::iter::repeat(&0.0))) { *o += x; }
        for (o, y) in out.iter_mut().zip(b) { *o += y; }
        out
    }
    (None, b) => b,
})
```

**Lock it in**: a `settled.rs` test that calls each kept sum at t and at t + 5 min and compares it with a fresh index (lengths and values). Generalise it: every `settled_read` caller is checked "kept + fresh = fresh" at two instants.

---

### M2: Two archive merge rules disagree with the raw path

**Severity**: Medium
**Requirement**: 3. Invariants (compacted = never compacted), 8. Declare once
**Location**: `src/index/archive.rs:378-400` (`rekey_sql`), `src/index/archive.rs:181-187` (`priced` as `MAX`); introduced by 4c40df7 and cf1dd50

- **Re-key double count.** The `DELETE` re-evaluates `account_at(first_ts)` after the `INSERT … ON CONFLICT` has merged rows and lowered their `first_ts` (`MIN`). A row that was itself a target no longer matches and stays. Two login changes in one quarter hour older than `rolled_until` gave Z = 11, W = 10 (21) instead of 11. Confirmed by test.
- **Unpriced hidden.** A zero-token request of an unknown model is priced at $0 (`priced = 1`); one with tokens is not (`priced = 0`). In one rollup key, `MAX` gives 1, so `n_unpriced` goes from 1 to 0 after compaction. Confirmed by test.

#### Current

```rust
Sum { col: "priced", ddl: "INTEGER NOT NULL CHECK (priced IN (0, 1))",
      from_raw: "MAX(priced)", merge: "MAX(priced, excluded.priced)", as_raw: &[] },
…
format!("DELETE FROM usage_rollup WHERE {}", moved("usage_rollup")),
```

#### Recommended

```rust
// Count, don't flag: the archive keeps how many requests had no price.
Sum { col: "n_unpriced", ddl: "INTEGER NOT NULL CHECK (n_unpriced >= 0)",
      from_raw: "SUM(priced = 0)", merge: "n_unpriced + excluded.n_unpriced", as_raw: &[] },
// Re-key: select the rows to move into a temp table first, then insert from it and delete exactly
// those keys (rowid list), so the merge cannot change which rows the delete sees.
```

**Lock it in**: a property test over random worlds (models with and without prices, login changes at random times, random cuts) asserting `total`, `n_unpriced` and `spend_by(Account)` are equal before and after `compact` and after a re-key.

---

### M3: Records are dropped silently: unreadable chunks and never-indexed sessions in a compacted seat

**Severity**: Medium
**Requirement**: 3. Invariants (nothing fails silently)
**Location**: `src/index/mod.rs:600` (`let Ok(chunk) = chunk else { continue };`), `src/index/marks.rs:64-69` (rules 2 and 3); rule 3 introduced by 7949013

A chunk that fails to parse is dropped with no count, no `diag::log` and no `problems` entry.

The read-mark rules then make such losses permanent. In a seat that has been compacted, *any* file of a session swarmy has never indexed is read with its records older than `rolled_until` skipped. That covers:

- a transcript synced from another machine;
- a file whose last read failed;
- a subagent file of a known session that was never read.

Only `skipped_records` shows the difference, and nobody reads it. The rule query also ignores the seat (`WHERE session_id = ?1`).

#### Current

```rust
// src/index/marks.rs:64-69
if self.rolled_seats.contains(&file.seat) {
    return Ok(true);
}
conn.prepare_cached("SELECT 1 FROM agents WHERE session_id = ?1 LIMIT 1")?
    .exists([&file.session_id])
```

#### Recommended

```rust
// Skip only what the archive provably holds: records of this (seat, session, agent).
conn.prepare_cached(
    "SELECT 1 FROM agent_rollup WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3 LIMIT 1",
)?
.exists(params![file.seat, file.session_id, file.agent_id])
// mod.rs: a failed parse is a counted, reported problem, never a silent `continue`
let chunk = match chunk { Ok(c) => c, Err(e) => { stats.unreadable.push((w.file.path.clone(), e)); continue } };
```

**Lock it in**: a compact scenario that adds a never-seen session with old records to a compacted seat and expects them in the archive. Add `unreadable` to `RefreshStats`, shown in `doctor` and ATTENTION, with a test that a corrupt chunk is reported.

---

### M4: A cache bump assumes every raw day's transcript is still on disk

**Severity**: Medium
**Requirement**: 3. Invariants, Step 12 data lifecycle
**Location**: `src/index/schema.rs:411-437` (raw tables dropped), `src/index/mod.rs:451-455` (`roll_late` only when a file was read from its start), `src/claude/seats.rs:68-74` (`cleanup_period_days` returns `None` on any read or parse error)

Raw rows newer than `rolled_until` are the only record once their transcript is gone. A `CACHE_VERSION` bump drops them, relying on `keep_raw_days ≤ retention − 2` (`core::storage`). Three paths break that assumption:

- A seat whose dir was removed or moved loses its last 14 days on the next bump.
- An unreadable `settings.json` silently counts as the 30-day default. A seat set to `cleanupPeriodDays: 7` with a damaged file passes the check.
- Old records appended to a known file (a clock far behind) stay raw below `rolled_until` until the next compaction. A bump in between skips them as rolled up. `docs/design/storage.md` admits this gap.

#### Current

```rust
pub fn cleanup_period_days(&self) -> Option<u32> {
    let text = std::fs::read_to_string(self.dir.join("settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let days = v.get("cleanupPeriodDays")?.as_f64()?;
    …
}
```

#### Recommended

```rust
// Before dropping raw tables: roll up every raw row whose transcript is not on disk.
if rebuild {
    compact::roll_orphans(&tx, prices)?;   // rows whose `files.path` no longer exists, and rows below rolled_until
    for t in RAW_TABLES { tx.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?; }
}
pub fn cleanup_period_days(&self) -> Result<Option<u32>, SettingsError> { … } // damaged ≠ unset
```

**Lock it in**: a scenario that removes a seat dir, bumps `CACHE_VERSION`, and expects the same totals. A test that a damaged `settings.json` makes the `keep_raw_days` check fail with a coded error.

---

### M5: The status line restamps stale numbers as fresh

**Severity**: Medium
**Requirement**: 3. Invariants (a reading's time is when its numbers were true)
**Location**: `src/claude/statusline.rs:672-686`

`live_from(&input, &dir, now)` sets `at = now`, and the file is rewritten whenever the numbers differ or five minutes have passed. `swarmy setup` adds `refreshInterval`, so idle sessions run the line every few seconds. Their `rate_limits` come from the process's last response, which can be hours old.

- **One idle session:** its old numbers become a "fresh" reading every five minutes. `merge` then prefers them over Claude Code's newer cached fetch, and `post_pass` stores them.
- **Two sessions on one seat (one idle at 40%, one active at 55%):** they overwrite each other's file. The stored readings alternate, so the week estimate and calibration jump, and `limit_events` can announce a crossing again on each upward flip.

#### Current

```rust
let now = Utc::now();
if let Some(live) = live_from(&input, &dir, now) {
    let old = read(&file);
    if old.as_ref().is_none_or(|o| o.five_hour != live.five_hour
        || o.seven_day != live.seven_day || (now - o.at).num_minutes() >= 5) && let Err(e) = write(&file, &live)
```

#### Recommended

```rust
// Keep the time the numbers were first seen; a repeat never moves `at` forward, and a reading
// older than the file's newest is never written back over it.
let at = match &old { Some(o) if o.same_numbers(&live) => o.at, _ => now };
if old.as_ref().is_some_and(|o| o.at > at) { return; }
// and keep per-session files (`<seat>/<session>.json`); the dashboard takes the newest per account.
```

**Lock it in**: tests on `run`'s pure core. Two sessions with different numbers must leave the newest reading. A repeat must not change `at`.

---

### M6: A login's identity is the hash of a token the code itself says rotates

**Severity**: Medium (High if Claude Code rotates refresh tokens)
**Requirement**: 2. Value objects (identity), 1. Ubiquitous language
**Location**: `src/claude/credentials.rs:57-81` (fingerprint = SHA-256 of `refreshToken`), `src/vault/mod.rs:603-612` (`sync`), vs `src/vault/mod.rs:3-5` and `src/errors.rs:24-28` ("Claude Code rotates the refresh token when it refreshes")

`sync` treats a new fingerprint in a seat as "another login now lives here". It unmounts the saved login and keeps its old tokens. If refresh tokens rotate, as the module doc and `SharedGrant` say, every refresh does three things:

- unmounts the login;
- marks the seat unverified, so the dashboard drops its account and limit reading (`drop_unconfirmed`);
- makes the next switch auto-save a duplicate login without an account, and lets a later switch back write a dead token.

A scratch-copy test reproduces this given that premise. The code holds two contradictory models of one concept. One of them is wrong, and the owner needs to settle which.

#### Current

```rust
// src/vault/mod.rs:603-612
Some(_) => {
    // Another login now lives in that seat; ours keeps its own tokens.
    if let Some(l) = self.index.find_mut(&mounted.id) { l.mounted = None; }
```

#### Recommended

```rust
// Grant identity = what does not change on refresh: the account plus the grant's issue time
// (refresh_expires_at − lifetime), or the access token's `sub`/`jti` if present.
pub struct GrantId { account: AccountId, issued: DateTime<Utc> }
Some(sec) if sec.grant_id() == mounted.grant_id => { /* same grant, rotated: keep the copy current */ }
```

**Lock it in**: a vault test "a rotated refresh token with the same account keeps the login mounted and current". A short note in `CLAUDE.md`'s format notes recording the verified rotation behaviour of the Claude Code version.

---

### M7: `.credentials.json` is overwritten without a concurrent-write check

**Severity**: Medium
**Requirement**: 9. Pipeline (one mechanism for writing a file another program also writes)
**Location**: `src/vault/mod.rs:679` and `:691-692` (plain `atomic_write`), `src/vault/store.rs:58-81` (`rewrite_merging`: `change(&text)?` aborts on a torn read; the 8th attempt writes anyway)

Only `.claude.json` gets the stamp-and-retry loop. `.credentials.json` is read at step 2 and replaced at step 3, after several fsyncs (vault save, backups). A token refresh by Claude Code inside that window (claude.ai or `mcpOAuth`) is overwritten. The newer token survives only in the step-2 backup.

The CLI refuses to switch while sessions run unless given `--force`. The TUI's `do_switch` (`src/tui/vault_ui.rs:154-164`) only warns in its confirmation text.

#### Current

```rust
let cred_text = store::read_optional(&seat.credentials_file())?;
…  // backups, fsyncs
let new_creds = credentials::with_login(cred_text.as_deref(), &secret)?;
atomic_write(&seat.credentials_file(), new_creds.as_bytes())?;
```

#### Recommended

```rust
store::rewrite_merging(&seat.credentials_file(), |t| credentials::with_login(Some(t), &secret))?;
// rewrite_merging: a parse error retries from the newer text; after ATTEMPTS it fails
// (`VaultError::SeatBusy`) instead of writing over the newest version.
```

**Lock it in**: one helper for "files Claude Code also writes", plus an architecture test that `credentials_file()` and `claude_json` are written only through it. The vault concurrency test can then rewrite `.credentials.json` as well.

---

### M8: The strict lint baseline is still 2,157 warnings away

**Severity**: Medium
**Requirement**: Static typing and analysis
**Location**: `Cargo.toml` `[lints]`; worst files `src/tui/boot.rs` (202), `src/demo.rs` (137), `src/tui/view/mod.rs` (84), `src/core/limits.rs` (64), `src/tui/view/panels/limits.rs` (61), `src/index/insights.rs` (60)

`[lints]` already denies `unwrap_used`, `expect_used`, `panic`, `unsafe_code`, `dead_code` and `allow_attributes_without_reason`, and `cargo clippy --all-targets -D warnings` is clean. The Step 11 strict run (`-W clippy::pedantic` plus the restriction lints) reports 2,157 distinct warnings, 140 of them in test files. Counted from `--message-format=json`; the short format prints the lint name only once per lint, so grepping it gives 2 of each.

| Lint | Count | Lint | Count |
|---|---|---|---|
| `as_conversions` | 445 | `cast_lossless` | 41 |
| `indexing_slicing` | 394 | `default_trait_access` | 33 |
| `float_arithmetic` | 258 | `let_underscore_must_use` | 28 |
| `must_use_candidate` | 184 | `wildcard_enum_match_arm` | 25 |
| `missing_errors_doc` | 133 | `many_single_char_names` | 17 |
| `cast_precision_loss` | 103 | `redundant_closure_for_method_calls` | 16 |
| `cast_sign_loss` | 100 | `needless_pass_by_value`, `single_match_else`, `wildcard_imports` | 14 each |
| `cast_possible_truncation` | 92 | `assert_is_empty` | 11 |
| `map_unwrap_or` | 55 | others (26 lints) | 117 |
| `cast_possible_wrap` | 47 | `too_many_lines` | 46 |

By module (`as_conversions` / `indexing_slicing` / lossy casts):

| Module | as / index / casts |
|---|---|
| `tui` | 230 / 163 / 138 |
| `index` | 90 / 12 / 89 |
| `demo` | 49 / 24 / 38 |
| `core` | 21 / 22 / 23 |
| `ui` | 9 / 9 / 11 |
| `cli` | 14 / 12 / 15 |
| `live` | 8 / 11 / 11 |
| `limits` | 0 / 13 / 0 |
| `vault` | 0 / 4 / 0 |
| `claude` | 5 / 43 / 3 |

No wrong-data path from a cast was found in `index`: tokens are far below `i64::MAX`, and the columns have `CHECK >= 0`. One exception: a transcript with a usage value above `i64::MAX` wraps to a negative (`write.rs:140-146`), fails the `CHECK` and aborts that refresh batch on every run. The fallback ratchet covers only `index|vault|app|cli|limits`. `claude` (100 fallback sites), `live` (37) and `tui` (141) are not counted.

#### Current

```toml
[lints.clippy]
allow_attributes_without_reason = "deny"
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
```

#### Recommended

```toml
[lints.clippy]
# … the above, plus lints that are cheap now:
let_underscore_must_use = "deny"       # 28: config.rs dir move, demo, terminal mouse capture
wildcard_enum_match_arm = "deny"       # 25: mostly crossterm KeyCode (allow there with a reason)
cast_possible_wrap = "deny"            # 47: u64 → i64 binds become i64::try_from
# ratcheted (count in tests/arch/ratchets.txt per module): as_conversions, indexing_slicing,
# cast_sign_loss, cast_possible_truncation, cast_precision_loss
```

**Lock it in**: a `clippy_counts` ratchet in `tests/arch/ratchets.txt`, fed from `cargo clippy --message-format=json` in `tools/check.sh`. Extend the fallback ratchet to `claude`, `live` and `tui`. Install `tools/check.sh` as the pre-push hook. It exists but is not installed.

---

### M9: Ids and money are half-adopted value objects

**Severity**: Medium
**Requirement**: 2. Value objects
**Location**: `src/core/ids.rs:15-50` (`SeatId: Default`, `from_stored` accepts any text, `Deserialize` bypasses both); 28 account-uuid fields and parameters typed `String`/`&str` (`limits/mod.rs:82,96,174,208`, `app/context.rs:33-34,299,329`, `index/query.rs:347,417,431`, `claude/config_json.rs:23,62`, `vault/mod.rs:37,58`, `live/mod.rs:49`, `index/live.rs:55`, `tui/mod.rs:102`, …); 18 money fields typed `f64` (`live/mod.rs` 8, `index/live.rs` 3, `index/insights.rs` 2, `claude/transcript.rs` 2, `index/query.rs:138`, `fleet.rs:58`, `claude/statusline.rs` 1); `Reading.utilization: f64` next to `Pct`

`SeatId::default()` is an empty id. `from_stored` deliberately accepts a legacy label, so a `SeatId` can hold a label: the exact confusion the type was introduced to remove. The index decodes every `seat` column through `from_stored` (`src/index/mod.rs:170`).

`AccountId` is used at 23 sites, but the hot paths (`latest_readings`, `account_name`, `login_labels`, `AccountReport`) are keyed by `String`. That is how H2's `merge(…, Option<&str>, …)` could take a label from anywhere.

`Usd::new` accepts NaN and negative values. Money as `f64` is a documented decision (an estimate shown with ≈), so the finding is the missing parser and the raw fields, not floats as such.

Seat id stability also has two gaps:

- A relative `CLAUDE_CONFIG_DIR` taken from another process is canonicalized against swarmy's cwd (`claude/procs.rs:141-146`).
- A re-pointed symlinked seat dir gets a new id and strands `mounted`.

#### Current

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeatId(String);
pub fn from_stored(s: impl Into<String>) -> SeatId { SeatId(s.into()) }
```

#### Recommended

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct SeatId(String);
impl TryFrom<String> for SeatId {           // 16 lowercase hex digits, nothing else
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, IdError> { if Self::looks_like_id(&s) { Ok(SeatId(s)) } else { Err(IdError::Seat(s)) } }
}
pub enum StoredSeat { Id(SeatId), LegacyLabel(String) } // what an old row holds, decided once
```

**Lock it in**: compile-fail doctests showing a label cannot become a `SeatId` and an `&str` cannot be passed for an `AccountId`. A ratchet on `account_uuid: String` and `(cost|usd|spend|burn)…: f64` fields in production code. A `Usd::try_new` that rejects non-finite values.

---

### M10: The reader layer still renders, executes and depends on the fleet contract

**Severity**: Medium
**Requirement**: 5. Cohesion and coupling, 7. Layers
**Location**: `src/claude/statusline.rs` (932 lines: input parsing, ANSI rendering at `:326-646`, `run_previous` spawning the user's command at `:752`, `fleet::read_fresh` at `:689`, registry scan); `src/config.rs:11` (`use crate::claude::project::Alias`) with `claude → config`; `src/limits/mod.rs:96-140` (`ctx.mask`, `ctx.show_path` inside the analysis; `limits → app`)

`claude/` is the anti-corruption layer over Claude Code's files. The status line is a whole feature (a front end) living inside it, so the reader layer points up to `config` (a cycle) and to `fleet`. The limit analysis takes the composition root and masks project paths, so the analysis cannot run without `Ctx`, and a presentation choice changes its output keys.

#### Current

```text
claude -> config     (claude/statusline.rs: Dirs, StatuslineConf)
config -> claude     (config.rs: claude::project::Alias)
claude -> fleet      (claude/statusline.rs: fleet::read_fresh)
limits -> app        (limits/mod.rs: Ctx; mask applied to by_project labels)
```

#### Recommended

```text
src/statusline/{mod.rs (run), input.rs (ACL: Claude Code's stdin), render.rs (pure)}  -> claude, fleet, config, core
config: project aliases as its own type (config::ProjectAlias), translated into claude::project::Alias by app
limits::all_reports(inputs: LimitInputs, idx: &Index) -> Vec<AccountReport>   // no Ctx, no mask
ui::mask applied by cli/tui when printing
```

**Lock it in**: remove `claude -> config`, `config -> claude`, `claude -> fleet` and `limits -> app` from `tests/arch/edges.txt`. The edge test then fails on their return.

---

### M11: Orchestration has grown fat again

**Severity**: Medium
**Requirement**: 1. Tactical DDD (thin use cases), 9. Pipeline
**Location**: `src/tui/worker.rs:26` (`worker`, 352 lines; the file went 551 → 632 lines since the split in f023af8), `src/live/mod.rs:146` (`build`, 310), `src/tui/analytics.rs:44` (`analytics`, 265), `src/index/write.rs:37` (`write_file`, 235), `src/index/mod.rs:659-800` (`post_pass`: accounts, readings, login history, re-attribution, rollup re-key, owners, projects, pricing in one function)

The worker loop again holds the first refresh, compaction scheduling, storage samples, fleet writing and error handling inline. `post_pass` is where H1 lives: attribution rules sit between project assignment and pricing, with no seam to test them alone.

#### Current

```rust
pub(super) fn worker(mut ctx: Ctx, tx: Sender<Msg>, wants: Arc<Mutex<Wants>>, eco: Arc<AtomicBool>, force_eco: bool) {
    let send = |m: Msg| tx.send(m).is_ok();
    let mut idx = match Index::open(&ctx.dirs.index_file()) { … };
    … // 340 more lines: refresh, jobs, compaction, analytics, fleet, storage samples
```

#### Recommended

```rust
// One function per job, dispatched from the job table; the loop only ticks.
for job in jobs.due_now() {
    match job {
        Job::Hot => run_hot(&mut w)?, Job::Full => run_full(&mut w)?, Job::Compact => run_compact(&mut w)?,
        Job::Limits => run_limits(&mut w)?, Job::FleetKeepAlive => run_fleet(&mut w)?, …
    }
}
// index: attribution::events(seat_info, history) -> Vec<LoginSeen> (pure), applied by post_pass
```

**Lock it in**: a `too_many_lines` threshold (150) for `src/index`, `src/tui/worker.rs` and `src/live`, with a ratchet, plus a pure, table-tested `attribution` function.

---

### M12: Index errors are untyped

**Severity**: Medium
**Requirement**: 9. Errors from a catalogue
**Location**: `src/index/` (109 `anyhow::` uses, 4 `bail!`); `src/errors.rs:56-60` (`IndexError` has one variant, `NewerVersion`)

The catalogue covers the vault, config, setup and lookups well. The index, the largest module, returns `anyhow` for everything. So the dashboard cannot tell these apart, and shows each as the same text:

- "index busy" (another process's 10 s `busy_timeout` expired during a compaction or VACUUM);
- "index damaged";
- "disk full";
- "transcript unreadable".

#### Current

```rust
pub enum IndexError {
    #[error("the index was written by a newer swarmy ({what} version {version}); update swarmy")]
    NewerVersion { what: &'static str, version: u32 },
}
```

#### Recommended

```rust
pub enum IndexError {
    NewerVersion { what: &'static str, version: u32 },
    #[error("another swarmy is writing the index (compaction or VACUUM); try again")] Busy,
    #[error("the index is damaged: {0}")] Damaged(String),
    #[error("no space left for the index at {0}")] NoSpace(PathBuf),
}
impl From<rusqlite::Error> for IndexError { /* map SQLITE_BUSY, CORRUPT, FULL */ }
```

**Lock it in**: the `errors.rs` code-uniqueness test covers the new variants. Add a test that a held write lock makes `refresh` return `IndexError::Busy`.

---

### L1: Some account changes do not bump the settled generation

**Severity**: Low
**Requirement**: 3. Invariants (settled = fresh)
**Location**: `src/index/mod.rs:767-781`

The owner-based and seat-fallback `UPDATE requests SET account_uuid = … WHERE account_uuid IS NULL` change the account of old, priced rows without `bump_settled`. `settled_total(account, span)` (keyed by a fixed span) then keeps a stale value for the life of the process.

#### Current

```rust
tx.execute_batch("UPDATE requests SET account_uuid = (SELECT o.account_uuid FROM owners o …) WHERE account_uuid IS NULL; …")?;
```

#### Recommended

```rust
let moved = tx.execute("UPDATE requests SET account_uuid = (…) WHERE account_uuid IS NULL AND ts < ?1", [settled_before])?;
if moved > 0 { bump_settled(&tx)?; }
```

**Lock it in**: one helper `reattribute(tx, sql)` that always bumps the generation. Check it with the same `settled` test as M1.

---

### L2: `fleet.json` is written on every tick, and `runs_out_at` drifts

**Severity**: Low
**Requirement**: 3. Invariants (battery budget)
**Location**: `src/tui/worker.rs:232`, `src/tui/worker.rs:556`; same pattern at `src/tui/alerts.rs:270-279`

`runs_out_at = ctx.now + eta` adds an eta computed at the last limits pass (every 55 s) to each tick's clock. The time therefore drifts later by up to a minute, and `changed` is always true while any seat has an eta. Every tick then pays two fsyncs, instead of one write every 5 s keep-alive.

#### Current

```rust
runs_out_at: week.and_then(|w| w.eta).map(|d| ctx.now + d),
…
let changed = Some(&fl) != fleet_last.as_ref();
```

#### Recommended

```rust
runs_out_at: week.and_then(|w| w.runs_out_at),   // an instant computed once in core::limits
```

**Lock it in**: return an instant (`runs_out_at: DateTime<Utc>`) from `core::limits`, not a duration. Add a worker test that two ticks with unchanged inputs produce an equal `Fleet`.

---

### L3: Status-line width counts ANSI escapes and cuts by characters

**Severity**: Low
**Requirement**: 4. Integrity of the published output
**Location**: `src/claude/statusline.rs:727-729`, `:639`

The user's previous status line is measured with `UnicodeWidthStr::width`, which counts escape sequences as visible columns (about 5 per escape). Segments are then dropped though they fit. The second line is cut with `chars().take(width)`, so a session name in Chinese, Japanese or Korean comes out up to twice the allowed width.

#### Current

```rust
.map(|t| unicode_width::UnicodeWidthStr::width(t.trim_end()) + 3)
```

#### Recommended

```rust
.map(|t| visible_width(t.trim_end()) + 3)   // strips CSI sequences, then unicode width
// second line: ui::fmt::truncate_to_width(s, width)
```

**Lock it in**: render tests with a coloured previous line and a session name in Chinese, Japanese or Korean.

---

### L4: Read paths take the vault's write lock, and its concurrency test is flaky

**Severity**: Low
**Requirement**: 4. Reads cannot write; 10. Tests
**Location**: `src/app/context.rs:179-204` (`drop_unconfirmed` → `v.edit(|tx| tx.seat_states(…))` on every `Ctx::load` and `reload_seat_info`), `src/tui/analytics.rs:87`, `src/vault/tests.rs:251`

`swarmy report`, `doctor` and every dashboard reload take the vault's OS lock and may rewrite `logins.json` and `secrets.json`. `save` writes only on change, but `seat_states` updates seat bookkeeping. A read-only command can therefore fail with `VaultError::Locked` when another process holds the lock; it is reported as a problem, not shown as an error.

In this review's full test run, `concurrent_changes_never_lose_a_login_or_corrupt_a_file` failed once while other builds loaded the machine, then passed five times in a row. A test that fails depending on machine load gets ignored.

#### Current

```rust
let states = v.edit(|tx| tx.seat_states(&self.seats))?;
```

#### Recommended

```rust
let states = v.read_view().seat_states(&self.seats);   // pure over a snapshot; bookkeeping only in commands that change the vault
```

**Lock it in**: give `Vault` a read type without `edit` (`VaultView`) that `Ctx` holds. In the test, scale `LOCK_TIMEOUT` and print the failing error, so a flake shows its cause.

---

### L5: API surface: a table name as `&str`, raw vault writers exported

**Severity**: Low
**Requirement**: 4. Minimal API surface
**Location**: `src/index/query.rs:495-501` (`pub fn count(&self, table: &str)` formats the name into SQL), `src/vault/mod.rs:16` (`pub mod store` exports `atomic_write`, `rewrite_merging`, `backup`, `restrict_dir`); 465 `pub fn` against 70 `pub(crate)`/`pub(super)`

Callers pass only literals today. Still, `count` is the one SQL builder that takes a name from outside, and `vault::store` lets any module write seat files without `Vault::edit`.

#### Current

```rust
pub fn count(&self, table: &str) -> anyhow::Result<u64> {
    Ok(self.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0))? as u64)
}
```

#### Recommended

```rust
pub enum Counted { Requests, ToolCalls, Sessions, Agents, UsageRollup }
pub fn count(&self, t: Counted) -> anyhow::Result<u64> { … t.table() … }
// vault/mod.rs: `mod store;` (private)
```

**Lock it in**: add `vault::store` to the architecture test's private-module list, and ban `format!` with `FROM {` outside `index/archive.rs` and `index/schema.rs`.

---

### L6: One concept, two names: `Pct` and "points"

**Severity**: Low
**Requirement**: 1. Ubiquitous language
**Location**: about 120 identifiers in `core/limits.rs`, `limits/mod.rs` and `tui/` (`usd_per_point` 31, `points_per_hour` 21, `visible_points`, `outside_points`, `limit_points` 15 each, `own_usd_per_point` 14) typed `Pct` / `UsdPerPct`; `burn`, `spend` and `cost` used side by side for API-equivalent dollars

The screen says "% of week" and "1% ≈ $X", and the type says `Pct`, but the fields say "points". It is harmless today, and it is the kind of drift the "mana" test already polices.

#### Current

```rust
pub usd_per_point: UsdPerPct,
pub points_per_hour: Option<Pct>,
```

#### Recommended

```rust
pub usd_per_pct: UsdPerPct,
pub pct_per_hour: Option<Pct>,
```

**Lock it in**: extend `limits_are_never_called_mana` into a forbidden-synonyms list (`mana`, `point`, `pt`) read from a glossary file.

---

### L7: Live caches: a new session shows Σ$0 for up to 10 s, and a second writer defeats them

**Severity**: Low
**Requirement**: 3. Invariants (battery budget, shown numbers)
**Location**: `src/live/mod.rs:380-389`, `src/index/mod.rs:327-338` and `:655`

A main row first seen on a light tick keeps `session_cost = 0`, because there is no `else` branch. The swarm panel shows Σ$0.00 and the fleet hides its burn until the next heavy tick.

Every refresh commits (`set_meta("refreshed_at")`). So with two dashboards, or a dashboard plus a CLI command, `PRAGMA data_version` moves every tick, `changes.all` resets, and every per-agent read runs each second. The numbers stay right; the battery budget does not.

#### Current

```rust
if let Some(o) = p.agents.iter().find(|o| o.is_main() && o.seat == a.seat && o.session_id == a.session_id) {
    a.session_cost = o.session_cost.max(a.cost);
    a.session_burn = o.session_burn;
}
```

#### Recommended

```rust
match p.agents.iter().find(|o| …) {
    Some(o) => { a.session_cost = o.session_cost.max(a.cost); a.session_burn = o.session_burn; }
    None => { a.session_cost = idx.session_total(a.seat, &a.session_id)?; } // read once for a new session
}
// and: write `refreshed_at` only when the refresh wrote rows
```

**Lock it in**: a `live` test with a session that appears on a light tick. Add a `tools/perf.sh --two` run with a second process refreshing the index.

---

## Roadmap

| # | Action | Findings | Effort | Impact |
|---|---|---|---|---|
| **1. Guardrails** | | | | |
| 1.1 | Install `tools/check.sh` as the pre-push hook (or a CI job); deny `let_underscore_must_use`, `wildcard_enum_match_arm`, `cast_possible_wrap`; add a per-module clippy-count ratchet; extend the fallback ratchet to `claude`, `live`, `tui` | M8 | S | High |
| 1.2 | Remove `claude ↔ config`, `claude → fleet` and `limits → app` from `edges.txt` as they are fixed | M10 | S | Medium |
| 1.3 | Multi-block responses in the scenario fixture; a property test for "compacted = never compacted" over random worlds, cuts and login changes | H3, M2 | M | High |
| **2. Correctness** | | | | |
| 2.1 | Login changes as events with provenance (vault switch time, first observation); `session-owner` wins over seat inference; no `profileFetchedAt` backdating | H1 | M | High |
| 2.2 | Account (and `v`) in `LiveLimits`; `merge` takes `&AccountId` and ignores other accounts' readings; keep the first-seen `at` | H2, M5 | S | High |
| 2.3 | Skip rolled-up records per response (first block), not per record | H3 | S | High |
| 2.4 | Make `switch` all or nothing (stage both seat files, restore the backup on failure); `sync` re-mounts by fingerprint; `.credentials.json` through `rewrite_merging`, which fails instead of overwriting | H4, M7 | M | High |
| 2.5 | `spend_series` sized from its own span; a two-instant test for every kept sum; bump the generation on every re-attribution | M1, L1 | S | Medium |
| 2.6 | `n_unpriced` as a count in the archive; re-key via a selected key set | M2 | S | Medium |
| 2.7 | Report unreadable chunks; read marks only for agents the archive holds; roll orphan raw rows before a cache bump; damaged `settings.json` is an error | M3, M4 | M | Medium |
| 2.8 | Settle and document the refresh-token rotation question; grant identity that survives rotation | M6 | S–M | Medium–High |
| **3. Structure** | | | | |
| 3.1 | Move the status line out of `claude/` into its own module with a pure renderer; aliases owned by `config` | M10, L3 | M | Medium |
| 3.2 | `limits::all_reports` over plain inputs (no `Ctx`, no mask) | M10 | S | Medium |
| 3.3 | One function per job in the worker; pure `attribution` out of `post_pass` | M11 | M | Medium |
| 3.4 | Typed `IndexError` (`Busy`, `Damaged`, `NoSpace`); `VaultView` for read paths | M12, L4 | S | Medium |
| **4. Model upgrades** | | | | |
| 4.1 | `SeatId` via `TryFrom` (no `Default`, legacy labels as `StoredSeat::LegacyLabel`); `AccountId` everywhere account uuids travel; `Usd::try_new` | M9 | M | Medium |
| 4.2 | `Counted` enum for `Index::count`; `vault::store` private | L5 | S | Low |
| 4.3 | `Pct` vocabulary in identifiers; glossary file with forbidden synonyms | L6 | S | Low |
| 4.4 | Fleet carries instants; live cache for new sessions; `refreshed_at` only on writes | L2, L7 | S | Low |

## Invariant map

| Invariant (business terms) | Enforced at | Write paths covered | Gap / finding |
|---|---|---|---|
| One API response is counted once | `requests.msg_id` primary key, merge by `max` | raw writes | raw vs archive: per-record skip double counts (H3) |
| A compacted index reads the same as one never compacted | `compact_scenarios.rs` on every surface | compactor, union reads | split responses (H3), re-key and `priced` merge (M2), settled series (M1) |
| A request is billed to the account that made it | `owners` (from transcripts), `seat_accounts` inferred from `profileFetchedAt` and reading times | `post_pass` | seat inference overrides owners and backdates a switch (H1); a stale status-line reading relabelled (H2) |
| A limit reading belongs to the account it was fetched for | Claude Code's cache carries `accountUuid`; the status-line file does not | `post_pass` readings insert | nowhere for status-line readings (H2) |
| A reading's time is when its numbers were true | none | `swarmy statusline` | restamped every 5 min (M5) |
| `cost` is NULL exactly when unpriced | `CHECK (CASE WHEN priced = 1 THEN cost IS NOT NULL ELSE cost IS NULL END)` | every write (DB) | archive keeps `MAX(priced)` (M2) |
| Tokens and costs are non-negative | `CHECK (… >= 0)` on every count and cost column | DB | none |
| A kept settled sum equals a fresh sum | settled generation bumped by reprice, re-attribution, late rows, seat adoption | most writers | owner/seat fallback updates (L1); key missing span end (M1) |
| Facts never die with a schema bump | four table lists; fact and archive tables never dropped; tests | `schema::migrate` | raw rows of gone transcripts (M4) |
| Raw detail stays re-readable from transcripts | `keep_raw_days ≤ retention − 2` (`core::storage`) | config load, `--keep-days` | unreadable `settings.json` counts as default; removed seat dirs (M4) |
| No record is dropped silently | `skipped_records` counter | refresh | parse errors `continue`; marks skip never-indexed sessions (M3) |
| Every saved login has its secret | `save` writes secrets before the index that adds them | `Vault::edit` | none |
| One OAuth grant is never mounted in two seats | `SharedGrant` check in `switch` | `switch` only | a failed switch frees the grant (H4) |
| A switch never loses a token | step 1 saves the seat's login first; backups | `switch` | concurrent `.credentials.json` refresh overwritten (M7) |
| A login stays the same login across refreshes | fingerprint of the refresh token | `sync`, `observe` | contradicts "refresh tokens rotate" (M6) |
| Vault and seat files are always whole | `fsutil::atomic_write` (unique temp, fsync file and dir), architecture test on renames | all writers | none |
| A seat's id is stable | SHA-256 of the canonical dir | discovery, status line | relative `CLAUDE_CONFIG_DIR`; re-pointed symlink (M9) |
| Limit thresholds have one definition | `core::limits::Level`, `limit_threshold_copies` ratchet = 0 | all front ends | none |
| A price applies from its date | dated `PriceBook`, `observed_prices.seen_on` with `CHECK (date(seen_on) IS seen_on)` | `price_all` (raw and rollups in one transaction) | none |

## Defects found along the way

| Severity | Location | Defect | Prevented by |
|---|---|---|---|
| High | `src/index/mod.rs:702-748`, `:48` | vault switch backdates the login change to the saved `profileFetchedAt`; days of the old account's requests and rollups move to the new one, overriding `session-owner` (confirmed by test) | login-change events with provenance; owner-first attribution |
| High | `src/claude/statusline.rs:172-212` | status-line reading relabelled with the seat's current account; wrong `readings` fact and re-attribution (confirmed by two tests) | account inside `LiveLimits` (a required typed field) |
| High | `src/index/write.rs:119-128` | split response straddling `rolled_until` inserted again on a re-read (confirmed by test) | per-response skip; multi-block fixtures |
| High | `src/vault/mod.rs:674-699` | failed `.claude.json` write after the credentials write: seat and vault disagree, `SharedGrant` bypassed (confirmed by test) | an all-or-nothing switch (stage, commit, restore) |
| Medium | `src/index/query.rs:538-550` | kept series shorter than the fresh one, so `zip` drops the newest buckets of the limit curve (confirmed by test) | length from the call's span; two-instant test |
| Medium | `src/index/archive.rs:383-400` | rollup re-key double counts when two changes fall in one quarter hour (confirmed by test) | re-key by selected key set; property test |
| Medium | `src/index/archive.rs:181-187` | `MAX(priced)` hides unpriced requests after compaction (confirmed by test) | `n_unpriced` count |
| Medium | `src/index/mod.rs:600`, `src/index/marks.rs:64-69` | parse errors dropped silently; old records of never-indexed sessions in a compacted seat skipped for good | reported `unreadable`; marks only for archived agents |
| Medium | `src/index/schema.rs:411-437`, `src/claude/seats.rs:68-74` | cache bump drops raw rows of gone transcripts; damaged `settings.json` passes the retention check | roll orphans before a bump; typed settings error |
| Medium | `src/claude/statusline.rs:672-686` | stale numbers restamped as fresh; two sessions on one seat flip the stored readings | first-seen `at`; per-session files |
| Medium | `src/claude/credentials.rs:57-81`, `src/vault/mod.rs:603-612` | grant identity = refresh-token hash, while the code says the token rotates | rotation-proof grant id; verified format note |
| Medium | `src/vault/mod.rs:679-692`, `src/vault/store.rs:58-81` | `.credentials.json` replaced without a change check; torn read aborts a half-done switch; 8th attempt overwrites | one merging writer that fails instead of overwriting |
| Low | `src/index/mod.rs:767-781` | account fallback updates without `bump_settled` | one re-attribution helper |
| Low | `src/tui/worker.rs:232`, `:556` | `fleet.json` rewritten every tick; `runs_out_at` drifts | instants from `core::limits` |
| Low | `src/claude/statusline.rs:727-729`, `:639` | ANSI counted as width; wide characters cut by count | visible-width helper |
| Low | `src/live/mod.rs:380-389` | new session shows Σ$0 until the next heavy tick | read once for a new session |
| Low | `src/index/write.rs:140-146` | a usage value above `i64::MAX` wraps, fails the `CHECK` and aborts the refresh batch on every run | `i64::try_from`, record skipped and counted |
| Low | `src/vault/tests.rs:251` | concurrency test failed once in six runs under load | load-tolerant timeout, error printed |
| Low | `src/tui/vault_ui.rs:470` | `let _ = remove_dir_all(&scratch)` can leave a live login in `vault/new-<ts>/` silently (the CLI path reports it) | `let_underscore_must_use = "deny"` |

## What is already good

- **Pure kernel, enforced.** `src/core` has no clock, files, database or environment (`tests/architecture.rs::core_is_pure`). Limit, pricing and storage rules are tested with numbers computed by hand in comments (`core/limits.rs`, `core/pricing.rs`, `core/storage.rs`).
- **Architecture as tests.** `tests/architecture.rs` checks module edges with a reason per line (`tests/arch/edges.txt`), six ratchets, SQL only in `index/`, renames only in `fsutil`, one word for limits, and no id slicing. These are the right kind of checks; extend them rather than replace them.
- **Declared once, derived.** `index/archive.rs` `KEYS`/`SUMS` generate the rollup DDL, the merge and the re-key SQL. `ToolClass`, `FileKind::TAGS` and `WindowKind` generate the `CHECK` lists. `tui/jobs.rs` holds every cadence, with tests pinning the measured values.
- **Data lifecycle by class.** Raw, dimension, archive and fact tables (`index/schema.rs`) with explicit migration steps. Facts survive cache bumps (test `a_login_swap_moves_later_requests_and_survives_a_cache_rebuild`). Prices are dated per local day.
- **Transactions where they matter.** Each compaction chunk is one immediate transaction covering inserts, deletes, `rolled_until` and the rowid high mark. Ingest re-checks offsets and `rolled_until` inside its write transaction. `price_all` reprices raw rows and rollups together.
- **The vault's lock and files.** `Vault::edit` holds a cross-process `flock` for the whole read-modify-write, re-reads under it, and saves only on change, with backups. Secrets are written before the index that names them. `fsutil::atomic_write` uses unique temp names, creates files 0600 before data and fsyncs file and dir. A damaged vault or seat file is an error, never "empty".
- **Error catalogue with stable codes** (`src/errors.rs`), tested for uniqueness and for surviving `anyhow` context.
- **Performance guarded by a tool.** `tools/perf.sh` measures CPU-seconds per minute on a realistic demo wall and fails above budget. The panel cache keys were checked and are complete: input, resize, eco, blink, theme and mask all invalidate correctly.
- **Value objects started right.** `SeatId`, `SessionId`, `AccountId`, `Usd`, `Pct`, `UsdPerPct` are `repr(transparent)`, serialize as the primitive, and `Usd + Pct` is a compile error (doctest in `core/units.rs`). `SessionId::short` cannot panic.
