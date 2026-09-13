#!/usr/bin/env bash
#
# Plays two boxes on one machine and lets provision/chaos.py loose on both, so
# the chaos tool, the node's takeover events and the summary are exercised
# together before any of them touch a real box.
#
#   sudo ./chaos-rig.sh [minutes]      default 15
#
# Each node is a systemd unit with Restart=always, like mesh-node.service on a
# box, because chaos.py kills and partitions by unit. Timings are compressed —
# fires every 15–30 s instead of 3–7 min, settled after 20 s instead of 60 — so a
# quarter of an hour yields a few dozen takeovers.
#
# This tests the logic and the tooling. It says nothing about real networks:
# both nodes share loopback, where nothing is ever slow or lost.
set -euo pipefail
set +m

[[ $EUID -eq 0 ]] || { echo "run me with sudo: nodes run as systemd units" >&2; exit 1; }

MINUTES="${1:-15}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$DIR/target/release/mesh-node"
JOB="$DIR/../jobs/factory/target/wasm32-unknown-unknown/release/factory_job.wasm"
CHAOS="$DIR/../provision/chaos.py"
SUMMARY="$DIR/../provision/takeover-summary.py"
OUT="$(mktemp -d)"
chmod 755 "$OUT"

[[ -x "$BIN" ]] || { echo "build first: cargo build --release" >&2; exit 1; }
[[ -f "$JOB" ]] || { echo "build the jobs first: ../jobs/build.sh" >&2; exit 1; }

NAMES=(rig-east rig-west)
PORTS=(4301 4302)
loops=()

cleanup() {
    for p in "${loops[@]:-}"; do kill "$p" 2>/dev/null || true; done
    for name in "${NAMES[@]}"; do
        systemctl stop "$name.service" 2>/dev/null || true
        systemctl reset-failed "$name.service" 2>/dev/null || true
        nft delete table inet "mesh_chaos_${name//-/_}" 2>/dev/null || true
    done
}
trap cleanup EXIT
cleanup

for i in 0 1; do
    name="${NAMES[$i]}" port="${PORTS[$i]}" other="${PORTS[$((1 - i))]}"
    own=(); (( i == 0 )) && own=(--own)
    systemd-run --quiet --unit="$name" \
        -p Restart=always -p RestartSec=10 \
        -p StandardOutput="append:$OUT/$name.events.jsonl" \
        -p StandardError="append:$OUT/$name.events.jsonl" \
        "$BIN" --label "$name" --mesh "chaos-rig-$$" --json \
        --port "$port" --no-map-port --no-mdns \
        --peer "/ip4/127.0.0.1/udp/$other/quic-v1" \
        --identity "$OUT/$name.identity" --ledger "$OUT/$name.ledger" \
        --job "$JOB" "${own[@]}"
done

chaos_env() {
    local name="$1"
    echo MESH_NODE_UNIT="$name.service" MESH_NODE_LABEL="$name" \
         MESH_NODE_LOG="$OUT/$name.events.jsonl" MESH_CHAOS_LOG="$OUT/$name.chaos.jsonl" \
         MESH_CHAOS_STATE="$OUT/$name.state" MESH_CHAOS_SETTLED_S=20
}

echo "two nodes up; chaos for $MINUTES minutes. logs in $OUT"
for name in "${NAMES[@]}"; do
    mkdir -p "$OUT/$name.state"
    # Armed by hand rather than with `chaos.py arm`, which would enable the
    # real timer on this machine.
    until=$(( $(( $(date +%s%N) / 1000000 )) + MINUTES * 60 * 1000 ))
    echo "$until" > "$OUT/$name.state/until"
    printf '{"ts_unix_ms": %s, "node": "%s", "action": "armed", "hours": %s}\n' \
        "$(( $(date +%s%N) / 1000000 ))" "$name" "$(awk "BEGIN {print $MINUTES / 60}")" >> "$OUT/$name.chaos.jsonl"
    (
        # shellcheck disable=SC2046
        while [[ -f "$OUT/$name.state/until" ]]; do
            sleep $(( 15 + RANDOM % 16 ))
            env $(chaos_env "$name") python3 "$CHAOS" fire > /dev/null 2>&1 || true
        done
    ) &
    loops+=($!)
done

deadline=$((SECONDS + MINUTES * 60 + 90))
while (( SECONDS < deadline )); do
    alive=0
    for p in "${loops[@]}"; do kill -0 "$p" 2>/dev/null && alive=1; done
    (( alive )) || break
    sleep 10
    printf '\r  %3d s   disruptions so far: %s ' "$SECONDS" \
        "$(cat "$OUT"/*.chaos.jsonl | grep -c '"action": "\(kill\|term\|partition\)"' || true)"
done
echo

# Give the last takeover time to land before stopping the nodes.
sleep 30
cleanup
trap - EXIT
python3 "$SUMMARY" "$OUT"/*.jsonl
echo
echo "logs in $OUT"
