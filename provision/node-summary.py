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
import gzip
import json
import os
import sys
import time

DEFAULT_LOG = "/var/log/mesh-node/events.jsonl"

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from chaos import rotations  # noqa: E402  — every rotation, the same way chaos.py finds them
from node_events import NOTABLE, describe  # noqa: E402  — shared with node-watch.sh


def load(path):
    events = []
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt", encoding="utf-8", errors="replace") as handle:
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
        description = describe(event)
        ts = event.get("ts_unix_ms", 0)
        print(f"  {clock(ts)}  {description}  ({ago(ts, now_ms)})")


def work_lines(events, kinds):
    """What the job did in this window, from the node's `working` lines.

    The node writes one a minute while it advances the job, carrying the
    ticks run since the last one, the tick reached, and the inventory held.
    This used to count `effect` and `produced` events, which the node
    stopped writing long ago, so a node working steadily reported "0 effects,
    0 productions" (review finding 20).
    """
    working = [e for e in events if e.get("event") == "working"]
    ticks = sum(int(e.get("ticks", 0) or 0) for e in working)
    lines = []
    if working:
        last = working[-1]
        lines.append(f"  work              {ticks} ticks run in this window, "
                     f"reaching tick {last.get('tick', '?')}")
        # The inventory is everything this node knows exists, from every node
        # and every run, so it is a level, not a count of what was made here.
        lines.append(f"  inventory now     {last.get('ledger', '?')} things known to exist "
                     "(all of history, not this window's output)")
    else:
        lines.append("  work              none in this window — standing by, or stopped")
    lines.append(f"  job ownership     {kinds.get('claimed', 0)} claimed, "
                 f"{kinds.get('took-over', 0)} taken over, {kinds.get('yielded', 0)} yielded")
    spec = kinds.get("speculating", 0)
    if spec:
        lines.append(f"  suspicions        {spec} raised, {kinds.get('speculation-abandoned', 0)} "
                     "abandoned as wrong")
    for kind, words in (("job-failed", "FAILURES"), ("effects-rejected", "ticks with production REFUSED"),
                        ("cold-start", "COLD STARTS from tick 0")):
        if kinds.get(kind):
            lines.append(f"                    {kinds[kind]} {words}")
    return lines


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
    # The log is rotated daily with copytruncate, so a run that has lasted past
    # a rotation has its start in an older file. Reading only today's reported
    # a node connected for days as one that never connected, which is what it
    # did on riverboat; reading only .1 did the same to any run older than two
    # days (review finding 15). So older files are read, newest first, until
    # the window asked for is covered: a start for the current run, the cutoff
    # for --since, everything for --all.
    cutoff_ms = int(time.time() * 1000) - since_minutes * 60_000 if scope == "since" else None
    for older in rotations(path):
        if scope == "run" and any(e.get("event") == "started" for e in events):
            break
        if scope == "since" and events and events[0].get("ts_unix_ms", 0) <= cutoff_ms:
            break
        events = load(older) + events
    if not events:
        print("the log is empty — the node may never have started, which is the finding")
        return 1

    total = len(events)
    now_ms = int(time.time() * 1000)

    # The current run is everything since the node last started, which is the
    # only boundary the log actually marks.
    partial = False
    if scope == "run":
        starts = [i for i, e in enumerate(events) if e.get("event") == "started"]
        if starts:
            events = events[starts[-1]:]
        else:
            # Said out loud rather than presented as the whole run. Every count
            # below is then a floor, and "never connected" cannot be concluded.
            partial = True
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
        window = "this run (PARTIAL — it began before the oldest event kept)" if partial else "this run"
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

    if kinds.get("job-loaded") or kinds.get("working"):
        for line in work_lines(events, kinds):
            print(line)

    timeline(events, now_ms)

    if scope == "run" and total > len(events):
        print(f"\n  {total - len(events)} earlier events are in the log. --all to see them.")

    if kinds.get("dialling") and not kinds.get("connected") and not partial:
        print("\n  Dialled and never connected. Either no peer was reachable, or the")
        print("  address given is stale — one behind a carrier is only good until")
        print("  that connection cycles.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
