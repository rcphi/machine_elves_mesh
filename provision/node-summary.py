#!/usr/bin/env python3
"""What a mesh node did while nobody was watching.

    ./node-summary.py /var/log/mesh-node/events.jsonl

Reads the events a node emits and reports the things that only show up over
days: how often it lost its peers, whether its address moved, whether the
router kept its promise, and what happened to the work.
"""
import collections
import json
import sys


def load(path):
    events = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return events


def when(event):
    return event.get("ts_unix_ms", 0)


def span(events):
    if not events:
        return "no events"
    first, last = when(events[0]), when(events[-1])
    hours = (last - first) / 3_600_000
    return f"{hours:.1f} hours"


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2

    events = load(sys.argv[1])
    if not events:
        print("no events — the node may never have started, which is itself the finding")
        return 1

    kinds = collections.Counter(e.get("event") for e in events)
    node = next((e.get("node") for e in events if e.get("node")), "?")

    print(f"{node}: {len(events)} events over {span(events)}\n")

    # A restart is not a failure on its own — the service is meant to come back
    # — but a node restarting often is a different machine from one that has
    # been up throughout, and the log looks similar either way.
    starts = kinds.get("started", 0)
    print(f"  starts            {starts}" + ("" if starts <= 1 else "   RESTARTED — see below"))

    print(f"  peers joined      {kinds.get('joined', 0)}")
    print(f"  peers left        {kinds.get('left', 0)} announced, "
          f"{kinds.get('vanished', 0)} vanished without warning")

    # Reachability is the thing most likely to fail quietly. A mapping that
    # stops being renewed costs nothing visible until nobody can reach this
    # node, by which time the cause is hours in the past.
    mapped = kinds.get("port-mapped", 0)
    failed = kinds.get("port-map-failed", 0)
    if mapped or failed:
        print(f"  router mapping    {mapped} accepted, {failed} refused or unanswered")
        if failed:
            last = [e for e in events if e.get("event") == "port-map-failed"][-1]
            print(f"                    last failure: {last.get('error', '?')[:70]}")
    else:
        print("  router mapping    never asked (or --no-map-port)")

    # An address that changes is a cached address that has gone stale, and every
    # peer holding the old one is now dialling nothing.
    addresses = [e.get("addr") for e in events if e.get("event") == "my-address"]
    distinct = list(dict.fromkeys(addresses))
    if not addresses:
        print("  own address       never learned — no peer ever observed this node")
    elif len(distinct) == 1:
        print(f"  own address       {distinct[0]} — unchanged")
    else:
        print(f"  own address       CHANGED {len(distinct) - 1}x")
        for addr in distinct[-3:]:
            print(f"                      {addr}")

    took = kinds.get("took-over", 0)
    if took or kinds.get("job-loaded"):
        print(f"  work              {kinds.get('effect', 0)} effects, "
              f"{kinds.get('produced', 0)} productions, {took} takeovers")
        if kinds.get("job-failed"):
            print(f"                    {kinds['job-failed']} job failures")

    if starts > 1:
        print("\nRestarts")
        print("--------")
        gaps = []
        previous = None
        for event in events:
            if event.get("event") == "started":
                if previous is not None:
                    gaps.append((when(event) - previous) / 60000)
                previous = when(event)
        for i, minutes in enumerate(gaps, 2):
            print(f"  start {i} came {minutes:.0f} minutes after the previous one")
        if gaps and min(gaps) < 5:
            print("\n  Restarting within minutes means it is failing rather than being")
            print("  restarted. Check the events just before each 'started'.")

    unreached = kinds.get("dialling", 0)
    if unreached and not kinds.get("connected"):
        print("\n  This node dialled and never connected to anything. Either no peer was")
        print("  reachable, or the addresses it was given are stale — an address behind")
        print("  a carrier is only good until that connection cycles.")

    return 0


if __name__ == "__main__":
    sys.exit(main())
