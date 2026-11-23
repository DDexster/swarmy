# Bounded index storage: raw detail for two weeks, rollups for the years

Status: steps 1–8 of §9 implemented (see §11 for where the code differs from this text);
step 9 (a slimmer raw tier) is optional and not done. The design was written against `main` at 497c94a; the tables and
queries named here are the ones that matter.

## 0. Summary

- **Keep full per-request and per-tool-call detail for `keep_raw_days` (default 14).** After that,
  move it into **quarter-hour rollups** that keep every dimension a report groups by: seat,
  session, agent, account, cwd (so project), model, requested model, effort, fast, plus a
  long-context flag. Tool calls roll up by tool and command key. Edited files are kept as hashes,
  prompts and API errors as counts. The rollups are kept forever. A heavy user adds
  ≈ 150 MB a year. A typical user adds ≈ 50 MB.
- **No number changes on any screen or report that groups by period, day, hour, weekday, project,
  model, account, seat, effort, agent type, session or agent.** Three things make this hold:
  1. The rollups keep agent and session identity.
  2. Every span that reaches past the raw window starts on a quarter hour.
  3. Every limit figure that is shown reads the raw window, which always covers the current 7-day
     window.
- **What is lost after 14 days** is detail that no screen can reach today: an old agent's
  per-request chart and tool timeline (commands, file names), exact request times, API error
  texts, and the paths of transcripts Claude Code has deleted.
- **Projected size after 12 months:** heavy (25k requests/day) ≈ 0.48 GB instead of 8.7 GB; typical
  (5k/day) ≈ 115 MB instead of 1.75 GB; light (500/day) ≈ 17 MB instead of 175 MB.
- **Compaction is a job in the dashboard's job table** (every 6 h, a no-op unless a day aged out)
  **and `swarmy compact`.** Each 6-hour chunk is one transaction. Runs are idempotent and can be
  interrupted. A heavy day costs ≈ 1 CPU-second.
- **The rollups become a new table class, *archive*.** A `CACHE_VERSION` bump never drops it.
  **This also fixes a data-loss hazard that exists today:** a cache rebuild drops `requests`, and
  everything older than Claude Code's 30-day retention is then gone for good.

## 1. Facts this design rests on

**Claude Code's retention.** `cleanupPeriodDays` defaults to **30 days**. This was verified in
the 2.1.329 binary: the settings schema describes it as *"Number of days to retain chat transcripts
before automatic cleanup (default: 30). Minimum 1."* Desktop-host transcripts are exempt unless
`desktopSessionCleanupPeriodDays` is set. A user can raise the value per seat in `settings.json`.

**Consequence.** Once a transcript is deleted, the index is the only record of it. Today the
owner's index reaches back exactly to 2026-10-22, 31 days ago. Nothing older survives.

**Measured on a read-only copy of the owner's index (2026-11-22).** It covers 31 days: 285,000
requests and 297,972 tool calls, an average of 9.2k requests a day with peaks of 28k. The file is
68,489 pages of 4 KB, `auto_vacuum = 0` and the free list is 502 pages.

| object | MB | bytes per row |
|---|---|---|
| `requests` table | 77.3 | 271 |
| `requests_agent` (seat, session, agent, ts) | 26.0 | 91 |
| `requests_account_ts` | 16.9 | 59 |
| `requests` PK (msg_id) | 12.0 | 42 |
| `requests_ts` | 4.9 | 17 |
| **request, all in** | **137.1** | **481** |
| `tool_calls` table (summary avg 103 B, target avg 93 B) | 83.3 | 280 |
| `tool_calls_agent` | 27.3 | 92 |
| `tool_calls` PK (id) | 12.9 | 43 |
| `tool_calls_ts` | 5.1 | 17 |
| **tool call, all in** | **128.7** | **432** |
| owners + PK | 1.8 | 212 (8,535 rows) |
| files + PK | 1.2 | 470 (2,669 rows) |
| agents + PK | 0.7 | 250 (2,669 rows) |
| everything else | < 0.6 | |

There are 1.045 tool calls per request, so the raw cost is **≈ 933 B per request**, or 958 B per
request including the dimension tables. Requests come from 2,669 agents in 347 sessions, with 296
cwds, 5 models, 12 agent types, 3 accounts and 3 seats.

**Rollups were built on the same copy and measured.**

| rollup key | rows / month | MB | B / row |
|---|---|---|---|
| quarter hour × agent × account × cwd × model × requested × effort × fast × long-ctx | 12,051 | 2.95 | 245 |
| … same, with indexes on (seat, session, agent) and (account, bucket) | | +4.26 | (dropped, §3.6) |
| hour × agent × same dims | 5,305 | | |
| quarter hour without agent identity | 3,676 | | |
| tool: quarter hour × name × command key | 24,462 | 0.98 | 40 |
| distinct edited files per quarter hour (hashes) | ≈ 9–12k | ≈ 0.2 | 18 |

The rollup is lossless for money: `TOTAL(cost)` over the rollup equals `TOTAL(cost)` over the raw
rows, $26,602.206008885 both ways. In the measured month, **one rollup row replaces about 24
requests**.

**Cost of compaction, measured on the copy.**
- One 28k-request day: insert the rollup, then delete 28k requests and 28.6k tool calls. This took
  **0.52 s user + 0.60 s sys** (2.35 s wall including fsync).
- Compacting all 17 days older than 14 days, plus `incremental_vacuum`, took 0.83 s user + 0.84 s
  sys. The file shrank from 280 MB to 180 MB.
- The one-time `VACUUM` that switches to `auto_vacuum = INCREMENTAL` took 0.75 s user + 1.36 s sys
  (5.3 s wall) for 280 MB.

**Reset times are on quarter hours.** 249 of the 264 stored `seven_day` readings have `resets_at`
exactly on a quarter hour. The rest are within a second of one. So limit-window starts line up
with quarter-hour buckets.

**The long-context rule is per request and not linear.** `cost_with` in `src/core/pricing.rs`
applies multipliers when the prompt exceeds `LongContext::threshold`. The built-in Haiku rule uses
100,000. In the measured month, 174k of 285k requests had prompts above 200k. Token sums alone
therefore cannot be repriced; the rollup has to keep the long-context band as a key.

## 2. What every surface needs

Abbreviations: **R** means raw tables (`requests`, `tool_calls`). **U** means the union of raw and
rollup (§3.4). **"Exact"** means the same number before and after compaction.

| Surface | Query (file) | Span | Grain needed | After this design |
|---|---|---|---|---|
| SWARM live tree | `live_agent_rows`, `last_request`, `open_tool`, `last_tool_done` (`index/live.rs`, `live/mod.rs`) | agents active in the last 30 min, unfinished ones in the last 24 h | per request / tool call | R; always inside the raw window |
| agent and session totals in the tree | `agent_totals`, `session_spend` (whole session) | an agent's or session's lifetime | per agent | R + `agent_rollup` (§3.3); exact even for sessions resumed after weeks |
| lanes (30 min), 15-min spend spark | `agent_request_times`, `agent_tool_spans`, `agent_spend_per_minute` | 30 min | per request / tool call | R |
| recently done | `recently_done(now−6h)` | 6 h, cost over the agent's lifetime | per agent | R + `agent_rollup` |
| event log | `agent_events(now−3h)`, `api_errors_since`, `prompts_since` | 3 h | per event | R |
| AGENT panel | `agent_requests(limit)`, `agent_tool_calls(limit)`, `transcript_path` | an agent's last N requests and tools | per request / tool call, summary | R. **Lost for agents idle more than `keep_raw_days`.** No such agent can be selected today: the tree shows live agents and agents done in the last 6 h. |
| BURN | `spend_per_minute_by_model(now−60min)`, `spend_since` | 60 min | per minute | R |
| ATTENTION | `edit_conflicts(now−30min)`, `api_error_counts` | 30 min / 24 h | per tool call, target | R |
| LIMITS: current window | `total`, `spend_by(Project)`, `spend_series` over `window.start..now`; burn over 60 min (`limits/mod.rs::load_window`) | the current window: at most 7 days, starting on a quarter hour or at a manual reset | per request | R, because `keep_raw_days ≥ 8` is enforced |
| LIMITS: calibration | `calibration_points` → `total_cached` over (window start of reading, reading time) for ≤ 12 readings (`core/limits.rs`) | usually the current and previous window; older for an idle account | span ending at a reading | R while inside the window. Older spans read U with the start snapped to the quarter hour (§3.5). |
| PROJECTS | `spend_by(Project)` for the period, the previous period, today and the last hour (`tui/analytics.rs`) | today / 7d / 30d / all | project × period | U, exact |
| MODELS | `spend_by(Model/Effort)`, `matrix(Project, Model)`, `model_substitutions` | the period | model, effort, requested model | U, exact |
| HISTORY | `spend_by(Day, 14d)`, `days_by_model(14d)`, `matrix(Weekday, Hour, 28d)` (`tui/worker.rs` ≈ line 353) | 14 and 28 days | local day, local hour | U, exact (quarter hours nest in hours) |
| EFFICIENCY: tools and commands | `efficiency` (`index/insights.rs`): per tool name count, errors, busy time; Bash by `command_key(summary)` | the period | tool, command key | U: `tool_rollup` |
| EFFICIENCY: prompts, errors, compactions | `prompts`, `api_errors` by kind, `sessions.compactions` | the period | counts | U: `prompt_rollup`, `api_error_rollup`; `sessions` is kept |
| EFFICIENCY: tokens, cache, split | per-model sums of input, cw5, cw1h, cread, output and cost | the period | model | U, exact |
| EFFICIENCY: agent types | `agents` joined to cost per agent in the span | the period | agent | U (query rewritten, §3.4) plus `agents`, which is kept |
| EFFICIENCY: files edited | `COUNT(DISTINCT target)` of Edit/Write/MultiEdit/NotebookEdit | the period | distinct file | U: `edit_rollup` with 64-bit hashes |
| EFFICIENCY: fleet, conflicts | requests per hour; edits per file per hour | 24 h | per agent / target | R |
| `swarmy report` | `spend_by(Dim, span)`, `total` (`cli/report.rs`) | any period, including `all` and `FROM..TO` | all 11 `Dim`s, including Session and Agent | U, exact (§3.5) |
| `swarmy models` | `spend_by`, `matrix`, `model_substitutions`, `total` | any | model × project | U |
| `swarmy limits` / `accounts` | `limits::all_reports` | the current window | | as LIMITS |
| `swarmy doctor` | `cost_crosscheck` (per-session cost against `cost_states`), counts, `api_errors`, `record_types` | the period | per session | U (rollup keeps session) |
| status line / fleet.json | from the live snapshot | live | | R |

