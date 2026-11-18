"""Tests for link-summary.py: how links were made, and work computed twice."""
import importlib.machinery
import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
loader = importlib.machinery.SourceFileLoader("link_summary", os.path.join(HERE, "link-summary.py"))
spec = importlib.util.spec_from_loader("link_summary", loader)
links_tool = importlib.util.module_from_spec(spec)
loader.exec_module(links_tool)

_clock = [0]


def ev(node, event, **fields):
    _clock[0] += 1
    return {"ts_unix_ms": _clock[0], "node": node, "event": event,
            **{k: str(v) for k, v in fields.items()}}


class DuplicatedWork(unittest.TestCase):
    def test_run_f_part_two_shape_counts_the_overlap(self):
        # diamond owns throughout; yoinkerz takes over at 150 when it loses
        # sight of diamond, and yields at 190 when it hears it again.
        events = [
            ev("diamond", "claimed", tick=100),
            ev("yoinkerz", "took-over", tick=150),
            ev("yoinkerz", "working", tick=170),
            ev("yoinkerz", "yielded", our_tick=190, their_tick=191),
            ev("diamond", "working", tick=200),
        ]
        spans = links_tool.owner_spans(events)
        self.assertEqual(spans, {"diamond": [(100, 200)], "yoinkerz": [(150, 190)]})
        self.assertEqual(links_tool.duplicated_ticks(spans), 40)

    def test_one_owner_at_a_time_duplicates_nothing(self):
        events = [
            ev("a", "claimed", tick=0),
            ev("a", "working", tick=50),
            ev("a", "started"),  # killed and restarted: a's span ends at its last working line
            ev("b", "took-over", tick=52),
            ev("b", "working", tick=90),
        ]
        spans = links_tool.owner_spans(events)
        self.assertEqual(spans, {"a": [(0, 50)], "b": [(52, 90)]})
        self.assertEqual(links_tool.duplicated_ticks(spans), 0)

    def test_a_killed_owner_ends_at_its_last_working_line(self):
        events = [
            ev("a", "took-over", tick=10),
            ev("a", "working", tick=40),
            ev("a", "working", tick=70),
            ev("a", "started"),
            ev("a", "working", tick=999),  # not an owner any more: ignored
        ]
        self.assertEqual(links_tool.owner_spans(events), {"a": [(10, 70)]})


class Links(unittest.TestCase):
    def joined(self):
        return [ev("diamond", "joined", peer_id="P-Y", label="yoinkerz"),
                ev("yoinkerz", "joined", peer_id="P-D", label="diamond")]

    def test_a_direct_link_and_a_relayed_one_are_counted_apart(self):
        events = self.joined() + [
            ev("diamond", "connected", peer_id="P-Y", relayed="true"),
            ev("yoinkerz", "connected", peer_id="P-D", relayed="false"),
            ev("diamond", "connected", peer_id="P-Y", relayed="false"),
        ]
        pair = links_tool.links(events)[("diamond", "yoinkerz")]
        self.assertEqual((pair["direct"], pair["relayed"]), (2, 1))

    def test_punches_are_counted_per_pair_with_their_outcome(self):
        events = self.joined() + [
            ev("diamond", "punch-failed", peer_id="P-Y", label="yoinkerz", error="timeout"),
            ev("yoinkerz", "punch-failed", peer_id="P-D", label="diamond", error="timeout"),
            ev("diamond", "punch-succeeded", peer_id="P-Y", label="yoinkerz"),
        ]
        pair = links_tool.links(events)[("diamond", "yoinkerz")]
        self.assertEqual((pair["punches_tried"], pair["punches_ok"]), (3, 1))


if __name__ == "__main__":
    unittest.main()
