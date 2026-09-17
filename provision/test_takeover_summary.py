#!/usr/bin/env python3
"""Tests for takeover-summary.py. `python3 -m unittest provision/test_takeover_summary.py`"""
import importlib.util
import io
import math
import os
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("summary", os.path.join(HERE, "takeover-summary.py"))
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)

T0 = 1_000_000_000_000


def ev(node, ms, kind, **fields):
    return {"ts_unix_ms": T0 + ms, "node": node, "event": kind, **fields}


def act(node, ms, action, **fields):
    return {"ts_unix_ms": T0 + ms, "node": node, "action": action, **fields}


def kill_and_takeover(at, silent=2200, resume=150, gap=2400, victim="diamond", successor="riverboat"):
    actions = [act(victim, at, "kill")]
    events = [
        ev(successor, at + silent, "vanished", label=victim, silent_ms=str(silent)),
        ev(successor, at + silent, "took-over", tick="10"),
        ev(successor, at + silent + resume, "resumed", resume_ms=str(resume), gap_ms=str(gap)),
    ]
    return actions, events


class Percentile(unittest.TestCase):
    def test_nearest_rank_is_a_value_that_occurred(self):
        values = list(range(1, 101))
        self.assertEqual(summary.percentile(values, 50), 50)
        self.assertEqual(summary.percentile(values, 95), 95)
        self.assertEqual(summary.percentile(values, 99), 99)
        self.assertEqual(summary.percentile([7], 99), 7)
        self.assertIsNone(summary.percentile([], 95))


class Match(unittest.TestCase):
    def test_a_kill_is_paired_with_the_takeover_it_caused(self):
        actions, events = kill_and_takeover(0)
        [r] = summary.match(actions, events)
        self.assertEqual((r["successor"], r["detect_ms"], r["resume_ms"], r["gap_ms"]),
                         ("riverboat", 2200, 150, 2400))

    def test_the_victims_own_log_never_counts_as_a_takeover(self):
        # After a restart the victim may log events inside the window; it did
        # not take over from itself.
        actions = [act("diamond", 0, "kill")]
        events = [ev("diamond", 12_000, "took-over", tick="3")]
        [r] = summary.match(actions, events)
        self.assertIsNone(r["successor"])

    def test_a_takeover_far_outside_the_window_is_not_claimed(self):
        actions, events = kill_and_takeover(0)
        for e in events:
            e["ts_unix_ms"] += summary.MATCH_WINDOW_MS + 5_000
        [r] = summary.match(actions, events)
        self.assertIsNone(r["successor"])

    def test_a_kill_racing_the_victims_yield_is_marked_as_a_standby(self):
        actions = [act("riverboat", 0, "kill")]
        events = [ev("riverboat", 0, "yielded", label="diamond")]
        [r] = summary.match(actions, events)
        self.assertIsNone(r["successor"])
        self.assertTrue(r["was_standby"])

    def test_a_disruption_of_a_node_that_did_not_hold_the_job_is_marked(self):
        # diamond, 2026-09-16: it yielded, then wrote a working line covering
        # ticks from before the yield, and the chaos tool on the box read that
        # as ownership thirteen hours later. No work moved, so this is not a
        # job nobody continued.
        actions = [act("diamond", 13 * 3600_000, "term")]
        events = [
            ev("diamond", 0, "took-over", tick="49344"),
            ev("diamond", 17_000, "working", tick="49429"),
            ev("diamond", 31_000, "yielded", label="riverboat"),
            ev("diamond", 77_000, "working", tick="49740"),
        ]
        [r] = summary.match(actions, events)
        self.assertIsNone(r["successor"])
        self.assertTrue(r["was_standby"])

    def test_a_disruption_of_the_real_owner_is_not_marked_as_a_standby(self):
        actions, events = kill_and_takeover(600_000)
        events += [ev("diamond", 540_000, "working", tick="10"),
                   ev("diamond", 599_000, "working", tick="20")]
        [r] = summary.match(actions, events)
        self.assertFalse(r["was_standby"])
        self.assertEqual(r["successor"], "riverboat")

    def test_two_takeovers_are_counted(self):
        actions, events = kill_and_takeover(0)
        events.append(ev("lightning", 2300, "took-over", tick="10"))
        [r] = summary.match(actions, events)
        self.assertEqual(r["takeovers"], 2)


