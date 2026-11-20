#!/usr/bin/env python3
"""How a run's members reached each other, and how much work ran twice.

    ./link-summary.py results/run-g/*/events.jsonl results/run-g/*/chaos.jsonl

Written for Run G, from Run F's lesson: two members behind translation never
met directly, so they reached each other only through riverboat, and when it
left the mesh split and ran the job twice for 57 minutes. This reports what
the relaying design (2026-11-18) changes about that:

  LINKS            per pair of members: direct and relayed connections, and
                   how many coordinated punches were tried and worked
  INTRODUCERS      who held introductions on whom, and how often they ended
  REACHABLE        which members a member confirmed it could reach
  DUPLICATED WORK  ticks computed by two owners at once

**Duplicated work is counted by tick number, not by clock.** Each node's spans
as owner run from its claim or takeover to its yield, or to its last
`working` line before it restarted. Two machines' spans overlapping is work
done twice, and no timestamp from one machine is compared with another's. A
killed owner's span ends at its last `working` line, up to a minute early, so
the count is a lower bound.

Reads logs, rotations and the armed window the same way takeover-summary.py
does.
"""
import argparse
import collections
import importlib.machinery
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_loader = importlib.machinery.SourceFileLoader("takeover_summary", os.path.join(HERE, "takeover-summary.py"))
_spec = importlib.util.spec_from_loader("takeover_summary", _loader)
takeover = importlib.util.module_from_spec(_spec)
_loader.exec_module(takeover)


def num(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def owner_spans(events):
    """Per node, the (from_tick, to_tick) spans it computed as owner."""
    spans = collections.defaultdict(list)
    open_from, last_working = {}, {}

    def close(node, at):
        start = open_from.pop(node, None)
        if start is not None:
            spans[node].append((start, at if at is not None else start))
        last_working.pop(node, None)

    for e in events:
        node, kind = e.get("node"), e.get("event")
        if kind in ("claimed", "took-over") and node not in open_from:
            tick = num(e.get("tick"))
            if tick is not None:
                open_from[node] = tick
        elif kind == "working" and node in open_from:
            last_working[node] = num(e.get("tick"))
        elif kind == "yielded" and node in open_from:
            close(node, num(e.get("our_tick")))
        elif kind == "started" and node in open_from:
            close(node, last_working.get(node))
    for node in list(open_from):
        close(node, last_working.get(node))
    return dict(spans)


def duplicated_ticks(spans):
    """Ticks inside the spans of two or more nodes at once."""
    edges = []
    for node_spans in spans.values():
        for start, end in node_spans:
            if end > start:
                edges += [(start, 1), (end, -1)]
    edges.sort(key=lambda edge: (edge[0], edge[1]))
    active, since, twice = 0, None, 0
    for tick, step in edges:
        if active >= 2:
            twice += tick - since
        active += step
        since = tick
    return twice


def labels(events):
    """peer_id -> label, from what nodes said about each other and themselves."""
    names = {}
    for e in events:
        if e.get("peer_id") and e.get("label") and e.get("event") in ("joined", "returned", "job-owner"):
            names[e["peer_id"]] = e["label"]
        if e.get("event") == "started" and e.get("peer_id"):
            names[e["peer_id"]] = e.get("node")
    return names


def links(events):
    """Per unordered pair of labels: direct, relayed, punches_tried, punches_ok."""
    names = labels(events)
    pairs = collections.defaultdict(lambda: {"direct": 0, "relayed": 0, "punches_tried": 0, "punches_ok": 0})
    for e in events:
        kind = e.get("event")
        other = e.get("label") or names.get(e.get("peer_id"))
        if not other or other == e.get("node"):
            continue
        pair = pairs[tuple(sorted((e.get("node"), other)))]
        if kind == "connected":
            pair["relayed" if e.get("relayed") == "true" else "direct"] += 1
        elif kind in ("punch-succeeded", "punch-failed"):
            pair["punches_tried"] += 1
            pair["punches_ok"] += kind == "punch-succeeded"
    return dict(pairs)


def report(events, out=sys.stdout):
    say = lambda line="": print(line, file=out)  # noqa: E731
    say("LINKS")
    for (a, b), p in sorted(links(events).items()):
        say(f"  {a} - {b}: {p['direct']} direct, {p['relayed']} relayed; "
            f"punches {p['punches_ok']} of {p['punches_tried']} worked")
    say()
    say("INTRODUCERS")
    held = collections.Counter((e["node"], e.get("label", "?")) for e in events if e.get("event") == "introducer-held")
    lost = collections.Counter(e["node"] for e in events if e.get("event") == "introducer-lost")
    for (node, by), n in sorted(held.items()):
        say(f"  {node} held an introduction on {by} {n} time(s)")
    for node, n in sorted(lost.items()):
        say(f"  {node}: {n} introduction(s) ended")
    if not held:
        say("  none")
    say()
    say("REACHABLE")
    reachable = sorted({e["node"] for e in events if e.get("event") == "reachable"})
    say(f"  {', '.join(reachable) if reachable else 'nobody confirmed reachable'}")
    say()
    spans = owner_spans(events)
    say("DUPLICATED WORK")
    say(f"  {duplicated_ticks(spans)} ticks computed by two owners at once (a lower bound)")
    for node, node_spans in sorted(spans.items()):
        say(f"  {node}: owner for {sum(end - start for start, end in node_spans)} ticks in {len(node_spans)} span(s)")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("logs", nargs="+", help="node and chaos logs from every box in the run")
    parser.add_argument("--all", action="store_true", help="ignore the armed window and read everything")
    args = parser.parse_args(argv)
    events, actions = takeover.load(args.logs)
    if not args.all:
        window = takeover.armed_window(actions)
        if window is None:
            sys.exit("no 'armed' record in the chaos logs — pass --all to read everything")
        start, end = window
        print(takeover.window_line(start, end))
        events = [e for e in events if start <= e.get("ts_unix_ms", 0) <= end]
    print()
    report(events)
    return 0


if __name__ == "__main__":
    sys.exit(main())
