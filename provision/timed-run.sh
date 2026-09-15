#!/usr/bin/env bash
#
# Switches a box to a named mesh and starts a timed takeover run on it, in one
# command, because nothing else about a run should depend on remembering steps.
#
#   sudo ./timed-run.sh --label diamond   --mesh night-a --peer 192.168.50.128 --own --hours 8
#   sudo ./timed-run.sh --label riverboat --mesh night-a --peer 192.168.50.48         --hours 8
#   sudo ./timed-run.sh --revert
#
# Expects, beside itself: mesh-node (the build to run), chaos.py,
# mesh-chaos.service and mesh-chaos.timer.
#
# What it does:
#   - keeps the current config and binary, so --revert puts the box back
#   - points the node at the named mesh, the given peer's LAN address, and a
#     ledger file of the run's own: the old one stays where it is, untouched,
#     and none of its history is dragged into the measurements
#   - installs the new binary and chaos tool, and arms chaos for --hours
#
# **A hotspot run must be short.** A night on a phone hotspot has already cost
# 7.54 GB. --hours is required, and over 2 it asks for --not-a-hotspot, which is
# a question to answer honestly rather than a flag to add by habit.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG=/etc/mesh-node/config
BIN=/usr/local/bin/mesh-node
LOG=/var/log/mesh-node/events.jsonl
PORT_DEFAULT=4001

[[ $EUID -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

LABEL="" MESH="" PEER="" OWN=no HOURS="" NOT_HOTSPOT=no REVERT=no
while [[ $# -gt 0 ]]; do
    case "$1" in
        --label) LABEL="$2"; shift 2 ;;
        --mesh) MESH="$2"; shift 2 ;;
        --peer) PEER="$2"; shift 2 ;;
        --own) OWN=yes; shift ;;
        --hours) HOURS="$2"; shift 2 ;;
        --not-a-hotspot) NOT_HOTSPOT=yes; shift ;;
        --revert) REVERT=yes; shift ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [[ $REVERT == yes ]]; then
    latest=$(ls -t "$CONFIG".before-* 2>/dev/null | head -1 || true)
    [[ -n "$latest" ]] || { echo "nothing to revert: no $CONFIG.before-* found" >&2; exit 1; }
    suffix="${latest##*.before-}"
    command -v mesh-chaos >/dev/null && mesh-chaos disarm || true
    systemctl stop mesh-node
    cp "$latest" "$CONFIG"
    [[ -f "$BIN.before-$suffix" ]] && install -m 0755 "$BIN.before-$suffix" "$BIN"
    systemctl start mesh-node
    echo "reverted to the config and binary from before $suffix"
    exit 0
fi

[[ -n "$LABEL" && -n "$MESH" && -n "$PEER" && -n "$HOURS" ]] || {
    echo "needs --label, --mesh, --peer and --hours (see the top of this file)" >&2; exit 1; }
[[ "$MESH" =~ ^[A-Za-z0-9_-]+$ ]] || { echo "mesh name: letters, digits, - and _ only" >&2; exit 1; }
if awk "BEGIN {exit !($HOURS > 2)}" && [[ $NOT_HOTSPOT != yes ]]; then
    echo "--hours $HOURS is longer than a hotspot run should be. If this box is on" >&2
    echo "wired or home broadband, add --not-a-hotspot." >&2
    exit 1
fi
for f in mesh-node chaos.py mesh-chaos.service mesh-chaos.timer; do
    [[ -f "$DIR/$f" ]] || { echo "missing beside me: $f" >&2; exit 1; }
done
chmod 0755 "$DIR/mesh-node"
"$DIR/mesh-node" --help >/dev/null 2>&1 || { echo "that mesh-node does not run on this machine" >&2; exit 1; }

port=$(grep '^PORT=' "$CONFIG" | cut -d= -f2 || true)
port="${port:-$PORT_DEFAULT}"

echo "== keeping what is here, as .before-$MESH"
cp -n "$CONFIG" "$CONFIG.before-$MESH"
cp -n "$BIN" "$BIN.before-$MESH"

echo "== pointing the node at mesh $MESH"
# A LAN address only. A home box's public address cannot be dialled from
# inside the same home network — the router will not hairpin — so listing it
# produces nothing but a log full of failed dials.
sed -i "s|^LABEL=.*|LABEL=$LABEL|" "$CONFIG"
sed -i "s|^MESH=.*|MESH=$MESH|" "$CONFIG"
sed -i "s|^PEERS=.*|PEERS=\"/ip4/$PEER/udp/$port/quic-v1\"|" "$CONFIG"
sed -i "s|^OWN=.*|OWN=$OWN|" "$CONFIG"
grep -q '^EXTRA=' "$CONFIG" || echo 'EXTRA=""' >> "$CONFIG"
sed -i "s|^EXTRA=.*|EXTRA=\"--ledger /var/lib/mesh-node/ledger-$MESH\"|" "$CONFIG"
sed 's/^/    /' "$CONFIG"

echo "== installing the node"
systemctl stop mesh-node
install -m 0755 "$DIR/mesh-node" "$BIN"
started_at=$(date +%s)
systemctl start mesh-node

echo "== installing chaos"
python3 "$DIR/chaos.py" install

echo "== waiting up to 60 s for the peer"
joined=no
for _ in $(seq 60); do
    if tail -n 400 "$LOG" | grep '"event":"joined"' | python3 -c "
import json,sys
print(any(json.loads(l)['ts_unix_ms'] >= $started_at * 1000 for l in sys.stdin))" | grep -q True; then
        joined=yes; break
    fi
    sleep 1
done
if [[ $joined == yes ]]; then
    echo "    joined"
else
    echo "    no peer yet — expected on the first box set up; the second will find it"
fi

# Armed either way. Chaos acts only when this node runs the job with a peer
# present, so arming the first box a few minutes early costs nothing.
mesh-chaos arm "$HOURS"
echo
mesh-chaos status
cat <<DONE

Running. Nothing needs to reach this box until the run ends. To stop early:
    sudo mesh-chaos disarm
To put everything back afterwards:
    sudo $DIR/timed-run.sh --revert
DONE
