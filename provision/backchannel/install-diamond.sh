#!/usr/bin/env bash
#
# diamond's half of its LTE back channel.
#
#   sudo ./install-diamond.sh            # user, key, files, units; prints the key for riverboat
#   sudo ./install-diamond.sh --start    # once riverboat has the key: enable link and tunnel
#   sudo ./install-diamond.sh --remove   # stop and remove units, routes and rules; keeps the user and key
#
# Expects, beside itself: mesh-backchannel-link, mesh-backchannel-link.service,
# mesh-backchannel-link.timer, mesh-backchannel.service.in, config,
# riverboat-host-key.pub, 60-mesh-backchannel.sysctl.conf,
# 60-mesh-backchannel.networkd.conf, 10-mesh-backchannel-wwan.network. Design:
# docs/superpowers/specs/2026-11-05-lte-backchannel-design.md
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ETC=/etc/mesh-backchannel
HOME_DIR=/var/lib/backchannel
BIN=/usr/local/sbin/mesh-backchannel-link
UNITS=/etc/systemd/system

[[ $EUID -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

case "${1:-}" in
--remove)
    systemctl disable --now mesh-backchannel.service mesh-backchannel-link.timer 2>/dev/null || true
    rm -f "$UNITS/mesh-backchannel.service" "$UNITS/mesh-backchannel-link.service" \
          "$UNITS/mesh-backchannel-link.timer" "$BIN"
    systemctl daemon-reload
    if uid="$(id -u backchannel 2>/dev/null)"; then
        ip rule del uidrange "$uid-$uid" lookup 4302 priority 1000 2>/dev/null || true
        ip -6 rule del uidrange "$uid-$uid" prohibit priority 1000 2>/dev/null || true
    fi
    ip route flush table 4302 2>/dev/null || true
    # Down before the Unmanaged file goes: networkd would otherwise take a
    # DHCP lease on the live bearer via dracut's catch-all, putting an LTE
    # default in main and the carrier's DNS in resolved.
    mmcli -m any --simple-disconnect >/dev/null 2>&1 || true
    rm -f /etc/sysctl.d/60-mesh-backchannel.conf /etc/systemd/networkd.conf.d/60-mesh-backchannel.conf \
          /etc/systemd/network/10-mesh-backchannel-wwan.network
    sysctl --system -q
    systemctl restart systemd-networkd
    echo "removed, and the modem disconnected. Kept: user backchannel, its key, $ETC."
    exit 0 ;;
--start)
    [[ -f "$ETC/known_hosts" && -x "$BIN" ]] || { echo "install first: sudo $0" >&2; exit 1; }
    systemctl enable --now mesh-backchannel-link.timer
    systemctl start mesh-backchannel-link.service
    systemctl enable --now mesh-backchannel.service
    systemctl --no-pager status mesh-backchannel.service | head -5
    exit 0 ;;
"") ;;
*) echo "usage: $0 [--start | --remove]" >&2; exit 1 ;;
esac

for f in mesh-backchannel-link mesh-backchannel-link.service mesh-backchannel-link.timer \
         mesh-backchannel.service.in config riverboat-host-key.pub \
         60-mesh-backchannel.sysctl.conf 60-mesh-backchannel.networkd.conf \
         10-mesh-backchannel-wwan.network; do
    [[ -f "$DIR/$f" ]] || { echo "missing beside me: $f" >&2; exit 1; }
done

id backchannel &>/dev/null ||
    adduser --system --group --home "$HOME_DIR" --shell /usr/sbin/nologin backchannel
install -d -m 0700 -o backchannel -g backchannel "$HOME_DIR/.ssh"
[[ -f "$HOME_DIR/.ssh/id_ed25519" ]] ||
    sudo -u backchannel ssh-keygen -q -t ed25519 -N "" -C backchannel@diamond -f "$HOME_DIR/.ssh/id_ed25519"

install -d -m 0755 "$ETC"
[[ -f "$ETC/config" ]] || install -m 0644 "$DIR/config" "$ETC/config"
# shellcheck source=config
. "$ETC/config"
for v in HOME_ADDR HOME_PORT LISTEN_PORT; do
    [[ -n "${!v:-}" ]] || { echo "$ETC/config sets no $v" >&2; exit 1; }
done
printf '[%s]:%s %s\n' "$HOME_ADDR" "$HOME_PORT" "$(cut -d' ' -f1,2 "$DIR/riverboat-host-key.pub")" \
    > "$ETC/known_hosts"
chmod 0644 "$ETC/known_hosts"

# Before the fence exists: networkd must stop treating it as foreign before
# anything creates it, and a restart is how networkd.conf changes take effect.
install -m 0644 "$DIR/60-mesh-backchannel.sysctl.conf" /etc/sysctl.d/60-mesh-backchannel.conf
sysctl -q -p /etc/sysctl.d/60-mesh-backchannel.conf
install -d -m 0755 /etc/systemd/networkd.conf.d
install -m 0644 "$DIR/60-mesh-backchannel.networkd.conf" /etc/systemd/networkd.conf.d/60-mesh-backchannel.conf
install -m 0644 "$DIR/10-mesh-backchannel-wwan.network" /etc/systemd/network/10-mesh-backchannel-wwan.network
systemctl restart systemd-networkd
# If networkd configured the modem before it was told not to - dracut's
# catch-all runs DHCP on every link - take back what it left: its routes in
# main, its address, and the carrier's DNS servers in resolved. The link
# script below puts back only what belongs.
for lte in $(networkctl list --no-legend 2>/dev/null | awk '$3 == "wwan" {print $2}'); do
    ip -4 route flush table main dev "$lte" 2>/dev/null || true
    ip -4 addr flush dev "$lte" 2>/dev/null || true
    resolvectl revert "$lte" 2>/dev/null || true
done

install -m 0755 "$DIR/mesh-backchannel-link" "$BIN"
install -m 0644 "$DIR/mesh-backchannel-link.service" "$DIR/mesh-backchannel-link.timer" "$UNITS/"
sed -e "s/@HOME_ADDR@/$HOME_ADDR/g" -e "s/@HOME_PORT@/$HOME_PORT/g" -e "s/@LISTEN_PORT@/$LISTEN_PORT/g" \
    "$DIR/mesh-backchannel.service.in" > "$UNITS/mesh-backchannel.service"
systemctl daemon-reload
systemd-analyze verify "$UNITS/mesh-backchannel.service" "$UNITS/mesh-backchannel-link.service" \
    "$UNITS/mesh-backchannel-link.timer"

# Running the script once with no tunnel yet checks the fence and the link now
# rather than at the first timer tick.
"$BIN" || echo "link not up yet (see above); the fence is in place either way" >&2

echo
echo "Give this to riverboat as diamond-backchannel.pub, then: sudo $0 --start"
cat "$HOME_DIR/.ssh/id_ed25519.pub"
