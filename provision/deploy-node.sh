#!/usr/bin/env bash
#
# Replaces the mesh-node binary on a machine that is already set up, and
# restarts it.
#
#   sudo ./deploy-node.sh ~/mesh-node-new
#   sudo ./deploy-node.sh ~/mesh-node-new --add-peer /ip4/192.168.50.48/udp/4001/quic-v1
#
# setup.sh is for a machine that has never run this before: it makes users,
# units, directories and firewall rules. This is for one that has, and only
# wants the program changed. Separated because reaching for setup.sh to ship a
# new binary means re-running everything else too, and the parts that are
# already correct are the parts most costly to get wrong a second time.
set -euo pipefail

BINARY="${1:-}"
[[ -n "$BINARY" ]] || { echo "usage: sudo $0 <path-to-new-mesh-node> [--add-peer <multiaddr>]" >&2; exit 1; }
[[ -f "$BINARY" ]] || { echo "no such file: $BINARY" >&2; exit 1; }
[[ $EUID -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

ADD_PEER=""
shift || true
while [[ $# -gt 0 ]]; do
    case "$1" in
        --add-peer) ADD_PEER="${2:-}"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

INSTALLED=/usr/local/bin/mesh-node
CONFIG=/etc/mesh-node/config

# Refuse a binary that will not run here rather than discovering it after the
# old one is gone. A node that fails to start looks identical, from every other
# machine, to one whose owner went out.
"$BINARY" --help >/dev/null 2>&1 || { echo "that binary does not run on this machine" >&2; exit 1; }

# What the job had reached, read before anything stops. If this deploy loses
# work, the number to compare against has to have been taken first.
BEFORE=$(grep '"event":"working"' /var/log/mesh-node/events.jsonl 2>/dev/null | tail -1 || true)
[[ -n "$BEFORE" ]] && echo "before: $BEFORE"

if [[ -n "$ADD_PEER" ]]; then
    if grep -q -- "$ADD_PEER" "$CONFIG"; then
        echo "peer already listed: $ADD_PEER"
    else
        # Added, never replacing. A node that moves between networks needs every
        # address it might reach a peer by; the one that is useless today is the
        # one that works tomorrow.
        current=$(grep '^PEERS=' "$CONFIG" | cut -d'"' -f2)
        sed -i "s|^PEERS=.*|PEERS=\"${current:+$current }$ADD_PEER\"|" "$CONFIG"
        echo "added peer: $ADD_PEER"
    fi
fi

systemctl stop mesh-node
install -m 0755 "$BINARY" "$INSTALLED"
systemctl start mesh-node

sleep 3
systemctl is-active --quiet mesh-node && echo "mesh-node is running" || {
    echo "mesh-node did NOT come back — check: journalctl -u mesh-node -n 40" >&2
    exit 1
}

echo
echo "Installed $(md5sum "$INSTALLED" | cut -c1-12)."
echo "Watch it settle with:  sudo $(dirname "$(readlink -f "$0")")/node-summary.py"
