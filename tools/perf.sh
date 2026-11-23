#!/usr/bin/env bash
# The battery budget, measured (CLAUDE.md): the dashboard of a release build on the made-up load
# of a busy 4K wall (`swarmy demo --wall`: three seats, ten days of history, about 18 agents at
# work) in a 326x81 tmux window. After a warm-up it sums the CPU time of the dashboard's threads
# (the demo's transcript writer left out) and fails when it is above the budget. CPU time, not
# wall time: other agents load the machine.
#
#   tools/perf.sh              # build, measure 60 s after a 90 s warm-up, fail above 2.0 cpu-s/min
#   tools/perf.sh --mask       # the same with --mask (budget PERF_BUDGET_MASK, default 2.3)
#   PERF_SECS=120 PERF_WARMUP=120 PERF_BUDGET=1.8 tools/perf.sh
#
# Needs tmux. Builds into $CARGO_TARGET_DIR (or target/) and writes the world under
# <target>/perf-<pid>, removed afterwards; never /tmp.
set -euo pipefail
cd "$(dirname "$0")/.."

mask=""
budget=${PERF_BUDGET:-2.0}
if [ "${1:-}" = "--mask" ]; then
    mask="--mask"
    budget=${PERF_BUDGET_MASK:-2.3}
fi
warmup=${PERF_WARMUP:-90}
secs=${PERF_SECS:-60}
command -v tmux >/dev/null || { echo "perf: tmux is needed" >&2; exit 2; }

target=${CARGO_TARGET_DIR:-target}
cargo build --release --quiet
bin="$(cd "$target/release" && pwd)/swarmy"
work="$(mkdir -p "$target" && cd "$target" && pwd)/perf-$$"
mkdir -p "$work"
session="swarmy-perf-$$"
cleanup() {
    tmux kill-session -t "$session" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

tmux new-session -d -s "$session" -x 326 -y 81 \
    "exec env SWARMY_DATA_DIR='$work' SWARMY_DEBUG=1 '$bin' demo --wall --no-boot $mask 2>'$work/debug.log'"
pid=$(tmux list-panes -t "$session" -F '#{pane_pid}')

# The world is written and indexed before the first tick.
for _ in $(seq 1 900); do
    grep -q '^tick' "$work/debug.log" 2>/dev/null && break
    kill -0 "$pid" 2>/dev/null || { echo "perf: the dashboard exited" >&2; tail -5 "$work/debug.log" >&2; exit 2; }
    sleep 1
done
grep -q '^tick' "$work/debug.log" || { echo "perf: no tick after 15 minutes" >&2; exit 2; }
sleep "$warmup"

# Clock ticks of utime + stime of every thread but the demo's writer. The command name may hold
# spaces: fields are counted after its closing parenthesis.
ticks() {
    local sum=0 t
    for t in /proc/"$pid"/task/*; do
        [ "$(cat "$t/comm" 2>/dev/null)" = "demo-writer" ] && continue
        sum=$((sum + $(sed 's/.*) //' "$t/stat" 2>/dev/null | awk '{print $12 + $13}')))
    done
    echo "$sum"
}
t0=$(ticks)
sleep "$secs"
t1=$(ticks)
kill -0 "$pid" 2>/dev/null || { echo "perf: the dashboard exited while measured" >&2; exit 2; }

hz=$(getconf CLK_TCK)
per_min=$(awk -v d=$((t1 - t0)) -v hz="$hz" -v s="$secs" 'BEGIN { printf "%.2f", d / hz * 60 / s }')
pct=$(awk -v m="$per_min" 'BEGIN { printf "%.1f", m / 60 * 100 }')
echo "perf: ${per_min} cpu-s/min (${pct}% of a core) on the wall load${mask:+ with $mask}, budget ${budget}"
awk -v m="$per_min" -v b="$budget" 'BEGIN { exit !(m <= b) }' || {
    echo "perf: over the battery budget; profile with SWARMY_DEBUG=1 (tick and draw timings)" >&2
    exit 1
}
