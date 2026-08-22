#!/usr/bin/env bash
#
# Runs several nodes on this machine, makes some of them disappear in different
# ways, and checks that the survivors noticed correctly and quickly enough.
#
#   ./rig.sh [node-count]
#
# This exercises the membership and failure-detection logic. It says nothing
# about real networks: every process here shares one host and reaches every
# other trivially, which is the one thing home connections do not do.
set -euo pipefail

COUNT="${1:-3}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$DIR/target/release/mesh-node"
OUT="$(mktemp -d)"
DETECT_MS=3000

# The threshold this is judged against, from docs/phase-0-plan.md. Detection may
# take a little longer than the window itself, since the check is periodic.
DETECT_BUDGET_MS=4000
# An announced departure travels as a message and should be near-instant. This
# is generous by a wide margin; the point is that it is a different order of
# magnitude from the timeout path, not that it hits a precise number.
ANNOUNCED_BUDGET_MS=500

[[ -x "$BIN" ]] || { echo "build first: cargo build --release" >&2; exit 1; }
[[ $COUNT -ge 3 ]] || { echo "need at least 3 nodes: one leaves, one freezes, one watches" >&2; exit 2; }

names=(); pids=()
for i in $(seq 1 "$COUNT"); do names+=("node$i"); done

cleanup() {
    for pid in "${pids[@]:-}"; do
        kill -CONT "$pid" 2>/dev/null || true
        kill -9 "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "starting $COUNT nodes…"
for name in "${names[@]}"; do
    "$BIN" --label "$name" --detect-ms "$DETECT_MS" --json > "$OUT/$name.log" 2>&1 &
    pids+=($!)
done

# Every node must have seen every other before anything is taken away, or a
# "did not notice" result would only mean "never met".
deadline=$((SECONDS + 20))
while (( SECONDS < deadline )); do
    ready=1
    for name in "${names[@]}"; do
        # grep -c prints 0 and exits non-zero when nothing matches, so the
        # fallback has to replace the value rather than be appended to it.
        seen=$(grep -c '"event":"joined"' "$OUT/$name.log" 2>/dev/null) || seen=0
        (( seen >= COUNT - 1 )) || ready=0
    done
    (( ready )) && break
    sleep 0.5
done
(( ready )) || { echo "FAIL: nodes never all found each other"; exit 1; }
echo "all $COUNT nodes see each other"

now_ms() { echo $(( $(date +%s%N) / 1000000 )); }

# node2 leaves on purpose. node3 is frozen: its process stops responding while
# its connections stay open, which is what a wedged or unreachable machine looks
# like from outside — and the case a clean socket close would never produce.
echo "node2 leaving on purpose…"
LEFT_AT=$(now_ms); kill -TERM "${pids[1]}"; sleep 3

echo "node3 freezing, without a goodbye…"
FROZE_AT=$(now_ms); kill -STOP "${pids[2]}"
sleep $(( (DETECT_BUDGET_MS / 1000) + 3 ))

WATCHER="$OUT/node1.log"
fails=0

check() {
    local label="$1" event="$2" base="$3" budget="$4"
    local line at delta
    line=$(grep "\"event\":\"$event\"" "$WATCHER" | grep "\"label\":\"$label\"" | head -1 || true)
    if [[ -z "$line" ]]; then
        echo "  FAIL  $label: no '$event' was ever reported"
        fails=$((fails + 1)); return
    fi
    at=$(sed 's/.*"ts_unix_ms":\([0-9]*\).*/\1/' <<< "$line")
    delta=$(( at - base ))
    if (( delta <= budget )); then
        printf '  ok    %-6s %-9s noticed after %5d ms (budget %d)\n' "$label" "$event" "$delta" "$budget"
    else
        printf '  FAIL  %-6s %-9s took %d ms, over the %d ms budget\n' "$label" "$event" "$delta" "$budget"
        fails=$((fails + 1))
    fi
}

echo
echo "what node1 saw:"
check node2 left     "$LEFT_AT"  "$ANNOUNCED_BUDGET_MS"
check node3 vanished "$FROZE_AT" "$DETECT_BUDGET_MS"

# A frozen machine that never closed its connections is the case the timeout
# exists for. If the transport had dropped, the freeze did not simulate what it
# was meant to and the timing above proves less than it appears to.
if grep '"event":"vanished"' "$WATCHER" | grep -q '"transport_dropped":"false"'; then
    echo "  ok    node3 was detected by silence alone, with its connections still open"
else
    echo "  WARN  node3's transport dropped, so this measured a disconnect rather than silence"
fi

echo
if (( fails )); then
    echo "$fails check(s) failed. Logs in $OUT"
    trap - EXIT; cleanup
    exit 1
fi
echo "all checks passed"
echo "logs in $OUT"
