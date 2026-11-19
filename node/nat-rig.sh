#!/usr/bin/env bash
#
# Run F's three connections in network namespaces, so relaying and punching
# are proven against real Linux NAT before any box is touched.
#
#   sudo node/nat-rig.sh preserving|randomised|upnp
#
#   mer-a 10.1.0.2 ── mer-nata (203.0.113.1) ──┐
#   mer-b 10.2.0.2 ── mer-natb (198.51.100.1) ─┼── mer-inet ── mer-pub 192.0.2.10
#                                               (the reachable member, --own)
#
# a and b are told only pub's address: they find each other through it, as
# diamond and yoinkerz did in Run F. Both NATs admit only replies, as both of
# Run F's did.
#
#   preserving  both NATs keep the port. The punch should work, and killing pub
#               must not split a from b.
#   randomised  natb picks a new random port per destination, which no punch
#               can predict. The punch must fail cheaply, killing pub splits
#               the mesh as in Run F, and pub's return heals it.
#   upnp        as randomised, but nata runs fake-igd.py: a gets a port by UPnP,
#               becomes reachable, b reaches it directly, and killing pub no
#               longer splits anything.
#
# Prints PASS or FAIL per check and exits non-zero if any failed. Logs stay in
# the directory it prints. Everything else it creates is removed on exit.
set -euo pipefail
SCENARIO="${1:-}"
[[ "$SCENARIO" =~ ^(preserving|randomised|upnp)$ ]] || { echo "usage: sudo $0 preserving|randomised|upnp" >&2; exit 2; }
[[ $EUID -eq 0 ]] || { echo "must run as root (sudo)" >&2; exit 1; }
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$DIR/target/release/mesh-node"
JOB="$DIR/../jobs/factory/target/wasm32-unknown-unknown/release/factory_job.wasm"
[[ -x "$BIN" ]] || { echo "no $BIN — cargo build --release first" >&2; exit 1; }
[[ -f "$JOB" ]] || { echo "no $JOB — jobs/build.sh first" >&2; exit 1; }
OUT="$(mktemp -d /tmp/nat-rig-$SCENARIO.XXXXXX)"
chmod 755 "$OUT"
NS=(mer-inet mer-pub mer-nata mer-a mer-natb mer-b)
declare -A PID=()

