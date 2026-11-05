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


class TheFence(unittest.TestCase):
    def test_the_unreachable_floor_goes_in_before_the_rule_that_uses_it(self):
        cmds = link.fence_commands(997)
        floor = cmds.index(["ip", "route", "replace", "unreachable", "default",
                            "metric", "65535", "table", "4302"])
        rule = cmds.index(["ip", "rule", "add", "uidrange", "997-997",
                           "lookup", "4302", "priority", "1000"])
        self.assertLess(floor, rule)

    def test_ipv6_is_prohibited_for_the_tunnel_user(self):
        self.assertIn(["ip", "-6", "rule", "add", "uidrange", "997-997",
                       "prohibit", "priority", "1000"], link.fence_commands(997))

    def test_the_fence_never_touches_the_main_table(self):
        for cmd in link.fence_commands(997):
            self.assertNotIn("main", cmd)
            if cmd[:3] == ["ip", "route", "replace"]:
                self.assertIn("table", cmd)


LINK = link.Link("wwp0s20f0u8i5", "100.66.12.9", "29", 1500)
IP_ADDR_JSON = json.dumps([{"ifname": "wwp0s20f0u8i5", "addr_info": [
    {"family": "inet", "local": "100.66.12.9", "prefixlen": 29}]}])


class TheLink(unittest.TestCase):
    def test_addresses_are_read_from_ip_json(self):
        self.assertEqual(link.parse_addresses(IP_ADDR_JSON), ["100.66.12.9/29"])

    def test_an_interface_with_no_address(self):
        self.assertEqual(link.parse_addresses("[]"), [])
        self.assertEqual(link.parse_addresses(""), [])

    def test_the_address_adds_no_route_to_the_main_table(self):
        cmds = link.link_commands(LINK, [])
        self.assertIn(["ip", "addr", "replace", "100.66.12.9/29",
                       "dev", "wwp0s20f0u8i5", "noprefixroute"], cmds)

    def test_the_default_route_lives_in_table_4302_only(self):
        cmds = link.link_commands(LINK, [])
        self.assertIn(["ip", "route", "replace", "default", "dev", "wwp0s20f0u8i5",
                       "metric", "100", "table", "4302"], cmds)
        for cmd in cmds:
            if cmd[:2] == ["ip", "route"]:
                self.assertEqual(cmd[-2:], ["table", "4302"])

    def test_ipv6_is_switched_off_on_the_lte_interface(self):
        self.assertEqual(link.link_commands(LINK, [])[0],
                         ["sysctl", "-qw", "net.ipv6.conf.wwp0s20f0u8i5.disable_ipv6=1"])

    def test_the_interface_comes_up_at_the_bearers_mtu(self):
        self.assertIn(["ip", "link", "set", "dev", "wwp0s20f0u8i5", "up", "mtu", "1500"],
                      link.link_commands(LINK, []))

    def test_a_new_lease_removes_the_old_address(self):
        cmds = link.link_commands(LINK, ["100.70.1.2/30"])
        self.assertIn(["ip", "addr", "del", "100.70.1.2/30", "dev", "wwp0s20f0u8i5"], cmds)

    def test_the_current_address_is_not_removed(self):
        cmds = link.link_commands(LINK, ["100.66.12.9/29"])
        self.assertFalse([c for c in cmds if c[:3] == ["ip", "addr", "del"]])


if __name__ == "__main__":
    unittest.main()
