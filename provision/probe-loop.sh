#!/usr/bin/env bash
#
# Keeps measuring, on a machine where installing system services would be
# overkill — a laptop that moves between networks, for instance.
#
#   ./provision/probe-loop.sh &          # every 30 minutes, into ~/probe.jsonl
#   ./provision/probe-loop.sh 600 &      # or every 10 minutes
#
# Stop it with: pkill -f probe-loop.sh
set -euo pipefail

EVERY="${1:-1800}"
OUT="${OUT:-$HOME/probe.jsonl}"
PROBE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/prebuilt/mesh-probe"

# The same local port every time, so two readings can be compared at all. Bound
# to zero the system picks a new one each run and every reading would differ for
# reasons that say nothing about the network.
PORT="${PORT:-41999}"

[[ -x "$PROBE" ]] || { echo "no probe at $PROBE" >&2; exit 1; }

echo "measuring every ${EVERY}s into $OUT (port $PORT); stop with pkill -f probe-loop.sh"
round=0
while true; do
    "$PROBE" --json --port "$PORT" >> "$OUT" 2>/dev/null || true

    # Occasionally ask how long an idle mapping survives. Rarely, because a
    # single one of these can sit silent for ten minutes, and its answer changes
    # far more slowly than the address does.
    if (( round % 4 == 3 )); then
        "$PROBE" --mapping-lifetime --json >> "$OUT" 2>/dev/null || true
    fi

    round=$((round + 1))
    sleep "$EVERY"
done
