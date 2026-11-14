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
# Each host's file is an archive, not a snapshot. Every collection reads the
# box's current log *and its rotations*, and adds whatever the archive does not
# already hold. It used to read only the current log and overwrite the local
# copy, so once a box's log rotated, everything older than the rotation
# vanished from the dataset at the next collection (review finding 15). The
# probe log had in fact never rotated — see logrotate.mesh-probe — which is
# the only reason nothing was lost.
#
# A box reached through a jump host, like diamond over its back channel, needs
# an ~/.ssh/config entry (Host, HostName localhost, Port 2201, ProxyJump).
#
# Reading is one-way and idempotent. Nothing is written to the boxes.
set -euo pipefail

OUT_DIR="${OUT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/results}"
# Overridable, because a machine running the userspace loop keeps its readings
# in a home directory rather than under /var/log.
REMOTE_LOG="${REMOTE_LOG:-/var/log/mesh-probe/results.jsonl}"

[[ $# -gt 0 ]] || { echo "usage: collect.sh <host> [host…]" >&2; exit 2; }
mkdir -p "$OUT_DIR"

# merge ARCHIVE NEW: adds NEW's records to ARCHIVE, once each, in time order.
# Prints how many the archive holds and how many were added.
merge() {
    python3 -I - "$1" "$2" <<'PY'
import json, os, sys
archive, incoming = sys.argv[1], sys.argv[2]
def lines(path):
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8", errors="replace") as handle:
        return [l.rstrip("\n") for l in handle if l.strip()]
def when(line):
    try:
        record = json.loads(line)
        return record.get("ts_unix", record.get("ts_unix_ms", 0) / 1000)
    except (ValueError, AttributeError):
        return 0
held = lines(archive)
seen = set(held)
added = [l for l in dict.fromkeys(lines(incoming)) if l not in seen]
merged = sorted(held + added, key=when)
tmp = archive + ".merging"
with open(tmp, "w", encoding="utf-8") as handle:
    handle.write("".join(l + "\n" for l in merged))
os.replace(tmp, archive)
print(f"{len(merged)} records, {len(added)} new")
PY
}

failures=0
hints=""
errors="$(mktemp)"
trap 'rm -f "$errors"' EXIT

for host in "$@"; do
    printf '%-26s ' "$host"
    # Filenames are cosmetic — every record carries its own label — so the host
    # string is flattened rather than trusted to be filesystem-friendly.
    safe="$(printf '%s' "$host" | tr -c 'A-Za-z0-9._-' '-')"

    # The current log first, so a missing one still fails with "No such file"
    # and the hint below; then every rotation, compressed or not.
    remote="cat $REMOTE_LOG && for f in $REMOTE_LOG.[0-9]*; do [ -e \"\$f\" ] || continue; \
case \"\$f\" in *.gz) zcat -- \"\$f\" ;; *) cat -- \"\$f\" ;; esac; done"
    if ssh -o ConnectTimeout=10 -o BatchMode=yes "$host" "$remote" \
            > "$OUT_DIR/$safe.jsonl.tmp" 2>"$errors"; then
        printf 'ok (%s)\n' "$(merge "$OUT_DIR/$safe.jsonl" "$OUT_DIR/$safe.jsonl.tmp")"
        rm -f "$OUT_DIR/$safe.jsonl.tmp"
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

# The merged file must be excluded from its own inputs. Rebuilt from the
# archives rather than appended to, and through `merge`, so a box collected
# under two names (an address, then an alias) is still counted once.
rm -f "$OUT_DIR/all.jsonl"
while IFS= read -r -d '' archive; do
    merge "$OUT_DIR/all.jsonl" "$archive" >/dev/null
done < <(find "$OUT_DIR" -maxdepth 1 -name '*.jsonl' ! -name 'all.jsonl' -print0 | sort -z)
touch "$OUT_DIR/all.jsonl"

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
