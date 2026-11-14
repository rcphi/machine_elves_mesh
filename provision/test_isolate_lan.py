#!/usr/bin/env python3
"""Tests for isolate-lan's decisions. `python3 provision/test_isolate_lan.py`

The rules it applies are exercised against real iptables by
isolate-lan-rig.sh; here, only the plan and the reading of counters.
"""
import importlib.machinery
import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
loader = importlib.machinery.SourceFileLoader("isolate_lan", os.path.join(HERE, "isolate-lan"))
spec = importlib.util.spec_from_loader("isolate_lan", loader)
iso = importlib.util.module_from_spec(spec)
loader.exec_module(iso)

# diamond at the friend's house, 2026-11-14, from `ip -j`.
DEFAULT = [{"dst": "default", "gateway": "192.168.1.1", "dev": "eno1", "protocol": "dhcp",
            "prefsrc": "192.168.1.125", "metric": 100, "flags": []}]
ROUTES4 = [{"dst": "192.168.1.0/24", "protocol": "kernel", "scope": "link", "prefsrc": "192.168.1.125"},
           {"dst": "192.168.1.1", "protocol": "dhcp", "scope": "link", "metric": 100}]
ROUTES6 = [{"dst": "fd34:84ae:2552:4269::/64", "protocol": "ra", "metric": 100},
           {"dst": "fd5c:2fa1:8f9d::/64", "gateway": "fe80::63:e393:a982:38d7", "protocol": "ra"},
           {"dst": "fe80::/64", "protocol": "kernel", "metric": 256}]


class ThePlan(unittest.TestCase):
    def setUp(self):
        self.p = iso.plan(DEFAULT, ROUTES4, ROUTES6)

    def test_the_lan_is_the_default_routes_interface(self):
        self.assertEqual((self.p["iface"], self.p["gateway"]), ("eno1", "192.168.1.1"))

    def test_ipv6_is_covered_now(self):
        # The IPv4-only version left both of the friend's unique-local
        # prefixes, and every link-local neighbour, reachable.
        for net in ("fc00::/7", "fe80::/10"):
            self.assertIn(net, self.p["v6"])

    def test_multicast_discovery_is_covered_in_both_families(self):
        self.assertIn("224.0.0.0/4", self.p["v4"])
        self.assertIn("ff02::fb/128", self.p["v6"])
        self.assertIn("ff02::1:3/128", self.p["v6"])

    def test_dhcpv6_and_neighbour_discovery_are_not_blocked(self):
        # ufw's before-rules let NDP, RS and MLD out; DHCPv6 is a plain UDP
        # send to ff02::1:2, which must not be caught by anything here.
        import ipaddress
        dhcp6 = ipaddress.ip_address("ff02::1:2")
        self.assertFalse(any(dhcp6 in ipaddress.ip_network(n) for n in self.p["v6"]))

    def test_a_lan_outside_the_private_ranges_is_still_covered(self):
        odd = [{"dst": "100.70.12.0/24", "protocol": "kernel", "scope": "link"}]
        self.assertIn("100.70.12.0/24", iso.plan(DEFAULT, odd, [])["v4"])

    def test_a_household_route_through_another_router_is_covered(self):
        p = iso.plan(DEFAULT, ROUTES4, [{"dst": "2001:db8:5::/64", "gateway": "fe80::9"}])
        self.assertIn("2001:db8:5::/64", p["v6"])

    def test_nothing_is_added_twice_or_for_the_gateway(self):
        self.assertEqual(len(self.p["v4"]), len(set(self.p["v4"])))
        self.assertNotIn("192.168.1.1/32", self.p["v4"])
        self.assertNotIn("192.168.1.0/24", self.p["v4"])  # inside 192.168.0.0/16

    def test_no_default_route_is_a_refusal_not_a_guess(self):
        with self.assertRaises(iso.Failed):
            iso.plan([], ROUTES4, ROUTES6)

    def test_the_lte_link_is_never_part_of_the_plan(self):
        # The modem's carrier address is 100.64/10, and the back channel's
        # tunnel user routes through it; nothing here may name it.
        self.assertFalse([n for n in self.p["v4"] if n.startswith("100.")])


class TheCommands(unittest.TestCase):
    def setUp(self):
        self.p = iso.plan(DEFAULT, ROUTES4, ROUTES6)

    def test_the_gateway_is_allowed_first_so_it_outranks_the_rejections(self):
        self.assertEqual(iso.apply_commands(self.p)[0], ["ufw", "insert", "1", "allow", "out", "to", "192.168.1.1"])

    def test_everything_else_is_rejected_not_dropped(self):
        for cmd in iso.apply_commands(self.p)[1:]:
            self.assertEqual(cmd[:4], ["ufw", "reject", "out", "to"])

    def test_removal_also_takes_out_the_ipv4_only_versions_deny_rules(self):
        removing = [" ".join(c) for c in iso.remove_commands(self.p)]
        self.assertIn("ufw --force delete deny out to 192.168.0.0/16", removing)
        self.assertIn("ufw --force delete reject out to fc00::/7", removing)
        self.assertIn("ufw --force delete allow out to 192.168.1.1", removing)


class TheCheck(unittest.TestCase):
    def test_test_packets_aim_inside_a_route_that_exists(self):
        p = iso.plan(DEFAULT, ROUTES4, ROUTES6)
        # fc00::2 has no route on a box without an IPv6 default; the friend's
        # own prefix does.
        self.assertEqual(p["targets"]["fc00::/7"], "fd34:84ae:2552:4269::2")
        self.assertEqual(p["targets"]["192.168.0.0/16"], "192.168.1.2")
        self.assertEqual(p["targets"]["ff02::fb/128"], "ff02::fb")

    def test_a_test_packet_never_aims_at_the_gateway(self):
        routes = [{"dst": "192.168.1.0/30", "protocol": "kernel"}]
        p = iso.plan([{"dst": "default", "gateway": "192.168.1.2", "dev": "eno1"}], routes, [])
        self.assertNotEqual(p["targets"]["192.168.0.0/16"], "192.168.1.2")

    def test_counters_are_read_from_real_iptables_output(self):
        listing = """Chain ufw-user-output (1 references)
    pkts      bytes target     prot opt in     out     source               destination
       4      240 ACCEPT     all  --  *      *       0.0.0.0/0            192.168.1.1
       2      120 REJECT     all  --  *      *       0.0.0.0/0            10.0.0.0/8           reject-with icmp-port-unreachable
       0        0 REJECT     all  --  *      *       0.0.0.0/0            224.0.0.0/4          reject-with icmp-port-unreachable
"""
        self.assertEqual(iso.parse_counters(listing), {"10.0.0.0/8": 2, "224.0.0.0/4": 0})

    def test_single_addresses_are_matched_as_iptables_prints_them(self):
        self.assertEqual(iso.canonical("ff02::fb/128"), "ff02::fb")
        self.assertEqual(iso.canonical("fc00::/7"), "fc00::/7")


if __name__ == "__main__":
    unittest.main()