cleanup() {
    for pid in "${PID[@]}"; do kill -9 "$pid" 2>/dev/null || true; done
    for ns in "${NS[@]}"; do ip netns pids "$ns" 2>/dev/null | xargs -r kill -9 2>/dev/null || true; done
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
link mer-pub  p0 192.0.2.10/24    mer-inet ip 192.0.2.254/24
link mer-nata ao 203.0.113.1/24   mer-inet ia 203.0.113.254/24
link mer-natb bo 198.51.100.1/24  mer-inet ib 198.51.100.254/24
link mer-a    a0 10.1.0.2/24      mer-nata ai 10.1.0.1/24
link mer-b    b0 10.2.0.2/24      mer-natb bi 10.2.0.1/24
ip -n mer-pub  route add default via 192.0.2.254
ip -n mer-nata route add default via 203.0.113.254
ip -n mer-natb route add default via 198.51.100.254
ip -n mer-a    route add default via 10.1.0.1
ip -n mer-b    route add default via 10.2.0.1
for ns in mer-inet mer-nata mer-natb; do ip netns exec "$ns" sysctl -qw net.ipv4.ip_forward=1; done
# Internet-like delay on each NAT's outside link, 10 ms each way per hop: about
# 40 ms between a and b, as between Run F's boxes. Without it the rig is
# unfair to punching in a way no real path is. At zero latency one side's
# first punch packet always reaches the other NAT before that side has sent,
# the NAT records it, and its own machine's packet then needs that port and
# is given another. DCUtR times both sides' sends by the round trip so that
# they cross in flight; with no flight to cross in, every punch fails.
LATENCY=10ms
for pair in "mer-nata ao" "mer-inet ia" "mer-natb bo" "mer-inet ib"; do
    set -- $pair
    ip netns exec "$1" tc qdisc add dev "$2" root netem delay "$LATENCY"
done

ip netns exec mer-nata nft -f - <<'NFT'
table ip nat {
    chain pre  { type nat hook prerouting priority -100; }
    chain post { type nat hook postrouting priority 100; oif "ao" masquerade; }
}
NFT
if [[ $SCENARIO == preserving ]]; then B_NAT="masquerade"; else B_NAT="masquerade random,fully-random"; fi
ip netns exec mer-natb nft -f - <<NFT
table ip nat {
    chain post { type nat hook postrouting priority 100; oif "bo" $B_NAT; }
}
NFT
# Both NATs keep a UDP flow 120 s, the friend's router's measured figure
# (Linux's own default for a flow with no reply is 30 s). Option 2's rejoin
# depends on it: a member that dials pub once a minute holds its router open
# toward pub, so pub, back, can dial straight in.
for ns in mer-nata mer-natb; do
    ip netns exec "$ns" sysctl -qw net.netfilter.nf_conntrack_udp_timeout=120
    ip netns exec "$ns" sysctl -qw net.netfilter.nf_conntrack_udp_timeout_stream=120
done
# What each side spends dialling the other's public address, as Run F's
# captures measured it.
ip netns exec mer-a nft -f - <<'NFT'
table inet cost { chain out { type filter hook output priority 0; ip daddr 198.51.100.1 ip protocol udp counter; }; }
NFT
ip netns exec mer-b nft -f - <<'NFT'
table inet cost { chain out { type filter hook output priority 0; ip daddr 203.0.113.1 ip protocol udp counter; }; }
NFT

if [[ $SCENARIO == upnp ]]; then
    ip netns exec mer-nata python3 "$DIR/fake-igd.py" --inside 10.1.0.1 --external 203.0.113.1 --wan-if ao \
        > "$OUT/fake-igd.log" 2>&1 &
    PID[igd]=$!
    sleep 1
fi

start() {  # start <name> <namespace> [extra flags...]
    local name="$1" ns="$2"; shift 2
    # MESH_NODE_TRACE, if set, passes libp2p's own diagnostics through to stderr.
    ip netns exec "$ns" env ${MESH_NODE_TRACE:+MESH_NODE_TRACE="$MESH_NODE_TRACE"} \
        "$BIN" --label "$name" --mesh "nat-rig-$$" --json --port 4001 --no-mdns \
        --identity "$OUT/$name.identity" --ledger "$OUT/$name.ledger" --peer-book "$OUT/$name.peers" \
        --job "$JOB" "$@" >> "$OUT/$name.events.jsonl" 2>> "$OUT/$name.trace" &
    PID[$name]=$!
}
now_ms() { echo $(( $(date +%s%N) / 1000000 )); }

# count <node> <event> <since_ms> [field=value ...]: how many such events.
count() {
    python3 - "$OUT/$1.events.jsonl" "$2" "$3" "${@:4}" <<'PY'
import json, sys
path, kind, since = sys.argv[1], sys.argv[2], int(sys.argv[3])
want = dict(a.split("=", 1) for a in sys.argv[4:])
n = 0
for line in open(path, errors="replace"):
    try: e = json.loads(line)
    except ValueError: continue
    if e.get("event") == kind and e.get("ts_unix_ms", 0) >= since and all(e.get(k) == v for k, v in want.items()):
        n += 1
print(n)
PY
}
peer_id() { count "$1" started 0 >/dev/null; python3 -c "
import json,sys
for l in open(sys.argv[1]):
    try: e=json.loads(l)
    except ValueError: continue
    if e.get('event')=='started': print(e['peer_id']); break" "$OUT/$1.events.jsonl"; }
# wait_for <seconds> <description> <shell condition>: polls once a second.
wait_for() {
    local limit="$1"; shift
    for _ in $(seq "$limit"); do eval "$1" && return 0; sleep 1; done
    return 1
}
fails=0
check() {  # check <description> <shell condition>
    if eval "$2"; then printf 'PASS  %s\n' "$1"; else printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); fi
}
cost_bytes() { ip netns exec "$1" nft list table inet cost | sed -n 's/.*bytes \([0-9]*\).*/\1/p'; }

echo "scenario $SCENARIO; logs in $OUT"
PUB_ADDR=/ip4/192.0.2.10/udp/4001/quic-v1
start pub mer-pub --own --no-map-port
sleep 8
T0=$(now_ms)
start a mer-a --peer "$PUB_ADDR"
start b mer-b --peer "$PUB_ADDR"
wait_for 30 '[[ -s "$OUT/a.events.jsonl" && -s "$OUT/b.events.jsonl" ]]' || true
sleep 2
A=$(peer_id a); B=$(peer_id b)

direct_ab() { [[ $(count a connected "$T0" peer_id="$B" relayed=false) -gt 0 || $(count b connected "$T0" peer_id="$A" relayed=false) -gt 0 ]]; }

case $SCENARIO in
preserving)
    wait_for 240 'direct_ab' || true
    # How the link formed is reported, not required: with port-preserving
    # NATs and real delay, both sides dialling the moment pub announces them
    # already crosses in flight, so a coordinated punch may never be needed.
    echo "      direct link by: $(( $(count a punch-succeeded "$T0") + $(count b punch-succeeded "$T0") )) coordinated punch(es); otherwise the simultaneous dial"
    check "a and b have a direct link (connected relayed=false)" 'direct_ab'
    ;;
