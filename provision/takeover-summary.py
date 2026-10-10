#!/usr/bin/env python3
"""How long takeovers took, judged against the thresholds fixed in advance.

    ./takeover-summary.py diamond.events.jsonl diamond.chaos.jsonl \\
                          riverboat.events.jsonl riverboat.chaos.jsonl

Give it every box's node log and chaos log from one run, in any order; each
line says which node wrote it. Only the armed period is read — from the first
`armed` to the last `disarmed` — because the node logs hold weeks of other runs.

**Every duration comes from a single machine's clock.** Detection, resume and
the visible gap are each measured by the survivor against its own time. The
boxes' clocks are used only to match a chaos action on one machine to the
takeover it caused on another, where the window is tens of seconds and NTP's
error is irrelevant. Subtracting one machine's timestamp from another's would
put clock error straight into numbers judged in milliseconds.

Thresholds, fixed 2026-08-21 in docs/phase-0-plan.md and evaluated at p95:

  detection   2–3 s of silence before a peer is presumed gone
  resume      500 ms from the takeover beginning to work continuing

The visible gap — last state heard to first state produced — is reported
beside them. It is not a threshold as written, and it is what a player sees.
What it contains depends on the run: without speculation it spans the whole
detection window, and with it only the silence before suspicion. The report
says which, per run, rather than leaving a reader to assume the old meaning.
"""
import argparse
import bisect
import glob
import gzip
import json
import math
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chaos  # noqa: E402  — the same reading of a node's log that chaos.py acts on

# The owner writes a `working` line once a minute, so a steady owner must
# appear at least once in a minute and a half either side of any instant.
WORKING_SPAN_MS = 90_000

DETECT_MAX_MS = 3000
RESUME_MAX_MS = 500

# How long after a chaos action its takeover may appear on another machine.
# Detection is ~2.5 s at most; the rest is margin for clock disagreement and a
# slow election, not an allowance for a slow takeover — that shows up in the
# numbers themselves, not as a missed match.
# What each abandonment cause means, in the node's own terms. Kept here rather
# than inline so that a cause the node emits and this tool does not know about
# prints as itself instead of being quietly folded into a neighbour.
ABANDONMENT = {
    "checkpoint": "checkpoint — the owner came back and kept the job",
    "heard-again": "heard-again — the suspect was alive, its checkpoints lost",
    "not-ours": "not-ours — another survivor took the job",
}

MATCH_WINDOW_MS = 30_000
MATCH_EARLY_MS = 2_000
# A partitioned node's return is expected shortly after its partition ends.
RETURN_GRACE_MS = 20_000
# A kill this close to the victim's own yield hit a node that was no longer
# the owner, whatever its log said a moment before.
YIELD_RACE_MS = 1_000
# How far back to read a victim's own log to decide whether it held the job.
# The owner writes a working line every minute, so this is generous.
OWNERSHIP_WINDOW_MS = 1_800_000


def with_rotations(paths):
    """Every path given, plus the rotated files beside it.

    Logs rotate daily, and a run that spans midnight leaves half its evidence
    in events.jsonl.1. A shell glob of "*.jsonl" does not match that, and the
    absence is silent: takeovers whose evidence is in the rotated file look
    like takeovers that never happened. This summary reported 24 orphaned jobs
    that way, from a night where nothing was orphaned at all.
    """
    found = []
    for path in paths:
        for candidate in [path] + sorted(glob.glob(path + ".[0-9]")
                                         + glob.glob(path + ".[0-9].gz")):
            if os.path.exists(candidate) and candidate not in found:
                found.append(candidate)
    return found


def load(paths):
    events, actions = [], []
    for path in with_rotations(paths):
        opener = gzip.open if path.endswith(".gz") else open
        with opener(path, mode="rt", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                line = line.strip()
                if not line.startswith("{"):
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                (actions if "action" in record else events).append(record)
    events.sort(key=lambda e: e.get("ts_unix_ms", 0))
    actions.sort(key=lambda a: a.get("ts_unix_ms", 0))
    return events, actions


# The same rule chaos.py applies on the box: the run asked about is the last
# one in these logs, not the first. Defined once, because reading the wrong
# period has now blinded three tools in this directory.
armed_window = chaos.armed_window


def window_line(start, end):
    """What period these numbers describe. Printed beside the file list, and
    checked the same way: against the hours the run was actually armed."""
    began = time.strftime("%Y-%m-%d %H:%M", time.localtime(start / 1000))
    if end == math.inf:
        return f"the run armed {began}, still armed"
    finished = time.strftime("%H:%M", time.localtime(end / 1000))
    return f"the run armed {began} -> {finished}, {(end - start) / 3600_000:.1f} h"


def percentile(values, p):
    """Nearest rank. No interpolation: with a hundred samples, an interpolated
    p99 is a number no takeover ever took."""
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(p / 100 * len(ordered)))
    return ordered[rank - 1]


