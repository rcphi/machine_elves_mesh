#!/usr/bin/env python3
"""Tests for backchannel/mesh-backchannel-link. `python3 -m unittest provision/test_backchannel_link.py`"""
import importlib.machinery
import importlib.util
import json
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "backchannel", "fixtures")
loader = importlib.machinery.SourceFileLoader(
    "backchannel_link", os.path.join(HERE, "backchannel", "mesh-backchannel-link"))
spec = importlib.util.spec_from_loader("backchannel_link", loader)
link = importlib.util.module_from_spec(spec)
loader.exec_module(link)


def fixture(name):
    with open(os.path.join(FIXTURES, name)) as f:
        return f.read()


BEARER_CONNECTED = """\
bearer.dbus-path                 : /org/freedesktop/ModemManager1/Bearer/3
bearer.status.connected          : yes
bearer.status.interface          : wwp0s20f0u8i5
bearer.ipv4-config.method        : static
bearer.ipv4-config.address       : 100.66.12.9
bearer.ipv4-config.prefix        : 29
bearer.ipv4-config.gateway       : 100.66.12.10
bearer.ipv4-config.dns.length    : 2
bearer.ipv4-config.dns.value[1]  : 198.224.166.135
bearer.ipv4-config.dns.value[2]  : 198.224.167.135
bearer.ipv4-config.mtu           : 1500
"""


class ParsingTheModemsOwnWords(unittest.TestCase):
    def test_lists_are_collected_in_order(self):
        kv = link.parse_keyed(BEARER_CONNECTED)
        self.assertEqual(kv["bearer.ipv4-config.dns"], ["198.224.166.135", "198.224.167.135"])

    def test_double_dash_means_empty(self):
        kv = link.parse_keyed("modem.generic.bearers : --\n")
        self.assertEqual(link.bearer_paths(kv), [])

    def test_a_value_containing_colons_survives(self):
        kv = link.parse_keyed("bearer.ipv6-config.address : fe80::1\n")
        self.assertEqual(kv["bearer.ipv6-config.address"], "fe80::1")

    def test_the_real_modem_list_names_one_modem(self):
        path = link.modem_path(link.parse_keyed(fixture("modem-list.txt")))
        self.assertTrue(path.startswith("/org/freedesktop/ModemManager1/Modem/"))

    def test_no_modem_means_no_path(self):
        self.assertIsNone(link.modem_path(link.parse_keyed(fixture("modem-list-empty.txt"))))

    def test_a_disabled_modem_has_no_bearers(self):
        self.assertEqual(link.bearer_paths(link.parse_keyed(fixture("modem-no-bearers.txt"))), [])

    def test_a_connected_modem_lists_its_bearer(self):
        paths = link.bearer_paths(link.parse_keyed(fixture("modem-connected.txt")))
        self.assertEqual(len(paths), 1)
        self.assertTrue(paths[0].startswith("/org/freedesktop/ModemManager1/Bearer/"))


class ReadingABearer(unittest.TestCase):
    def test_a_connected_bearer_becomes_a_link(self):
        got = link.connected_link(link.parse_keyed(BEARER_CONNECTED))
        self.assertEqual(got, link.Link("wwp0s20f0u8i5", "100.66.12.9", "29", 1500))

    def test_the_real_connected_bearer_becomes_a_link(self):
        got = link.connected_link(link.parse_keyed(fixture("bearer-connected.txt")))
        self.assertIsNotNone(got, "key names in connected_link must match the fixture")
        self.assertTrue(got.interface.startswith("wwp"))
        self.assertGreater(got.mtu, 0)

    def test_a_disconnected_bearer_is_no_link(self):
        self.assertIsNone(link.connected_link(link.parse_keyed(fixture("bearer-disconnected.txt"))))

    def test_connected_without_an_address_is_no_link(self):
        text = BEARER_CONNECTED.replace("100.66.12.9", "--")
        self.assertIsNone(link.connected_link(link.parse_keyed(text)))

    def test_a_missing_mtu_falls_back_to_1428(self):
        text = BEARER_CONNECTED.replace(": 1500", ": --")
        self.assertEqual(link.connected_link(link.parse_keyed(text)).mtu, 1428)


if __name__ == "__main__":
    unittest.main()