### What can be dropped, and when

- **After `keep_raw_days`, from `requests`:**
  - **dropped:** `msg_id`, the exact `ts`, and the per-row token values;
  - **kept as quarter-hour sums per key:** all token columns, `tools`, `cost`, request counts and
    the priced status;
  - **kept as key columns:** `requested_model` (only when it differs from `model`), `fast`, and
    the long-context band.
- **After `keep_raw_days`, from `tool_calls`:**
  - **dropped:** `id`, `msg_id`, agent identity, `summary`, `target`, `done_ts`;
  - **kept as sums per (quarter hour, tool name, command key):** count, errors and busy time;
  - **kept for edits:** a hash of `target`, per quarter hour, so files edited can still be
    counted.
- **After `keep_raw_days`, the rest:**
  - `prompts` becomes counts and character sums per quarter hour;
  - `api_errors` becomes counts per (quarter hour, kind), and `text` is dropped;
  - `owners` keeps only the newest row per session;
  - `files` loses rows whose transcript no longer exists.
- **Kept forever** (small, and they label the rollups):
  - `agents`, about 250 B per agent: agent type, description, kind, first and last time;
  - `sessions`: title, compactions;
  - `projects`;
  - `cost_states`;
  - every fact table.

**Why there is no separate "drop text first" tier.** A tier that blanks only `summary` and
`target` (say after 7 days) would save ≈ 180 B per tool call. For a heavy user that is ≈ 33 MB
over the days it covers. It would need a `cmd` column in `tool_calls` so commands can still be
grouped. With a 14-day raw window this is not worth a second mechanism. The text goes when the row
goes. If someone wants the raw window to be 30–60 days, this tier becomes worthwhile (§9, step 8).

## 3. Tiers, rollups and how reads use them

### 3.1 Tiers

| tier | content | how long | class |
|---|---|---|---|
| raw | `requests`, `tool_calls`, `prompts`, `api_errors`, `owners`, `files`, `record_types` | `keep_raw_days` (default 14, minimum 8, maximum transcript retention − 2) | **raw**: dropped on a `CACHE_VERSION` bump and re-read from the transcripts |
| dimension | `sessions`, `agents`, `projects`, `cost_states` | forever | **dimension**: survives a bump; re-reads upsert into it with the existing `COALESCE`/`MIN`/`MAX` merges |
| archive | the rollups below, `rollup_seats`, `agent_rollup`, `storage_samples` | forever | **archive**: never dropped; `ARCHIVE_VERSION` migrations, like `FACTS_VERSION` |
| facts | `meta`, `seat_accounts`, `accounts`, `readings`, `observed_prices` | forever | unchanged |

`schema.rs` gets four lists instead of two: `RAW_TABLES`, `DIMENSION_TABLES`, `ARCHIVE_TABLES`,
`FACT_TABLES`. The test `every_table_is_either_cache_or_fact` becomes "every table is in exactly
one list".

**Why the raw window must be shorter than retention.** Then the raw tier is always fully
re-derivable from the transcripts still on disk. A cache rebuild drops the raw tables, keeps the
archive and the read marks (§4.3), and re-reads the files, skipping what is already rolled up. If
`keep_raw_days` were ≥ retention, a rebuild would lose the oldest raw days. The config check in
§5 refuses that.

### 3.2 Why the bucket is a quarter hour

- Every UTC offset in use today is a multiple of 15 minutes: India +5:30, Nepal +5:45, Chatham
  +12:45. A UTC quarter hour therefore falls inside one local hour and one local day in every
  zone, and `Dim::Day`/`Hour`/`Weekday` and dated prices stay exact.
- `query.rs::hourly` already groups by the UTC hour and converts it to local time in Rust. That is
  30 minutes off for half-hour zones today. Quarter hours are as good as or better than what is
  shipped.
- The cost is 2× the rows of hourly buckets (10.5k against 5.3k rows a month at agent grain). That
  is ≈ 1.5 MB a month for a heavy user.

### 3.3 Rollup schema (archive)

```sql
-- One row per quarter hour and key: the sums of the requests that were rolled up into it.
CREATE TABLE IF NOT EXISTS usage_rollup (
  bucket INTEGER NOT NULL,                       -- ts / 900000 (UTC quarter hour)
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,                        -- '' = main thread, as in requests
  account_uuid TEXT NOT NULL DEFAULT '',         -- '' = NULL in requests (PK columns cannot be NULL)
  cwd TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL,
  requested_model TEXT NOT NULL DEFAULT '',      -- only when it differs from model (model_substitutions)
  effort TEXT NOT NULL DEFAULT '',
  fast INTEGER NOT NULL CHECK (fast IN (0, 1)),
  long_ctx INTEGER NOT NULL CHECK (long_ctx IN (0, 1)),  -- prompt > lc_threshold
  requests INTEGER NOT NULL CHECK (requests > 0),
  input INTEGER NOT NULL CHECK (input >= 0),
  cw5 INTEGER NOT NULL CHECK (cw5 >= 0),
  cw1h INTEGER NOT NULL CHECK (cw1h >= 0),
  cread INTEGER NOT NULL CHECK (cread >= 0),
  output INTEGER NOT NULL CHECK (output >= 0),
  thinking INTEGER NOT NULL CHECK (thinking >= 0),
  web INTEGER NOT NULL CHECK (web >= 0),
  tools INTEGER NOT NULL CHECK (tools >= 0),
  cost REAL CHECK (cost IS NULL OR cost >= 0),   -- at meta.price_fp, like requests.cost
  priced INTEGER NOT NULL CHECK (priced IN (0, 1)),  -- never NULL: pending rows are not rolled up
  lc_threshold INTEGER NOT NULL DEFAULT 0,       -- the long-context threshold long_ctx was taken against (0: none)
  first_ts INTEGER NOT NULL,
  last_ts INTEGER NOT NULL,
  CHECK (CASE WHEN priced = 1 THEN cost IS NOT NULL ELSE cost IS NULL END),
  CHECK (first_ts / 900000 = bucket AND last_ts / 900000 = bucket AND first_ts <= last_ts),
  PRIMARY KEY (bucket, seat, session_id, agent_id, account_uuid, cwd, model,
               requested_model, effort, fast, long_ctx)
) WITHOUT ROWID;

-- Lifetime sums of an agent's rolled-up part: what the live totals add to their raw sums.
-- Derived from usage_rollup, rewritten for the agents a compaction, reprice or re-attribution touches.
CREATE TABLE IF NOT EXISTS agent_rollup (
  seat TEXT NOT NULL, session_id TEXT NOT NULL, agent_id TEXT NOT NULL,
  requests INTEGER NOT NULL, cost REAL NOT NULL, unpriced INTEGER NOT NULL,
  first_ts INTEGER NOT NULL, last_ts INTEGER NOT NULL, last_model TEXT NOT NULL,
  PRIMARY KEY (seat, session_id, agent_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS tool_rollup (
  bucket INTEGER NOT NULL,
  name TEXT NOT NULL,
  cmd TEXT NOT NULL DEFAULT '',                  -- insights::command_key(summary) for Bash, else ''
  calls INTEGER NOT NULL CHECK (calls > 0),
  errors INTEGER NOT NULL CHECK (errors >= 0),
  busy_ms INTEGER NOT NULL CHECK (busy_ms >= 0), -- SUM(done_ts - ts) where done_ts >= ts, as efficiency()
  PRIMARY KEY (bucket, name, cmd)
) WITHOUT ROWID;

-- Files edited (Edit, Write, MultiEdit, NotebookEdit), for COUNT(DISTINCT target).
CREATE TABLE IF NOT EXISTS edit_rollup (
  bucket INTEGER NOT NULL,
  file_h INTEGER NOT NULL,                       -- first 8 bytes of sha256(target), as i64
  PRIMARY KEY (bucket, file_h)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS prompt_rollup (
  bucket INTEGER PRIMARY KEY,
  prompts INTEGER NOT NULL, chars INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS api_error_rollup (
  bucket INTEGER NOT NULL, kind TEXT NOT NULL, n INTEGER NOT NULL,
  PRIMARY KEY (bucket, kind)
) WITHOUT ROWID;

-- Config dirs (canonical path) whose transcripts were indexed when a compaction ran (see §4.3).
CREATE TABLE IF NOT EXISTS rollup_seats (seat_dir TEXT PRIMARY KEY, seat TEXT NOT NULL);

-- One sample a day, for growth and projection.
CREATE TABLE IF NOT EXISTS storage_samples (
  day TEXT PRIMARY KEY CHECK (date(day) IS day),
  file_bytes INTEGER NOT NULL, raw_requests INTEGER NOT NULL, raw_tool_calls INTEGER NOT NULL,
  rollup_rows INTEGER NOT NULL
);
```