def num(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def held_the_job(by_node, victim, at):
    """Whether the victim's own log says it was running the job when hit.

    Read the way chaos.py reads it, because the question is what the tool
    believed. A tool that misreads its own node's log disturbs a standby, which
    moves no work and produces no takeover — and would otherwise be counted as
    a job nobody continued.
    """
    stamps, rows = by_node.get(victim, ([], []))
    end = bisect.bisect_right(stamps, at)
    start = bisect.bisect_left(stamps, at - OWNERSHIP_WINDOW_MS)
    return chaos.situation(rows[start:end], at)[0]


def kept_working_elsewhere(events, victim, at):
    """Which other node was already advancing the job when this one was hit.

    On a flapping link two nodes can both believe they own the job, and
    chaos fires at whichever one believes it. If another node was advancing
    the work before the disruption and still advancing it after, nothing
    stopped and no takeover was needed — so the absence of a `took-over` is
    not a job nobody continued. Run D produced two of these and the summary
    called them orphans while the survivor's ticks never missed a beat.

    `working` is the evidence because only the node advancing the job writes
    it. It is written once a minute, so a minute and a half either side is
    the span in which a steady owner must appear at least once.
    """
    for node in {e.get("node") for e in events if e.get("node") != victim}:
        lines = [e["ts_unix_ms"] for e in events
                 if e.get("node") == node and e.get("event") == "working"]
        before = any(at - WORKING_SPAN_MS <= t < at for t in lines)
        after = any(at < t <= at + WORKING_SPAN_MS for t in lines)
        if before and after:
            return node
    return None


def speculations_ended_by_a_yield(events):
    """Suspicions that ended because the node gave the job up instead.

    A speculator stood down by a yield emits no abandonment cause — the node
    resolved two owners rather than changing its mind about one — so these
    fell into `started - confirmed - abandoned` and were reported as silent
    exits. Whatever happens first to that node after the suspicion is what
    ended it.
    """
    ended = 0
    for index, event in enumerate(events):
        if event.get("event") != "speculating":
            continue
        node = event.get("node")
        following = (e.get("event") for e in events[index + 1:]
                     if e.get("node") == node
                     and e.get("event") in ("speculation-abandoned", "took-over",
                                            "yielded", "speculating"))
        if next(following, None) == "yielded":
            ended += 1
    return ended


def by_node_ownership_events(events):
    """Each node's ownership-relevant events, in time order, for fast lookup."""
    relevant = chaos.OWNING | chaos.NOT_OWNING | {chaos.WORKING, "started"}
    grouped = {}
    for event in events:
        if event.get("event") not in relevant:
            continue
        stamps, rows = grouped.setdefault(event.get("node"), ([], []))
        stamps.append(event.get("ts_unix_ms", 0))
        rows.append(event)
    return grouped


def match(actions, events):
    """Pairs each disruptive chaos action with what the survivors logged."""
    disruptions = [a for a in actions if a.get("action") in ("kill", "term", "partition")]
    by_node = by_node_ownership_events(events)
    results = []
    for action in disruptions:
        victim, at = action["node"], action["ts_unix_ms"]
        window = [e for e in events
                  if e.get("node") != victim
                  and at - MATCH_EARLY_MS <= e.get("ts_unix_ms", 0) <= at + MATCH_WINDOW_MS]
        lost = next((e for e in window
                     if e.get("event") in ("vanished", "left") and e.get("label") == victim), None)
        takeovers = [e for e in window if e.get("event") == "took-over"
                     and (lost is None or e["ts_unix_ms"] >= lost["ts_unix_ms"])]
        first = takeovers[0] if takeovers else None
        resumed = None
        if first:
            # `resumed` carries no label, so nothing in the event itself says
            # which disruption it belongs to. The match window runs ~30 s past
            # the action and a successor can be disrupted several times inside
            # it, so an unbounded search — in either direction — will happily
            # report another disruption's number as this one's. Both ends have
            # to be pinned to events that DO name this disruption.
            candidates = sorted((e for e in window
                                 if e.get("event") == "resumed"
                                 and e.get("node") == first["node"]),
                                key=lambda e: e["ts_unix_ms"])
            # Where this disruption's evidence stops: the successor's next
            # suspicion or takeover. Anything at or after that was caused by
            # something else, whatever it is.
            later = [e["ts_unix_ms"] for e in window
                     if e.get("node") == first["node"]
                     and e.get("event") in ("speculating", "took-over")
                     and e["ts_unix_ms"] > first["ts_unix_ms"]]
            until = min(later) if later else math.inf
            if first.get("speculated") == "true":
                # A speculated resume happens BEFORE the takeover it belongs
                # to: the standby started the work on suspicion and only later
                # was told it had been right. So the search runs backwards from
                # the takeover, anchored on the `speculating` event — the only
                # event in this sequence carrying label == victim, and so the
                # only thing that ties a resume to THIS disruption. Taking the
                # last such suspicion also steps over an earlier one that was
                # abandoned: its resume is not this takeover's resume.
                suspecting = [e for e in window
                              if e.get("event") == "speculating"
                              and e.get("label") == victim
                              and e.get("node") == first["node"]
                              and e["ts_unix_ms"] <= first["ts_unix_ms"]]
                if suspecting:
                    suspected = suspecting[-1]
                    resumed = next((e for e in candidates
                                    if suspected["ts_unix_ms"] <= e["ts_unix_ms"] <= first["ts_unix_ms"]),
                                   None)
                # No speculating event for this disruption means no speculated
                # resume for it either — a missing number is honest; grabbing
                # one from elsewhere in the window is not.
            else:
                # Nothing was speculated, so the resume follows its takeover in
                # the ordinary way — but only up to the successor's next
                # suspicion or takeover. Past that point a `resumed` belongs to
                # the next disruption, and it is a smaller number, so taking it
                # would bias the headline metric optimistic by exactly the
                # noise a run is built to measure.
                resumed = next((e for e in candidates
                                if first["ts_unix_ms"] <= e["ts_unix_ms"] < until), None)
        results.append({
            "action": action["action"],
            "victim": victim,
            "at": at,
            "successor": first["node"] if first else None,
            "noticed_as": lost.get("event") if lost else None,
            "detect_ms": num(lost.get("silent_ms")) if lost and lost.get("event") == "vanished" else None,
            "resume_ms": num(resumed.get("resume_ms")) if resumed else None,
            "gap_ms": num(resumed.get("gap_ms")) if resumed else None,
            "speculated": bool(first and first.get("speculated") == "true"),
            "takeovers": len({t["node"] for t in takeovers}),
            "seconds": action.get("seconds"),
            # The victim's own log said it owned the job when chaos read it,
            # but it yielded at almost the same moment. Nobody needed to take
            # over, so this is not an orphaned job.
            "was_standby": any(e.get("node") == victim and e.get("event") == "yielded"
                               and at - YIELD_RACE_MS <= e.get("ts_unix_ms", 0) <= at + YIELD_RACE_MS
                               for e in events)
            or not held_the_job(by_node, victim, at),
            "kept_elsewhere": kept_working_elsewhere(events, victim, at),
        })
    return results


def false_alarms(events, actions):
    """Returns without a restart that no partition explains.

    A node that was partitioned on purpose comes back without restarting; that
    is the experiment. Anything else that comes back without restarting was
    never gone, and the work moved for nothing.

    A partition explains a return seen from **either side** of it. To the
    partitioned node, it is the peer that went away and came back unrestarted.
    """
    partitions = [(a["node"], a["ts_unix_ms"], a["ts_unix_ms"] + 1000 * (a.get("seconds") or 60))
                  for a in actions if a.get("action") == "partition"]
    alarms = []
    for event in events:
        if event.get("event") != "returned" or event.get("restarted") != "false":
            continue
        who, seen_by, ts = event.get("label"), event.get("node"), event["ts_unix_ms"]
        explained = any(node in (who, seen_by) and start - MATCH_EARLY_MS <= ts <= end + RETURN_GRACE_MS
                        for node, start, end in partitions)
        if not explained:
            alarms.append(event)
    return alarms


def row(name, values, limit=None, unit="ms"):
    """One distribution. `unit` because not everything measured here is a duration —
    ticks wasted on a wrong suspicion are a count, and printing them as
    milliseconds states a number in units it was never in."""
    n = len(values)
    if not n:
        return f"  {name:<22} no samples"
    p50, p95, p99 = (percentile(values, p) for p in (50, 95, 99))
    verdict = ""
    if limit is not None:
        verdict = "  PASS" if p95 <= limit else f"  FAIL (p95 over {limit} ms)"
    thin = "  (p99 is one sample or fewer)" if n < 100 else ""
    return (f"  {name:<22} n={n:<4} p50 {p50:>5} {unit}   p95 {p95:>5} {unit}   "
            f"p99 {p99:>5} {unit}   max {max(values):>5} {unit}{verdict}{thin}")


def report(results, events, actions, out=sys.stdout):
    say = lambda text="": print(text, file=out)  # noqa: E731

    groups = {}
    for r in results:
        groups.setdefault((r["action"], r["victim"], r["successor"]), []).append(r)

    say("TAKEOVERS BY CAUSE AND DIRECTION")
    for (action, victim, successor), rs in sorted(groups.items(), key=lambda g: (g[0][0], g[0][1], str(g[0][2]))):
        say(f"\n{action}: {victim} -> {successor or 'NOBODY'}  ({len(rs)})")
        if action != "term":
            say(row("detection", [r["detect_ms"] for r in rs if r["detect_ms"] is not None]))
        say(row("resume", [r["resume_ms"] for r in rs if r["resume_ms"] is not None]))
        say(row("visible gap", [r["gap_ms"] for r in rs if r["gap_ms"] is not None]))

    unannounced = [r for r in results if r["action"] in ("kill", "partition")]
    say("\nAGAINST THE THRESHOLDS (unannounced losses: kill and partition together)")
    say(row("detection", [r["detect_ms"] for r in unannounced if r["detect_ms"] is not None], DETECT_MAX_MS))
    say(row("resume", [r["resume_ms"] for r in unannounced if r["resume_ms"] is not None], RESUME_MAX_MS))
    say(row("visible gap", [r["gap_ms"] for r in unannounced if r["gap_ms"] is not None]))
    # What the gap contains is not a property of the tool, it is a property of
    # the run. Under speculation the work restarts on suspicion, so the gap
    # stops at that suspicion and never holds the detection window — printing
    # the old sentence over speculated rows would describe a number that is
    # not there.
    speculated = [r for r in unannounced if r["speculated"]]
    say("  The visible gap is not a threshold as written. It is what a player waits "
        "through.")
    if not speculated:
        say("  It includes detection: nothing here restarted before the owner was "
            "declared gone.")
    elif len(speculated) == len(unannounced):
        say("  It does NOT include detection: every takeover here was speculated, so the "
            "gap\n  spans the silence before suspicion, not the wait for a decision.")
    else:
        say(f"  {len(speculated)} of {len(unannounced)} takeovers here were speculated: those "
            "gaps span the silence\n  before suspicion, and the rest include the whole "
            "detection window.")

    say("\nTHINGS THAT SHOULD NOT HAPPEN")
    orphaned = [r for r in results if r["successor"] is None and not r["kept_elsewhere"]]
    kept = [r for r in results if r["successor"] is None and r["kept_elsewhere"]]
    doubled = [r for r in results if r["takeovers"] > 1]
    alarms = false_alarms(events, actions)
    unnoticed = [e for e in events if e.get("event") == "restarted-unnoticed"]
    yields = [e for e in events if e.get("event") == "yielded"]
    say(f"  nobody took over          {len(orphaned):>4}  of {len(results)} disruptions"
        + (f"  ({sum(1 for r in orphaned if r['was_standby'])} of them hit a node that had "
           "just yielded — no work stopped)" if any(r["was_standby"] for r in orphaned) else ""))
    say(f"  more than one took over   {len(doubled):>4}")
    say(f"  false alarms              {len(alarms):>4}  returned without restarting, "
        "and no partition explains it")
    say(f"  restarted unnoticed       {len(unnoticed):>4}  back inside one detection window")
    say(f"  yields                    {len(yields):>4}  two owners resolved "
        "(expected after partitions: "
        f"{sum(1 for a in actions if a.get('action') == 'partition')})")
    if kept:
        say(f"  (another node was already advancing the job through {len(kept)} more "
            "disruption(s), so no takeover was needed and nothing stopped)")
    for r in orphaned:
        say(f"    orphaned: {r['action']} of {r['victim']} at {r['at']}")
    for r in kept:
        say(f"    kept going: {r['action']} of {r['victim']} at {r['at']} — "
            f"{r['kept_elsewhere']} never stopped working")
    for e in alarms:
        say(f"    false alarm: {e.get('label')} seen by {e.get('node')} at {e['ts_unix_ms']}, "
            f"away {e.get('away_ms')} ms")

    started = [e for e in events if e.get("event") == "speculating"]
    abandoned = [e for e in events if e.get("event") == "speculation-abandoned"]
    confirmed = [r for r in results if r["speculated"]]
    if started:
        say("\nSPECULATION")
        say(f"  started                   {len(started):>4}  suspicions acted on before any decision")
        say(f"  confirmed                 {len(confirmed):>4}  the owner really had gone")
        say(f"  abandoned                 {len(abandoned):>4}  the suspicion was wrong")
        # Broken down because one line cannot describe all three. Only
        # "checkpoint" is the owner coming back to keep the job; "heard-again"
        # is a peer that was alive all along on a link losing its checkpoints,
        # and "not-ours" is another survivor taking the job while the suspect
        # stays gone. Run C is the first run that can produce the last two, and
        # a single description would have mislabelled them both.
        by_cause = {}
        for e in abandoned:
            by_cause.setdefault(e.get("cause") or "unlabelled", []).append(e)
        for cause, group in sorted(by_cause.items()):
            say(f"      {ABANDONMENT[cause] if cause in ABANDONMENT else cause:<50}"
                f"{len(group):>4}")
        # A speculation can also end without a word: a job that fails stands it
        # down and emits nothing, because the node has stopped working rather
        # than changed its mind. Without this line the three counts simply fail
        # to add up, and a reader cannot tell a silent exit from a lost event.
        # Its own line, not one of the abandonment causes. These carry no
        # cause because the node emitted none: it resolved two owners rather
        # than changing its mind about one. Listed under `abandoned` the
        # sub-counts would not add up to the subtotal above them.
        by_yield = speculations_ended_by_a_yield(events)
        if by_yield:
            say(f"  ended by a yield          {by_yield:>4}  two owners resolved instead; "
                "the node gave the job up rather than deciding about the suspect")
        unaccounted = len(started) - len(confirmed) - len(abandoned) - by_yield
        if unaccounted:
            say(f"  unaccounted               {unaccounted:>4}  ended without an event — a job that "
                "failed, or a log that begins mid-suspicion")
        wasted = [num(e.get("ticks_wasted")) for e in abandoned
                  if num(e.get("ticks_wasted")) is not None]
        if wasted:
            say(row("ticks wasted", wasted, unit="ticks"))
        say("  A wrong suspicion costs ticks, not correctness: the same job from the same")
        say("  state makes the same items, so discarded work merges to nothing.")

    skips = sum(1 for a in actions if a.get("action") == "skip")
    say(f"\n{len(results)} disruptions, {skips} fires skipped (standby, no peer, or not settled)")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("logs", nargs="+", help="node and chaos logs from every box in the run")
    parser.add_argument("--all", action="store_true", help="ignore the armed window and read everything")
    args = parser.parse_args()

    events, actions = load(args.logs)
    print(f"read {len(with_rotations(args.logs))} files: "
          f"{', '.join(os.path.basename(p) for p in with_rotations(args.logs))}")
    if not args.all:
        window = armed_window(actions)
        if window is None:
            sys.exit("no 'armed' record in the chaos logs — pass --all to read everything")
        start, end = window
        print(window_line(start, end))
        # A little past the end, so a takeover caused by the last action counts.
        inside = lambda r: start <= r.get("ts_unix_ms", 0) <= end + MATCH_WINDOW_MS  # noqa: E731
        events = [e for e in events if inside(e)]
        actions = [a for a in actions if inside(a)]
    else:
        print("every run in these logs, armed or not")
    print()
    report(match(actions, events), events, actions)


if __name__ == "__main__":
    main()
