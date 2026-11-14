#!/usr/bin/env python3
"""Summarise collected probe records.

What the probe records show about each machine's connection, per network it
has been on, and whether that is stable over time. It reports evidence; it
does not decide Phase 0's go/no-go, which needs peer evidence these records
cannot carry.

    ./summarise.py [results/all.jsonl]
"""
import collections
import json
import os
import sys

DEFAULT_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results", "all.jsonl")

# Addresses STUN reports as untranslated. A machine with one is a candidate to
# accept incoming connections, and so to carry relay duty; whether a firewall
# lets anything in is a separate question STUN cannot answer.
REACHABLE = {"public", "global"}


def load(path):
    records = []
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                print(f"  (skipped unparseable line {number} in {path})", file=sys.stderr)
    return records


# setup.sh binds the scheduled connectivity probe to this port. Records from
# before the probe wrote `local_port` are read as having used it.
DEFAULT_PROBE_PORT = 41999

# A mapping test is placed on a network by the nearest connectivity reading of
# the same machine; further away than this, nothing is assumed.
NEAREST_MS = 2 * 3600 * 1000

PRESERVING_NOTE = "preserves port numbers"


def subnet(ip):
    parts = (ip or "").split(".")
    return ".".join(parts[:3]) + ".0/24" if len(parts) == 4 else None


def site_of(record):
    """A machine on a particular network. diamond at home and diamond at a
    friend's house are different routers, and averaging them describes
    neither."""
    label = record.get("label", "unlabelled")
    lan = subnet(record.get("ipv4", {}).get("local"))
    return f"{label} on {lan}" if lan else label


def preserves_ports(connectivity_record):
    """Whether the router kept the probe's local port as the external port.
    None when the record cannot say."""
    v4 = connectivity_record.get("ipv4", {})
    local = v4.get("local_port") or DEFAULT_PROBE_PORT
    ports = {o["mapped"].rsplit(":", 1)[1] for o in v4.get("observers") or [] if o.get("mapped")}
    if not ports:
        return None
    return ports == {str(local)}


def site_preserves(connectivity):
    """{site: True/False} — whether the router preserves ports, by the
    majority of that site's readings.

    A property of the router, so it is decided over all of them. Single
    readings mislead: at home, diamond and riverboat both send from local port
    41999, so whenever both hold a live mapping one of them is bumped to a
    random port. Those clashes are not evidence that the router randomises."""
    votes = collections.defaultdict(collections.Counter)
    for record in connectivity:
        verdict = preserves_ports(record)
        if verdict is not None:
            votes[site_of(record)][verdict] += 1
    return {site: v[True] > v[False] for site, v in votes.items()}


def place(mapping, connectivity):
    """Each mapping record with its site, found from the nearest connectivity
    record of the same machine, and whether that site's router preserves
    ports (None when unknown)."""
    preserving = site_preserves(connectivity)
    timeline = collections.defaultdict(list)
    for record in connectivity:
        timeline[record.get("label")].append(record)
    placed = []
    for record in mapping:
        runs = timeline.get(record.get("label"), [])
        at = record.get("ts_unix", 0)
        nearest = min(runs, key=lambda r: abs(r.get("ts_unix", 0) - at), default=None)
        if nearest is None or abs(nearest.get("ts_unix", 0) - at) * 1000 > NEAREST_MS:
            placed.append((record, record.get("label", "unlabelled"), None))
        else:
            site = site_of(nearest)
            placed.append((record, site, preserving.get(site)))
    return placed


def evidence(record, preserving):
    """What one mapping record proves: survived, expired, unproven or
    inconclusive.

    --mapping-lifetime's second query is itself outbound, so on a router that
    preserves ports an expired mapping comes back on the same port and reads
    as survival. Its "survived" counts only where the router demonstrably does
    not preserve ports. Its "expired" always counts: a changed port is expiry
    whatever the policy. --mapping-inbound never speaks after the silence, so
    both its answers count."""
    outcome = record.get("outcome", "inconclusive")
    if PRESERVING_NOTE in (record.get("note") or ""):
        return "unproven"
    if record.get("test") == "mapping-inbound" or outcome != "survived":
        return outcome if outcome in ("survived", "expired") else "inconclusive"
    first = record.get("first") or ""
    local = record.get("local_port")
    if local:
        return "unproven" if first.endswith(f":{local}") else "survived"
    return "survived" if preserving is False else "unproven"


