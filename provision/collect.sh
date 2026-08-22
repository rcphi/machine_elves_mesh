#!/usr/bin/env bash
#
# Pulls measurement records off the volunteer boxes onto the operator's machine.
#
#   ./collect.sh vol-1 vol-2 vol-3
#
# Hosts are anything ssh understands. Without DNS on your network that means
# user@address:
#
#   ./collect.sh rpc@192.168.50.48
#
# A bare name only works if it is in DNS, /etc/hosts, or ~/.ssh/config.
#
# Results land in ./results/<host>.jsonl and are merged into ./results/all.jsonl.
#
# Reading is one-way and idempotent. Nothing is written to the boxes.
set -euo pipefail

OUT_DIR="${OUT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/results}"
REMOTE_LOG="/var/log/mesh-probe/results.jsonl"

[[ $# -gt 0 ]] || { echo "usage: collect.sh <host> [host…]" >&2; exit 2; }
mkdir -p "$OUT_DIR"

failures=0
hints=""
errors="$(mktemp)"
trap 'rm -f "$errors"' EXIT

for host in "$@"; do
    printf '%-26s ' "$host"
    # Filenames are cosmetic — every record carries its own label — so the host
    # string is flattened rather than trusted to be filesystem-friendly.
    safe="$(printf '%s' "$host" | tr -c 'A-Za-z0-9._-' '-')"

    if ssh -o ConnectTimeout=10 -o BatchMode=yes "$host" "cat $REMOTE_LOG" \
            > "$OUT_DIR/$safe.jsonl.tmp" 2>"$errors"; then
        mv "$OUT_DIR/$safe.jsonl.tmp" "$OUT_DIR/$safe.jsonl"
        printf 'ok (%s records)\n' "$(wc -l < "$OUT_DIR/$safe.jsonl" | tr -d ' ')"
        continue
    fi

    rm -f "$OUT_DIR/$safe.jsonl.tmp"
    failures=$((failures + 1))

    # Say why. Swallowing ssh's own message turns every distinct failure into
    # one indistinguishable word, which is worse than no error at all.
    reason="$(grep -v '^Warning: Permanently added' "$errors" | tail -1)"
    printf 'FAILED — %s\n' "${reason:-ssh gave no reason}"

    case "$reason" in
        *"Could not resolve"*|*"Name or service not known"*|*"nodename nor servname"*)
            hints="${hints}  '$host' is not a name this machine can look up. With no DNS,
  use the address instead:  ./collect.sh rpc@192.168.50.48
"   ;;
        *"Permission denied"*)
            hints="${hints}  '$host' refused the key. This script forbids password prompts on
  purpose, so that an unattended collection cannot hang waiting for input.
  Set up key-based login:   ssh-copy-id $host
"   ;;
        *"Connection refused"*|*"No route to host"*|*"timed out"*)
            hints="${hints}  '$host' did not answer. Check it is powered on and that you can
  reach it at all:          ssh $host
"   ;;
        *"No such file"*)
            hints="${hints}  '$host' has no results yet at $REMOTE_LOG.
  Check the timers there:   systemctl list-timers 'mesh-probe*'
"   ;;
    esac
done

# The merged file must be excluded from its own inputs: the shell truncates it
# before cat runs, so leaving it in the glob would silently read an empty file.
find "$OUT_DIR" -maxdepth 1 -name '*.jsonl' ! -name 'all.jsonl' -print0 \
    | sort -z | xargs -0 -r cat > "$OUT_DIR/all.jsonl"

echo
if [[ -n "$hints" ]]; then
    printf '%s\n' "$hints"
fi
if [[ $failures -gt 0 ]]; then
    echo "$failures host(s) could not be collected from."
    echo "A box you cannot reach is also a box a peer could not have reached, so if this"
    echo "is the network rather than the tooling, it is a result rather than an obstacle."
    echo
fi

collected=$(wc -l < "$OUT_DIR/all.jsonl" | tr -d " ")
echo "$collected records in $OUT_DIR/all.jsonl"
if [[ "$collected" != "0" ]]; then
    echo "summarise with: ./summarise.py"
fi
