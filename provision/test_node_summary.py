#!/usr/bin/env python3
"""Tests for node-summary.py and the shared event descriptions.
`python3 provision/test_node_summary.py`"""
import gzip
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import node_events  # noqa: E402


def ev(ms_ago, kind, **fields):
    return {"ts_unix_ms": int(time.time() * 1000) - ms_ago, "node": "diamond", "event": kind, **fields}


def summarise(*files, args=()):
    """Writes {name: [events]} into a directory and runs the summary on it."""
    directory = tempfile.mkdtemp()
    for name, events in files:
        path = os.path.join(directory, name)
        body = "".join(json.dumps(e) + "\n" for e in events)
        if name.endswith(".gz"):
            with gzip.open(path, "wt") as handle:
                handle.write(body)
        else:
            with open(path, "w") as handle:
                handle.write(body)
    return subprocess.run([sys.executable, "-I", os.path.join(HERE, "node-summary.py"),
                           os.path.join(directory, "events.jsonl"), *args],
                          capture_output=True, text=True).stdout


class Work(unittest.TestCase):
    def test_a_working_node_reports_its_ticks_not_zero(self):
        # The review's reproduction of finding 20.
        out = summarise(("events.jsonl", [ev(120_000, "started"), ev(119_000, "job-loaded"),
                                          ev(60_000, "working", ticks="300", tick="300", ledger="120")]))
        self.assertIn("300 ticks run in this window, reaching tick 300", out)
        self.assertIn("120 things known to exist", out)
        self.assertNotIn("0 effects", out)

    def test_speculation_is_reported(self):
        out = summarise(("events.jsonl", [ev(120_000, "started"), ev(119_000, "job-loaded"),
                                          ev(100_000, "speculating", label="riverboat", silent_ms="500", tick="9"),
                                          ev(99_000, "speculation-abandoned", label="riverboat",
                                             cause="checkpoint", ticks_wasted="2")]))
        self.assertIn("suspicions        1 raised, 1 abandoned", out)
        self.assertIn("suspected riverboat after 500 ms", out)


class Rotations(unittest.TestCase):
    def test_a_start_in_an_old_gzipped_rotation_is_found(self):
        # Finding 15: only .1 was read, so a run older than two days lost its
        # start and was reported as partial.
        out = summarise(("events.jsonl", [ev(1_000, "working", ticks="300", tick="9000", ledger="5")]),
                        ("events.jsonl.1", [ev(90_000_000, "working", ticks="300", tick="8000")]),
                        ("events.jsonl.2.gz", [ev(180_000_000, "joined", label="riverboat")]),
                        ("events.jsonl.3.gz", [ev(270_000_000, "started"), ev(269_000_000, "job-loaded")]))
        self.assertIn("this run:", out)
        self.assertNotIn("PARTIAL", out)
        self.assertIn("peers joined      1", out)


class SharedDescriptions(unittest.TestCase):
    def test_every_event_the_watcher_and_summary_show_is_described_the_same_way(self):
        said = node_events.describe({"event": "took-over", "tick": "12", "previous": "diamond"})
        self.assertEqual(said, "TOOK OVER the job at tick 12 from diamond")

    def test_a_missing_field_is_a_question_mark_not_a_crash(self):
        self.assertEqual(node_events.describe({"event": "took-over", "tick": "12"}),
                         "TOOK OVER the job at tick 12 from ?")

    def test_the_steady_hum_is_not_described(self):
        for kind in ("working", "rtt", "dialling", "listening"):
            self.assertIsNone(node_events.describe({"event": kind}))


if __name__ == "__main__":
    unittest.main()
