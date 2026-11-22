# CLAUDE.md — swarmy

Mission control for Claude Code agents: a Rust CLI + TUI that reads what Claude Code writes to disk
(transcripts, session registry, `.claude.json`) across several config dirs, shows the live agent
swarm, API-equivalent spend, weekly-limit attribution per project, and keeps a vault of OAuth
logins. Users run it in a spare terminal, mostly on laptops.

## Ground rules

- **English only**: UI text, docs, code, commits.
- **Offline and read-only.** No network calls, no telemetry. Claude Code's directories are only read;
  the single write is an explicit vault switch (`src/vault`), which saves the seat's current login
  first, backs up both files, writes atomically, merges only `oauthAccount` into `.claude.json` as
  it is right before the write, and keeps `mcpOAuth`. Every vault change goes through
  `Vault::edit` (OS lock, re-read under it, backup, atomic writes); never write vault files elsewhere.
- **No personal data in git**: no real home paths, user names, emails, account UUIDs or real config
  dir names in code, tests, fixtures, docs or screenshots. Tests use synthetic data; screenshots and
  GIFs come only from `swarmy demo`. Check the staged diff before every commit.
- **Never test the vault on real config dirs.** Use temp dirs (see `src/vault/tests.rs`) or a fake
  `HOME`.
- **Battery budget.** The dashboard must stay around 2 CPU-seconds per minute (≈3.4% of one core)
  on a 4K wall with 15–20 live agents.
  Measure CPU time of the process, not wall time (other agents load the machine):
  ```sh
  P=$(pgrep -x swarmy); T0=$(awk '{print $14+$15}' /proc/$P/stat); sleep 30
  T1=$(awk '{print $14+$15}' /proc/$P/stat); echo "$(( (T1-T0)/30 ))% of a core"
  ```
  Measure *your* process (`pgrep -x swarmy` may find the owner's own instance), per thread from
  `/proc/<pid>/task/*/stat`. `SWARMY_DEBUG=1 swarmy 2>log` prints per-tick and per-frame
  timings (and the job table at start). Every cadence lives in the job table `src/tui/jobs.rs`
  (period, battery period, what it reads; tests pin the measured values and keep heavy jobs at
  ≥ 30 s); the worker holds no intervals of its own. Per-second work may only touch running sessions (`Index::refresh_hot` with cached dir
  listings, `ProcTable::for_pids`, per-agent totals reused when no new request arrived); period
  aggregates (2 min), limits (1 min), efficiency (5 min), the previous period (10 min), lanes and
  session totals (10 s), history charts (5 min), the full process table and disks
  (SYSTEM open) run rarely or on demand. `.claude.json` is rewritten by Claude Code every few
  seconds — never trigger heavy work on its mtime.
- Prices are versioned data (`src/core/pricing.rs`, overridable by Claude Code's cache and
  `config.toml`), never ad-hoc constants. Built-in prices carry their model's date; each version
  of Claude Code's cached prices applies from the day the index first saw it; an override derives
  what it leaves out from the entry it overrides (`Rates::derive`). Money numbers in tests are
  computed by hand in comments.
- Limit rules live once, in `src/core/limits.rs`: `Level` (60/85/100 thresholds; front ends only
  map a level to their colour), `current_window`, `runs_out`, `projection_at_reset`, `was_reset`, and `window_of`, the window in force (a reset by hand moves its start; `limits`, the dashboard and `report --period week` all use it). Readings of one window are named by their reset to the minute; a fall counts as a reset only when it holds across sources (`effective_start`).
  Every surface reads an account's limit from `Ctx::latest_readings` (index + seats, newest wins)
  through `limits::all_reports`; the `limit_threshold_copies` ratchet keeps copies out.
- The index schema (`src/index/schema.rs`) has cache tables (rebuilt from transcripts when
  `CACHE_VERSION` changes) and fact tables that nothing else records (`seat_accounts`, `accounts`,
  `readings`, `observed_prices`: migrated by `FACTS_VERSION` steps, never dropped). A new table
  goes into exactly one list; data rules are `CHECK`s, enum lists generated from the Rust enums.
  `cost` is NULL until priced (`priced` NULL) or when no price is known (`priced = 0`).

## Layout

| Path | What |
|---|---|
| `src/lib.rs`, `src/main.rs` | the library holds everything; the binary only parses arguments and calls `cli::run` |
| `src/core/` | pure domain: usage, pricing, periods, limits math (no I/O) |
| `src/claude/` | readers of Claude Code's files: seats discovery, transcripts, registry, `.claude.json`, credentials, processes, projects |
| `src/index/` | SQLite index (incremental by inode/offset, offsets checked again inside an immediate transaction), schema (in-place `CACHE_STEPS`), queries; `write.rs`: one file's records into the batch transaction; `insights.rs`: efficiency figures (tools, commands, waits, cache, agent types, fleet, conflicts); `prices.rs`: dating Claude Code's prices, repricing; `scenarios.rs`: scenario tests on synthetic seats; `live.rs`: typed reads for `live` (the connection is private: SQL stays in `index/`) |
| `src/live/` | live agent states for the dashboard; `state.rs`: how an agent's state is decided (pure, table-tested) |
| `src/vault/` | login vault and switching |
| `src/app/` | composition root `Ctx` (dirs, config, seats, prices, index), used by commands and dashboard |
| `src/limits/` | limit analysis per account (`all_reports`): queries feeding the pure rules of `core::limits` |
| `src/cli/` | commands: report, models, limits, accounts, doctor, prices |
| `src/tui/` | dashboard: UI loop (`mod.rs`), worker thread (`worker.rs`) on the cadences of `jobs.rs`, analytics passes (`analytics.rs`), state (`app/mod.rs`) and keys (`app/keys.rs`), what needs attention and what gets logged (`alerts.rs`: pure, tested), vault in the LIMITS · SEATS panel (`vault_ui.rs`), boot animation, themes |
| `src/tui/view/` | drawing: layouts wall / column / tabs, header, footer (`mod.rs`); dialogs and toast (`overlays.rs`); help and legend (`help.rs`); panels that fill any rect (`panels/`, one file per panel, shared helpers in `panels/mod.rs`); primitives (`kit.rs`) |
| `src/demo.rs` | made-up world for `swarmy demo` and README media |
| `tools/render_frames.py` | renders recorded frames to PNG/GIF |
| `tools/record-readme.sh` | regenerates `docs/img/*` |

## Claude Code format notes (verified on 2.1.329)

- One API response is written as several `assistant` records (one per content block) with the same
  usage: dedupe by `message.id`, merge with `max`.
- `session-owner` records give the account per turn; subagent files inherit it from the session.
- `sessions/<pid>.json` holds live status (`busy`/`idle`/`waiting` + `waitingFor`), tmux pane and
  `procStart` (check it against `/proc/<pid>/stat` to ignore reused pids).
- `.claude.json`: `cachedUsageUtilization` (limits, no network needed), `additionalModelCostsCache`
  (prices), `oauthAccount`. For `~/.claude` it lives at `~/.claude.json`, otherwise inside the dir.
- Unknown record types are counted; `swarmy doctor` reports them (format drift).
- Status line (optional, `swarmy setup`): Claude Code runs `statusLine.command` after each response
  with a JSON on stdin; `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at (epoch s)}`
  are exact. `swarmy statusline` must stay fast (no index, no scan): it writes
  `<data>/statusline/<sha256(config dir)[..16]>.json` and prints one line. The dashboard merges
  these readings when newer than `.claude.json`'s. `setup` writes only `settings.json` (backup
  first, never over an older backup) and only after a yes. The status line reads the dashboard's
  `statusline/fleet.json` (`src/fleet.rs`; fresh while < `fresh_seconds` and its writer pid runs)
  and otherwise scans the session registries; segments shorten in a fixed order to `$COLUMNS`
  (Claude Code sets it). Keep its vocabulary and glyphs the same as the dashboard's.
