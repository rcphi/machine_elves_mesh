#!/usr/bin/env python3
"""Tests for summarise.py. `python3 provision/test_summarise.py`"""
import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import summarise  # noqa: E402


def connectivity(label, ts, lan_ip, mapped_port, local_port=None, candidate=False, tag="nat-endpoint-independent"):
    v4 = {"tag": tag, "local": lan_ip,
          "observers": [{"server": "a", "mapped": f"198.51.100.4:{mapped_port}"},
                        {"server": "b", "mapped": f"198.51.100.4:{mapped_port}"}],
          "errors": []}
    if local_port is not None:
        v4["local_port"] = local_port
    return {"label": label, "ts_unix": ts, "ts_utc": "t", "test": "connectivity",
            "ipv4": v4, "ipv6": {"tag": "unreachable", "observers": []},
            "relay_candidate": candidate}


def lifetime(label, ts, idle, outcome, first="198.51.100.4:10809", local_port=None, note=None):
    record = {"label": label, "ts_unix": ts, "test": "mapping-lifetime", "idle_seconds": idle,
              "first": first, "second": first, "outcome": outcome, "note": note}
    if local_port is not None:
        record["local_port"] = local_port
    return record


def inbound(label, ts, idle, outcome):
    return {"label": label, "ts_unix": ts, "test": "mapping-inbound", "idle_seconds": idle,
            "outcome": outcome, "note": None}


HOME = "192.168.50.48"
FRIEND = "192.168.1.125"


class WhichResultsProveAnything(unittest.TestCase):
    def tally(self, mapping, conn):
        return summarise.tally(summarise.place(mapping, conn))

    def test_survival_on_a_randomising_router_counts(self):
        # A carrier NAT: the probe binds 41999 and gets another port back.
        conn = [connectivity("yoinkerz", 1000, "172.20.10.3", 10809),
                connectivity("yoinkerz", 2800, "172.20.10.3", 22113)]
        got = self.tally([lifetime("yoinkerz", 1100, 120, "survived")], conn)
        self.assertEqual(got["yoinkerz on 172.20.10.0/24"][120]["survived"], 1)

    def test_a_port_clash_does_not_make_a_preserving_router_randomise(self):
        # Home: 41999 kept almost always; once bumped because riverboat held
        # a mapping from the same local port. The nearest reading is the clash.
        conn = [connectivity("diamond", t, HOME, 41999) for t in (0, 1800, 3600)]
        conn.append(connectivity("diamond", 5400, HOME, 10809))
        got = self.tally([lifetime("diamond", 5450, 600, "survived")], conn)
        site = got["diamond on 192.168.50.0/24"][600]
        self.assertEqual((site["survived"], site["unproven"]), (0, 1))

    def test_survival_on_a_preserving_router_is_unproven(self):
        # The friend's house: 41999 in, 41999 out. An expired mapping would
        # come back on the same port, so "unchanged" proves nothing.
        conn = [connectivity("diamond", 1000, FRIEND, 41999)]
        got = self.tally([lifetime("diamond", 1100, 600, "survived", first="198.51.100.4:39615")], conn)
        site = got["diamond on 192.168.1.0/24"][600]
        self.assertEqual((site["survived"], site["unproven"]), (0, 1))

    def test_a_record_that_names_its_own_port_is_judged_by_it(self):
        got = self.tally([lifetime("x", 1, 60, "survived", first="198.51.100.4:5000", local_port=5000)], [])
        self.assertEqual(got["x"][60]["unproven"], 1)
        got = self.tally([lifetime("x", 1, 60, "survived", first="198.51.100.4:6000", local_port=5000)], [])
        self.assertEqual(got["x"][60]["survived"], 1)

    def test_the_probes_own_preserving_verdict_is_unproven_not_inconclusive(self):
        record = lifetime("x", 1, 60, "inconclusive",
                          note="the router preserves port numbers, so an expired mapping would come back")
        self.assertEqual(self.tally([record], [])["x"][60]["unproven"], 1)

    def test_expiry_counts_whatever_the_router_does(self):
        conn = [connectivity("diamond", 1000, FRIEND, 41999)]
        got = self.tally([lifetime("diamond", 1100, 300, "expired")], conn)
        self.assertEqual(got["diamond on 192.168.1.0/24"][300]["expired"], 1)

    def test_inbound_results_count_both_ways(self):
        conn = [connectivity("diamond", 1000, FRIEND, 41999)]
        got = self.tally([inbound("diamond", 1100, 120, "survived"),
                          inbound("diamond", 1200, 300, "expired")], conn)
        site = got["diamond on 192.168.1.0/24"]
        self.assertEqual((site[120]["survived"], site[300]["expired"]), (1, 1))

    def test_one_machine_on_two_networks_is_two_sites(self):
        conn = [connectivity("diamond", 1000, HOME, 10809),
                connectivity("diamond", 9_000_000, FRIEND, 41999)]
        got = self.tally([lifetime("diamond", 1100, 60, "survived"),
                          lifetime("diamond", 9_000_100, 60, "survived")], conn)
        self.assertEqual(set(got), {"diamond on 192.168.50.0/24", "diamond on 192.168.1.0/24"})

    def test_with_no_nearby_reading_survival_is_not_assumed(self):
        conn = [connectivity("diamond", 1000, HOME, 10809)]
        got = self.tally([lifetime("diamond", 1000 + 3 * 3600, 60, "survived")], conn)
        self.assertEqual(got["diamond"][60]["unproven"], 1)


