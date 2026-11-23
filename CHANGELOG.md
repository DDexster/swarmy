# Changelog

All notable changes to swarmy. Versions follow [Semantic Versioning](https://semver.org/); the
release workflow takes a release's notes from its section here.

## v0.1.0

The first release: mission control for Claude Code agents in the terminal.

- **Live swarm**: every agent of every Claude Code session across several config dirs as a live
  tree with activity lanes, timers, context fill and cost; agents that need you blink, ring the
  bell and come first in ATTENTION; `J` jumps to their tmux pane.
- **Spend at API prices** by project, account, model, agent type, session and day, from the
  transcripts Claude Code writes; prices are versioned data.
- **Weekly limits**: how much of each account's week is used, where it went per project, and when
  it runs out at the current burn rate; optional exact readings through the status line
  (`swarmy setup`, `swarmy statusline`).
- **Login vault**: save a config dir's OAuth login, switch another account in when a limit runs
  out, and back, with backups and atomic writes.
- Commands: `report`, `models`, `limits`, `accounts`, `doctor`, `prices`, `compact`, `setup`,
  `jump`, `statusline`, `demo`, `man`, `completions`.
- Offline and read-only (the vault switch is the one write); a bounded SQLite index with a
  quarter-hour archive; built to stay light on a laptop battery.
- Static binaries for Linux (x86_64, aarch64, ARMv7, ARMv6 — Raspberry Pi included), Windows
  x86_64 and macOS (universal), `.deb` and `.rpm` packages, and a Homebrew formula.
