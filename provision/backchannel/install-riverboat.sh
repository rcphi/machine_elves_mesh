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
    rm -f "$DROPIN"
    sshd -t && systemctl try-reload-or-restart ssh.service
    id backchannel &>/dev/null && userdel -r backchannel
    echo "removed: drop-in and user backchannel"
    exit 0
fi
[[ $# -eq 0 ]] || { echo "usage: $0 [--remove]" >&2; exit 1; }

for f in 40-backchannel.conf diamond-backchannel.pub; do
    [[ -f "$DIR/$f" ]] || { echo "missing beside me: $f" >&2; exit 1; }
done
key="$(head -1 "$DIR/diamond-backchannel.pub")"
[[ "$key" == ssh-ed25519\ * ]] || { echo "diamond-backchannel.pub is not an ed25519 public key" >&2; exit 1; }

id backchannel &>/dev/null ||
    adduser --system --group --home "$HOME_DIR" --shell /usr/sbin/nologin backchannel
passwd -l backchannel >/dev/null
install -d -m 0700 -o backchannel -g backchannel "$HOME_DIR/.ssh"
printf 'restrict,port-forwarding,permitlisten="localhost:%s" %s\n' "$LISTEN_PORT" "$key" \
    > "$HOME_DIR/.ssh/authorized_keys"
chown backchannel:backchannel "$HOME_DIR/.ssh/authorized_keys"
chmod 0600 "$HOME_DIR/.ssh/authorized_keys"

install -m 0644 "$DIR/40-backchannel.conf" "$DROPIN"
if ! sshd -t; then
    rm -f "$DROPIN"
    echo "sshd rejected the drop-in; removed it and reloaded nothing" >&2
    exit 1
fi
systemctl try-reload-or-restart ssh.service
echo "riverboat ready: backchannel may hold localhost:$LISTEN_PORT and nothing else"
