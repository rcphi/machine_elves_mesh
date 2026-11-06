#!/usr/bin/env bash
#
# riverboat's half of diamond's LTE back channel.
#
#   sudo ./install-riverboat.sh            # user, restricted key, sshd drop-in
#   sudo ./install-riverboat.sh --remove
#
# Expects, beside itself: 40-backchannel.conf, and diamond-backchannel.pub
# (printed by install-diamond.sh). Design:
# docs/superpowers/specs/2026-11-05-lte-backchannel-design.md
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR=/var/lib/backchannel
DROPIN=/etc/ssh/sshd_config.d/40-backchannel.conf
LISTEN_PORT=2201

[[ $EUID -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

if [[ "${1:-}" == "--remove" ]]; then
    # The key goes first and the limits last, so at no point is the key
    # usable without them. A connected tunnel holds the user open, so end
    # its processes before userdel, which would otherwise refuse.
    rm -f "$HOME_DIR/.ssh/authorized_keys"
    pkill -u backchannel 2>/dev/null || true
    if id backchannel &>/dev/null; then userdel -r backchannel; fi
    rm -f "$DROPIN"
    sshd -t
    systemctl try-reload-or-restart ssh.service
    echo "removed: key, user backchannel, drop-in"
    exit 0
fi
[[ $# -eq 0 ]] || { echo "usage: $0 [--remove]" >&2; exit 1; }

for f in 40-backchannel.conf diamond-backchannel.pub; do
    [[ -f "$DIR/$f" ]] || { echo "missing beside me: $f" >&2; exit 1; }
done
key="$(head -1 "$DIR/diamond-backchannel.pub")"
[[ "$key" == ssh-ed25519\ * ]] || { echo "diamond-backchannel.pub is not an ed25519 public key" >&2; exit 1; }

# The limits go in before the key exists, so the key is never usable without
# them: if sshd rejects the drop-in, there is no key to worry about.
install -m 0644 "$DIR/40-backchannel.conf" "$DROPIN"
if ! sshd -t; then
    rm -f "$DROPIN"
    echo "sshd rejected the drop-in; removed it, installed no key, reloaded nothing" >&2
    exit 1
fi
systemctl try-reload-or-restart ssh.service

id backchannel &>/dev/null ||
    adduser --system --group --home "$HOME_DIR" --shell /usr/sbin/nologin backchannel
passwd -l backchannel >/dev/null
install -d -m 0700 -o backchannel -g backchannel "$HOME_DIR/.ssh"
# `restrict,port-forwarding` re-enables -L as well as -R, so -L is pinned to a
# port nothing listens on: the key line alone, without the drop-in, still
# cannot reach into the home network.
trap 'rm -f "$HOME_DIR/.ssh/authorized_keys"' ERR
printf 'restrict,port-forwarding,permitlisten="localhost:%s",permitopen="127.0.0.1:1" %s\n' \
    "$LISTEN_PORT" "$key" > "$HOME_DIR/.ssh/authorized_keys"
chown backchannel:backchannel "$HOME_DIR/.ssh/authorized_keys"
chmod 0600 "$HOME_DIR/.ssh/authorized_keys"
trap - ERR
echo "riverboat ready: backchannel may hold localhost:$LISTEN_PORT and nothing else"
