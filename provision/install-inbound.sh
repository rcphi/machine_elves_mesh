#!/usr/bin/env bash
#
# Installs the inbound mapping-lifetime test: a responder on a machine that
# can be reached from the internet, and a client timer on the machine whose
# router is being measured. See probe/src/inbound.rs for why it exists.
#
#   on the responder (riverboat):
#     sudo ./install-inbound.sh --responder --token T [--port 3479] [--binary PATH]
#   on the client (diamond):
#     sudo ./install-inbound.sh --client HOST:PORT --token T [--binary PATH]
#
# The token is shared, has no spaces, and only stops strangers from asking the
# responder to send them packets. Generate one with:
#     head -c 18 /dev/urandom | base64 | tr -d '/+='
#
# The responder's port must also be forwarded to it by its router (UDP). That
# is a setting in the router's own pages; this script opens it in ufw only.
#
# --binary installs a new mesh-probe first. The existing timers use it too,
# which is intended: a newer probe is a superset, and it is what now marks
# --mapping-lifetime results on port-preserving routers as inconclusive.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MODE="" RESPONDER="" TOKEN="" PORT=3479 BINARY=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --responder) MODE=responder; shift ;;
        --client) MODE=client; RESPONDER="${2:-}"; shift 2 ;;
        --token) TOKEN="${2:-}"; shift 2 ;;
        --port) PORT="${2:-}"; shift 2 ;;
        --binary) BINARY="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[[ $EUID -eq 0 ]] || { echo "must run as root (sudo)" >&2; exit 1; }
[[ -n "$MODE" ]] || { echo "say --responder or --client HOST:PORT" >&2; exit 2; }
[[ -n "$TOKEN" && "$TOKEN" != *[[:space:]]* ]] || { echo "--token is required, without spaces" >&2; exit 2; }
id -u meshprobe >/dev/null 2>&1 || { echo "no meshprobe user: run setup.sh on this box first" >&2; exit 1; }

if [[ -n "$BINARY" ]]; then
    "$BINARY" --help >/dev/null || { echo "$BINARY does not run here" >&2; exit 1; }
    install -m 0755 "$BINARY" /usr/local/bin/mesh-probe
    echo "installed $(sha256sum /usr/local/bin/mesh-probe | cut -c1-16)… as /usr/local/bin/mesh-probe"
fi
/usr/local/bin/mesh-probe --help | grep -q -- --mapping-inbound \
    || { echo "the installed mesh-probe predates --mapping-inbound; pass --binary" >&2; exit 1; }

umask 027
if [[ $MODE == responder ]]; then
    printf 'RESPONDER_PORT=%s\nMESH_PROBE_INBOUND_TOKEN=%s\n' "$PORT" "$TOKEN" > /etc/mesh-probe/inbound
    chgrp meshprobe /etc/mesh-probe/inbound; chmod 0640 /etc/mesh-probe/inbound
    install -m 0644 "$HERE/mesh-probe-responder.service" /etc/systemd/system/
    if command -v ufw >/dev/null && ufw status | grep -q '^Status: active'; then
        ufw allow "$PORT/udp" comment 'mesh-probe inbound responder' >/dev/null
        echo "ufw: udp/$PORT open"
    fi
    systemctl daemon-reload
    systemctl enable mesh-probe-responder.service >/dev/null 2>&1
    systemctl restart mesh-probe-responder.service
    sleep 1
    systemctl is-active --quiet mesh-probe-responder.service \
        || { journalctl -u mesh-probe-responder -n 20 --no-pager; exit 1; }
    echo "responder running on udp/$PORT. Forward udp/$PORT to this machine in the router."
else
    [[ "$RESPONDER" == *:* ]] || { echo "--client needs HOST:PORT" >&2; exit 2; }
    printf 'RESPONDER=%s\nMESH_PROBE_INBOUND_TOKEN=%s\n' "$RESPONDER" "$TOKEN" > /etc/mesh-probe/inbound
    chgrp meshprobe /etc/mesh-probe/inbound; chmod 0640 /etc/mesh-probe/inbound
    install -m 0644 "$HERE/mesh-probe-inbound.service" "$HERE/mesh-probe-inbound.timer" /etc/systemd/system/
    systemctl daemon-reload
    systemctl enable --now mesh-probe-inbound.timer >/dev/null 2>&1
    # One now, without waiting for it: a run lasts as long as the silence it
    # drew, up to ten minutes. Its record lands in results.jsonl.
    systemctl start --no-block mesh-probe-inbound.service
    echo "client installed; first run started. Results: /var/log/mesh-probe/results.jsonl"
fi
