#!/usr/bin/env bash
# Regenerates the README media in docs/img from `agentmana demo` (made-up data only).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --quiet
BIN="${CARGO_TARGET_DIR:-target}/release/agentmana"
TMP=$(mktemp -d "${TMPDIR:-$HOME/tmp}/agentmana-rec.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
mkdir -p docs/img
rm -f docs/img/*.png docs/img/*.gif
# The wall (a 4K terminal): select an agent, then open the login picker of a seat.
"$BIN" demo --record "$TMP/wall.jsonl" --frames 130 --size 326x81 --keys "20:j,22:j,90:2,95:enter,120:esc"
python3 tools/render_frames.py "$TMP/wall.jsonl" docs/img --png 60:wall.png 110:vault-picker.png --size 14
python3 tools/render_frames.py "$TMP/wall.jsonl" docs/img --gif demo.gif --every 2 --scale 0.5 --size 14
# The column (half a 4K screen) and tabs (a laptop terminal).
"$BIN" demo --record "$TMP/column.jsonl" --frames 40 --size 160x81 --keys "20:j"
python3 tools/render_frames.py "$TMP/column.jsonl" docs/img --png 35:column.png --size 14
"$BIN" demo --record "$TMP/tabs.jsonl" --frames 40 --size 140x34 --keys "20:j"
python3 tools/render_frames.py "$TMP/tabs.jsonl" docs/img --png 35:tabs.png --size 14
ls -la docs/img
