#!/usr/bin/env python3
"""Tests for chaos.py's decisions. `python3 -m unittest provision/test_chaos.py`

Only the decisions are tested here. Killing, stopping and partitioning are one
systemctl or nft call each, and are exercised on a real box before a run.
"""
import json
import os
import random
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chaos  # noqa: E402

T0 = 1_000_000_000_000
PEER = "12D3KooWpeer"


def ev(offset_s, kind, **fields):
    return {"ts_unix_ms": T0 + int(offset_s * 1000), "event": kind, **fields}


def owner_with_peer():
    return [
        ev(0, "started"),
        ev(1, "joined", peer_id=PEER),
        ev(6, "claimed", tick="0"),
        ev(66, "working", tick="300"),
    ]


class Decide(unittest.TestCase):
    def at(self, seconds):
        return T0 + seconds * 1000

    def test_fires_on_a_settled_owner_with_a_peer(self):
        action, _ = chaos.decide(owner_with_peer(), self.at(200), random.Random(1))
        self.assertIn(action, {"kill", "term", "partition"})

    def test_never_kills_a_standby(self):
        # Killing a standby measures nothing: no work moves.
        events = [ev(0, "started"), ev(1, "joined", peer_id=PEER), ev(2, "job-owner")]
        self.assertIsNone(chaos.decide(events, self.at(200))[0])

    def test_never_kills_a_node_with_nobody_to_take_over(self):
        # That would time the restart, not the mesh.
        events = owner_with_peer() + [ev(100, "vanished", peer_id=PEER)]
        self.assertIsNone(chaos.decide(events, self.at(400))[0])

    def test_waits_for_the_last_disruption_to_settle(self):
        # A node that has just taken over is mid-recovery; disturbing it again
        # would time two recoveries as one.
        events = owner_with_peer() + [ev(100, "took-over", tick="500")]
        self.assertIsNone(chaos.decide(events, self.at(130))[0])
        self.assertIsNotNone(chaos.decide(events, self.at(100 + chaos.SETTLED_SECONDS + 1))[0])

    def test_a_node_that_yielded_no_longer_owns(self):
        events = owner_with_peer() + [ev(80, "yielded", peer_id=PEER)]
        self.assertIsNone(chaos.decide(events, self.at(400))[0])

    def test_a_returning_peer_counts_again(self):
        events = owner_with_peer() + [
            ev(70, "vanished", peer_id=PEER),
            ev(90, "joined", peer_id=PEER),
        ]
        self.assertIsNotNone(chaos.decide(events, self.at(400))[0])

    def test_a_peer_rejoining_must_settle_before_anything_fires(self):
        # After a partition both sides own the job until one yields. The
        # rejoin is logged first, the yield a moment later.
        events = owner_with_peer() + [
            ev(70, "vanished", peer_id=PEER),
            ev(200, "joined", peer_id=PEER),
        ]
        self.assertIsNone(chaos.decide(events, self.at(200.2))[0])

    def test_no_events_means_no_action(self):
        self.assertIsNone(chaos.decide([], self.at(400))[0])

    def test_the_mix_is_mostly_unannounced_loss(self):
        rng = random.Random(7)
        picks = [chaos.decide(owner_with_peer(), self.at(400), rng)[0] for _ in range(2000)]
        self.assertAlmostEqual(picks.count("kill") / len(picks), 0.6, delta=0.05)
        self.assertEqual(set(picks), {"kill", "term", "partition"})


