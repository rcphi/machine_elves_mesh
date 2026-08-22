#!/usr/bin/env bash
#
# Runs a job on one node, takes that node away twice — once politely, once not —
# and checks that exactly one survivor continued the work, and how long it took.
#
#   ./migration-rig.sh
set -euo pipefail
# Job control announces every killed background process, which buries the
# results in noise that looks like errors and is not.
set +m

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$DIR/target/release/mesh-node"
JOB="$DIR/../jobs/factory/target/wasm32-unknown-unknown/release/factory_job.wasm"
OUT="$(mktemp -d)"

# An announced handover is a message, so it should be near-instant. Generous by
# a wide margin: the point is that it is a different order of magnitude from the
# timeout path, not that it hits a precise number.
ANNOUNCED_BUDGET_MS=500
# An unannounced loss cannot beat the detection window, and the sweep that
# notices it runs periodically.
DETECT_MS=3000
UNANNOUNCED_BUDGET_MS=4500

[[ -x "$BIN" ]] || { echo "build first: cargo build --release" >&2; exit 1; }
[[ -f "$JOB" ]] || { echo "build the jobs first: ../jobs/build.sh" >&2; exit 1; }

pids=()
cleanup() {
    for p in "${pids[@]:-}"; do kill -9 "$p" 2>/dev/null || true; done
    pids=()
}
trap cleanup EXIT

# Nodes find each other by broadcasting on the local network, so anything left
# running from an earlier attempt joins this mesh and changes the result. Each
# case also gets its own mesh name, since a node killed a moment ago may still
# be exiting.
pkill -x mesh-node 2>/dev/null || true
sleep 1

now_ms() { echo $(( $(date +%s%N) / 1000000 )); }
fails=0

run_case() {
    local name="$1" signal="$2" budget="$3"
    rm -f "$OUT"/*.log
    # Every early return below must leave nothing running, or the next case
    # inherits a mesh it did not ask for — which is exactly how this rig first
    # reported a failure that was its own doing.
    cleanup
    local mesh="rig-$$-$RANDOM-$name"
    mesh="${mesh// /-}"

    "$BIN" --label owner --mesh "$mesh" --job "$JOB" --own --detect-ms "$DETECT_MS" --json > "$OUT/owner.log" 2>&1 &
    local owner=$!
    pids+=($owner)
    sleep 1
    for s in a b; do
        "$BIN" --label "standby-$s" --mesh "$mesh" --job "$JOB" --detect-ms "$DETECT_MS" --json > "$OUT/$s.log" 2>&1 &
        pids+=($!)
    done

    # Readiness is not "the job started" — it is "both standbys are following
    # it". Waiting for the weaker condition kills the owner before anyone has
    # received a checkpoint, so nobody has anything to continue and the rig
    # reports a failure of its own making.
    local deadline=$((SECONDS + 25)) ready=0
    while (( SECONDS < deadline )); do
        # Both conditions matter. Following the owner is not enough: the
        # standbys must also know *each other*, or each will believe it is the
        # last node standing and both will continue the job.
        local following=0 s
        for s in a b; do
            grep -q '"event":"job-owner"' "$OUT/$s.log" 2>/dev/null || continue
            grep -q '"label":"standby-' "$OUT/$s.log" 2>/dev/null && following=$((following+1))
        done
        local effects
        effects=$(grep -c '"event":"effect"' "$OUT/owner.log" 2>/dev/null) || effects=0
        if (( following == 2 && effects > 0 )); then ready=1; break; fi
        sleep 0.5
    done
    (( ready )) || {
        echo "  FAIL  $name: the standbys never picked up the job (effects=$effects, following=$following)"
        fails=$((fails+1)); cleanup; return
    }

    local at; at=$(now_ms)
    kill "-$signal" "$owner" 2>/dev/null || true
    sleep $(( (budget / 1000) + 4 ))

    local takeovers
    takeovers=$(cat "$OUT"/*.log | grep -c '"event":"took-over"') || takeovers=0
    # At least one node must continue the work. More than one is wasteful
    # rather than wrong — they run identical ticks from identical state — but it
    # is worth seeing, because whatever applies effects has to tolerate it.
    if (( takeovers < 1 )); then
        echo "  FAIL  $name: nobody continued the job"
        fails=$((fails+1)); cleanup; return
    fi
    if (( takeovers > 1 )); then
        echo "  note  $name: $takeovers nodes continued it — duplicate work, and"
        echo "        a reminder that effects must be identified by (job, tick)"
    fi

    local line ts delta tick
    line=$(cat "$OUT"/*.log | grep '"event":"took-over"' | head -1)
    ts=$(sed 's/.*"ts_unix_ms":\([0-9]*\).*/\1/' <<< "$line")
    tick=$(sed 's/.*"tick":"\([0-9]*\)".*/\1/' <<< "$line")
    delta=$(( ts - at ))

    if (( delta <= budget )); then
        printf '  ok    %-24s resumed at tick %-4s after %5d ms (budget %d)\n' "$name" "$tick" "$delta" "$budget"
    else
        printf '  FAIL  %-24s took %d ms, over the %d ms budget\n' "$name" "$delta" "$budget"
        fails=$((fails+1))
    fi

    cleanup
    sleep 1
}

echo "migrating a running job away from a node that goes away:"
run_case "announced departure" TERM "$ANNOUNCED_BUDGET_MS"
run_case "unannounced loss"    KILL "$UNANNOUNCED_BUDGET_MS"

echo
if (( fails )); then
    echo "$fails case(s) failed. Logs in $OUT"
    exit 1
fi
echo "all cases passed"
