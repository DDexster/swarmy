#!/usr/bin/env bash
# Regenerates the README media in docs/img from `agentmana demo` (made-up data only).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --quiet
BIN="${CARGO_TARGET_DIR:-target}/release/agentmana"
TMP=$(mktemp -d "${TMPDIR:-$HOME/tmp}/agentmana-rec.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
# Frame:key script — OPS, then AGENTS (select an agent), SPEND, MODELS, MANA, VAULT (pick a login), back.
KEYS="40:j,44:j,60:2,64:j,66:j,68:j,70:j,72:j,95:3,120:4,140:5,165:6,175:enter,177:j,179:j,192:esc,195:1"
"$BIN" demo --record "$TMP/frames.jsonl" --frames 215 --size 140x34 --keys "$KEYS"
mkdir -p docs/img
python3 tools/render_frames.py "$TMP/frames.jsonl" docs/img \
  --png 34:ops.png 88:agents.png 115:spend.png 135:models.png 160:mana.png 185:vault.png \
  --gif demo.gif --every 2 --scale 0.75