- Testing `setup` or anything that discovers seats: seats are also found through running
  `claude` processes' `CLAUDE_CONFIG_DIR`, so a fake `HOME` is NOT isolation — the real seats
  still show up. Set `SWARMY_ISOLATED=1` together with the fake `HOME` (and unset
  `CLAUDE_CONFIG_DIR`), and check the seat list before any `--yes`.
- Projects: a cwd resolves to its git root; removed worktrees under `<repo>-wt/` fold into
  `<repo>`; a dir outside any repo (gone, or a scratch dir) takes the project of the session it ran
  in, or the session's own dir when it lies under it (never a home dir). Changing these rules means
  bumping `PROJECTS_REV` in `src/index/mod.rs`, which recomputes the table once.

## Commands

```sh
export CARGO_TARGET_DIR=/path/with/space   # builds are large
cargo test --no-fail-fast
cargo clippy --all-targets
cargo fmt
cargo run --release -- doctor
cargo run --release -- demo                # try the dashboard on fake data

# static Linux binary (zig 0.16 + cargo-zigbuild)
cargo zigbuild --release --target x86_64-unknown-linux-musl
readelf -l target/x86_64-unknown-linux-musl/release/swarmy | grep -c INTERP   # must print 0

tools/record-readme.sh                     # refresh docs/img (screenshots + demo.gif)
```

Visual changes to the dashboard: record at the sizes that matter (326×81 wall, 160×81 column,
140×34 tabs) with `swarmy demo --record … --size WxH --keys …` and `tools/render_frames.py`,
look at the PNGs, fix, repeat; `tools/record-readme.sh` refreshes the README media.

UX principles (from the owner): use the whole screen — no tabs of mostly empty space on a big
terminal; every panel fills its rect; positions are stable (the selection is kept by key, the agent
tree is in start order, panel sizes do not depend on the agent count); colour only where it means
something; everything works from the keyboard.

Words on screen (keep them, and the legend in `view/help.rs::legend_lines`, in sync):
- **seat** = a Claude Code config dir; **login** = an OAuth sign-in (saved ones live in the vault);
  **account** = the Anthropic account behind a login, named after its saved login.
- Limits: "week used N%", "runs out in 7h (Mon 03:34)", "limit reached"; shares as "% of week",
  "1% ≈ $X". Never "mana" or "pt" on screen.
- Trends say what they compare with ("▲▼ vs previous 7d", "vs day before").
- Every new glyph, unit or colour gets a legend line; the footer keeps "? help & legend" visible.
- `--mask` blanks emails and the OS user name in the finished frame (`mask_user`).

Style rules the owner asked for:
- Panel titles and in-panel section headings sit on a plate (`Theme::heading()`, the
  `title`/`title_bg` colours of each theme); the focused panel's plate is the accent colour. New
  headings use `heading()`, not bold text in a data colour.
- Sessions in SWARM are separated by a rule; project and directory names use the theme's `dir`
  colour everywhere (session rows, agent card, recently done).
- Big block digits (`bigdigits.rs`) show the number only, with a blank column on each side of
  the decimal point; the unit goes in the caption under them (`$ per hour`), because a block `$`
  reads as another digit.