def tally(placed):
    """{site: {idle: Counter(evidence)}}"""
    by_site = collections.defaultdict(lambda: collections.defaultdict(collections.Counter))
    for record, site, preserving in placed:
        idle = record.get("idle_seconds")
        if isinstance(idle, int):
            by_site[site][idle][evidence(record, preserving)] += 1
    return by_site


def safe_idle(intervals):
    """The largest interval such that it and every shorter tested interval
    always survived, on confirmed evidence only; None if nothing confirmed.
    Also returns the first interval that ever expired."""
    safe, first_loss = None, None
    for idle in sorted(intervals):
        counts = intervals[idle]
        survived, expired = counts.get("survived", 0), counts.get("expired", 0)
        if not survived + expired:
            continue
        if expired:
            if first_loss is None:
                first_loss = idle
        elif first_loss is None:
            safe = idle
    return safe, first_loss


def keepalive(by_site):
    """(interval or None, sites without confirmed evidence).

    Set by the worst router, so every site must have confirmed evidence; a
    site that has none is not a site that is fine. Halved, because the
    measured value is where the mapping was still alive, not where it dies.
    No floor: a floor that rounds up erases the margin it exists to keep."""
    missing = [site for site, intervals in sorted(by_site.items()) if safe_idle(intervals)[0] is None]
    if missing or not by_site:
        return None, missing
    worst = min(safe_idle(intervals)[0] for intervals in by_site.values())
    return worst // 2, []


def report_mapping(mapping, connectivity, out=print):
    """How long each router remembers an idle connection."""
    out("Mapping lifetime")
    out("----------------")
    by_site = tally(place(mapping, connectivity))

    for site, intervals in sorted(by_site.items()):
        out(f"\n{site}")
        for idle in sorted(intervals):
            c = intervals[idle]
            trials = c.get("survived", 0) + c.get("expired", 0)
            detail = f"  {idle:>4}s   survived {c.get('survived', 0)}/{trials}"
            if c.get("unproven"):
                detail += f"   ({c['unproven']} unproven: router preserves ports)"
            if c.get("inconclusive"):
                detail += f"   ({c['inconclusive']} inconclusive)"
            if c.get("survived") and c.get("expired"):
                detail += "   VARIABLE — this router is not consistent"
            out(detail)
        safe, first_loss = safe_idle(intervals)
        if safe is None and first_loss is None:
            out("  → no confirmed evidence. Where the router preserves ports only")
            out("    --mapping-inbound can measure it.")
        elif first_loss is None:
            out(f"  → survived every confirmed interval up to {safe}s; "
                "the timeout is longer than anything measured")
        else:
            out(f"  → the router forgets somewhere between {safe or 0}s and {first_loss}s")

    out("")
    interval, missing = keepalive(by_site)
    if interval is None:
        out("  No mesh-wide keepalive interval: it is set by the worst router, and these")
        out(f"  sites have no confirmed evidence: {', '.join(missing) or 'none'}.")
    else:
        worst_site = min(by_site, key=lambda s: safe_idle(by_site[s])[0])
        out(f"  Shortest confirmed-safe idle: {safe_idle(by_site[worst_site])[0]}s ({worst_site})")
        out(f"  → keepalive interval for the mesh: {interval}s (half of it)")
    out(f"  This covers every site in these records: {', '.join(sorted(by_site))}.")
    out("")


