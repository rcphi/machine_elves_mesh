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


MODEM = "/org/freedesktop/ModemManager1/Modem/2"


class Box:
    """A fake `run`. Answers by command prefix, records every call."""

    def __init__(self, connected_after_connect=True, modem=True, already_connected=False):
        self.calls, self.tolerant = [], []
        self.connected = already_connected
        self.connected_after_connect = connected_after_connect
        self.modem = modem

    def __call__(self, cmd, tolerate_exists=False):
        self.calls.append(cmd)
        if tolerate_exists:
            self.tolerant.append(cmd)
        key = " ".join(cmd)
        if key == "mmcli -L -K":
            return f"modem-list.length : 1\nmodem-list.value[1] : {MODEM}\n" if self.modem \
                else "modem-list.length : 0\n"
        if key == f"mmcli -m {MODEM} -K":
            return ("modem.generic.bearers.length : 1\n"
                    "modem.generic.bearers.value[1] : /org/freedesktop/ModemManager1/Bearer/3\n"
                    if self.connected else "modem.generic.bearers : --\n")
        if key.startswith(f"mmcli -m {MODEM} --simple-connect="):
            self.connected = self.connected_after_connect
            return ""
        if key.startswith("mmcli -b "):
            return BEARER_CONNECTED
        if key.startswith("ip -j -4 addr show dev "):
            return "[]"
        return ""

    def index(self, prefix):
        return next(i for i, c in enumerate(self.calls) if c[:len(prefix)] == prefix)


class TheRunner(unittest.TestCase):
    def test_fence_only_runs_the_fence_and_nothing_else(self):
        box = Box()
        self.assertEqual(link.ensure(box, 997, "vzwinternet", fence_only=True, say=lambda m: None), 0)
        self.assertEqual(box.calls, link.fence_commands(997))

    def test_every_fence_command_tolerates_already_existing(self):
        box = Box()
        link.ensure(box, 997, "vzwinternet", fence_only=True, say=lambda m: None)
        self.assertEqual(box.tolerant, link.fence_commands(997))

    def test_the_fence_goes_up_before_the_modem_is_touched(self):
        box = Box()
        link.ensure(box, 997, "vzwinternet", say=lambda m: None)
        self.assertLess(box.index(["ip", "rule", "add"]), box.index(["mmcli"]))

    def test_no_modem_leaves_the_fence_and_succeeds(self):
        box = Box(modem=False)
        self.assertEqual(link.ensure(box, 997, "vzwinternet", say=lambda m: None), 0)
        self.assertFalse([c for c in box.calls if c[:2] == ["ip", "addr"]])

    def test_uses_the_listed_modem_path_not_an_index(self):
        box = Box()
        link.ensure(box, 997, "vzwinternet", say=lambda m: None)
        self.assertIn(["mmcli", "-m", MODEM, "-K"], box.calls)
        self.assertFalse([c for c in box.calls if c[:3] == ["mmcli", "-m", "0"]])

    def test_a_disconnected_modem_is_connected_with_the_apn(self):
        box = Box()
        link.ensure(box, 997, "vzwinternet", say=lambda m: None)
        self.assertIn(["mmcli", "-m", MODEM, "--simple-connect=apn=vzwinternet,ip-type=ipv4"], box.calls)
        self.assertIn(["ip", "route", "replace", "default", "dev", "wwp0s20f0u8i5",
                       "metric", "100", "table", "4302"], box.calls)

    def test_an_already_connected_modem_is_not_reconnected(self):
        box = Box(already_connected=True)
        link.ensure(box, 997, "vzwinternet", say=lambda m: None)
        self.assertFalse([c for c in box.calls if "--simple-connect" in " ".join(c)])

    def test_a_connect_that_yields_no_link_is_a_failure(self):
        box = Box(connected_after_connect=False)
        self.assertEqual(link.ensure(box, 997, "vzwinternet", say=lambda m: None), 1)

    def test_no_user_means_no_commands_and_a_failure(self):
        box = Box()
        self.assertNotEqual(link.main(["x"], user="no-such-user-here", run=box), 0)
        self.assertEqual(box.calls, [])


class RunningCommands(unittest.TestCase):
    def test_already_exists_is_success_when_asked(self):
        out = link.sh(["sh", "-c", "echo 'RTNETLINK answers: File exists' >&2; exit 2"],
                      tolerate_exists=True)
        self.assertEqual(out, "")

    def test_already_exists_is_a_failure_otherwise(self):
        with self.assertRaises(link.Failed):
            link.sh(["sh", "-c", "echo 'RTNETLINK answers: File exists' >&2; exit 2"])

    def test_other_failures_are_never_tolerated(self):
        with self.assertRaises(link.Failed):
            link.sh(["sh", "-c", "echo 'Operation not permitted' >&2; exit 2"], tolerate_exists=True)


class Config(unittest.TestCase):
    def test_keys_values_comments_and_blanks(self):
        self.assertEqual(link.read_config("# c\n\nAPN=vzwinternet\nHOME_PORT=2302\n"),
                         {"APN": "vzwinternet", "HOME_PORT": "2302"})


if __name__ == "__main__":
    unittest.main()
