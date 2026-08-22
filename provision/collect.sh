#!/usr/bin/env bash
#
# Pulls measurement records off the volunteer boxes onto the operator's machine.
#
#   ./collect.sh vol-1 vol-2 vol-3
#
# Hosts are anything ssh understands: a hostname, an alias from ~/.ssh/config,
# or user@host. Results land in ./results/<host>.jsonl and are merged into
# ./results/all.jsonl for analysis.
#
# Reading is one-way and idempotent. Nothing is written to the boxes.
set -euo pipefail

OUT_DIR="${OUT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/results}"
REMOTE_LOG="/var/log/mesh-probe/results.jsonl"

[[ $# -gt 0 ]] || { echo "usage: collect.sh <host> [host…]" >&2; exit 2; }
mkdir -p "$OUT_DIR"

failures=0
for host in "$@"; do
    printf '%-24s ' "$host"
    if ssh -o ConnectTimeout=10 -o BatchMode=yes "$host" "cat $REMOTE_LOG" > "$OUT_DIR/$host.jsonl.tmp" 2>/dev/null; then
        mv "$OUT_DIR/$host.jsonl.tmp" "$OUT_DIR/$host.jsonl"
        printf 'ok (%s records)\n' "$(wc -l < "$OUT_DIR/$host.jsonl" | tr -d ' ')"
    else
        rm -f "$OUT_DIR/$host.jsonl.tmp"
        printf 'UNREACHABLE\n'
        failures=$((failures + 1))
    fi
done

# The merged file must be excluded from its own inputs: the shell truncates it
# before cat runs, so leaving it in the glob would silently read an empty file.
find "$OUT_DIR" -maxdepth 1 -name '*.jsonl' ! -name 'all.jsonl' -print0 \
    | sort -z | xargs -0 -r cat > "$OUT_DIR/all.jsonl"

echo
if [[ $failures -gt 0 ]]; then
    echo "$failures host(s) unreachable — which is itself worth recording, since a box"
    echo "that cannot be reached is a box a peer could not have reached either."
fi
echo "merged into $OUT_DIR/all.jsonl"
echo "summarise with: ./summarise.py $OUT_DIR/all.jsonl"
