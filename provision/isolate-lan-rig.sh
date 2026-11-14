#!/usr/bin/env bash
#
# Checks isolate-lan's plan and its blocking check against real iptables in a
# throwaway network namespace shaped like the friend's network on 2026-11-14:
# a /24 behind 192.168.1.1, an on-link unique-local IPv6 prefix, and a second
# one reached through another router on the LAN.
#
#   sudo provision/isolate-lan-rig.sh
#
# ufw itself is not used: it keeps its rules in files under /etc and would
# write this host's configuration. The rig builds the same chains ufw builds
# (ufw-user-output, ufw6-user-output) with the rules `ufw --dry-run` shows it
# writing, which is the part isolate-lan's check reads.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root (sudo)" >&2; exit 1; }
HERE="$(cd "$(dirname "$0")" && pwd)"
NS=mei-isolate
GW=mei-isolate-gw

cleanup() { ip netns del "$NS" 2>/dev/null || true; ip netns del "$GW" 2>/dev/null || true; }
trap cleanup EXIT
cleanup
ip netns add "$NS"; ip netns add "$GW"
ip -n "$NS" link set lo up
ip link add lan0 netns "$NS" type veth peer name gw0 netns "$GW"
ip -n "$GW" addr add 192.168.1.1/24 dev gw0; ip -n "$GW" link set gw0 up
ip -n "$NS" addr add 192.168.1.125/24 dev lan0
ip -n "$NS" addr add fd34:84ae:2552:4269::125/64 dev lan0 nodad
ip -n "$NS" link set lan0 up
ip -n "$NS" route add default via 192.168.1.1
sleep 1  # let lan0 get its link-local address
ip -n "$NS" -6 route add fd5c:2fa1:8f9d::/64 via fe80::1 dev lan0

ip netns exec "$NS" python3 -I - "$HERE/isolate-lan" <<'PY'
import importlib.machinery, importlib.util, subprocess, sys
loader = importlib.machinery.SourceFileLoader("isolate", sys.argv[1])
spec = importlib.util.spec_from_loader("isolate", loader)
iso = importlib.util.module_from_spec(spec); loader.exec_module(iso)

def sh(*cmd):
    subprocess.run(cmd, check=True)

p = iso.current_plan()
print("plan:", p["iface"], "via", p["gateway"])
print("  v4:", ", ".join(p["v4"]))
print("  v6:", ", ".join(p["v6"]))
assert p["iface"] == "lan0" and p["gateway"] == "192.168.1.1", p
assert "fd5c:2fa1:8f9d::/64" not in p["v6"], "covered by fc00::/7 already"
assert p["targets"]["fc00::/7"].startswith("fd34:84ae:2552:4269::"), p["targets"]

for tool, chain in (("iptables", "ufw-user-output"), ("ip6tables", "ufw6-user-output")):
    sh(tool, "-N", chain); sh(tool, "-A", "OUTPUT", "-j", chain)
sh("iptables", "-A", "ufw-user-output", "-d", p["gateway"], "-j", "ACCEPT")

failures = 0
def expect(label, got, want):
    global failures
    ok = got == want
    failures += not ok
    print(("PASS " if ok else "FAIL ") + label + ("" if ok else f": got {got}, wanted {want}"))

# Negative control first: with no rejections nothing can be caught, and the
# check must say so rather than pass.
missed, unroutable = iso.check_blocked(p)
expect("with no rules, every routable destination is reported missed",
       sorted(missed), sorted(n for n in p["v4"] + p["v6"] if n not in unroutable))

for net in p["v4"]:
    sh("iptables", "-A", "ufw-user-output", "-d", net, "-j", "REJECT")
for net in p["v6"]:
    sh("ip6tables", "-A", "ufw6-user-output", "-d", net, "-j", "REJECT")
missed, unroutable = iso.check_blocked(p)
expect("with the rules, nothing routable is missed", missed, [])
print("  no route at all, so nothing to block:", ", ".join(unroutable) or "none")

# A rule missing from the middle must be named.
sh("ip6tables", "-D", "ufw6-user-output", "-d", "fc00::/7", "-j", "REJECT")
missed, _ = iso.check_blocked(p)
expect("a missing unique-local rule is named", missed, ["fc00::/7"])
sys.exit(1 if failures else 0)
PY
