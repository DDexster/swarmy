# CLAUDE.md — agentmana

Mission control for Claude Code agents: a Rust CLI + TUI that reads what Claude Code writes to disk
(transcripts, session registry, `.claude.json`) across several config dirs, shows the live agent
swarm, API-equivalent spend, weekly-limit attribution per project, and keeps a vault of OAuth
logins. Users run it in a spare terminal, mostly on laptops.

## Ground rules

- **English only**: UI text, docs, code, commits.
- **Offline and read-only.** No network calls, no telemetry. Claude Code's directories are only read;
  the single write is an explicit vault switch (`src/vault`), which saves the seat's current login
  first, backs up both files, writes atomically and keeps `mcpOAuth`.
- **No personal data in git**: no real home paths, user names, emails, account UUIDs or real config
  dir names in code, tests, fixtures, docs or screenshots. Tests use synthetic data; screenshots and
  GIFs come only from `agentmana demo`. Check the staged diff before every commit.
- **Never test the vault on real config dirs.** Use temp dirs (see `src/vault/tests.rs`) or a fake
  `HOME`.
- **Battery budget.** The dashboard must stay around 2 CPU-seconds per minute (≈3.4% of one core)
  on a 4K wall with 15–20 live agents.
  Measure CPU time of the process, not wall time (other agents load the machine):
  ```sh
  P=$(pgrep -x agentmana); T0=$(awk '{print $14+$15}' /proc/$P/stat); sleep 30
  T1=$(awk '{print $14+$15}' /proc/$P/stat); echo "$(( (T1-T0)/30 ))% of a core"
  ```
  Measure *your* process (`pgrep -x agentmana` may find the owner's own instance), per thread from
  `/proc/<pid>/task/*/stat`. `AGENTMANA_DEBUG=1 agentmana 2>log` prints per-tick and per-frame
  timings. Per-second work may only touch running sessions (`Index::refresh_hot` with cached dir
  listings, `ProcTable::for_pids`, per-agent totals reused when no new request arrived); period
  aggregates (2 min), limits (1 min), efficiency (5 min), the previous period (10 min), lanes and
  session totals (10 s), history charts (5 min), the full process table and disks
  (SYSTEM open) run rarely or on demand. `.claude.json` is rewritten by Claude Code every few
  seconds — never trigger heavy work on its mtime.
- Prices are versioned data (`src/core/pricing.rs`, overridable by Claude Code's cache and
  `config.toml`), never ad-hoc constants. Money numbers in tests are computed by hand in comments.

## Layout

| Path | What |
|---|---|
| `src/core/` | pure domain: usage, pricing, periods, limits math (no I/O) |
| `src/claude/` | readers of Claude Code's files: seats discovery, transcripts, registry, `.claude.json`, credentials, processes, projects |
| `src/index/` | SQLite index (incremental by inode/offset), schema, queries; `insights.rs`: efficiency figures (tools, commands, waits, cache, agent types, fleet, conflicts) |
| `src/live/` | live agent states for the dashboard |
| `src/vault/` | login vault and switching |
| `src/cli/` | commands: report, models, limits, accounts, doctor, prices, shell-alias |
| `src/tui/` | dashboard: worker thread + UI loop (`mod.rs`), state and keys (`app.rs`), vault in the MANA panel (`vault_ui.rs`), boot animation, themes |
| `src/tui/view/` | drawing: layouts wall / column / tabs, header, footer, dialogs (`mod.rs`); panels that fill any rect (`panels.rs`); primitives (`kit.rs`) |
| `src/demo.rs` | made-up world for `agentmana demo` and README media |
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
- Unknown record types are counted; `agentmana doctor` reports them (format drift).
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
readelf -l target/x86_64-unknown-linux-musl/release/agentmana | grep -c INTERP   # must print 0

tools/record-readme.sh                     # refresh docs/img (screenshots + demo.gif)
```

Visual changes to the dashboard: record at the sizes that matter (326×81 wall, 160×81 column,
140×34 tabs) with `agentmana demo --record … --size WxH --keys …` and `tools/render_frames.py`,
look at the PNGs, fix, repeat; `tools/record-readme.sh` refreshes the README media.

UX principles (from the owner): use the whole screen — no tabs of mostly empty space on a big
terminal; every panel fills its rect; positions are stable (the selection is kept by key, the agent
tree is in start order, panel sizes do not depend on the agent count); colour only where it means
something; everything works from the keyboard.

Words on screen (keep them, and the legend in `view/mod.rs::legend_lines`, in sync):
- **seat** = a Claude Code config dir; **login** = an OAuth sign-in (saved ones live in the vault);
  **account** = the Anthropic account behind a login, named after its saved login.
- Limits: "week used N%", "runs out in 7h (Mon 03:34)", "limit reached"; shares as "% of week",
  "1% ≈ $X". Never "mana N%" or "pt" on screen (mana is only the product's name).
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
