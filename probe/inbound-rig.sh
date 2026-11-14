#!/usr/bin/env bash
#
# Runs --mapping-inbound against real Linux NAT in network namespaces, where
# the timeouts are known, so the test is checked against an answer rather than
# against itself. Loopback cannot expire anything; this can.
#
#   sudo probe/inbound-rig.sh [path/to/mesh-probe]
#
#   client ── cnat ──── inet ──── rnat ── resp
#   10.1.0.2   friend's  one hop   riverboat's
#              router:   between   router: forwards
#              preserves           udp/3479, and
#              ports               RANDOMISES ports
#
# cnat forgets an idle flow after CNAT_TIMEOUT, rnat after RNAT_TIMEOUT, which
# is shorter on purpose: it is the case the responder's TTL-limited keepalives
# exist for. inet is the hop a keepalive with TTL 2 must die at; without it the
# packet would die *inside cnat*, after cnat's conntrack had already seen it,
# and refresh the very mapping being timed.
#
# Everything it creates is removed on exit.
set -euo pipefail

PROBE="${1:-$(dirname "$0")/target/release/mesh-probe}"
PROBE="$(readlink -f "$PROBE")"
[[ -x "$PROBE" ]] || { echo "no probe binary at $PROBE — cargo build --release first" >&2; exit 1; }
[[ $EUID -eq 0 ]] || { echo "must run as root (sudo)" >&2; exit 1; }

CNAT_TIMEOUT=20
RNAT_TIMEOUT=12
TOKEN=rig-token
NS=(mei-client mei-cnat mei-inet mei-rnat mei-resp)

cleanup() {
    for ns in "${NS[@]}"; do ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null || true; done
    for ns in "${NS[@]}"; do ip netns del "$ns" 2>/dev/null || true; done
}
trap cleanup EXIT
cleanup

for ns in "${NS[@]}"; do ip netns add "$ns"; ip -n "$ns" link set lo up; done

link() {  # link <ns-a> <if-a> <addr-a> <ns-b> <if-b> <addr-b>
    ip link add "$2" netns "$1" type veth peer name "$5" netns "$4"
    ip -n "$1" addr add "$3" dev "$2"; ip -n "$1" link set "$2" up
    ip -n "$4" addr add "$6" dev "$5"; ip -n "$4" link set "$5" up
}
link mei-client c0 10.1.0.2/24       mei-cnat   ci 10.1.0.1/24
link mei-cnat   co 203.0.113.1/24    mei-inet   ia 203.0.113.254/24
link mei-inet   ib 198.51.100.254/24 mei-rnat   ro 198.51.100.1/24
link mei-rnat   ri 10.2.0.1/24       mei-resp   r0 10.2.0.2/24

ip -n mei-client route add default via 10.1.0.1
ip -n mei-cnat   route add default via 203.0.113.254
ip -n mei-rnat   route add default via 198.51.100.254
ip -n mei-resp   route add default via 10.2.0.1
for ns in mei-cnat mei-inet mei-rnat; do ip netns exec "$ns" sysctl -qw net.ipv4.ip_forward=1; done

# The friend's router: plain masquerade, which keeps the source port when it
# can, exactly the behaviour that blinds --mapping-lifetime.
ip netns exec mei-cnat nft -f - <<'NFT'
table ip nat {
    chain post { type nat hook postrouting priority 100; oif "co" masquerade; }
}
NFT
# riverboat's router: forwards udp/3479 in, and randomises ports going out.
ip netns exec mei-rnat nft -f - <<'NFT'
table ip nat {
    chain pre  { type nat hook prerouting priority -100; iif "ro" udp dport 3479 dnat to 10.2.0.2:3479; }
    chain post { type nat hook postrouting priority 100; oif "ro" masquerade random,fully-random; }
}
NFT
# conntrack loads with the first NAT rule; the timeouts are per namespace.
for pair in "mei-cnat $CNAT_TIMEOUT" "mei-rnat $RNAT_TIMEOUT"; do
    set -- $pair
    ip netns exec "$1" sysctl -qw net.netfilter.nf_conntrack_udp_timeout="$2"
    ip netns exec "$1" sysctl -qw net.netfilter.nf_conntrack_udp_timeout_stream="$2"
done

responder() {  # responder <keep-ttl>
    ip netns pids mei-resp | xargs -r kill 2>/dev/null || true
    sleep 0.3
    ip netns exec mei-resp env MESH_PROBE_INBOUND_TOKEN="$TOKEN" \
        "$PROBE" --inbound-responder --port 3479 --keep-ttl "$1" >/dev/null 2>&1 &
    sleep 0.5
}

pass=0; fail=0
case_() {  # case_ <name> <keep-ttl> <idle> <expected outcome> [expected note fragment]
    local name="$1" ttl="$2" idle="$3" want="$4" note="${5:-}"
    responder "$ttl"
    # Each case starts with both routers' tables empty, so no case inherits
    # a mapping, or its absence, from the one before.
    ip netns exec mei-cnat conntrack -F >/dev/null 2>&1 || true
    ip netns exec mei-rnat conntrack -F >/dev/null 2>&1 || true
    local record
    record="$(ip netns exec mei-client env MESH_PROBE_INBOUND_TOKEN="$TOKEN" \
        "$PROBE" --mapping-inbound --responder 198.51.100.1:3479 --idle "$idle" --json --label rig)"
    local got; got="$(sed -n 's/.*"outcome":"\([a-z]*\)".*/\1/p' <<<"$record")"
    if [[ "$got" == "$want" && ( -z "$note" || "$record" == *"$note"* ) ]]; then
        pass=$((pass + 1)); printf 'PASS  %-58s %s\n' "$name" "$got"
    else
        fail=$((fail + 1)); printf 'FAIL  %-58s got %s, wanted %s\n      %s\n' "$name" "$got" "$want" "$record"
    fi
}

echo "cnat forgets after ${CNAT_TIMEOUT}s, rnat after ${RNAT_TIMEOUT}s"
case_ "silence shorter than both timeouts"                      2 8  survived
case_ "silence past cnat's timeout is expiry"                   2 30 expired
case_ "past rnat's timeout but not cnat's, with keepalives"     2 16 survived
case_ "same, keepalives off: rnat's new port is filtered out"   0 16 expired
case_ "a keepalive with too high a TTL is caught, not counted"  5 16 inconclusive "keepalive"

echo
echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
