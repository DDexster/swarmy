#!/usr/bin/env bash
# Regenerates the README media in docs/img from `swarmy demo` (made-up data only).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --quiet
BIN="${CARGO_TARGET_DIR:-target}/release/swarmy"
mkdir -p "${TMPDIR:-$HOME/tmp}"
TMP=$(mktemp -d "${TMPDIR:-$HOME/tmp}/swarmy-rec.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
# The demo keeps its made-up world (and its vault) here, not in your own data dir.
export SWARMY_DATA_DIR="$TMP/data" SWARMY_CONFIG_DIR="$TMP/config"
RENDER="python3 tools/render_frames.py"
mkdir -p docs/img
rm -f docs/img/*.png docs/img/*.gif

# The wall (a 4K terminal) with an agent selected.
"$BIN" demo --record "$TMP/wall.jsonl" --frames 70 --size 326x81 --keys "20:j,22:j"
$RENDER "$TMP/wall.jsonl" docs/img --png 60:wall.png --size 14

# The dashboard GIF on a 200×60 terminal: the payments session asks for permission (from about
# frame 125 to 185 of the recording); Enter on the first alert selects it, the AGENT panel shows
# what it waits for.
"$BIN" demo --record "$TMP/demo.jsonl" --frames 240 --size 200x60 --keys "20:j,150:4,152:enter"
$RENDER "$TMP/demo.jsonl" docs/img --png 170:wall-small.png --size 14
$RENDER "$TMP/demo.jsonl" docs/img --gif demo.gif --start 90 --every 2 --scale 0.6 --size 14

# The boot swarm (frames recorded while the boot screen runs).
SWARMY_RECORD_BOOT=1 "$BIN" demo --record "$TMP/boot.jsonl" --frames 90 --size 200x50
$RENDER "$TMP/boot.jsonl" docs/img --gif boot.gif --every 1 --scale 0.5 --size 14

# LIMITS · SEATS zoomed, then its login picker.
"$BIN" demo --record "$TMP/limits.jsonl" --frames 60 --size 160x40 --keys "20:2,22:z,30:enter"
$RENDER "$TMP/limits.jsonl" docs/img --png 28:limits.png 50:vault-picker.png --size 14

# The column (half a 4K screen) and tabs (a laptop terminal).
"$BIN" demo --record "$TMP/column.jsonl" --frames 40 --size 160x81 --keys "20:j"
$RENDER "$TMP/column.jsonl" docs/img --png 35:column.png --size 14
"$BIN" demo --record "$TMP/tabs.jsonl" --frames 40 --size 140x34 --keys "20:j"
$RENDER "$TMP/tabs.jsonl" docs/img --png 35:tabs.png --size 14

# The status line under a Claude Code prompt: `swarmy statusline` in a fake home, fed made-up
# input and a made-up fleet summary (as the dashboard would write it) whose writer is this shell.
H="$TMP/home"
mkdir -p "$H/.claude-work/sessions" "$SWARMY_DATA_DIR/statusline"
SEAT=$(python3 -c 'import hashlib, os, sys; print(hashlib.sha256(os.path.realpath(sys.argv[1]).encode()).hexdigest()[:16])' "$H/.claude-work")
now() { date -u -d "$1" +%Y-%m-%dT%H:%M:%SZ; }
cat >"$SWARMY_DATA_DIR/statusline/fleet.json" <<EOF
{"v":2,"written_at":"$(now now)","writer_pid":$$,
 "needs_you":[{"session_id":"00000000-0000-4000-8000-000000000001","seat":"$SEAT","name":"shop-api","what":"permission","since":"$(now now)","tmux":null},
              {"session_id":"00000000-0000-4000-8000-000000000002","seat":"$SEAT","name":"docs","what":"question","since":"$(now now)","tmux":null}],
 "counts":{"sessions":4,"agents":11,"working":6,"thinking":3,"waiting":2,"stalled":0},
 "sessions":{"00000000-0000-4000-8000-000000000003":{"agents":3,"working":2,"thinking":1,"waiting":0,"stalled":0,"burn_usd_h":3.1}},
 "seats":{"$SEAT":{"label":"work","account_name":"work","week_used":38.0,"runs_out_at":"$(now '+7 hours')","resets_at":"$(now '+2 days')"}}}
EOF
INPUT='{"session_id":"00000000-0000-4000-8000-000000000003","model":{"display_name":"Opus"},
 "cost":{"total_cost_usd":4.2},"context_window":{"used_percentage":42.0},
 "rate_limits":{"five_hour":{"used_percentage":12,"resets_at":'$(date -d '+3 hours' +%s)'},
                "seven_day":{"used_percentage":38,"resets_at":'$(date -d '+2 days' +%s)'}}}'
LINE=$(echo "$INPUT" | env HOME="$H" CLAUDE_CONFIG_DIR="$H/.claude-work" SWARMY_ISOLATED=1 COLUMNS=150 "$BIN" statusline)
BAR=$(printf '─%.0s' $(seq 1 148))
{
  printf '\e[2m╭%s╮\e[0m\n' "$BAR"
  printf '\e[2m│\e[0m > %145s\e[2m│\e[0m\n' ""
  printf '\e[2m╰%s╯\e[0m\n' "$BAR"
  printf '  %s\n' "$LINE"
} >"$TMP/statusline.ans"
$RENDER --ansi "$TMP/statusline.ans" docs/img --png 0:statusline.png --size 14
ls -la docs/img
