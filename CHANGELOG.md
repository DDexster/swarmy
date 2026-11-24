# Changelog

All notable changes to swarmy. Versions follow [Semantic Versioning](https://semver.org/); the
release workflow takes a release's notes from its section here.

## v0.2.0

Antigravity next to Claude Code, worktrees and branches, and logins shared between seats.

- **Antigravity (`agy`) as a harness next to Claude Code**: its seats (`~/.gemini/antigravity-cli`,
  the `HOME` of running agys, `[[seat]] harness = "agy"`), live conversations and subagents,
  spend, tools and projects in every panel and command; `--harness` and `--by harness`.
- **Gemini prices** (Gemini 4 Neon, Carbon, Argon, Hydrogen; 3.x; 2.5) as dated built-in rows,
  including the Carbon/Argon change on 2026-12-01; `tools/prices-from-litellm.py` compares the
  built-in rows with LiteLLM's list.
- **Logins in several seats**: a saved login stays listed after it goes into a seat and can go
  into more seats; the seat's own login is still saved first. `--share` is gone (no longer
  needed).
- **Vault for Antigravity**: save, add (`swarmy accounts add --harness agy`) and switch Google
  logins between agy seats; a login only goes into seats of its harness.
- LIMITS · SEATS groups seats and logins per harness; MODELS shows which harness ran a model;
  BURN splits today's spend per harness.
- **Worktrees and branches**: SWARM names a session's checkout (`shop/fix-1` for a linked git
  worktree), its branch, and `⇄N` when several live sessions work in the same checkout; the
  AGENT card has a checkout line; PROJECTS shows the share of spend made in worktrees (IN WT);
  `g` groups by checkout; `report --by worktree`, `--by branch` and `--checkout worktrees|main`.
  The index is read again once (cache version 7) to pick up the branches.
- **Jump beyond tmux**: `J` and `swarmy jump` reach a Claude Code in Zellij, kitty, WezTerm and
  iTerm2 too (read from the process's environment, on Linux and macOS).
- **Accounts per organization**: an account is the pair of user and organization, so one login
  in two organizations keeps two weeks and two plans.
- Accounts: the organization migration keeps parked organizations apart, finishes an
  interrupted re-attribution, and completes `session-owner` records that have no organization.

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
