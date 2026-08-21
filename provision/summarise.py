#!/usr/bin/env python3
"""Summarise collected probe records.

Answers the question Phase 0 actually asks: can these machines reach each
other without renting a server, and is the answer stable over time?

    ./summarise.py results/all.jsonl
"""
import collections
import json
import sys

# Whether a machine could accept an incoming connection, which is what
# decides whether it can carry relay duty for peers that cannot.
REACHABLE = {"public", "global"}


def load(path):
    records = []
    with open(path) as handle:
        for number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                print(f"  (skipped unparseable line {number} in {path})", file=sys.stderr)
    return records


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2

    records = load(sys.argv[1])
    if not records:
        print("no records")
        return 1

    by_label = collections.defaultdict(list)
    for record in records:
        by_label[record.get("label", "unlabelled")].append(record)

    print(f"{len(records)} records from {len(by_label)} machines\n")

    relay_capable = []
    hole_punchable = []

    for label, runs in sorted(by_label.items()):
        runs.sort(key=lambda r: r.get("ts_unix", 0))
        latest = runs[-1]
        v4 = latest.get("ipv4", {}).get("tag", "?")
        v6 = latest.get("ipv6", {}).get("tag", "?")

        # Instability matters as much as the current value: a connection whose
        # behaviour changes between runs cannot be planned around.
        v4_seen = collections.Counter(r.get("ipv4", {}).get("tag", "?") for r in runs)
        v6_seen = collections.Counter(r.get("ipv6", {}).get("tag", "?") for r in runs)

        print(f"{label}")
        print(f"  runs        {len(runs)}  ({runs[0].get('ts_utc')} → {latest.get('ts_utc')})")
        print(f"  ipv4        {v4}" + ("" if len(v4_seen) == 1 else f"   UNSTABLE: {dict(v4_seen)}"))
        print(f"  ipv6        {v6}" + ("" if len(v6_seen) == 1 else f"   UNSTABLE: {dict(v6_seen)}"))

        # A run where nothing answered is a gap in coverage, not a zero.
        silent = sum(1 for r in runs if not r.get("ipv4", {}).get("observers")
                     and not r.get("ipv6", {}).get("observers"))
        if silent:
            print(f"  silent runs {silent} of {len(runs)}  (no STUN server answered)")

        if latest.get("relay_capable"):
            relay_capable.append(label)
        if v4 in REACHABLE or v6 in REACHABLE or v4 == "nat-endpoint-independent":
            hole_punchable.append(label)
        print()

    print("Verdict")
    print("-------")
    print(f"  can accept incoming (relay-capable): {', '.join(relay_capable) or 'NONE'}")
    print(f"  direct connection plausible:         {', '.join(hole_punchable) or 'NONE'}")
    print()

    if not relay_capable:
        print("  No machine can accept an incoming connection. With no rented")
        print("  infrastructure there is nowhere for a relay to live, so peers that")
        print("  cannot hole-punch to each other have no path at all.")
        print()
        print("  Per docs/phase-0-plan.md this meets the stop criterion. It is a")
        print("  finding, not a failure — and it arrived for the cost of an afternoon.")
    elif len(hole_punchable) < len(by_label):
        print("  Some machines will need relaying through a peer. That is the design's")
        print("  intent (§11.6's fifth contribution lever), so proceed — and measure")
        print("  what relay duty actually costs the machine carrying it.")
    else:
        print("  Every machine can plausibly connect directly. Proceed to the mesh node,")
        print("  and do not let this result make anyone complacent: three connections")
        print("  establish that a problem class is absent here, not that it is rare.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