New `meta` keys:
- `archive_version`;
- `rolled_until`: epoch ms, a quarter-hour boundary. Every raw row older than it has been rolled
  up, except the one row named in §4.1.

The key columns and their mapping back to the `requests` columns are listed once in Rust (one
`const` with name, raw expression and rollup expression). The `INSERT … SELECT`, the union and the
tests are all generated from that list, so they cannot drift. This follows the way
`schema::window_kinds()` is generated from the enum.

### 3.4 Reads: one union, used only when a span reaches the archive

`query.rs::from_for` builds `FROM requests r` plus the joins. It becomes:

```sql
FROM (
  SELECT ts, seat, session_id, agent_id, account_uuid, cwd, model, requested_model, effort, fast,
         input, cw5, cw1h, cread, output, thinking, web, cost,
         1 AS n, (priced = 0) AS n_unpriced, (priced IS NULL) AS n_pending
    FROM requests WHERE ts >= :a AND ts < :b
  UNION ALL
  SELECT bucket * 900000, seat, session_id, agent_id, NULLIF(account_uuid, ''), NULLIF(cwd, ''), model,
         NULLIF(requested_model, ''), NULLIF(effort, ''), fast,
         input, cw5, cw1h, cread, output, thinking, web, cost,
         requests, CASE WHEN priced = 0 THEN requests ELSE 0 END, 0
    FROM usage_rollup WHERE bucket >= :a / 900000 AND bucket < :b / 900000
) r LEFT JOIN projects p … (only the joins the query refers to, as today)
```

- In `SUMS`, `COUNT(*)` becomes `SUM(r.n)`, `SUM(r.priced = 0)` becomes `SUM(r.n_unpriced)` and
  `SUM(r.priced IS NULL)` becomes `SUM(r.n_pending)`.
- Every `Dim::exprs` key works unchanged: `r.ts`, `r.cwd` → `projects`, `r.agent_id` → `agents`
  for the agent type, `r.session_id` → `sessions` for the title, `r.account_uuid` → `accounts`.
- Raw and rollup rows are **disjoint by construction**: the compactor inserts and deletes in one
  transaction. So the union needs no cutoff predicate.

**Fast path.** When `span.start >= rolled_until`, `from_for` emits plain `FROM requests r`. This
covers live, limits, burn, today and every span inside the raw window. The per-second and
per-minute work runs exactly the SQL it runs today, at zero added CPU.

**Queries rewritten** (all in `src/index/`):

| Query | Change |
|---|---|
| `spend_by`, `hourly`, `matrix`, `total_on`, `model_substitutions`, `spend_series` | via `from_for` |
| `insights.rs::days_by_model`, `efficiency` token split | read the union |
| `efficiency` tools and commands | raw `GROUP BY name` and Rust `command_key` loop, plus `tool_rollup` rows in the span, merged in the same `HashMap`s |
| files edited | distinct raw targets hashed in Rust, unioned with `edit_rollup.file_h`, in a `HashSet` |
| prompts | `prompts` + `prompt_rollup` |
| errors | `api_errors` + `api_error_rollup` |
| agent types | the correlated per-agent `SELECT TOTAL(r.cost) FROM requests …` becomes one `GROUP BY seat, session_id, agent_id` over the union, joined to `agents`. This is faster today too. |
| `live.rs::agent_totals`, `session_spend`, `recently_done`, `agent_events` | add the `agent_rollup` row, looked up by PK, only when `agents.first_ts < rolled_until` |
| `query.rs::cost_crosscheck` | per-session sums over the union |

**Indexes.** `usage_rollup` needs no secondary index:
- reports scan it by `bucket`, the PK prefix;
- per-agent lifetime reads go to `agent_rollup`;
- per-account reads are only for limit windows, which are raw.

The two indexes in the measurement (+4.3 MB a month, more than the table itself) are therefore
left out. A year of heavy use is ≈ 365k rollup rows. That is less than the 9M raw rows a year of
`all` would scan today.

### 3.5 Spans that reach into the archive start on a quarter hour

A rollup bucket cannot be split. A span `[a, b)` with `a < rolled_until` is exact only if `a` is a
multiple of 15 minutes (`b` is always in the raw window or a boundary).

| spans | how |
|---|---|
| `today`, `yesterday`, `month`, `FROM..TO`, `all` | local midnights or `MIN_UTC`: already aligned |
| `Period::Days(n)`, `Period::Hours(n)` (`core/period.rs::span`) | **start floored to the quarter hour, always**, whether or not anything is compacted. "Last 30d" then means "since the quarter hour 30 days ago". This is a one-time change of definition shipped as its own step. After it, compaction provably changes no number, and the rolling figure moves in 15-minute steps instead of smoothly at its far edge. |
| previous period (`tui/analytics.rs`: `start − (end − start)`) | floored the same way |
| HISTORY's `days_span` / `heat_span` (`tui/worker.rs`) | floored the same way |
| `Period::LimitWeek` and every limits span | not floored; they lie inside the raw window (`keep_raw_days ≥ 8` > 7-day window) |

**Calibration readings** (`limits::calibration_points`) are the one exception. They are older than
the raw window only when an account's 12 latest distinct readings are all older than 14 days,
that is, an idle account. Their spans start at a window start, which is on the quarter hour (§1),
or at a manual reset, which may not be. For a manual reset older than `keep_raw_days`, the start
is snapped down to its quarter hour. The effect is at most 15 minutes of spend in one of 12 inputs
to a median that is shown as an estimate (≈). For every account in use, nothing changes.

A `debug_assert!` in `from_for` checks that `a < rolled_until` implies `a % 900000 == 0`. Unknown
callers can therefore not silently get snapped numbers in tests.

### 3.6 Money: store cost at write time, reprice from token sums

**Rollup rows carry `cost`**, the exact `SUM(requests.cost)` of their rows at the current
`price_fp`. So compaction changes no number, not even in the last bit of the floating sum, since
the test compares with a 1e-9 relative tolerance.

**On a price change**, `prices.rs::price_all` already sets every raw row to NULL and reprices it
in one transaction. In the same transaction it also recomputes every rollup row with
`cost_with(rates(model, local_day(bucket)), sums, fast)`:
- the long-context multipliers are taken from the row's `long_ctx` flag instead of from
  `u.prompt() > threshold`;
- `cost_with` is linear in the tokens for a fixed band, so this is exact;
- the local day of a quarter hour is unique (§3.2), so dated prices stay exact;
- for a heavy user, a year is ≈ 365k rows, a few hundred ms. A price change is rare.

**Rows with `priced = 0`** (no price known) keep their tokens. A price that appears later (a
`[[price]]` in config) prices them like raw rows.

**The one inexact case.** A later price version changes a model's long-context threshold (`lc_threshold`
on the row differs from the new rule's threshold). Rows rolled under the old threshold are then
priced by their old band. `doctor` lists such (model, day) ranges. No such change has happened
yet. A row's band is decided per request at compaction, so a mix shows up as two rows.

**Rows that are never rolled up.** Rows still pending (`priced IS NULL`) or with
`account_uuid IS NULL` after `post_pass` stay raw. Compaction of that day waits (§4.1), and
`rolled_until` stops at the first day that waited.

### 3.7 Account re-attribution (`seat_accounts`)

> Superseded (third review, H1): a login change is dated when swarmy sees it, never at the
> profile's or a reading's time, and the transcripts' `session-owner` records decide a request's
> account (`seat_accounts` only fills in for sessions without one). Rollups are never re-keyed:
> they were attributed while their raw rows and owners existed, the archive cannot tell an
> owner's attribution from the seat's, and a change dated when seen is newer than
> `rolled_until`. `archive::rekey_sql` is gone. The original design follows.

`post_pass` in `index/mod.rs` runs
`UPDATE requests SET account_uuid = SEAT_ACCOUNT_AT WHERE seat = ? AND ts >= ?` when a seat's
login changes. The new `seat_accounts.ts` is the time the seat recorded the profile or reading.
That is normally now, so only raw rows move.

It can be older when swarmy did not run for weeks. In that case the rollup gets the same
treatment, re-keyed because `account_uuid` is in the PK:

1. Take the rows of the seat with `first_ts >= ts`.
2. Insert them again under the account in force at `first_ts`, with `ON CONFLICT … DO UPDATE SET`
   sums added.
3. Delete the old keys.
4. Rewrite the touched `agent_rollup` rows.