randomised|upnp)
    if [[ $SCENARIO == randomised ]]; then
        wait_for 240 '[[ $(count a introducer-held "$T0") -gt 0 && $(count b introducer-held "$T0") -gt 0 && $(( $(count a punch-failed "$T0") + $(count b punch-failed "$T0") )) -gt 0 ]]' || true
        check "a and b both hold an introduction on pub" \
            '[[ $(count a introducer-held "$T0" label=pub) -gt 0 && $(count b introducer-held "$T0" label=pub) -gt 0 ]]'
        check "at least one punch failed" '[[ $(( $(count a punch-failed "$T0") + $(count b punch-failed "$T0") )) -gt 0 ]]'
        check "no direct link between a and b" '! direct_ab'
        # The steady dial cost of an address that never answers: the second
        # six minutes, once the backoff has climbed (2, 4, 8 ... 300 s) and
        # the first punches (60, 120, 240 s) are past. Under 1 MB/h is under
        # 100 KB in six minutes. Every port counts: behind natb, b's public
        # port is not 4001.
        while (( $(now_ms) - T0 < 360000 )); do sleep 5; done
        early_b=$(cost_bytes mer-b); early_a=$(cost_bytes mer-a)
        while (( $(now_ms) - T0 < 720000 )); do sleep 5; done
        spent_b=$(( $(cost_bytes mer-b) - early_b )); spent_a=$(( $(cost_bytes mer-a) - early_a ))
        echo "      dialling the other's public address: first 6 min b $early_b, a $early_a bytes; next 6 min b $spent_b, a $spent_a bytes"
        check "b's dialling of a costs under 1 MB/h" '(( spent_b * 10 < 1000000 ))'
        check "a's dialling of b costs under 1 MB/h" '(( spent_a * 10 < 1000000 ))'
    else
        wait_for 150 '[[ $(count a reachable "$T0") -gt 0 ]] && direct_ab' || true
        check "a got a port by UPnP (port-mapped method=upnp)" '[[ $(count a port-mapped "$T0" method=upnp) -gt 0 ]]'
        check "a is confirmed reachable" '[[ $(count a reachable "$T0") -gt 0 ]]'
        check "b has a direct link to a" 'direct_ab'
    fi
    ;;
esac

# The reachable member goes, as riverboat did in Run F's part 2.
kill -9 "${PID[pub]}"; unset 'PID[pub]'
T_KILL=$(now_ms)
sleep 45
took=$(( $(count a took-over "$T_KILL") + $(count b took-over "$T_KILL") ))
lost=$(( $(count a vanished "$T_KILL" label=b) + $(count b vanished "$T_KILL" label=a) ))
echo "      after pub went: $took takeover(s), a and b lost each other $lost time(s)"
if [[ $SCENARIO == randomised ]]; then
    check "losing pub splits a from b, as in Run F (both take over)" '(( took == 2 ))'
else
    check "losing pub does not split a from b" '(( lost == 0 ))'
    check "exactly one of a and b took the job over" '(( took == 1 ))'
fi

# And comes back. In randomised, after longer than a and b's two minutes of
# quick redials, as riverboat's 55 minutes in Run F part 2: they are down to
# one dial a minute, and pub must dial where it last saw them (option 2) to
# rejoin in seconds rather than at their next try.
if [[ $SCENARIO == randomised ]]; then
    while (( $(now_ms) - T_KILL < 150000 )); do sleep 2; done
    REJOIN=15
else
    REJOIN=75
fi
start pub mer-pub --own --no-map-port
T_BACK=$(now_ms)
wait_for "$REJOIN" '[[ $(count pub joined "$T_BACK") -gt 0 ]]' || true
echo "      pub rejoined after $(( ($(now_ms) - T_BACK) / 1000 )) s"
check "pub rejoins within $REJOIN s" '[[ $(count pub joined "$T_BACK") -gt 0 ]]'
if [[ $SCENARIO == randomised ]]; then
    wait_for 30 '[[ $(( $(count a yielded "$T_BACK") + $(count b yielded "$T_BACK") )) -gt 0 ]]' || true
    check "the split heals: one of a and b yields" '[[ $(( $(count a yielded "$T_BACK") + $(count b yielded "$T_BACK") )) -gt 0 ]]'
fi

echo
if (( fails == 0 )); then echo "nat-rig $SCENARIO: all checks passed"; else echo "nat-rig $SCENARIO: $fails check(s) FAILED"; fi
exit $(( fails > 0 ))
