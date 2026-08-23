#!/usr/bin/env python3
"""What a mesh node did, most recently by default.

    ./node-summary.py                  the current run only
    ./node-summary.py --since 20m      the last twenty minutes
    ./node-summary.py --all            everything in the log

The log outlives every restart, so reading all of it mixes together runs that
had nothing to do with each other. **The current run is the default**, because
that is nearly always the question being asked, and a summary that silently
includes yesterday is worse than one that says it is partial.
"""
import collections
import json
import os
import sys
import time

DEFAULT_LOG = "/var/log/mesh-node/events.jsonl"

# Moments worth a line of their own: things that happened *to* the node, rather
# than the steady hum of it working.
NOTABLE = {
    "identity-created": "made a new identity — peers will not know it yet",
    "identity-loaded": "was itself again across the restart",
    "identity-unreadable": "IDENTITY FILE UNREADABLE — became a stranger",
    "identity-not-saved": "could not save its identity — a stranger after every restart",
    "started": "started up",
    "claimed": "took the job — nobody else was running it",
    "job-owner": "saw {label} running the job",
    "yielded": "gave the job to {label}, which was further along",
    "took-over": "TOOK OVER the job at tick {tick}",
    "joined": "{label} appeared",
    "left": "{label} left, announced",
    "vanished": "{label} VANISHED without warning",
    "redialling": "lost a peer — trying its address again",
    "connected": "connected to {addr}",
    "my-address": "learned its own address: {addr}",
    "port-mapped": "the router opened {external}",
    "port-map-failed": "the router refused a mapping",
    "job-failed": "the job failed",
    "listen-failed": "could not listen on an address",
}


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


def ago(ms, now_ms):
    seconds = max(0, (now_ms - ms) / 1000)
    if seconds < 90:
        return f"{seconds:.0f}s ago"
    if seconds < 5400:
        return f"{seconds / 60:.0f}m ago"
    return f"{seconds / 3600:.1f}h ago"


def clock(ms):
    return time.strftime("%H:%M:%S", time.localtime(ms / 1000))


def timeline(events, now_ms):
    shown = [e for e in events if e.get("event") in NOTABLE]
    if not shown:
        print("\nNothing notable happened — the node has been quietly working.")
        return

    print("\nWhat happened")
    print("-------------")
    previous, repeats = None, 0
    for index, event in enumerate(shown):
        kind = event.get("event")
        last = index == len(shown) - 1
        # A run of the same kind is collapsed. Twenty consecutive connections
        # would otherwise bury the one line that mattered.
        if kind == previous and not last:
            repeats += 1
            continue
        if repeats:
            print(f"              … and {repeats} more like that")
            repeats = 0
        previous = kind
        fields = {k: event.get(k, "?") for k in ("label", "tick", "addr", "external")}
        try:
            description = NOTABLE[kind].format(**fields)
        except (KeyError, IndexError):
            description = kind
        ts = event.get("ts_unix_ms", 0)
        print(f"  {clock(ts)}  {description}  ({ago(ts, now_ms)})")


def main():
    args = list(sys.argv[1:])
    scope, since_minutes, path = "run", None, DEFAULT_LOG

    while args:
        arg = args.pop(0)
        if arg == "--all":
            scope = "all"
        elif arg == "--since":
            try:
                raw = args.pop(0)
                since_minutes = float(raw.rstrip("mh")) * (60 if raw.endswith("h") else 1)
                scope = "since"
            except (IndexError, ValueError):
                print("--since wants something like 20m or 2h")
                return 2
        elif arg in ("--help", "-h"):
            print(__doc__)
            return 0
        else:
            path = arg

    if not os.path.exists(path):
        print(f"no log at {path}")
        print("\nThe node writes there once installed as a service. If it is not")
        print("running, that is itself the finding:  systemctl status mesh-node")
        return 1

    events = load(path)
    if not events:
        print("the log is empty — the node may never have started, which is the finding")
        return 1

    total = len(events)
    now_ms = int(time.time() * 1000)

    # The current run is everything since the node last started, which is the
    # only boundary the log actually marks.
    if scope == "run":
        starts = [i for i, e in enumerate(events) if e.get("event") == "started"]
        if starts:
            events = events[starts[-1]:]
    elif scope == "since":
        cutoff = now_ms - since_minutes * 60_000
        events = [e for e in events if e.get("ts_unix_ms", 0) >= cutoff]

    if not events:
        print("nothing in that window")
        return 0

    kinds = collections.Counter(e.get("event") for e in events)
    node = next((e.get("node") for e in events if e.get("node")), "?")
    # Chosen with branches rather than a lookup: a dict evaluates every value,
    # including the one describing a window that was never asked for.
    if scope == "run":
        window = "this run"
    elif scope == "all":
        window = "the whole log"
    else:
        window = f"the last {since_minutes:.0f} minutes"
    began = events[0].get("ts_unix_ms", 0)

    print(f"{node} — {window}: {len(events)} of {total} events, "
          f"from {clock(began)} ({ago(began, now_ms)})\n")

    print(f"  peers joined      {kinds.get('joined', 0)}")
    print(f"  peers left        {kinds.get('left', 0)} announced, "
          f"{kinds.get('vanished', 0)} vanished without warning")
    print(f"  redials           {kinds.get('redialling', 0)}")

    # Whether this node was recognisable across its restarts. A node that
    # cannot be is one no peer can ever have cached, however long it runs.
    if kinds.get("identity-loaded"):
        print("  identity          kept across restarts")
    elif kinds.get("identity-created"):
        print("  identity          newly made this run")
    elif kinds.get("identity-not-saved") or kinds.get("identity-unreadable"):
        print("  identity          NOT PERSISTED — a stranger after every restart")

    mapped, refused = kinds.get("port-mapped", 0), kinds.get("port-map-failed", 0)
    if mapped or refused:
        print(f"  router mapping    {mapped} accepted, {refused} refused")
        if refused and not mapped:
            print("                    (expected on a mobile connection — there is no")
            print("                     home router there to open anything)")
    else:
        print("  router mapping    not asked yet")

    addresses = [e.get("addr") for e in events if e.get("event") == "my-address"]
    distinct = list(dict.fromkeys(addresses))
    if not addresses:
        print("  own address       not learned yet — no peer has observed this node")
    elif len(distinct) == 1:
        print(f"  own address       {distinct[0]}")
    else:
        print(f"  own address       moved {len(distinct) - 1}x, now {distinct[-1]}")

    if kinds.get("job-loaded") or kinds.get("effect"):
        print(f"  work              {kinds.get('effect', 0)} effects, "
              f"{kinds.get('produced', 0)} productions")
        print(f"  job ownership     {kinds.get('claimed', 0)} claimed, "
              f"{kinds.get('took-over', 0)} taken over, {kinds.get('yielded', 0)} yielded")
        if kinds.get("job-failed"):
            print(f"                    {kinds['job-failed']} FAILURES")

    timeline(events, now_ms)

    if scope == "run" and total > len(events):
        print(f"\n  {total - len(events)} earlier events are in the log. --all to see them.")

    if kinds.get("dialling") and not kinds.get("connected"):
        print("\n  Dialled and never connected. Either no peer was reachable, or the")
        print("  address given is stale — one behind a carrier is only good until")
        print("  that connection cycles.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
