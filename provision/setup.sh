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
MESH_PORT=4001
DIST=""
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    cat <<'USAGE'
setup.sh --label <name> [options]

  --label <name>   Identifies this machine in every record. Required.
  --isolate-lan    Additionally block this box from reaching the volunteer's
                   other devices. Off by default: see the warning below.
  --mesh-port <n>  Port the mesh node listens on (default 4001). Opened
                   inbound so peers on the same network can reach it.
  --dist <dir>     Install prebuilt binaries from here instead of compiling.
                   This is the normal path for a volunteer machine, which
                   should never need a compiler. Produce one with ./release.sh.
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
        --mesh-port) MESH_PORT="${2:-}"; shift 2 ;;
        --dist) DIST="${2:-}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    esac
done

[[ $EUID -eq 0 ]] || { echo "must run as root (use sudo)" >&2; exit 1; }
[[ -n "$LABEL" ]] || { echo "--label is required" >&2; usage; exit 2; }
[[ "$LABEL" =~ ^[A-Za-z0-9_-]+$ ]] || { echo "label must be letters, digits, dashes or underscores" >&2; exit 2; }

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

# ---------------------------------------------------------------- the binary

say "Installing the probe"

# Prebuilt is the normal path. A volunteer machine should never need a compiler,
# and shipping the same binary everywhere means every box is demonstrably
# running the same thing rather than something each one built for itself.
if [[ -z "$DIST" && -d "$REPO_ROOT/dist" ]]; then
    DIST="$REPO_ROOT/dist"
    echo "using $DIST"
fi

if [[ -n "$DIST" ]]; then
    [[ -x "$DIST/mesh-probe" ]] || { echo "no mesh-probe in $DIST — run ./release.sh" >&2; exit 1; }

    # Checksums travel with the files precisely so this can be checked. A box
    # quietly running a truncated copy would produce measurements nobody could
    # explain.
    if [[ -f "$DIST/SHA256SUMS" ]]; then
        ( cd "$DIST" && sha256sum --quiet --check SHA256SUMS ) \
            || { echo "checksums do not match — the files were damaged in transit" >&2; exit 1; }
        echo "checksums verified"
    else
        echo "no SHA256SUMS present; installing unverified"
    fi

    install -m 0755 "$DIST/mesh-probe" /usr/local/bin/mesh-probe
    [[ -x "$DIST/mesh-node" ]] && install -m 0755 "$DIST/mesh-node" /usr/local/bin/mesh-node
    if [[ -d "$DIST/jobs" ]]; then
        install -d -m 0755 /usr/local/share/mesh-probe/jobs
        install -m 0644 "$DIST"/jobs/*.wasm /usr/local/share/mesh-probe/jobs/ 2>/dev/null || true
    fi
    [[ -f "$DIST/VERSION" ]] && install -m 0644 "$DIST/VERSION" /usr/local/share/mesh-probe/VERSION
else
    echo "no prebuilt binaries given; compiling here"
    command -v cargo >/dev/null || {
        echo "cargo not found. Either install Rust, or build elsewhere with" >&2
        echo "./release.sh and pass --dist <dir>." >&2
        exit 1
    }
    ( cd "$REPO_ROOT/probe" && cargo build --release )
    install -m 0755 "$REPO_ROOT/probe/target/release/mesh-probe" /usr/local/bin/mesh-probe
fi

install -d -m 0755 /usr/local/share/mesh-probe
[[ -f "$REPO_ROOT/probe/README.md" ]] && install -m 0644 "$REPO_ROOT/probe/README.md" /usr/local/share/mesh-probe/README.md

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

    # The mesh node needs inbound. The probe never did — it only ever dials
    # out — so the original default-deny was correct for it and silently wrong
    # the moment a node was added, which is exactly how this was found.
    #
    # This matters less than it looks for the real thing: peers behind address
    # translation reach each other by both dialling outward at once, and a
    # stateful firewall already permits the replies to a connection it opened.
    # It matters here because a direct dial on a local network is not that.
    ufw allow "$MESH_PORT/udp" >/dev/null
    ufw allow "$MESH_PORT/tcp" >/dev/null

    ufw --force enable >/dev/null
    echo "incoming denied except SSH and the mesh port ($MESH_PORT/tcp, $MESH_PORT/udp)"
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
  version       $(head -2 /usr/local/share/mesh-probe/VERSION 2>/dev/null | tr '\n' ' ' || echo "built here")
  results       /var/log/mesh-probe/results.jsonl
  mesh port     $MESH_PORT (tcp and udp) open inbound
  connectivity  every 30 minutes (plus up to 5 minutes of jitter)
  mapping test  every 25 minutes (plus up to 10 minutes of jitter), one idle
                interval per run, accumulating across days

Collect results from your own machine with:

  provision/collect.sh <this-host>

SUMMARY

tail -n 1 /var/log/mesh-probe/results.jsonl 2>/dev/null || echo "(no record yet; check: journalctl -u mesh-probe.service)"