All of this happens in the same transaction. A row that straddles the switch
(`first_ts < ts <= last_ts`) keeps its account. That is at most 15 minutes of ambiguity, smaller
than the ambiguity the switch time already has.

**Ordering.** The compaction job runs only right after a full refresh and `post_pass`. A login
change seen in that refresh is therefore applied to raw rows before they are rolled up.

### 3.8 Projects

The rollup keeps `cwd`, not the project, so `[[project]]` aliases and `PROJECTS_REV` changes still
apply to old data through `projects`. Two changes keep that table right when the repositories are
long gone:

- **Source of cwds.** The recompute takes its cwds and the orphan "votes" from the union, not only
  from `requests`: `SELECT DISTINCT cwd` over both, and votes counting `usage_rollup.requests` by
  session.
- **Never downgrade.** A cwd whose directory no longer exists and that now resolves as an orphan
  with no votes keeps its previous mapping. The resolver cannot do better than what was recorded
  while the repository existed.

## 4. Mechanics

### 4.1 The compaction step

`src/index/compact.rs`, with `Index::compact(&mut self, opts) -> CompactStats`. The steps:

1. **Cutoff.** `cutoff = local_midnight(today − keep_raw_days)`. This is a quarter-hour boundary
   in every zone.
2. **No-op check**, microseconds: `SELECT MIN(ts) FROM requests` (index `requests_ts`) and the same
   for `tool_calls`, `prompts` and `api_errors`. If all are ≥ `cutoff`, return.
3. **Chunks of 6 hours, oldest first**, from the oldest raw row to `cutoff`. One `BEGIN IMMEDIATE`
   transaction per chunk `[c0, c1)`. The measured cost is ≈ 0.3 CPU-seconds per heavy chunk, so
   the hot refresh, which also writes, never waits more than a fraction of a second.
   1. **Guard.** If any request in the chunk has `priced IS NULL` or `account_uuid IS NULL`,
      roll back and stop. `price_all` and `post_pass` will fix it on the next refresh.
   2. **Requests.**
      `INSERT INTO usage_rollup SELECT <key exprs>, COUNT(*), SUM(…), TOTAL(cost), MAX(priced), MIN(ts), MAX(ts) FROM requests WHERE ts >= c0 AND ts < c1 AND rowid != (SELECT MAX(rowid) FROM requests) GROUP BY <key> ON CONFLICT(<pk>) DO UPDATE SET <sums> = <sums> + excluded.<sums>, first_ts = MIN(…), last_ts = MAX(…)`.
      - `long_ctx` and `lc_threshold` come from the price entry in force for the request's model
        and day. They are computed in Rust while reading the chunk, which also keeps the key
        logic in one place.
      - `ON CONFLICT` exists for rows of a seat discovered after its days were compacted (§4.3).
   3. **Tools.** The chunk's `tool_calls` are read in Rust. The tool name, `command_key(summary)`
      for Bash, errors and busy time go into `tool_rollup` (same `ON CONFLICT` add). The hash of
      `target` for edit tools goes into `edit_rollup` with `INSERT OR IGNORE`.
   4. **Prompts and errors** are summed into `prompt_rollup` and `api_error_rollup`.
   5. **Delete** those raw rows. Excluded is the single request row with `MAX(rowid)`, see the
      note below.
   6. **Prune `owners`**: rows of sessions whose `agents.last_ts < c1`, except the newest per
      session.
   7. **Prune `files`**: rows whose path no longer exists and whose `last_ts < c1`.
   8. Upsert `agent_rollup` for the agents touched.
   9. Insert every current seat's canonical config dir into `rollup_seats`, and set
      `meta.rolled_until = c1`.
   10. `COMMIT`.
4. **Free space.** `PRAGMA incremental_vacuum(4096)` in steps of 16 MB until the free list is
   empty, then `PRAGMA wal_checkpoint(PASSIVE)`, and `TRUNCATE` when no reader holds the WAL.
   `journal_size_limit = 32 MB` is set at open, so the WAL file shrinks after a checkpoint.
5. **Sample.** Write today's `storage_samples` row if it is missing.

**Idempotent.** Each raw row is either in the rollup or in the raw table, never in both and never
in neither. A second run finds nothing older than `cutoff`.

**Interruptible.** A crash or kill rolls back the open chunk. Earlier chunks are complete.
`rolled_until` only advances with a committed chunk.

**Why the `MAX(rowid)` row is excluded.** `refresh_files` relies on *"rows added by this refresh
have a larger rowid (requests are never deleted)"*: it reads `MAX(rowid)` before inserting and
attributes rows above it in `post_pass`. If the compactor deleted the highest-rowid row, for
example an old row backfilled from a newly found seat, a concurrent refresh could reuse a rowid
at or below the one it read and miss attributing its rows. Keeping that one row raw (the union
still counts it once) keeps the invariant. The comment in `refresh_files` must be changed to say
so. A cleaner alternative is a monotonic `meta.request_seq`; see the risks.

### 4.2 When it runs

**Dashboard.** A new row in `src/tui/jobs.rs::JOBS`:

```rust
// Rolls up what is older than [storage].keep_raw_days and frees the space; a no-op check
// otherwise. Runs right after a full refresh (prices and accounts are settled then).
Spec { job: Job::Compact, name: "compaction", every: secs(6 * 3600), eco: secs(6 * 3600),
       start: Start::Waits, needs: Needs::Heavy },
```

- It runs on the worker after `FullRefresh`. The first run is 10 minutes after start
  (`Start::Waits` with a shorter first delay, or simply the next full refresh after 10 min), so it
  never competes with the boot refresh. The job-table test pins the period, and the existing test
  keeps every `Heavy` job at ≥ 30 s.
- **Catching up a backlog** (the first run on an existing index, or after weeks away) processes at
  most 4 chunks (one day) per run. The job is then due again after 60 s until it reaches `cutoff`.
  The UI thread is never blocked: the worker yields between chunks, and the per-chunk transaction
  stays well under a second.
- **CPU.** Steady state is one day per day, ≈ 1.1 CPU-seconds for a 28k-request day as measured,
  plus the Rust `command_key` loop over ≈ 2.7 MB of Bash summaries, tens of ms. The battery budget
  is 2 CPU-seconds per *minute*, so this is noise. The three no-op runs a day cost microseconds.
  The one-time catch-up of the owner's index (17 days) is ≈ 15 CPU-seconds, spread over 17
  minutes.
- `[storage] auto_compact = false` removes the job and leaves only the CLI.

**CLI.** `swarmy compact` runs the same function in the foreground, with progress, and may run
while a dashboard is open. Both use `BEGIN IMMEDIATE` per chunk, and the steps are idempotent, so
two compactors only take turns. Other commands (`report`, `limits`, …) never compact; they stay
read-mostly.

**One-time switch to incremental auto-vacuum.** `auto_vacuum` is 0 today, and changing it needs a
full `VACUUM`.
- The dashboard does it once, after the first compaction, when the index has ≥ 25% free pages, and
  only on mains power (`src/power.rs`).
- `swarmy compact --vacuum` does it on demand.
- Measured cost for 280 MB: ≈ 2.1 CPU-seconds and 5.3 s wall. After the first compaction the file
  is ≈ 180 MB, so it costs less.
- `VACUUM` holds the write lock for those seconds. The hot refresh waits (`busy_timeout` is 10 s).
- `VACUUM` writes a temporary copy of the database. swarmy sets `SQLITE_TMPDIR` to its data dir
  before opening, so the copy never lands in a RAM-backed `/tmp`.
- After the switch, `meta.auto_vacuum = incremental` is recorded and the step never runs again.

### 4.3 Incremental indexing after compaction: never read a rolled-up record twice

Today a file is read from its stored offset. A file that shrank or changed inode is read again
from 0 (`refresh_files`, `Work::reset`). Re-read requests merge by `msg_id` (`MAX` of usages), so
nothing counts twice. After compaction the old `msg_id`s are gone. A re-read, or a new file that
copies old records, would insert them again and **double count** them against the rollup.

**The rule in `write_file`.** A `Request` event with `ts < rolled_until` is skipped, together with
its tool uses, when either of these holds:

1. **its session is already known**: a row in `agents` (dimension table, kept forever) with that
   `session_id`. This covers re-reads of known files, resumed sessions whose old part is in the
   file, subagent files of a known session, and a seat whose config dir was moved or relabelled.
   Its sessions keep their ids.
2. **its seat's canonical config dir is in `rollup_seats`**: the seat was indexed when a
   compaction ran. This covers a new file in a known seat that copies old records, such as a
   forked session (new session id, old `msg_id`s and timestamps).

Otherwise (a seat found for the first time, with sessions never seen) the records are new
information. They go into raw, and the next compaction rolls them up with the `ON CONFLICT` add
into existing buckets, then adds the seat to `rollup_seats`.

**Details:**
- `ToolResult` events for rolled-up tool ids update nothing, as today for unknown ids. A tool
  that finishes more than `keep_raw_days` after it started loses its busy time; that does not
  happen in practice.
- **`Work::reset` also needs care.** Its `UPDATE requests SET tools = 0 …` and the deletes of
  `prompts` and `api_errors` must be limited to `ts >= rolled_until`, because the re-read skips
  older records. The `sessions.compactions` reset-and-recount still reads the whole file, which is
  correct because compactions are not rolled up.