def main():
    if len(sys.argv) > 2:
        print(__doc__)
        return 2

    path = sys.argv[1] if len(sys.argv) == 2 else DEFAULT_PATH

    # A missing or empty file is the ordinary state before anything has been
    # collected, not a crash. Say what to do about it.
    if not os.path.exists(path):
        print(f"No collected results at {path}")
        print()
        print("Collect from the boxes first. Without DNS on your network, name them")
        print("by address rather than hostname:")
        print()
        print("    ./collect.sh rpc@192.168.50.48")
        return 1

    records = load(path)
    if not records:
        print(f"{path} is empty — collect.sh reached the boxes but found no records.")
        print()
        print("On a box, check that measurements are actually being taken:")
        print()
        print("    systemctl list-timers 'mesh-probe*'")
        print("    tail /var/log/mesh-probe/results.jsonl")
        return 1

    connectivity = [r for r in records if r.get("test", "connectivity") == "connectivity"]
    mapping = [r for r in records if r.get("test") in ("mapping-lifetime", "mapping-inbound")]

    by_label = collections.defaultdict(list)
    for record in connectivity:
        by_label[site_of(record)].append(record)

    print(f"{len(records)} records from "
          f"{len(set(r.get('label') for r in records))} machines "
          f"({len(connectivity)} connectivity, {len(mapping)} mapping)\n")

    relay_candidates = []
    hole_punchable = []
    # A machine that speaks both families is the only route between members
    # that speak one and members that speak the other. Counting them is not
    # bookkeeping: a city with one is a city that splits the day that one
    # leaves, and nobody will have noticed beforehand.
    bridges = []
    bridge_readings = []

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

        # Whether a remembered address would still have worked. This is the
        # question a peer cache turns on, and it is answerable only by looking
        # across time — which is why the probe binds a fixed port, so two
        # readings are comparable at all.
        addresses = []
        for r in runs:
            seen = r.get("ipv4", {}).get("observers") or []
            if seen:
                addresses.append(seen[0]["mapped"])
        if not addresses:
            print("  address     never observed")
        else:
            # How often the usual address held, not how many times it changed.
            # Counting transitions made an address seen 1,759 times out of 1,780
            # read as "CHANGED 81x", which is true and thoroughly misleading:
            # what a peer cache cares about is the chance a remembered address
            # still works, and that is a proportion.
            counts = collections.Counter(addresses)
            usual, seen_count = counts.most_common(1)[0]
            share = 100.0 * seen_count / len(addresses)
            print(f"  address     {usual}")
            print(f"              seen in {seen_count}/{len(addresses)} readings ({share:.1f}%)")
            if len(counts) > 1:
                print(f"              {len(counts) - 1} other address(es) seen; a remembered one")
                print(f"              would have been wrong about {100 - share:.1f}% of the time")

        # Averaged over every reading rather than taken from the latest,
        # because what matters is how often a bridge was *present*, not whether
        # one happened to be there when someone last looked.
        both = sum(
            1 for r in runs
            if r.get("ipv4", {}).get("tag") not in (None, "unreachable")
            and r.get("ipv6", {}).get("tag") == "global"
        )
        if both:
            share = 100.0 * both / len(runs)
            print(f"  bridging    yes, in {both}/{len(runs)} readings ({share:.0f}%)")
            bridges.append(label)
        else:
            print("  bridging    no — speaks only one address family")
        bridge_readings.append(both / len(runs) if runs else 0.0)

        if latest.get("relay_candidate", latest.get("relay_capable")):
            relay_candidates.append(label)
        if v4 in REACHABLE or v6 in REACHABLE or v4 == "nat-endpoint-independent":
            hole_punchable.append(label)
        print()

    if mapping:
        report_mapping(mapping, connectivity)

    print("Verdict")
    print("-------")
    print(f"  machines reporting:                  {len(by_label)}")
    expected = sum(bridge_readings)
    print(f"  can bridge the address families:     "
          f"{', '.join(bridges) or 'NONE'}  ({expected:.1f} present on average)")
    print(f"  relay candidates (STUN only):        {', '.join(relay_candidates) or 'NONE'}")
    print(f"  direct connection plausible:         {', '.join(hole_punchable) or 'NONE'}")
    print()

    # Said before anything else, because it is the only failure here that is
    # not a degradation. Everything else makes the mesh worse; this one cuts it
    # in half, with each half believing the other simply left.
    if not bridges and len(by_label) > 1:
        print("  NO MACHINE SPEAKS BOTH ADDRESS FAMILIES. If every machine here uses the")
        print("  same one that is merely a fact; if they differ, these machines cannot")
        print("  reach each other at all and no amount of relaying helps, because a relay")
        print("  must itself speak both.")
        print()
    elif len(bridges) == 1:
        print(f"  ONLY {bridges[0]} CAN BRIDGE. It is a single point of failure nobody would")
        print("  notice until it left, and its leaving would not slow the mesh down — it")
        print("  would split it into two halves unable to see one another.")
        print()

    # STUN shows how a machine's address is translated, not whether anything
    # can reach it: a firewall can drop every inbound packet to a "global"
    # address, and a router that opens a port on request turns a NAT'd machine
    # into a reachable one. So nothing here can decide the stop criterion.
    print("  A relay candidate has an address STUN calls public or global. Whether it")
    print("  actually accepts inbound connections is shown only by peer evidence: a mesh")
    print("  node's incoming connections or port-mapped events, or a --mapping-inbound")
    print("  survival. These records cannot meet or clear a stop criterion by themselves;")
    print("  see docs/phase-0-plan.md for the evidence that has.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
