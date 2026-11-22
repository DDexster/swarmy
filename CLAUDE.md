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
- **Battery budget.** The dashboard must stay around 1 CPU-second per minute on a busy machine.
  Measure CPU time of the process, not wall time (other agents load the machine):
  ```sh
  P=$(pgrep -x agentmana); T0=$(awk '{print $14+$15}' /proc/$P/stat); sleep 30
  T1=$(awk '{print $14+$15}' /proc/$P/stat); echo "$(( (T1-T0)/30 ))% of a core"
  ```
  `AGENTMANA_DEBUG=1 agentmana 2>log` prints per-phase timings of the worker. Per-second work may
  only touch running sessions (`Index::refresh_hot`, `ProcTable::for_pids`); full scans, history
  charts and the full process table run rarely or on demand.
- Prices are versioned data (`src/core/pricing.rs`, overridable by Claude Code's cache and
  `config.toml`), never ad-hoc constants. Money numbers in tests are computed by hand in comments.

## Layout

| Path | What |
|---|---|
| `src/core/` | pure domain: usage, pricing, periods, limits math (no I/O) |
| `src/claude/` | readers of Claude Code's files: seats discovery, transcripts, registry, `.claude.json`, credentials, processes, projects |
| `src/index/` | SQLite index (incremental by inode/offset), schema, queries |
| `src/live/` | live agent states for the dashboard |
| `src/vault/` | login vault and switching |
| `src/cli/` | commands: report, models, limits, accounts, doctor, prices, shell-alias |
| `src/tui/` | dashboard: worker thread + UI loop, screens in `draw.rs`, vault UI, boot animation, themes |
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

Visual changes to the dashboard: record with `tools/record-readme.sh`, look at the PNGs, fix, repeat.