class TheKeepaliveRecommendation(unittest.TestCase):
    def test_a_site_that_only_ever_expired_blocks_a_recommendation(self):
        # The review's reproduction (finding 12): one machine survives 600 s,
        # another expires at 15 s. The old code recommended 300 s.
        by_site = {"a": {600: {"survived": 1}}, "b": {15: {"expired": 1}}}
        interval, missing = summarise.keepalive(by_site)
        self.assertIsNone(interval)
        self.assertEqual(missing, ["b"])

    def test_inconclusive_or_unproven_only_is_no_evidence(self):
        by_site = {"a": {600: {"survived": 3}}, "b": {60: {"unproven": 40, "inconclusive": 2}}}
        self.assertEqual(summarise.keepalive(by_site), (None, ["b"]))

    def test_no_floor_erases_the_margin(self):
        by_site = {"a": {15: {"survived": 5}, 30: {"expired": 1}}}
        self.assertEqual(summarise.keepalive(by_site), (7, []))

    def test_the_worst_site_sets_it(self):
        by_site = {"a": {600: {"survived": 1}},
                   "b": {60: {"survived": 2}, 120: {"survived": 1}, 300: {"expired": 1}}}
        self.assertEqual(summarise.keepalive(by_site), (60, []))

    def test_an_expiry_below_a_survival_caps_safety_at_the_shorter(self):
        intervals = {30: {"survived": 2}, 60: {"expired": 1}, 120: {"survived": 1}}
        self.assertEqual(summarise.safe_idle(intervals), (30, 60))


class TheWholeReport(unittest.TestCase):
    def run_on(self, records):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            for r in records:
                f.write(json.dumps(r) + "\n")
        try:
            out = subprocess.run([sys.executable, "-I", os.path.join(HERE, "summarise.py"), f.name],
                                 capture_output=True, text=True, check=True).stdout
        finally:
            os.unlink(f.name)
        return out

    def test_a_candidate_is_not_reported_as_accepting_connections(self):
        out = self.run_on([connectivity("a", 1, HOME, 10809, candidate=True, tag="public")])
        self.assertIn("relay candidates (STUN only):        a", out)
        self.assertNotIn("can accept incoming", out)

    def test_no_stop_verdict_from_stun_alone(self):
        # Finding 13: three endpoint-independent machines with no candidate
        # used to "meet the stop criterion".
        out = self.run_on([connectivity(n, 1, ip, 10809) for n, ip in
                           (("a", "10.0.0.2"), ("b", "10.0.1.2"), ("c", "10.0.2.2"))])
        self.assertNotIn("meets the stop criterion", out)
        self.assertNotIn("Proceed", out)

    def test_the_report_runs_with_mapping_records_of_both_kinds(self):
        out = self.run_on([connectivity("diamond", 1000, FRIEND, 41999),
                           lifetime("diamond", 1100, 60, "survived", first="198.51.100.4:41000"),
                           inbound("diamond", 1200, 60, "survived")])
        self.assertIn("diamond on 192.168.1.0/24", out)
        self.assertIn("unproven: router preserves ports", out)


if __name__ == "__main__":
    unittest.main()
