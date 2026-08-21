#!/usr/bin/env bash
#
# Prepares one Ubuntu Server box to measure its connection unattended.
#
# Run by the operator on a fresh machine before it is shipped, not by the
# volunteer. Idempotent: safe to re-run after changing the label or updating
# the binary.
#
#   sudo ./setup.sh --label vol-1
#
set -euo pipefail

LABEL=""
ISOLATE_LAN=0
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    cat <<'USAGE'
setup.sh --label <name> [options]

  --label <name>   Identifies this machine in every record. Required.
  --isolate-lan    Additionally block this box from reaching the volunteer's
                   other devices. Off by default: see the warning below.
  --help           Show this.

--isolate-lan is a courtesy to the household, but it can strand the box if
this network is unusual. It is therefore opt-in, it verifies connectivity
after applying the rules, and it rolls itself back automatically if the box
loses the internet. Prefer running it while the box is still on your bench.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --label) LABEL="${2:-}"; shift 2 ;;
        --isolate-lan) ISOLATE_LAN=1; shift ;;
        --help|-h) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    esac
done

[[ $EUID -eq 0 ]] || { echo "must run as root (use sudo)" >&2; exit 1; }
[[ -n "$LABEL" ]] || { echo "--label is required" >&2; usage; exit 2; }
[[ "$LABEL" =~ ^[A-Za-z0-9_-]+$ ]] || { echo "label must be letters, digits, dashes or underscores" >&2; exit 2; }

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

# ---------------------------------------------------------------- the binary

say "Building the probe"
if [[ -x "$REPO_ROOT/probe/target/release/mesh-probe" ]]; then
    echo "using existing release build"
else
    command -v cargo >/dev/null || { echo "cargo not found; install Rust first" >&2; exit 1; }
    ( cd "$REPO_ROOT/probe" && cargo build --release )
fi
install -m 0755 "$REPO_ROOT/probe/target/release/mesh-probe" /usr/local/bin/mesh-probe
install -d -m 0755 /usr/local/share/mesh-probe
install -m 0644 "$REPO_ROOT/probe/README.md" /usr/local/share/mesh-probe/README.md

# ---------------------------------------------------------------- the service

say "Creating the service account"
if ! id -u meshprobe >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin meshprobe
    echo "created user meshprobe"
else
    echo "user meshprobe already exists"
fi

install -d -m 0755 -o meshprobe -g meshprobe /var/log/mesh-probe
install -d -m 0755 /etc/mesh-probe
printf 'LABEL=%s\n' "$LABEL" > /etc/mesh-probe/config
chmod 0644 /etc/mesh-probe/config

say "Installing the timer"
for unit in mesh-probe.service mesh-probe.timer \
            mesh-probe-mapping.service mesh-probe-mapping.timer; do
    install -m 0644 "$REPO_ROOT/provision/$unit" "/etc/systemd/system/$unit"
done
install -m 0644 "$REPO_ROOT/provision/logrotate.mesh-probe" /etc/logrotate.d/mesh-probe
systemctl daemon-reload
systemctl enable --now mesh-probe.timer
systemctl enable --now mesh-probe-mapping.timer

# ------------------------------------------------------- keep the box awake

say "Preventing the box from going quiet on its own"

# A machine that suspends stops measuring, and a suspend that happens at 3am
# looks exactly like a network failure in the results.
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target >/dev/null 2>&1 || true

# Security updates still apply; the reboot does not happen on its own, because
# an unannounced reboot mid-measurement is indistinguishable from a node
# genuinely disappearing, which is the thing being measured.
cat > /etc/apt/apt.conf.d/52-mesh-probe <<'APT'
Unattended-Upgrade::Automatic-Reboot "false";
APT

# Logs must survive a reboot or a power cut proves nothing.
install -d -m 2755 /var/log/journal
systemd-tmpfiles --create --prefix /var/log/journal >/dev/null 2>&1 || true

# ---------------------------------------------------------------- firewalling

say "Firewall"
if command -v ufw >/dev/null; then
    ufw --force default deny incoming >/dev/null
    ufw --force default allow outgoing >/dev/null
    ufw allow OpenSSH >/dev/null 2>&1 || ufw allow 22/tcp >/dev/null
    ufw --force enable >/dev/null
    echo "incoming denied except SSH; outgoing allowed"
else
    echo "ufw not installed; skipping"
fi

if [[ $ISOLATE_LAN -eq 1 ]]; then
    say "Isolating the box from the household network"
    GATEWAY="$(ip route show default 2>/dev/null | awk '/default/ {print $3; exit}')"
    if [[ -z "$GATEWAY" ]]; then
        echo "could not determine the default gateway; refusing to apply LAN rules" >&2
    else
        echo "gateway is $GATEWAY"

        # Public resolvers first: blocking the LAN removes the router's DNS,
        # and a box that cannot resolve names cannot probe anything.
        install -d -m 0755 /etc/systemd/resolved.conf.d
        cat > /etc/systemd/resolved.conf.d/mesh-probe.conf <<'DNS'
[Resolve]
DNS=1.1.1.1 9.9.9.9
FallbackDNS=8.8.8.8
DNS
        systemctl restart systemd-resolved || true

        for net in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16; do
            ufw --force delete deny out to "$net" >/dev/null 2>&1 || true
            ufw deny out to "$net" >/dev/null
        done
        # The gateway itself must stay reachable, and must outrank the denials.
        ufw --force delete allow out to "$GATEWAY" >/dev/null 2>&1 || true
        ufw insert 1 allow out to "$GATEWAY" >/dev/null
        ufw reload >/dev/null

        echo "verifying the box can still reach the internet…"
        sleep 2
        if /usr/local/bin/mesh-probe --json --label "$LABEL" 2>/dev/null | grep -q '"observers":\[{'; then
            echo "verified — LAN isolation is active and the internet still works"
        else
            echo "FAILED to reach any STUN server after isolating; rolling back" >&2
            for net in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16; do
                ufw --force delete deny out to "$net" >/dev/null 2>&1 || true
            done
            rm -f /etc/systemd/resolved.conf.d/mesh-probe.conf
            systemctl restart systemd-resolved || true
            ufw reload >/dev/null
            echo "rolled back. The box is reachable again; LAN isolation is off." >&2
        fi
    fi
fi

# ---------------------------------------------------------------- first run

say "Taking one measurement now"
systemctl start mesh-probe.service || true
sleep 1

cat <<SUMMARY

Done.

  label         $LABEL
  binary        /usr/local/bin/mesh-probe
  results       /var/log/mesh-probe/results.jsonl
  connectivity  every 30 minutes (plus up to 5 minutes of jitter)
  mapping test  every 25 minutes (plus up to 10 minutes of jitter), one idle
                interval per run, accumulating across days

Collect results from your own machine with:

  provision/collect.sh <this-host>

SUMMARY

tail -n 1 /var/log/mesh-probe/results.jsonl 2>/dev/null || echo "(no record yet; check: journalctl -u mesh-probe.service)"