- `refresh_files` loads `rolled_until`, `rollup_seats` and, lazily, the known-session set once per
  refresh. The set is a `HashSet` of a few thousand session ids per month, cached on `Index` and
  extended as sessions appear.
- A `files` row pruned because its transcript is gone means nothing is left to re-read. If the
  file comes back (restored from a backup), it is read from 0 and rule 1 skips its old records.

**What this costs.** Records older than `rolled_until` in a genuinely new file of a known seat and
a new session, other than a fork, are skipped. Example: transcripts restored from a backup into a
seat. That is the conservative error: data missing, never counted twice. `doctor` counts skipped
records per seat, so this is visible.

### 4.4 Repricing and re-attribution, in one place

| event | raw | rollup |
|---|---|---|
| price fingerprint changes | `price_all` (unchanged) | same transaction: recompute `usage_rollup.cost` from sums (§3.6), rewrite `agent_rollup` |
| new raw rows | priced by `price_all` | not involved (pending rows are never rolled up) |
| login change seen at time t | `UPDATE requests …` for requests without an owner | untouched (§3.7, superseded) |
| `PROJECTS_REV` / aliases | `projects` recompute over the union (§3.8) | nothing stored per project |
| `CACHE_VERSION` bump | raw tables dropped and re-read from the transcripts (all still on disk, §3.1) | untouched; `write_file` skips records older than `rolled_until` (§4.3) |

`limits::total_cached` memoizes settled spans by `price_fp`. That stays correct: compaction
changes no total, and a reprice changes `price_fp`.

## 5. Config and visibility

`~/.config/swarmy/config.toml`:

```toml
[storage]
keep_raw_days = 14   # per-request and per-tool-call detail; then quarter-hour rollups, kept forever
                     # 8 .. (the shortest transcript retention of your seats − 2): the current weekly
                     # limit window must stay raw, and a cache rebuild must find every raw day on disk
budget_mb = 1024     # doctor and SYSTEM warn when the 12-month projection goes over this
auto_compact = true  # the dashboard compacts on its own; `swarmy compact` works either way
```

**Validation** happens in `config.rs`, with an error that names the limit:
- Retention per seat is `cleanupPeriodDays` from the seat's `settings.json`, else 30. It is read
  once with the seat info.
- The budget **only warns**. swarmy never deletes rollups on its own, and never shrinks
  `keep_raw_days` behind the user's back.

**`swarmy doctor`** gets a `storage` section. All figures come from `PRAGMA page_count`,
`freelist_count`, row counts × measured bytes per row, and `storage_samples`. `dbstat` is not used
because the bundled SQLite of `rusqlite` is built without it. Paths go through `ctx.show_path`, so
they are masked like the rest.

```
storage    index 412 MB (+ WAL 3 MB, 0.2 MB free) · auto-vacuum incremental
  detail   last 14 days: 351k requests, 367k tool calls ≈ 327 MB        keep_raw_days = 14
  history  since 2026-10-22 in rollups: 121k rows ≈ 84 MB
  growth   +0.42 MB/day over the last 14 days → ≈ 480 MB in 12 months  (budget 1024 MB)
  claude   transcripts are deleted after 30 days (cleanupPeriodDays, default) on 3 seats:
           older history exists only in this index
  last compaction today 06:02, 4 chunks, 1.1 s · skipped as already rolled up: 0 records
```

**Projection** = `raw_bytes` (constant once the raw window is full) + archive growth per day ×
365. Archive growth is the slope of `file_bytes − raw estimate` over the last 14 samples. Before
14 samples exist, it is estimated from the per-row sizes in §6.