class FalseAlarms(unittest.TestCase):
    def test_a_return_explained_by_a_partition_is_not_an_alarm(self):
        actions = [act("diamond", 0, "partition", seconds=30)]
        events = [ev("riverboat", 33_000, "returned", label="diamond", restarted="false", away_ms="31000")]
        self.assertEqual(summary.false_alarms(events, actions), [])

    def test_the_partitioned_node_seeing_its_peer_return_is_not_an_alarm(self):
        # From inside the partition, it is the peer that went away. Found in
        # the second chaos-rig dry run, where both partitions were reported as
        # false alarms from the partitioned side.
        actions = [act("diamond", 0, "partition", seconds=35)]
        events = [ev("diamond", 35_100, "returned", label="riverboat", restarted="false", away_ms="33500")]
        self.assertEqual(summary.false_alarms(events, actions), [])

    def test_an_unexplained_return_without_a_restart_is(self):
        events = [ev("riverboat", 500_000, "returned", label="diamond", restarted="false", away_ms="2500")]
        self.assertEqual(len(summary.false_alarms(events, [])), 1)

    def test_a_restart_is_never_an_alarm(self):
        events = [ev("riverboat", 500_000, "returned", label="diamond", restarted="true", away_ms="14000")]
        self.assertEqual(summary.false_alarms(events, []), [])


class Report(unittest.TestCase):
    def test_verdicts_follow_p95(self):
        actions, events = [], []
        for i in range(20):
            # One slow detection in twenty is the 20th rank, above p95's 19th,
            # so a single outlier must not fail the run — that is what p95 is for.
            a, e = kill_and_takeover(i * 400_000, silent=2200 if i else 3900)
            actions += a
            events += e
        out = io.StringIO()
        summary.report(summary.match(actions, events), events, actions, out)
        text = out.getvalue()
        threshold_section = text.split("AGAINST THE THRESHOLDS")[1]
        self.assertIn("detection              n=20   p50  2200 ms   p95  2200 ms", threshold_section)
        self.assertIn("PASS", threshold_section)

    def test_armed_window_spans_first_arm_to_last_disarm(self):
        actions = [act("a", 100, "armed"), act("b", 200, "armed"),
                   act("a", 9_000, "disarmed"), act("b", 9_500, "disarmed")]
        self.assertEqual(summary.armed_window(actions), (T0 + 100, T0 + 9_500))


class ArmedWindow(unittest.TestCase):
    """A box's chaos log holds every run it has ever done. The window must be
    the run being asked about — the last one — or the summary answers about a
    different night without saying so."""

    def test_a_run_still_going_is_read_and_the_run_before_it_is_not(self):
        actions = [act("a", 100, "armed"), act("a", 9_000, "disarmed"),
                   act("a", 50_000, "armed")]
        self.assertEqual(summary.armed_window(actions), (T0 + 50_000, math.inf))

    def test_the_last_of_two_finished_runs_is_the_one_read(self):
        actions = [act("a", 100, "armed"), act("a", 9_000, "disarmed"),
                   act("a", 50_000, "armed"), act("a", 80_000, "disarmed")]
        self.assertEqual(summary.armed_window(actions), (T0 + 50_000, T0 + 80_000))

    def test_the_window_opens_at_the_first_box_to_arm_not_the_last(self):
        # Boxes are armed by hand a minute apart. A disruption in that minute
        # is part of the run and must not fall outside it.
        actions = [act("a", 100, "armed"), act("a", 9_000, "disarmed"),
                   act("a", 50_000, "armed"), act("b", 110_000, "armed")]
        self.assertEqual(summary.armed_window(actions), (T0 + 50_000, math.inf))

    def test_a_box_that_sat_out_the_last_run_does_not_reopen_the_window(self):
        # `b` was in the first run only; its log is collected with the rest.
        actions = [act("a", 100, "armed"), act("b", 200, "armed"),
                   act("a", 9_000, "disarmed"), act("b", 9_500, "disarmed"),
                   act("a", 50_000, "armed")]
        self.assertEqual(summary.armed_window(actions), (T0 + 50_000, math.inf))

    def test_no_arm_at_all_is_no_window(self):
        self.assertIsNone(summary.armed_window([act("a", 100, "kill")]))

    def test_the_window_is_printed_so_it_can_be_checked_against_the_run(self):
        line = summary.window_line(T0, T0 + 8 * 3600_000)
        self.assertIn(time.strftime("%Y-%m-%d %H:%M", time.localtime(T0 / 1000)), line)
        self.assertIn(time.strftime("%H:%M", time.localtime(T0 / 1000 + 8 * 3600)), line)
        self.assertIn("8.0 h", line)

    def test_a_run_still_going_says_so_rather_than_printing_a_false_end(self):
        line = summary.window_line(T0, math.inf)
        self.assertIn("still armed", line)
        self.assertIn(time.strftime("%Y-%m-%d %H:%M", time.localtime(T0 / 1000)), line)


if __name__ == "__main__":
    unittest.main()