class AcrossARotation(unittest.TestCase):
    """The night of 2026-09-15: riverboat held the job, its log rotated at
    00:26, and its chaos fired nothing for the five hours that remained."""

    def write(self, today, yesterday=None):
        directory = tempfile.mkdtemp()
        path = os.path.join(directory, "events.jsonl")
        for name, rows in ((path, today), (path + ".1", yesterday)):
            if rows is None:
                continue
            with open(name, "w") as handle:
                for row in rows:
                    handle.write(json.dumps(row) + "\n")
        return path

    def test_a_run_that_outlived_a_rotation_is_read_from_both_files(self):
        path = self.write(
            today=[ev(400, "working", tick="50060"), ev(460, "working", tick="50360")],
            yesterday=[ev(0, "started"), ev(1, "joined", peer_id=PEER), ev(6, "took-over")],
        )
        events = chaos.read_events(path)
        owns, peers, _, _ = chaos.situation(events, T0 + 500_000)
        self.assertTrue(owns, "it was running the job")
        self.assertEqual(peers, 1, "its peer joined before the rotation")
        self.assertIsNotNone(chaos.decide(events, T0 + 500_000)[0])

    def test_a_working_line_after_a_yield_does_not_prove_ownership(self):
        # diamond, 2026-09-16: took over at 00:25:25, yielded at 00:25:56, and
        # wrote a working line at 00:26:42 covering ticks done before the yield.
        # Thirteen hours later chaos killed it as though it held the job.
        events = [
            ev(0, "started"), ev(1, "joined", peer_id=PEER),
            ev(25 * 60 + 25, "took-over", tick="49344"),
            ev(25 * 60 + 42, "working", tick="49429"),
            ev(25 * 60 + 56, "yielded", label="riverboat"),
            ev(26 * 60 + 42, "working", tick="49740"),
        ]
        self.assertFalse(chaos.situation(events, T0 + 27 * 60_000)[0], "it had yielded")
        self.assertIsNone(chaos.decide(events, T0 + 13 * 3600_000)[0])

    def test_ownership_implied_by_a_working_line_goes_stale(self):
        # The owner writes one a minute. An hour of silence is not ownership.
        events = [ev(0, "joined", peer_id=PEER), ev(60, "working", tick="9")]
        self.assertTrue(chaos.situation(events, T0 + 120_000)[0])
        self.assertFalse(chaos.situation(events, T0 + 3600_000)[0])

    def test_a_claim_after_a_yield_is_ownership_again(self):
        events = [
            ev(0, "started"), ev(1, "joined", peer_id=PEER),
            ev(10, "yielded", label="riverboat"), ev(300, "took-over", tick="5"),
        ]
        self.assertTrue(chaos.situation(events, T0 + 3600_000)[0])

    def test_working_alone_proves_ownership(self):
        # Even with yesterday's file gone, a node writing `working` is the one
        # advancing the job — no other node writes it.
        path = self.write(today=[ev(400, "joined", peer_id=PEER), ev(460, "working", tick="9")])
        self.assertTrue(chaos.situation(chaos.read_events(path), T0 + 500_000)[0])

    def test_a_standby_is_still_not_a_target_after_a_rotation(self):
        path = self.write(
            today=[ev(400, "job-owner", label="diamond"), ev(460, "rtt")],
            yesterday=[ev(0, "started"), ev(1, "joined", peer_id=PEER), ev(6, "took-over")],
        )
        self.assertIsNone(chaos.decide(chaos.read_events(path), T0 + 500_000)[0])


class ReadEvents(unittest.TestCase):
    def test_only_the_current_run_is_read(self):
        # Ownership from yesterday's run must not make today's standby a target.
        lines = [
            ev(0, "started"), ev(1, "joined", peer_id=PEER), ev(6, "claimed"),
            ev(100, "started"), ev(101, "job-owner"),
        ]
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as handle:
            handle.write("not json\n")
            for line in lines:
                handle.write(json.dumps(line) + "\n")
            path = handle.name
        try:
            events = chaos.read_events(path)
        finally:
            os.remove(path)
        self.assertEqual([e["event"] for e in events], ["started", "job-owner"])
        self.assertFalse(chaos.situation(events, T0 + 400_000)[0])

    def test_a_missing_log_is_no_events(self):
        self.assertEqual(chaos.read_events("/nonexistent/events.jsonl"), [])


if __name__ == "__main__":
    unittest.main()