**SYSTEM panel** (`tui/view/panels/system.rs`): one line under the disks,
`index 412 MB · +0.4 MB/day · 12 mo ≈ 480 MB`. It turns warning-coloured over the budget, with a
legend line in `view/help.rs::legend_lines` ("index: swarmy's own database; 12 mo ≈ where it will
be in a year at the current pace").

**`swarmy compact`:**

```
swarmy compact                  # roll up what is older than keep_raw_days and free the space
swarmy compact --dry-run        # what would move and how many bytes it would free; changes nothing
swarmy compact --keep-days 10   # once, with another raw window (≥ 8)
swarmy compact --vacuum         # also the one-time VACUUM that switches to incremental auto-vacuum
swarmy compact --verify         # on a scratch copy in the data dir (VACUUM INTO): snapshot every
                                # report, compact, snapshot again, print differences; the real index
                                # is not written
```

## 6. Size projections

**Per-row bytes**, measured (§1) unless marked as estimated:

| item | bytes | rows per day, light / typical / heavy |
|---|---|---|
| raw request + its 1.045 tool calls, with indexes | 933 | 500 / 5,000 / 25,000 |
| `usage_rollup` row | 245 | 60 / 330 / 1,000 (estimated: one row per 8 / 15 / 25 requests; measured 24 at 9.2k/day) |
| `tool_rollup` row | 40 | 120 / 500 / 1,500 (measured 790 at 9.2k/day) |
| `edit_rollup` row | 18 | 20 / 210 / 1,040 (measured 0.042 per request) |
| `agents` row (kept) | 250 | 5 / 47 / 234 (measured 0.0094 per request) |
| `sessions` + `cost_states` + newest `owners` row | ≈ 500 | 3 / 6 / 30 |
| `readings` (facts, with the status line) | 170 | 20 / 60 / 120 |
| prompts, errors, `agent_rollup`, samples | | < 5 KB/day |
| `files` (only transcripts on disk) | 470 | steady state: 30 days × agents/day |

**Archive growth per day:** light ≈ 29 KB (11 MB/year), typical ≈ 138 KB (50 MB/year), heavy
≈ 420 KB (154 MB/year).

**Raw window at 14 days**, constant once full: light 6.5 MB, typical 65 MB, heavy 327 MB, plus
`files`: 0.1, 0.7 and 3.3 MB.

| user | design | 1 month | 6 months | 12 months |
|---|---|---|---|---|
| light, 500/day | today (nothing ever deleted, 958 B per request all in) | 14 MB | 86 MB | 175 MB |
| | **keep_raw_days = 14** | **8 MB** | **12 MB** | **17 MB** |
| typical, 5k/day | today | 144 MB | 860 MB | 1.75 GB |
| | **keep_raw_days = 14** | **70 MB** | **91 MB** | **116 MB** |
| heavy, 25k/day | today | 720 MB | 4.3 GB | 8.7 GB |
| | **keep_raw_days = 14** | **343 MB** | **407 MB** | **484 MB** |
| | keep_raw_days = 30 | 712 MB | 776 MB | 853 MB |
| | keep_raw_days = 60 | 1.4 GB | 1.47 GB | 1.55 GB |

Check against reality: the owner's 31 days at 9.2k a day measure 273 MB, and the "today" model
gives 285k × 958 B = 273 MB. The simulated compaction of that index to 14 days gave 180 MB,
because the last 15 days held 193k requests (heavier than average). That matches
193k × 933 B ≈ 180 MB.

**Target.** A heavy user stays **under 0.5 GB after a year and grows ≈ 150 MB a year** after
that. This is well under the 3–4 GB a year the owner tolerates, and well under 1 GB a year. The
raw window is the larger part. Halving it is a config change, not a design change, and step 8
(§9) cuts it by a further ≈ 35% if ever needed.

**Query cost falls too.** `report -p all` after a year of heavy use scans ≈ 350k raw + 365k
rollup rows instead of ≈ 9M raw rows.

## 7. What each screen loses after compaction

| screen | after `keep_raw_days` |
|---|---|
| SWARM, BURN, ATTENTION, LIMITS (current window), event log | nothing: they read hours or the current week |
| AGENT panel | the per-request chart (cost per request, context growth) and the tool timeline with commands and file names, for agents idle more than `keep_raw_days`. The card (type, description, model, lifetime) and the totals stay. No screen selects such an agent today. |
| PROJECTS, MODELS, HISTORY, `report`, `models` (every `Dim`, every period) | nothing |
| EFFICIENCY | nothing for the shown figures. Commands are grouped by the `command_key` rules in force when the day was rolled up; a later change to those rules re-groups only the raw window. |
| `doctor` | API error texts and transcript paths of deleted transcripts; counts stay |
| LIMITS calibration | for idle accounts only: a manual reset older than `keep_raw_days` is snapped to its quarter hour (§3.5) |

Not possible for old data, and not shown anywhere today: tool statistics per agent, project or
account (`tool_rollup` has no such dimension); exact request times; individual `msg_id`s. Adding
a dimension to `tool_rollup` later costs rows only from that day on.

## 8. Migration and the tests that prove "no number changes"

### 8.1 Migration of existing indexes

1. **`ARCHIVE_VERSION` 0 → 1 at open** (`schema::migrate`): create the archive tables and set
   `rolled_until = 0`. The table lists are reclassified (§3.1). From now on a `CACHE_VERSION`
   bump keeps `sessions`, `agents`, `projects` and `cost_states`. No data moves at open.
2. **First compaction.** The dashboard job, after 10 minutes, or `swarmy compact`, rolls up
   everything older than `keep_raw_days`. For the owner that is 17 days, ≈ 100 MB of raw rows,
   one day per minute in the dashboard and ≈ 15 s in the CLI.
3. **One-time `VACUUM`** to `auto_vacuum = INCREMENTAL` (§4.2).
4. **An index older than one cache version** (no archive and a stale `CACHE_VERSION`) is rebuilt
   as today first. The archive starts empty.

### 8.2 Tests

All tests use synthetic data: scenario seats in `src/index/scenarios.rs` and temp dirs, never the
owner's files.

**1. The snapshot.** A `Snapshot` function collects into one comparable structure:
- `spend_by` for all 11 `Dim`s, for the periods today, yesterday, `24h`, `7d`, `30d`, `90d`,
  month, all and two `FROM..TO` ranges;
- `matrix(Project, Model)` and `matrix(Weekday, Hour)`;
- `total`, `model_substitutions` and `days_by_model(14d)`;
- every field of `efficiency` except `fleet` and `conflicts`, which are compared separately
  because they read only the last 24 h;
- `limits::all_reports` for the current windows;
- `cost_crosscheck`;
- the live totals of a long-running session.

Rows are compared by key. `u64` values must be equal; `f64` values must agree within 1e-9
relative.

**2. The world.** 45 synthetic days of transcripts: 3 seats, 2 accounts with a login swap, main
threads, subagents and a workflow agent. Requests use 3 models, one of them unpriced, effort
levels, `fast`, prompts above and below the long-context threshold, and requested ≠ served
models. Tool calls include Bash commands with wrappers, edits of shared files and open tools.
There are prompts, API errors and compactions. One session spans day 1 to day 40.

**The tests:**

1. **`compaction_changes_no_number`.** Snapshot, compact with `keep_raw_days` 14, snapshot, assert
   equal. Repeat for 8 and 28.
2. **`compaction_is_idempotent`.** Compacting twice gives the same as compacting once: the same
   rows in every table.
3. **`compaction_is_atomic`.** A fault hook (cfg(test)) returns an error after the insert and
   before the delete of chunk 3. The state is identical to after chunk 2, and a later run finishes
   with the same result as test 1.
4. **`reread_after_compaction_counts_once`.** After compaction:
   - replace a transcript with a copy (new inode) and append records;
   - truncate and rewrite another;
   - add a fork file with copied old records;
   - relabel a seat in the config.

   The snapshot equals that of a never-compacted index fed the same files.
5. **`new_seat_backfill_is_rolled_up`.** A seat that appears after compaction with 40-day-old
   transcripts is counted once and rolled up on the next run. Its rows `ON CONFLICT`-add into
   existing buckets.
6. **`reprice_after_compaction`.** Change a price version and add a price for the unpriced model.
   The snapshot of the compacted index equals that of a never-compacted one. A threshold change is
   listed by `doctor` instead.
7. **`a_login_change_found_after_compaction_moves_nothing`** (was
   `reattribution_after_compaction`, superseded by §3.7's note). A login change found after weeks
   off moves no request and no rollup; compacted and never compacted agree.
8. **`cache_rebuild_keeps_history`.** Bump `CACHE_VERSION` in the test: drop raw, re-read. The
   snapshot is unchanged. With the transcripts of days 1–20 deleted first, it is still unchanged;
   the same scenario fails on today's code.
9. **`spans_into_the_archive_are_aligned`.** Unit tests for `Period::span` (floors) and the
   `from_for` `debug_assert!`.
10. **Schema tests**, extending the existing ones: every table is in exactly one class; the
    archive `CHECK`s refuse bad rows (`priced` NULL, `first_ts` outside the bucket); a legacy
    database migrates.
11. **Job table** (`tui/jobs.rs` tests): `Compact` is `Heavy`, its period ≥ 30 s, and it waits
    for the first full refresh.
12. **Real data, by hand.** `swarmy compact --verify` runs test 1 on a scratch copy of the real
    index and prints any difference. That is how the owner checks his own index before the first
    real compaction.

## 9. Implementation plan

Each step is sized for one agent, leaves `cargo test`, `clippy` and `fmt` green, and can ship on
its own.

| # | step | files | proves |
|---|---|---|---|
| 1 | **Table classes.** Four lists in `schema.rs`. A `CACHE_VERSION` bump keeps dimension tables. This closes today's history loss for agent types and session titles. | `index/schema.rs`, `index/mod.rs` (`migrate`) | schema tests; a bump keeps `agents`/`sessions` rows |
| 2 | **Quarter-hour spans.** Floor `Days`/`Hours` starts, the previous period and HISTORY spans. Changes nothing else. | `core/period.rs`, `tui/analytics.rs`, `tui/worker.rs` | period tests; README "Periods" note |
| 3 | **Archive schema and union read path.** Empty rollup tables, `ARCHIVE_VERSION`, the key-column list, `from_for` union with the fast path, `SUMS` over `n`, the efficiency and live rewrites (§3.4), `cost_crosscheck`. | `index/schema.rs`, `index/query.rs`, `index/insights.rs`, `index/live.rs` | a test that fills the rollup by SQL from raw rows and deletes them gives the same snapshot (test 1 without the compactor) |
| 4 | **Compactor.** `index/compact.rs`: chunks, guard, rollups with `command_key` and edit hashes, prompt and error rollups, `owners`/`files` pruning, the `MAX(rowid)` exception, `agent_rollup`, `rolled_until`, `rollup_seats`, the fault hook. | `index/compact.rs`, `index/scenarios.rs` | tests 1–3 |
| 5 | **Read marks in the writer.** The skip rule in `write_file`, `Work::reset` limited to `ts >= rolled_until`, the known-session cache, the skipped-records counter. | `index/mod.rs` | tests 4, 5, 8 |
| 6 | **Reprice and re-attribution on rollups.** `price_all` extension with the long-context band, the `post_pass` re-key, `projects` over the union with "never downgrade". | `index/prices.rs`, `index/mod.rs` | tests 6, 7 |
| 7 | **Storage mechanics and the CLI.** `journal_size_limit`, `SQLITE_TMPDIR`, incremental vacuum, checkpoint, the one-time `VACUUM`, `Job::Compact`, `swarmy compact` with `--dry-run`, `--keep-days`, `--vacuum`, `--verify`. | `index/compact.rs`, `tui/jobs.rs`, `tui/worker.rs`, `cli/mod.rs`, `cli/compact.rs` | test 11; CLI test on a temp index; CPU measured per CLAUDE.md |
| 8 | **Config and visibility.** `[storage]` with validation against seat retention, `storage_samples`, doctor section, SYSTEM line and legend, README "Configuration" and "How the numbers are made". | `config.rs`, `claude/` (read `cleanupPeriodDays`), `cli/doctor.rs`, `tui/view/panels/system.rs`, `tui/view/help.rs`, `README.md` | doctor snapshot test; legend sync |
| 9 | *(optional, only if the raw window must grow to 30–60 days)* Slim raw tier: intern `session_id`/`agent_id`/`account_uuid` into integer ids in `requests` and `tool_calls` (≈ −35% raw), or blank tool text after 7 days with a `cmd` column. | `index/schema.rs` and every raw query | test 1 |

**Order:** step 1 first and soon, because it closes a live hazard. Steps 2 and 3 can run in
parallel. Step 4 needs 3. Step 5 needs 4. Steps 6, 7 and 8 need 4 and are independent of each
other.

## 10. Risks

- **Double counting is the failure that matters.** It happens if raw and rollup ever overlap: a
  compactor bug, or a re-read path that bypasses the skip rule. Mitigations:
  - one transaction per chunk;
  - the skip rule in one function;
  - tests 1, 4 and 5 compare against a never-compacted index;
  - `doctor` cross-checks `cost_states` (Claude Code's own per-session totals) against the union.
    A session whose ratio jumps to ~2× is flagged.
- **The `MAX(rowid)` invariant** in `refresh_files`/`post_pass` is fragile once rows are deleted.
  The single-row exception keeps it, but a monotonic `meta.request_seq` written by the inserts
  would be cleaner. Decide in step 4 together with the agent who owns `src/index/` now.
- **The parallel work in `src/index/`.** Steps 3–6 touch the same files. They should start after
  the current refactor lands, and the key-column list should live where that refactor puts the
  query builder.
- **`command_key` is frozen** into old buckets (§7). This is acceptable. Document it in the code
  next to `command_key`.
- **Long-context threshold changes** make old rollup rows reprice by band (§3.6). This is rare,
  and `doctor` lists it.
- **`live_agent_rows` returns every main thread** (`OR a.agent_id = ''`). With `agents` kept for
  a year, that is a few thousand rows a second for the live tick. Add a time bound (sessions
  active in the last 24 h, or live per the registry) in step 3, or earlier. This is independent
  of compaction, but kept tables make it grow.
- **The one-time `VACUUM`** needs free disk equal to the index size, and blocks writers for
  seconds. It runs on mains power only, uses `SQLITE_TMPDIR` set to the data dir, and is skipped
  with a doctor hint when free disk is short.
- **Clock or time zone changes.** Buckets are UTC and local days are computed at read time, as
  for raw rows. A machine that moves zones sees old days re-bucketed exactly like raw data would
  be.
- **`keep_raw_days` ≥ retention.** Validation refuses it, because a cache rebuild could no longer
  re-derive the raw window. A seat with a longer `cleanupPeriodDays` only makes this safer.

## 11. Implementation notes

Where the code differs from the text above, and why.

**Step 1 (table classes).**
- `compact_events` (added to the cache after this design was written) is a **raw** table: it is
  re-derivable from the transcripts, and its count lives on in `sessions.compactions`, which is
  kept. A rebuild re-reads the events of the transcripts still on disk and recounts.
- Dimension tables are kept across a bump only when the stored cache version is at least
  `DIMENSIONS_SINCE` (6, the first version whose dimension tables have today's shape). An older
  or unreadable version drops them with the raw tables, as in §8.1 item 4. A later change to a
  dimension table's shape is an in-place `CACHE_STEPS` step.
- `projects_rev` is no longer a cache meta key (it describes `projects`, a dimension). A
  `PROJECTS_REV` change recomputes the cwds that still have raw requests and leaves the others as
  recorded while their directories existed, a first part of §3.8's "never downgrade".

**Step 2 (quarter-hour spans).**
- `core::period::quarter_floor` floors `Days`/`Hours` starts; `Span::previous` is the previous
  period (same length, start floored) used by the dashboard's trends; HISTORY's 14- and 28-day
  spans are `Period::Days(14|28).span(..)`; `doctor`'s week is `Period::Days(7)` too, so it never
  trips the alignment check of step 3.

**Step 3 (archive schema and the union read path).**
- The rollup columns are declared once in `src/index/archive.rs`: `KEYS` (column, type, value
  from a `requests` row, how a rollup row reads back as the `requests` column) and `SUMS`. The
  `usage_rollup` table, `rollup_insert_sql` (the compactor's `INSERT … SELECT`) and the union
  (`Plan::source`) are generated from them. `usage_rollup` also sums `tools` and the six cost
  parts (`cost_input` … `cache_saved`, added after this design) so the efficiency split stays a
  `SUM` over the same rows as the cost.
- `long_ctx` and `lc_threshold` come from a bound `:lc`, the model's long-context threshold on
  the chunk's day: the compactor runs the generated insert per model (and per local day when a
  chunk straddles a price change) instead of computing the band in Rust per row.
- `agent_rollup` also keeps `first_model`, which the event log shows for an agent. It is derived
  by `agent_rollup_sql`.
- **Choosing the plan.** A span that starts less than `MIN_RAW_DAYS` (8) ago reads `requests`
  with no I/O to decide; `keep_raw_days` must never be below it. An older span reads
  `meta.rolled_until` in the same read transaction as the query, so a compaction committed in
  between cannot move rows out from under it. Only `start < rolled_until` takes the union.
- A span **end** inside the archive counts the whole quarter hour it falls in (the union's `ts`
  of a rollup row is its bucket start). Old limit readings are the only such ends; the
  calibration span start is floored (`settled_total` plans with `snap`).
- **Agent types.** §3.4's single `GROUP BY seat, session_id, agent_id` over the union measured
  4–8× slower on the owner's index than the correlated per-agent sum (0.66 s against 0.085 s CPU
  for 30 days). The correlated raw sum stays; the archive path adds the rollups grouped once in
  a `LEFT JOIN`.
- `live.rs` adds the `agent_rollup` row by primary key unconditionally: one probe into a small
  table is cheaper than deciding whether it is needed.
- **`live_agent_rows` is bounded:** an `agents(last_ts)` index, agents active since
  `min(recent, unfinished_since)`, plus the main threads of running sessions by primary key, sorted
  in Rust (an `ORDER BY` made SQLite walk the primary key instead of the index).
- Empty `effort`, `cwd` and `requested_model` are stored as NULL by the parser, so the rollup's
  `''` for NULL reads back exactly. Rows with `''` written by older versions would read as
  unknown once rolled up.
- `efficiency` sums busy time in integer milliseconds before converting, and groups commands in
  a `BTreeMap`, so raw plus rollup equals raw alone and ties sort the same way.
- `cost_crosscheck` reads the union only when `rolled_until > 0`.
- **Checked on real data:** on a read-only copy of the owner's index, rolling up the 65.6k
  requests older than 14 days (2.7 s, into 2.8k rows) changed no number in `spend_by` (all 11
  dimensions, 5 periods), totals, substitutions, matrices, `days_by_model`, `efficiency`, API
  errors, `cost_crosscheck` or any agent's live totals. `efficiency` over 30 days took 0.64 s
  before and 0.62 s after.
- **Left for step 4:** the compactor must keep `rolled_until` on a quarter hour, guard pending
  rows and missing accounts, bind `:lc`, rewrite `agent_rollup` for the agents it touched, roll
  tool calls up as the proof test does (`command_key` only for shell tools with a summary, the
  edit hash only for edit tools with a target), and decide on the `MAX(rowid)` note in §4.1.

**Step 4 (the compactor, `src/index/compact.rs`).**
- `Index::compact(&CompactOpts) -> CompactReport` is the library function; the CLI and the
  dashboard job come in step 7. `keep_raw_days` below `MIN_RAW_DAYS` (8) is refused, and `now`
  is never taken later than the real clock (the read plans measure the raw window from it).
- Chunks are six UTC hours aligned on multiples of six hours, starting at the oldest raw row of
  any rolled table (`requests`, `tool_calls`, `prompts`, `api_errors`), so gaps cost nothing.
  `rolled_until` becomes the end of the last committed chunk (never lowered); it can stay below
  the cutoff when no raw row lies between them.
- **The guard holds pending requests only.** A request whose `account_uuid` is still NULL after
  `post_pass` stays NULL for good (no owner record, no login known for its seat): the owner's
  index has 68 such rows on two days. Waiting for them would stop compaction forever; the
  rollup's `''` reads back as NULL, so they roll up exactly. Step 6's re-key treats `''` like any
  account.
- **The `MAX(rowid)` question is settled with a high mark, not the single-row exception.**
  `write_file` gives every new request an explicit rowid above `compact::requests_rowid_high`
  (the larger of `MAX(rowid)` and `meta.requests_rowid_high`), and the compactor records that
  value in `meta` before it deletes. A rowid is never given out twice, so "rows above the
  rowid read before the refresh are new" holds without keeping any old row raw, and the no-op
  check and idempotence need no exception.
- `agent_rollup` grows by the chunk's per-agent sums (`ON CONFLICT` add, first and last model
  by time) instead of being rewritten from `usage_rollup` by `agent_rollup_sql`: `usage_rollup`
  has no index by agent, so the rewrite scans the whole table per agent touched. The tests check
  that the grown table equals the derived one.
- `owners` loses every row of a session whose agents all ended before the chunk's end, except
  the newest. `files` is pruned once per run, after the chunks, for transcripts that are gone and
  hold nothing newer than `rolled_until`.
- Tool calls roll up as the step-3 proof did; `command_key` carries a note that rolled-up days
  keep the keys of the rules they were rolled under.
- The fault hook is a test-only thread-local (`FAIL_IN_CHUNK`), so `CompactOpts` stays the same
  in every build.

**Step 5 (read marks, `src/index/marks.rs`).**
- The rule is `Marks::applies` (whether a file may repeat rolled-up records) plus
  `marks::rolled_up` (whether one record is older than `rolled_until`). It applies only to a file
  read **from its start** (replaced, truncated, found again, every file after a rebuild): bytes
  past a stored offset were never read, so an incremental read is always new, and the per-second
  path pays nothing.
- It is decided per file in `plan`, before the refresh writes anything, so a session first seen
  in this refresh stays new for all its files whatever their order. "Known session" is a probe of
  a new `agents(session_id)` index instead of a cached set; `rollup_seats` and the seats'
  canonical dirs are read only when some file is read from its start. `rolled_until` itself is
  read inside each write transaction, so a compaction committed after the plan counts.
- Skipped with a request are its tool calls; prompts and API errors older than `rolled_until`
  are skipped too (they are rolled up as well). Their times, working directory and owner record
  still count, so `agents`, `sessions` and `owners` end up as if the records had been written.
- `Work::reset` deletes prompts and API errors only from `rolled_until` on, and only for a file
  the rule applies to; any other file read again forgets all its rows as before.
- Skipped records are counted in `RefreshStats::records_skipped` and in `meta.skipped_records`
  (a running total for `doctor`, step 8).
- Covered by tests: an append, files replaced and truncated (old and recent sessions), a fork
  in a compacted seat, a rebuild (also with the first 20 days' transcripts deleted), a seat found
  after the compaction (raw first, rolled up into existing buckets by the next run). A relabelled
  seat is covered by rule 1 (session ids stay) and has no test of its own.
- One gap stays: records of a seat found after a compaction that are older than `rolled_until`
  are raw until the next compaction; a cache rebuild in between skips them (their session is
  known by then) and they are lost. The dashboard compacts after its full refresh, so the window
  is a few hours at most.

**Step 6 (reprice, re-attribution, projects).**
- `price_all` reprices the rollups in its own transaction whenever the price fingerprint
  changed: one local day of buckets at a time, each row by `PriceBook::cost_parts_in_band` (the
  band is the row's `long_ctx`, not its summed prompt), written back by primary key. The
  multipliers still apply in one place (`core::pricing::cost_parts_in_band`, which `cost_parts`
  calls with the band of one request). `agent_rollup` then takes its cost and unpriced count
  from one `GROUP BY` over `usage_rollup` (`UPDATE … FROM`); its first and last model do not
  change.
- `Index::band_drift` lists the models and days whose rows took their band against another
  threshold than the prices in force give (the inexact case; `doctor` shows it in step 8).
- The re-key was `archive::rekey_sql`; removed with the third review's H1 (§3.7).
- `projects` moved to `index/projects.rs`: a new rule revision takes the cwds of raw rows and
  rollups, orphan votes count rolled-up requests by session, and a cwd whose dir is gone, that
  resolves as an orphan and has no votes keeps the project recorded for it.
- Tests: `reprice_after_compaction` (a dated price inside the archive, a price for the unpriced
  model, new long-context multipliers; then a threshold change listed by `band_drift`),
  `a_login_change_found_after_compaction_moves_nothing`,
  `projects_come_from_the_union_and_are_never_downgraded`.

**Step 7 (mechanics, the job, the CLI).**
- `journal_size_limit` is 32 MB, set at open. SQLite's temp dir is `<data>/tmp`, set once per
  process by `index::use_temp_dir` from `Ctx` (`PRAGMA temp_store_directory`: Rust 2024 makes
  `set_var` unsafe and the crate forbids unsafe code, so `SQLITE_TMPDIR` is not set). Checked
  with `strace`: the `VACUUM` copy is an `O_TMPFILE` there.
- After a compaction that moved anything, `Index::free_space` runs `incremental_vacuum` in steps
  of 4096 pages while the free list shrinks (only with incremental auto-vacuum), then a passive
  checkpoint, and a truncating one (waiting at most a second for readers) when the passive one
  copied the whole WAL.
- The one-time switch is `Index::vacuum_to_incremental`, decided by the pure `vacuum_verdict`:
  never twice; unless the user asked (`--vacuum`), only on mains power and with a quarter of the
  file free; always only with twice the file plus 64 MB free on the data dir's disk (the copy,
  and the WAL the rewrite goes through). A skip is recorded in `meta.vacuum_skipped` for
  `doctor`.
- `Job::Compact`: every 6 h (also on battery), first ten minutes after the start
  (`Start::After`), only in a tick that just ran the full refresh; a backlog goes on 4 chunks
  per run every 50 s (so each full refresh, every 60 s, takes the next day; 300 s on battery,
  `Schedule::again_in`). When nothing is left to roll up,
  the job tries the one-time switch, which waits for mains power.
- **The gap step 5 left is closed in the refresh:** a refresh that read any file from its start
  rolls up at once whatever raw rows lie below `rolled_until` (`Index::roll_late`, the
  compactor's chunks with `rolled_until` as the cutoff; a no-op check otherwise). A seat found
  after a compaction therefore never keeps old records raw, and a cache rebuild cannot skip and
  lose them (test `a_seat_found_after_compaction_is_counted_once_and_rolled_up_at_once`).
- `swarmy compact` (`cli/compact.rs`) refreshes first, then compacts in the foreground with
  progress. `--dry-run` reads counts below the cutoff and estimates rollup rows (distinct keys
  without the band) and bytes from the measured bytes per row. `--verify` copies the index
  with `VACUUM INTO` to `<data>/tmp`, compares `total` and `spend_by` for every `Dim` over five
  periods before and after compacting the copy, and deletes it; the index is only read.
- **Measured on a copy of the owner's index (2026-11-23, 290 MB, 31 days):** rolling up the
  86,156 requests older than 14 days (46 chunks) took 1.9 s user + 1.9 s sys (12.4 s wall,
  fsync per chunk); a no-op run 0.07 s user; the one-time `VACUUM` afterwards 0.6 s user + 1.3 s
  sys and took the file from 290 MB to 191 MB; `--verify` found 0 differences in 14,177 numbers.
  The dashboard on the same copy (isolated, no seats; CPU of the process): 2.8 CPU-seconds a
  minute idle before the job, 2.7 over the 15 minutes it caught up (runs of 0.1–2.2 s wall); the
  last run with the one-time `VACUUM` took 3.5 s wall and ≈ 4.4 CPU-seconds on top of that
  minute, and the file went from 304 MB to 201 MB.

**Step 8 (config and visibility).**
- The rules are pure, in `core::storage`: `MIN_RAW_DAYS`, the defaults, the measured bytes per
  row, `keep_raw_days` (the check of §5) and `outlook` (growth and projection). `archive` and
  `compact` re-export the constants they used to own.
- `[storage]` (`config::StorageConf`): `keep_raw_days` unset means 14, shrunk to fit a seat whose
  retention is shorter (never below 8); a value set outside 8 ..= shortest retention − 2 is
  refused when the context loads, naming the seat and its `cleanupPeriodDays`. When a seat's
  retention is below 10 days only 8 fits. `compact --keep-days` goes through the same check.
  `Seat::cleanup_period_days` reads `settings.json` each time it is asked (`Ctx::retention`).
- `auto_compact = false` keeps the job, which then only records the day's size sample.
- `storage_samples.file_bytes` is the bytes **in use** (the file less its free pages): free pages
  are reused before the file grows, so they are no growth. Samples are written by every
  compaction run (the job's no-op runs included) and by `doctor`.
- Growth per day is the least-squares slope of the kept bytes (in use less the raw estimate)
  over the last 14 samples once there are 7; before that, the requests a day of the raw window
  × 18 B (`ARCHIVE_BYTES_PER_REQUEST`, §6). The projection is the raw window once full at
  today's pace, plus the kept bytes, plus a year of growth.
- `doctor` prints the STORAGE section of §5 (`detail` names the day raw detail starts from,
  `rolled_until` or the oldest request), plus the one-time `VACUUM` still waiting and why, and
  `band_drift` rows; without incremental auto-vacuum it says how much free space waits in the
  file. `Index::storage_report` gathers it: page counts, row counts, the samples,
  `meta.last_compaction` (written by a compaction that moved something), `meta.skipped_records`.
- SYSTEM reads the outlook in the transcript-sizes cadence (every 5 min) and shows
  `index 290.2 MB · +252 KB/day · 12 mo ≈ 300.4 MB`, in the warning colour over `budget_mb`; the
  legend has a line for it (and a test that keeps it).
- On a copy of the owner's index before compaction: 290 MB, 287k requests since 2026-10-22,
  +161 KB/day estimated, ≈ 206 MB in 12 months; after `swarmy compact`, 201k requests of detail,
  21k rollup rows ≈ 1.4 MB.

**After the third architecture review (`BACKEND_REVIEW_RESULTS.md`, H3, M1–M4, L1, L7).**
- **A response is skipped or written whole** (`write::write_file`). Claude Code writes one
  response as several records, one per content block, each with its own time; the raw row keeps
  the first one's, and the compactor rolls the response up by it. A file read again from its
  start decides each message id by its first record, so a block after `rolled_until` of a
  response rolled up before it no longer comes back as a new row. Its tool calls are rolled up by
  their own time, so a tool call after `rolled_until` is still written (a no-op when it is raw).
  The archive fixture writes responses as Claude Code does, one record per block.
- **§4.3's rule is narrower: nothing is skipped on a guess** (`marks::Marks::of`). A known agent
  (session and agent id, any seat) repeats; a new agent of a known session (a subagent file never
  read) is new; a session never seen in a compacted seat (§4.3 rule 2) is decided by its records:
  it repeats only when every response older than `rolled_until` in it is one the archive holds
  (`marks::all_held`: a rollup row of its quarter hour and model with at least its tokens, as a
  fork's copies are), otherwise it is new and rolled up by the same refresh (`roll_late`). The
  "conservative error" of §4.3 (records of a restored or synced transcript skipped) is gone.
- **Nothing is dropped silently.** A transcript that cannot be read goes into the raw table
  `unreadable_files` (removed when a read succeeds), `RefreshStats::unreadable`, `doctor` and
  ATTENTION; it is read again on the next refresh. Lines that are no JSON record and records with
  numbers beyond i64 are counted per file in `record_types` (`transcript::MALFORMED`) and
  reported by `doctor`.
- **A cache rebuild keeps what no transcript gives back** (`schema::stash_unrereadable`). §3.1
  assumed every raw row could be read again; a seat dir removed or moved, a retention shorter than
  the raw window, or an old record appended after a compaction (raw below `rolled_until`, which a
  re-read skips) broke that. Before the raw tables are dropped, rows of an agent with no
  transcript left on disk (by `files`) and rows older than `rolled_until` go to temp tables and
  back into the new ones; the first refresh rolls the late ones up. A damaged `settings.json` is
  an error (`ConfigError::SeatSettings`), listed in `problems`, never taken for "not set".
- **Unpriced requests are counted in the archive** (`ARCHIVE_VERSION` 2). One key can hold a
  request priced at $0 (no tokens, any model) next to one without a price; `MAX(priced)` hid the
  second. `usage_rollup` keeps `n_unpriced` and `n_empty` (requests without tokens, so a reprice
  from the sums knows how many stay priced); `priced` now means "any of them is priced" (`cost`
  is not NULL). Rows rolled up before keep what the flag said.
- **Settled parts:** `spend_series` resizes the kept part to its own span's length (a later span
  end in the same horizon cut the limit curve); the owner and seat fallbacks of `post_pass` bump
  the settled generation when settled requests got an account.
- **Two writers:** a refresh that finds nothing commits nothing (`refreshed_at` only after a read,
  the `accounts` row only when it changed or is an hour old), so a second process no longer makes
  every per-agent cache read again each second.
