# LTE back channel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `diamond` a management-only SSH back channel over its LTE dongle, which reaches home through `riverboat`. Routing enforces it in both directions: the mesh can never use LTE, and management can never use the friend's ethernet.

**Architecture:** A root script, run at boot and every minute, connects the modem through ModemManager and puts the carrier's address on the LTE interface. It also maintains a "fence":
- the `backchannel` user's IPv4 traffic looks up routing table 4302 only;
- that table holds a default route out LTE, with an `unreachable` default beneath it;
- the user's IPv6 is prohibited.

A systemd unit, running as that user, holds `ssh -N -R` open to `riverboat` on home port 2302. On `riverboat`, the key can only listen on `localhost:2201`.

**Tech Stack:** Python 3 standard library (the link script), bash (install scripts), systemd units, iproute2, ModemManager's `mmcli`, OpenSSH 10.2.

**Spec:** `docs/superpowers/specs/2026-11-05-lte-backchannel-design.md`

## Global Constraints

- **No route is ever added to the main table.** That includes the `/prefix` route the kernel adds with an address, so addresses go on with `noprefixroute`.
- **The fence is installed before every start of the tunnel** (`ExecStartPre=+…--fence`). The `unreachable` route goes in before the rule that points at its table.
- Routing table `4302`, rule priority `1000`, LTE default metric `100`, unreachable metric `65535`.
- The user is `backchannel` on both boxes: a system user with home `/var/lib/backchannel`, shell `/usr/sbin/nologin`, and no password.
- Home `204.210.210.200`, external port `2302` → `riverboat:22`, listen port `2201` on riverboat's loopback.
- `APN=vzwinternet` (US Mobile on Verizon's network). The bearer is IPv4 only: `ip-type=ipv4`.
- If the bearer reports no MTU, use `1428`.
- **Read the modem path and interface name fresh on every run.** The modem's index changed from `Modem/0` to `Modem/2` when it re-enumerated on 2026-11-05, and `wwp0s20f0u8i5` encodes the USB port.
- Python standard library only, as `chaos.py` is. Tests are `unittest`, run from `provision/` with `python3 -m unittest discover -p 'test_*.py'`; the box has no pytest.
- **Privileged steps are run by the operator.** The agent can `ssh`/`scp` as `rpc` to `192.168.50.48` (diamond) and `192.168.50.128` (riverboat), but `sudo` there needs a password the agent does not have. Each operator step is given as one command to paste.
- Commit messages carry the reasoning and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **The modem re-enumerates between runs** (a new D-Bus index, possibly a new interface name). The script must use the path `mmcli -L` lists, not a fixed index. Pinned in Task 3: `test_uses_the_listed_modem_path_not_an_index`.
2. **Running every minute must not pile up rules.** `ip rule add` of an identical rule fails with `File exists` on this kernel, and that must count as success, not an error or a duplicate. Pinned in Task 4 (`sh` tolerates it, `ensure` asks for tolerance on every fence command). It is checked on the box in Task 8, where the rule count must still be 1 after two runs.
3. **The bearer reports no MTU** (`--`). Use 1428, never crash or set `mtu 0`. Pinned in Task 2: `test_a_missing_mtu_falls_back_to_1428`.
4. **The carrier assigns a different address on reconnect.** The stale address must be removed, or the box answers from two addresses and the route may use the dead one. Pinned in Task 3: `test_a_new_lease_removes_the_old_address`.
5. **The `backchannel` user is missing**, from a half-done install or a `--remove`. With no uid there is no fence, so the script must refuse loudly and run no command at all. The tunnel's `ExecStartPre` then fails, and the tunnel never starts unfenced. Pinned in Task 4: `test_no_user_means_no_commands_and_a_failure`.

---

## File Structure

| File | Responsibility |
|---|---|
| `provision/backchannel/eno1-dhcp.yaml` | *(exists, `c3d09d5`)* netplan for `eno1` on DHCP |
| `provision/backchannel/fixtures/*.txt` | real `mmcli -K` output from this modem, for tests |
| `provision/backchannel/mesh-backchannel-link` | the link + fence script: pure functions plus a thin runner |
| `provision/test_backchannel_link.py` | its tests (sits beside `test_chaos.py` so `discover` finds it) |
| `provision/backchannel/config` | default `/etc/mesh-backchannel/config` |
| `provision/backchannel/mesh-backchannel-link.service`, `.timer` | run the link script at boot and every 60 s |
| `provision/backchannel/mesh-backchannel.service.in` | the tunnel unit, with `@HOME_ADDR@` `@HOME_PORT@` `@LISTEN_PORT@` placeholders |
| `provision/backchannel/riverboat-host-key.pub` | riverboat's ed25519 host key, pinned by diamond |
| `provision/backchannel/40-backchannel.conf` | riverboat sshd drop-in limiting the user |
| `provision/backchannel/install-diamond.sh` | diamond: user, key, files, units; `--start`, `--remove` |
| `provision/backchannel/install-riverboat.sh` | riverboat: user, restricted key, sshd drop-in; `--remove` |

---

### Task 0: Put `eno1` on DHCP (operator)

The file is already staged at `~/backchannel/eno1-dhcp.yaml` on diamond (sha256 `5f0471b8…`).

- [ ] **Step 1: Operator looks at what is being replaced**

```bash
ssh -t rpc@192.168.50.48 'sudo cat /etc/netplan/00-installer-config.yaml'
```

Expected: only `eno1` is defined, with `192.168.50.48/24`. If anything else is defined (another interface, routes, or anything other than name servers), stop: the replacement would drop it.

- [ ] **Step 2: Operator applies it under a dead-man switch**

```bash
ssh -t rpc@192.168.50.48 'sudo cp -p /etc/netplan/00-installer-config.yaml /etc/netplan/00-installer-config.yaml.pre-dhcp \
 && sudo systemd-run --unit=netplan-undo --on-active=180 /bin/sh -c "cp -p /etc/netplan/00-installer-config.yaml.pre-dhcp /etc/netplan/00-installer-config.yaml && netplan apply" \
 && sudo install -m 0600 -o root -g root ~/backchannel/eno1-dhcp.yaml /etc/netplan/00-installer-config.yaml \
 && sudo netplan apply; echo applied'
```

The SSH session may drop for a moment. Either way, the old config comes back by itself 180 s later unless step 3 cancels it.

- [ ] **Step 3: Check it, then cancel the switch**

```bash
ssh rpc@192.168.50.48 'ip -br addr show eno1; ip route; networkctl status eno1 | grep -E "Address|Gateway|DHCP"'
```

Expected: `192.168.50.48` marked as DHCP-assigned, and the default route via `192.168.50.1`, `proto dhcp`. Then:

```bash
ssh -t rpc@192.168.50.48 'sudo systemctl stop netplan-undo.timer && echo kept'
```

If diamond does not answer on `.48`, wait 3 minutes and it will be back on the static config. Then check the router's DHCP reservation against diamond's MAC address, `88:a4:c2:30:77:92`.

---

### Task 1: Capture the modem's real output, and prove the APN (operator + agent)

**Files:**
- Create: `provision/backchannel/fixtures/modem-list.txt`, `modem-no-bearers.txt`, `modem-connected.txt`, `bearer-connected.txt`, `bearer-disconnected.txt`, `modem-list-empty.txt`

**Interfaces:**
- Produces: fixtures that Tasks 2–4 read by file name.

- [ ] **Step 1: Agent captures the disabled state**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision && mkdir -p backchannel/fixtures
ssh rpc@192.168.50.48 'mmcli -L -K' > backchannel/fixtures/modem-list.txt
M=$(awk -F': ' '/value\[1\]/{print $2}' backchannel/fixtures/modem-list.txt)
ssh rpc@192.168.50.48 "mmcli -m $M -K" > backchannel/fixtures/modem-no-bearers.txt
grep -E "generic.state |generic.bearers" backchannel/fixtures/modem-no-bearers.txt
```

Expected: `state : disabled` and `bearers : --`.

- [ ] **Step 2: Operator connects once by hand**

```bash
ssh -t rpc@192.168.50.48 'sudo mmcli -m any --simple-connect="apn=vzwinternet,ip-type=ipv4"'
```

Expected: `successfully connected the modem`. If it fails with an APN or authentication error, try again with `apn=VZWINTERNET`. If that also fails, look up the APN on US Mobile's own page (usmobile.com/apn-download), change `APN=` in `backchannel/config` (Task 5), and record the working value in the spec.

- [ ] **Step 3: Agent captures the connected state**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision
M=$(awk -F': ' '/value\[1\]/{print $2}' backchannel/fixtures/modem-list.txt)
ssh rpc@192.168.50.48 "mmcli -m $M -K" > backchannel/fixtures/modem-connected.txt
B=$(awk -F': ' '/generic.bearers.value\[1\]/{print $2}' backchannel/fixtures/modem-connected.txt)
ssh rpc@192.168.50.48 "mmcli -b $B -K" > backchannel/fixtures/bearer-connected.txt
grep -E "status.connected|status.interface|ipv4-config" backchannel/fixtures/bearer-connected.txt
```

Expected: `connected : yes`, an interface `wwp…`, and an IPv4 `address`, `prefix`, and probably `mtu`. **These key names are the authority for Task 2.** Note the exact spelling of each.

- [ ] **Step 4: Operator disconnects. Agent captures the bearer it leaves behind**

```bash
ssh -t rpc@192.168.50.48 'sudo mmcli -m any --simple-disconnect'
```

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision
B=$(awk -F': ' '/generic.bearers.value\[1\]/{print $2}' backchannel/fixtures/modem-connected.txt)
ssh rpc@192.168.50.48 "mmcli -b $B -K" > backchannel/fixtures/bearer-disconnected.txt 2>&1 || true
printf 'modem-list.length : 0\n' > backchannel/fixtures/modem-list-empty.txt
head -3 backchannel/fixtures/bearer-disconnected.txt
```

Expected: `connected : no`. If the bearer was deleted on disconnect, the file holds `mmcli`'s error instead; delete it and drop the test that reads it in Task 2. `modem-list-empty.txt` is written by hand because the modem cannot be unplugged remotely, so the commit message must say so.

- [ ] **Step 5: Commit**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh
git add provision/backchannel/fixtures
git commit -m "Capture the modem's own words before parsing them

Fixtures for the back channel's tests, taken from diamond's SIM7600G-H
rather than from documentation: disabled, connected through US Mobile
with apn=vzwinternet, and the bearer left after disconnecting.
modem-list-empty.txt is hand-written; the modem cannot be unplugged
remotely. The connect also settles the spec's open question about the APN.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Parse `mmcli -K` and read a bearer

**Files:**
- Create: `provision/backchannel/mesh-backchannel-link`
- Test: `provision/test_backchannel_link.py`

**Interfaces:**
- Consumes: Task 1's fixtures.
- Produces:
  - `parse_keyed(text: str) -> dict[str, str | list[str]]`
  - `as_list(value) -> list[str]`
  - `modem_path(modem_list: dict) -> str | None`
  - `bearer_paths(modem: dict) -> list[str]`
  - `Link(interface: str, address: str, prefix: str, mtu: int)`, a frozen dataclass
  - `connected_link(bearer: dict) -> Link | None`
  - constants `DEFAULT_MTU = 1428`, `TABLE = "4302"`, `PRIORITY = "1000"`, `ROUTE_METRIC = "100"`, `UNREACHABLE_METRIC = "65535"`, `USER = "backchannel"`, `CONFIG = "/etc/mesh-backchannel/config"`

- [ ] **Step 1: Write the failing tests**

`provision/test_backchannel_link.py`:

```python
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd provision && python3 -m unittest test_backchannel_link -v`
Expected: an error loading the module, `FileNotFoundError … mesh-backchannel-link`.

- [ ] **Step 3: Write the minimal implementation**

`provision/backchannel/mesh-backchannel-link` (then `chmod +x`):

```python
#!/usr/bin/env python3
"""Keeps diamond's LTE management link up, and fences the tunnel user onto it.

    mesh-backchannel-link           fence, then connect the modem and apply its address
    mesh-backchannel-link --fence   fence only: run before every start of the tunnel

The fence is what makes the back channel management-only. The `backchannel`
user's IPv4 traffic looks up table 4302 and nothing else; that table holds a
default route out the LTE interface and, beneath it, an `unreachable` default,
so a lookup with LTE down fails here instead of falling through to the main
table and leaving by eno1. Its IPv6 is prohibited outright. Nothing is ever
added to the main table, so no other process can reach LTE.

Design: docs/superpowers/specs/2026-11-05-lte-backchannel-design.md
"""
import re
from dataclasses import dataclass

USER = "backchannel"
CONFIG = "/etc/mesh-backchannel/config"
TABLE = "4302"
PRIORITY = "1000"
ROUTE_METRIC = "100"
UNREACHABLE_METRIC = "65535"
# LTE paths commonly carry less than 1500; too large a value shows up as an
# SSH session that hangs on large output, which is a miserable thing to debug
# over the very link that is misbehaving.
DEFAULT_MTU = 1428

LIST_ITEM = re.compile(r"^(.*)\.value\[(\d+)\]$")


def parse_keyed(text):
    """`mmcli -K` output as {key: value}. A `x.length` / `x.value[n]` family
    becomes a list under `x`; mmcli prints `--` for empty."""
    out, lists = {}, {}
    for line in text.splitlines():
        key, sep, value = line.partition(":")
        if not sep:
            continue
        key, value = key.strip(), value.strip()
        if value == "--":
            value = ""
        item = LIST_ITEM.match(key)
        if item:
            lists.setdefault(item.group(1), []).append((int(item.group(2)), value))
        elif key.endswith(".length"):
            lists.setdefault(key[: -len(".length")], [])
        else:
            out[key] = value
    for key, items in lists.items():
        out[key] = [value for _, value in sorted(items)]
    return out


def as_list(value):
    """mmcli prints an empty list as a scalar `--`, which parses to ""."""
    if isinstance(value, list):
        return [v for v in value if v]
    return [value] if value else []


def modem_path(modem_list):
    paths = as_list(modem_list.get("modem-list", []))
    return paths[0] if paths else None


def bearer_paths(modem):
    return as_list(modem.get("modem.generic.bearers", []))


@dataclass(frozen=True)
class Link:
    interface: str
    address: str
    prefix: str
    mtu: int


def connected_link(bearer):
    if bearer.get("bearer.status.connected") != "yes":
        return None
    interface = bearer.get("bearer.status.interface", "")
    address = bearer.get("bearer.ipv4-config.address", "")
    prefix = bearer.get("bearer.ipv4-config.prefix", "")
    if not (interface and address and prefix):
        return None
    mtu = bearer.get("bearer.ipv4-config.mtu", "")
    return Link(interface, address, prefix, int(mtu) if mtu.isdigit() else DEFAULT_MTU)
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd provision && python3 -m unittest test_backchannel_link -v`
Expected: 12 tests, OK.

`test_the_real_connected_bearer_becomes_a_link` may fail. If it does, a key name in `connected_link` differs from the one in `bearer-connected.txt`. **The fixture is the authority**: change the key in `connected_link` and in `BEARER_CONNECTED` to match it, and rerun.

- [ ] **Step 5: Run the whole provision suite**

Run: `cd provision && python3 -m unittest discover -p 'test_*.py'`
Expected: 71 tests, OK (59 existing plus 12 new).

- [ ] **Step 6: Commit**

```bash
git add provision/backchannel/mesh-backchannel-link provision/test_backchannel_link.py
git commit -m "Read the modem's state without trusting its index

…reasoning: the modem was Modem/0 at 13:00 and Modem/2 after it
re-enumerated at 16:09, so paths come from mmcli -L every time; mmcli
prints an empty list as a scalar '--'; a missing MTU falls back to 1428
rather than 0.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Build the fence and the link as commands

**Files:**
- Modify: `provision/backchannel/mesh-backchannel-link` (append)
- Test: `provision/test_backchannel_link.py` (append classes before `if __name__`)

**Interfaces:**
- Consumes: `Link`, the constants.
- Produces:
  - `fence_commands(uid: int) -> list[list[str]]`
  - `parse_addresses(ip_json: str) -> list[str]`, which turns `ip -j -4 addr show dev IF` into `["a.b.c.d/p", …]`
  - `link_commands(link: Link, current: list[str]) -> list[list[str]]`

- [ ] **Step 1: Write the failing tests**

```python
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd provision && python3 -m unittest test_backchannel_link -v`
Expected: `AttributeError: module 'backchannel_link' has no attribute 'fence_commands'` (and the same for `parse_addresses` / `link_commands`).

- [ ] **Step 3: Implement**

Add `import json` to the imports, then append:

```python
def fence_commands(uid):
    """The floor first, so the rule never points at an empty table: an empty
    table falls through to main, and main leaves by eno1."""
    uids = f"{uid}-{uid}"
    return [
        ["ip", "route", "replace", "unreachable", "default",
         "metric", UNREACHABLE_METRIC, "table", TABLE],
        ["ip", "rule", "add", "uidrange", uids, "lookup", TABLE, "priority", PRIORITY],
        ["ip", "-6", "rule", "add", "uidrange", uids, "prohibit", "priority", PRIORITY],
    ]


def parse_addresses(ip_json):
    addresses = []
    for iface in json.loads(ip_json or "[]"):
        for info in iface.get("addr_info", []):
            if info.get("family") == "inet":
                addresses.append(f"{info['local']}/{info['prefixlen']}")
    return addresses


def link_commands(link, current):
    """`noprefixroute` because the kernel otherwise adds the carrier's subnet
    to the main table, which is a route via LTE that every process can see.
    IPv6 off because a router advertisement from the carrier would put a
    default IPv6 route via LTE into main by itself."""
    want = f"{link.address}/{link.prefix}"
    iface = link.interface
    commands = [
        ["sysctl", "-qw", f"net.ipv6.conf.{iface}.disable_ipv6=1"],
        ["ip", "link", "set", "dev", iface, "up", "mtu", str(link.mtu)],
    ]
    commands += [["ip", "addr", "del", stale, "dev", iface] for stale in current if stale != want]
    commands += [
        ["ip", "addr", "replace", want, "dev", iface, "noprefixroute"],
        ["ip", "route", "replace", "default", "dev", iface,
         "metric", ROUTE_METRIC, "table", TABLE],
    ]
    return commands
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd provision && python3 -m unittest test_backchannel_link -v`
Expected: 23 tests, OK.

- [ ] **Step 5: Commit**

```bash
git add provision/backchannel/mesh-backchannel-link provision/test_backchannel_link.py
git commit -m "Fence first, and add nothing to the main table

…reasoning: the unreachable floor goes in before the rule because an
empty table falls through to main; noprefixroute because an address
otherwise puts the carrier's subnet in main; IPv6 off because a carrier
advertisement would add a default route via LTE by itself.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The runner, with the fence always first

**Files:**
- Modify: `provision/backchannel/mesh-backchannel-link` (append)
- Test: `provision/test_backchannel_link.py` (append)

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `class Failed(Exception)`
  - `sh(cmd: list[str], tolerate_exists: bool = False) -> str`
  - `read_config(text: str) -> dict[str, str]`
  - `find_link(run, path: str) -> Link | None`
  - `ensure(run, uid: int, apn: str, fence_only: bool = False, say=print) -> int`
  - `main(argv: list[str], user: str = USER, run=None, config: str = CONFIG) -> int`
  - `run` is any callable `(cmd, tolerate_exists=False) -> str`

- [ ] **Step 1: Write the failing tests**

```python
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd provision && python3 -m unittest test_backchannel_link -v`
Expected: `AttributeError` for `ensure`, `main`, `sh`, `Failed` and `read_config`.

- [ ] **Step 3: Implement**

Add `import pwd`, `import subprocess` and `import sys` to the imports, then append:

```python
class Failed(Exception):
    pass


def sh(cmd, tolerate_exists=False):
    """`ip rule add` of a rule that already exists fails with "File exists";
    for the fence, run every minute, that is the answer we want."""
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=90)
    if result.returncode != 0:
        if tolerate_exists and "File exists" in result.stderr:
            return ""
        raise Failed(f"{' '.join(cmd)}: {result.stderr.strip() or result.returncode}")
    return result.stdout


def read_config(text):
    config = {}
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, value = line.partition("=")
            config[key.strip()] = value.strip()
    return config


def find_link(run, path):
    modem = parse_keyed(run(["mmcli", "-m", path, "-K"]))
    for bearer in bearer_paths(modem):
        found = connected_link(parse_keyed(run(["mmcli", "-b", bearer, "-K"])))
        if found:
            return found
    return None


def ensure(run, uid, apn, fence_only=False, say=print):
    for cmd in fence_commands(uid):
        run(cmd, tolerate_exists=True)
    if fence_only:
        return 0
    path = modem_path(parse_keyed(run(["mmcli", "-L", "-K"])))
    if not path:
        say("no modem: the fence holds, and the tunnel will fail cleanly")
        return 0
    found = find_link(run, path)
    if found is None:
        say(f"connecting {path} with apn={apn}")
        run(["mmcli", "-m", path, f"--simple-connect=apn={apn},ip-type=ipv4"])
        found = find_link(run, path)
    if found is None:
        say(f"{path} connected but offers no usable IPv4 bearer")
        return 1
    current = parse_addresses(run(["ip", "-j", "-4", "addr", "show", "dev", found.interface]))
    for cmd in link_commands(found, current):
        run(cmd)
    return 0


def main(argv, user=USER, run=None, config=CONFIG):
    run = run or sh
    fence_only = argv[1:] == ["--fence"]
    try:
        uid = pwd.getpwnam(user).pw_uid
    except KeyError:
        print(f"no user {user!r}: refusing to run, because without it there is no fence",
              file=sys.stderr)
        return 2
    try:
        with open(config) as f:
            apn = read_config(f.read()).get("APN", "vzwinternet")
    except FileNotFoundError:
        apn = "vzwinternet"
    try:
        return ensure(run, uid, apn, fence_only=fence_only)
    except Failed as failure:
        print(failure, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd provision && python3 -m unittest test_backchannel_link -v`
Expected: 36 tests, OK.

- [ ] **Step 5: Run the whole suite**

Run: `cd provision && python3 -m unittest discover -p 'test_*.py'`
Expected: 95 tests, OK.

- [ ] **Step 6: Commit**

```bash
git add provision/backchannel/mesh-backchannel-link provision/test_backchannel_link.py
git commit -m "Run the fence before anything, and refuse to run without one

…reasoning: the fence is applied before the modem is even looked at, so
no ordering at boot can leave the tunnel user unfenced; 'File exists'
from ip rule add is success, because the script runs every minute; a
missing user means no uid and so no fence, which is a refusal, not a
partial run.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Units and config

**Files:**
- Create: `provision/backchannel/config`, `mesh-backchannel-link.service`, `mesh-backchannel-link.timer`, `mesh-backchannel.service.in`

**Interfaces:**
- Consumes: `/usr/local/sbin/mesh-backchannel-link` (installed by Task 7).
- Produces: the placeholders `@HOME_ADDR@`, `@HOME_PORT@` and `@LISTEN_PORT@`, which Task 7 replaces.

- [ ] **Step 1: Write the files**

`provision/backchannel/config`:

```sh
# /etc/mesh-backchannel/config — read by mesh-backchannel-link and by
# install-diamond.sh, which writes the tunnel unit from it. Edit, then re-run
# install-diamond.sh so the unit picks up a changed address.
APN=vzwinternet
HOME_ADDR=204.210.210.200
HOME_PORT=2302
LISTEN_PORT=2201
```

`provision/backchannel/mesh-backchannel-link.service`:

```ini
[Unit]
Description=Keep the LTE management link up, fenced to the backchannel user
Wants=ModemManager.service
After=ModemManager.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/mesh-backchannel-link
```

`provision/backchannel/mesh-backchannel-link.timer`:

```ini
[Unit]
Description=Check the LTE management link every minute

[Timer]
OnBootSec=20
OnUnitActiveSec=60
AccuracySec=5

[Install]
WantedBy=timers.target
```

`provision/backchannel/mesh-backchannel.service.in`:

```ini
# Written by install-diamond.sh from /etc/mesh-backchannel/config.
[Unit]
Description=Management back channel to riverboat over LTE
After=mesh-backchannel-link.service
StartLimitIntervalSec=0

[Service]
User=backchannel
# `+` runs it as root. Before every start, so the tunnel never runs unfenced.
ExecStartPre=+/usr/local/sbin/mesh-backchannel-link --fence
ExecStart=/usr/bin/ssh -N -T \
    -o BatchMode=yes -o IdentitiesOnly=yes \
    -o ExitOnForwardFailure=yes -o ConnectTimeout=30 \
    -o ServerAliveInterval=60 -o ServerAliveCountMax=3 \
    -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/etc/mesh-backchannel/known_hosts \
    -i /var/lib/backchannel/.ssh/id_ed25519 \
    -R localhost:@LISTEN_PORT@:localhost:22 \
    -p @HOME_PORT@ backchannel@@HOME_ADDR@
# A second limit that does not depend on routing: home, and loopback where
# forwarded connections land. Nothing else.
IPAddressDeny=any
IPAddressAllow=localhost @HOME_ADDR@
Restart=always
RestartSec=30

[Install]
WantedBy=multi-user.target
```

The `-R` names `localhost` explicitly so the request matches riverboat's `PermitListen localhost:2201` exactly, instead of depending on what the client sends when the bind address is left out.

- [ ] **Step 2: Check the rendered units on diamond**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision/backchannel
sed -e s/@HOME_ADDR@/204.210.210.200/g -e s/@HOME_PORT@/2302/g -e s/@LISTEN_PORT@/2201/g \
    mesh-backchannel.service.in > /tmp/claude-1000/mesh-backchannel.service
grep -n "@" /tmp/claude-1000/mesh-backchannel.service
```

Expected: the only `@` left is the one in `backchannel@204.210.210.200`. `systemd-analyze verify` is run in Task 7, once the user and binary exist; run before then, it would only report that they are missing.

- [ ] **Step 3: Commit**

```bash
git add provision/backchannel/config provision/backchannel/mesh-backchannel-link.service \
        provision/backchannel/mesh-backchannel-link.timer provision/backchannel/mesh-backchannel.service.in
git commit -m "Units for the back channel: fenced before every start

…reasoning: ExecStartPre=+ runs the fence as root before each start, so
no boot ordering can start the tunnel unfenced; IPAddressAllow is a
second limit independent of routing; -R names localhost so riverboat's
PermitListen matches exactly; StartLimitIntervalSec=0 because a back
channel that gives up after five failures is not one.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: riverboat's side

**Files:**
- Create: `provision/backchannel/40-backchannel.conf`, `provision/backchannel/install-riverboat.sh`, `provision/backchannel/riverboat-host-key.pub`

**Interfaces:**
- Consumes: `diamond-backchannel.pub`, staged beside the script by Task 8. It is diamond's public key and is not committed.
- Produces: user `backchannel` on riverboat, whose key may only `-R localhost:2201`.

- [ ] **Step 1: Write the sshd drop-in**

`provision/backchannel/40-backchannel.conf`:

```
# Installed by provision/backchannel/install-riverboat.sh.
# diamond's back channel: this user may hold localhost:2201 open and do
# nothing else. The key line in authorized_keys says the same; either alone
# holds.
Match User backchannel
    AllowTcpForwarding remote
    PermitListen localhost:2201
    AllowStreamLocalForwarding no
    PermitTunnel no
    PermitTTY no
    X11Forwarding no
    AllowAgentForwarding no
    ForceCommand /usr/sbin/nologin
```

- [ ] **Step 2: Write the install script**

`provision/backchannel/install-riverboat.sh` (`chmod +x`):

```bash
#!/usr/bin/env bash
#
# riverboat's half of diamond's LTE back channel.
#
#   sudo ./install-riverboat.sh            # user, restricted key, sshd drop-in
#   sudo ./install-riverboat.sh --remove
#
# Expects, beside itself: 40-backchannel.conf, and diamond-backchannel.pub
# (printed by install-diamond.sh). Design:
# docs/superpowers/specs/2026-11-05-lte-backchannel-design.md
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR=/var/lib/backchannel
DROPIN=/etc/ssh/sshd_config.d/40-backchannel.conf
LISTEN_PORT=2201

[[ $EUID -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

if [[ "${1:-}" == "--remove" ]]; then
    rm -f "$DROPIN"
    sshd -t && systemctl try-reload-or-restart ssh.service
    id backchannel &>/dev/null && userdel -r backchannel
    echo "removed: drop-in and user backchannel"
    exit 0
fi
[[ $# -eq 0 ]] || { echo "usage: $0 [--remove]" >&2; exit 1; }

for f in 40-backchannel.conf diamond-backchannel.pub; do
    [[ -f "$DIR/$f" ]] || { echo "missing beside me: $f" >&2; exit 1; }
done
key="$(head -1 "$DIR/diamond-backchannel.pub")"
[[ "$key" == ssh-ed25519\ * ]] || { echo "diamond-backchannel.pub is not an ed25519 public key" >&2; exit 1; }

id backchannel &>/dev/null ||
    adduser --system --group --home "$HOME_DIR" --shell /usr/sbin/nologin backchannel
passwd -l backchannel >/dev/null
install -d -m 0700 -o backchannel -g backchannel "$HOME_DIR/.ssh"
printf 'restrict,port-forwarding,permitlisten="localhost:%s" %s\n' "$LISTEN_PORT" "$key" \
    > "$HOME_DIR/.ssh/authorized_keys"
chown backchannel:backchannel "$HOME_DIR/.ssh/authorized_keys"
chmod 0600 "$HOME_DIR/.ssh/authorized_keys"

install -m 0644 "$DIR/40-backchannel.conf" "$DROPIN"
if ! sshd -t; then
    rm -f "$DROPIN"
    echo "sshd rejected the drop-in; removed it and reloaded nothing" >&2
    exit 1
fi
systemctl try-reload-or-restart ssh.service
echo "riverboat ready: backchannel may hold localhost:$LISTEN_PORT and nothing else"
```

`sshd -t` runs before any reload, and a rejected drop-in is removed. A typo here can't take down riverboat's SSH, the only way into the home network from outside.

- [ ] **Step 3: Pin riverboat's host key**

The key was read on 2026-11-05 from `/etc/ssh/ssh_host_ed25519_key.pub` and matches `ssh-keyscan -t ed25519 192.168.50.128`:

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision/backchannel
ssh rpc@192.168.50.128 'cat /etc/ssh/ssh_host_ed25519_key.pub' | cut -d' ' -f1,2 > riverboat-host-key.pub
diff <(cut -d' ' -f1,2 riverboat-host-key.pub) <(ssh-keyscan -t ed25519 192.168.50.128 2>/dev/null | cut -d' ' -f2,3) && echo match
```

Expected: `match`, and the file holds `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILCRaBXaPLp4Rpj56uQMbGhE82lvY70qtzVyj2p9Us4d`.

- [ ] **Step 4: Check the script's syntax**

Run: `bash -n provision/backchannel/install-riverboat.sh && echo ok`
Expected: `ok`. Its real test is on riverboat, in Task 8.

- [ ] **Step 5: Commit**

```bash
git add provision/backchannel/40-backchannel.conf provision/backchannel/install-riverboat.sh \
        provision/backchannel/riverboat-host-key.pub
git commit -m "riverboat's half: a key that can hold one loopback port

…reasoning: limits in both authorized_keys and sshd so either holds;
sshd -t before reload, removing a rejected drop-in, because riverboat's
SSH is the only way into the house from outside; host key pinned from
the file and checked against a scan.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: diamond's install script

**Files:**
- Create: `provision/backchannel/install-diamond.sh`

**Interfaces:**
- Consumes: the files from Tasks 2–6, staged beside it.
- Produces: user `backchannel`, `/etc/mesh-backchannel/{config,known_hosts}`, the binary in `/usr/local/sbin`, and three units. With `--start` it enables them. It prints the public key for riverboat.

- [ ] **Step 1: Write it**

`provision/backchannel/install-diamond.sh` (`chmod +x`):

```bash
#!/usr/bin/env bash
#
# diamond's half of its LTE back channel.
#
#   sudo ./install-diamond.sh            # user, key, files, units; prints the key for riverboat
#   sudo ./install-diamond.sh --start    # once riverboat has the key: enable link and tunnel
#   sudo ./install-diamond.sh --remove   # stop and remove units, routes and rules; keeps the user and key
#
# Expects, beside itself: mesh-backchannel-link, mesh-backchannel-link.service,
# mesh-backchannel-link.timer, mesh-backchannel.service.in, config,
# riverboat-host-key.pub. Design:
# docs/superpowers/specs/2026-11-05-lte-backchannel-design.md
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ETC=/etc/mesh-backchannel
HOME_DIR=/var/lib/backchannel
BIN=/usr/local/sbin/mesh-backchannel-link
UNITS=/etc/systemd/system

[[ $EUID -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

case "${1:-}" in
--remove)
    systemctl disable --now mesh-backchannel.service mesh-backchannel-link.timer 2>/dev/null || true
    rm -f "$UNITS/mesh-backchannel.service" "$UNITS/mesh-backchannel-link.service" \
          "$UNITS/mesh-backchannel-link.timer" "$BIN"
    systemctl daemon-reload
    if uid="$(id -u backchannel 2>/dev/null)"; then
        ip rule del uidrange "$uid-$uid" lookup 4302 priority 1000 2>/dev/null || true
        ip -6 rule del uidrange "$uid-$uid" prohibit priority 1000 2>/dev/null || true
    fi
    ip route flush table 4302 2>/dev/null || true
    echo "removed. Kept: user backchannel, its key, $ETC."
    echo "The modem stays connected until: mmcli -m any --simple-disconnect"
    exit 0 ;;
--start)
    [[ -f "$ETC/known_hosts" && -x "$BIN" ]] || { echo "install first: sudo $0" >&2; exit 1; }
    systemctl enable --now mesh-backchannel-link.timer
    systemctl start mesh-backchannel-link.service
    systemctl enable --now mesh-backchannel.service
    systemctl --no-pager status mesh-backchannel.service | head -5
    exit 0 ;;
"") ;;
*) echo "usage: $0 [--start | --remove]" >&2; exit 1 ;;
esac

for f in mesh-backchannel-link mesh-backchannel-link.service mesh-backchannel-link.timer \
         mesh-backchannel.service.in config riverboat-host-key.pub; do
    [[ -f "$DIR/$f" ]] || { echo "missing beside me: $f" >&2; exit 1; }
done

id backchannel &>/dev/null ||
    adduser --system --group --home "$HOME_DIR" --shell /usr/sbin/nologin backchannel
install -d -m 0700 -o backchannel -g backchannel "$HOME_DIR/.ssh"
[[ -f "$HOME_DIR/.ssh/id_ed25519" ]] ||
    sudo -u backchannel ssh-keygen -q -t ed25519 -N "" -C backchannel@diamond -f "$HOME_DIR/.ssh/id_ed25519"

install -d -m 0755 "$ETC"
[[ -f "$ETC/config" ]] || install -m 0644 "$DIR/config" "$ETC/config"
# shellcheck source=config
. "$ETC/config"
for v in HOME_ADDR HOME_PORT LISTEN_PORT; do
    [[ -n "${!v:-}" ]] || { echo "$ETC/config sets no $v" >&2; exit 1; }
done
printf '[%s]:%s %s\n' "$HOME_ADDR" "$HOME_PORT" "$(cut -d' ' -f1,2 "$DIR/riverboat-host-key.pub")" \
    > "$ETC/known_hosts"
chmod 0644 "$ETC/known_hosts"

install -m 0755 "$DIR/mesh-backchannel-link" "$BIN"
install -m 0644 "$DIR/mesh-backchannel-link.service" "$DIR/mesh-backchannel-link.timer" "$UNITS/"
sed -e "s/@HOME_ADDR@/$HOME_ADDR/g" -e "s/@HOME_PORT@/$HOME_PORT/g" -e "s/@LISTEN_PORT@/$LISTEN_PORT/g" \
    "$DIR/mesh-backchannel.service.in" > "$UNITS/mesh-backchannel.service"
systemctl daemon-reload
systemd-analyze verify "$UNITS/mesh-backchannel.service" "$UNITS/mesh-backchannel-link.service" \
    "$UNITS/mesh-backchannel-link.timer"

# Running the script once with no tunnel yet checks the fence and the link now
# rather than at the first timer tick.
"$BIN" || echo "link not up yet (see above); the fence is in place either way" >&2

echo
echo "Give this to riverboat as diamond-backchannel.pub, then: sudo $0 --start"
cat "$HOME_DIR/.ssh/id_ed25519.pub"
```

- [ ] **Step 2: Check its syntax**

Run: `bash -n provision/backchannel/install-diamond.sh && echo ok`
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add provision/backchannel/install-diamond.sh
git commit -m "diamond's half: install, then start once riverboat has the key

…reasoning: install and --start are separate because the tunnel cannot
succeed until riverboat holds diamond's key, and a unit that fails every
30 s until then only fills the journal; the address is entered once, in
the config, and the unit and known_hosts are written from it; --remove
keeps the key so a reinstall doesn't need riverboat touched again.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Deploy, and verify on the box (agent stages, operator runs)

**Files:**
- Modify: `docs/phase-0-plan.md`, under "Volunteer machines and remote management"

- [ ] **Step 1: Agent stages diamond's files**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision/backchannel
scp -q mesh-backchannel-link mesh-backchannel-link.service mesh-backchannel-link.timer \
       mesh-backchannel.service.in config riverboat-host-key.pub install-diamond.sh \
       60-mesh-backchannel.sysctl.conf 60-mesh-backchannel.networkd.conf \
       rpc@192.168.50.48:backchannel/
ssh rpc@192.168.50.48 'cd ~/backchannel && sha256sum *' ; sha256sum *
```

Expected: matching sums for every staged file.

- [ ] **Step 2: Operator installs on diamond**

```bash
ssh -t rpc@192.168.50.48 'cd ~/backchannel && sudo ./install-diamond.sh'
```

Expected: `systemd-analyze verify` prints nothing, the link comes up, and the last line is an `ssh-ed25519 … backchannel@diamond` key.

- [ ] **Step 3: Agent carries the key to riverboat and stages its files**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh/provision/backchannel
ssh rpc@192.168.50.48 'cat /var/lib/backchannel/.ssh/id_ed25519.pub' > /tmp/claude-1000/diamond-backchannel.pub 2>/dev/null \
  || echo "not readable as rpc: paste the key printed in step 2 into /tmp/claude-1000/diamond-backchannel.pub"
ssh rpc@192.168.50.128 'mkdir -p ~/backchannel'
scp -q 40-backchannel.conf install-riverboat.sh /tmp/claude-1000/diamond-backchannel.pub rpc@192.168.50.128:backchannel/
```

`/var/lib/backchannel` is mode 0755 and `id_ed25519.pub` is 0644, so `rpc` should be able to read it.

- [ ] **Step 4: Operator installs on riverboat, then starts diamond's tunnel**

```bash
ssh -t rpc@192.168.50.128 'cd ~/backchannel && sudo ./install-riverboat.sh'
ssh -t rpc@192.168.50.48 'cd ~/backchannel && sudo ./install-diamond.sh --start'
```

Expected: riverboat prints `riverboat ready…`, and diamond's tunnel shows `active (running)`.

Then read sshd's *effective* settings for the user, not just the syntax check (added after the branch review):

```bash
ssh -t rpc@192.168.50.128 'sudo sshd -T -C user=backchannel,host=x,addr=203.0.113.1 | grep -Ei "^(allowtcpforwarding|permitlisten|permitopen|permittty|forcecommand) "'
```

Expected: `allowtcpforwarding remote`, `permitlisten localhost:2201`, `permitopen none`, `permittty no`, `forcecommand /usr/sbin/nologin`.

- [ ] **Step 5: Agent runs the read-only checks**

```bash
ssh rpc@192.168.50.48 'U=$(id -u backchannel); I=$(ip -br link | awk "/^wwp/{print \$1}")
 echo "rp_filter all/eno1/lte: $(sysctl -n net.ipv4.conf.all.rp_filter)/$(sysctl -n net.ipv4.conf.eno1.rp_filter)/$(sysctl -n net.ipv4.conf.$I.rp_filter)"
 echo "unit limit: $(systemctl show -p RestrictNetworkInterfaces mesh-backchannel.service)"
 echo "main:   $(ip route get 1.1.1.1 | head -1)"
 echo "fenced: $(ip route get 204.210.210.200 uid $U | head -1)"
 echo "rules4: $(ip rule show | grep -c uidrange)  rules6: $(ip -6 rule show | grep -c uidrange)"
 echo "main has LTE: $(ip route show table main | grep -c wwp)"
 systemctl is-active mesh-backchannel.service mesh-backchannel-link.timer'
ssh rpc@192.168.50.128 'ss -ltn | grep 2201'
ssh -J rpc@192.168.50.128 -p 2201 rpc@localhost 'hostname; echo "came in over: $SSH_CLIENT"'
```

Expected:
- `rp_filter all/eno1/lte: 0/2/0`, so eno1 stays loose and LTE replies aren't dropped;
- `unit limit` names `eno1`, as a deny-list;
- `main` is via `dev eno1`;
- `fenced` is `dev wwp…`;
- `rules4: 1 rules6: 1`, which confirms on this kernel that the minute-by-minute `ip rule add` does not pile up rules;
- `main has LTE: 0`;
- both units `active`;
- riverboat listens on `127.0.0.1:2201` and **not** `0.0.0.0`;
- the last command prints `diamond`.

Wait two minutes and run the `rules4` line again: still `1`.

Record the starting LTE byte count, so the spec's data estimate (under 20 MB a month) can be checked against a day of real use. The counter resets when the modem re-enumerates, so read it daily rather than trusting it as a total:

```bash
ssh rpc@192.168.50.48 'ip -s link show $(ip -br link | awk "/^wwp/{print \$1}") | sed -n "3,6p"; date -u'
```

- [ ] **Step 6: Operator runs the contamination checks**

```bash
ssh -t rpc@192.168.50.48 'I=$(ip -br link | awk "/^wwp/{print \$1}")
 echo "--- management on eno1 (expect nothing)"; sudo timeout 300 tcpdump -ni eno1 -c 20 tcp port 2302
 echo "--- anything else on LTE (expect nothing)"; sudo timeout 300 tcpdump -ni "$I" -c 20 not tcp port 2302'
```

Expected: both captures end at the timeout having captured **0 packets**.

Then check that the fence survives networkd reconfiguring, which is what a package upgrade or `netplan apply` does:

```bash
ssh -t rpc@192.168.50.48 'sudo systemctl restart systemd-networkd; sleep 5; ip rule show | grep uidrange; ip -6 rule show | grep uidrange; ip route show table 4302'
```

Expected: the IPv4 `lookup 4302` rule, the IPv6 `prohibit` rule, and both routes in table 4302 (`default dev wwp… metric 100` and `unreachable default metric 65535`), all still present.

- [ ] **Step 7: Operator runs the failure checks**

1. **The fence holds with LTE down.**
   ```bash
   ssh -t rpc@192.168.50.48 'sudo mmcli -m any --simple-disconnect; sudo ip route del default dev $(ip -br link | awk "/^wwp/{print \$1}") table 4302; ip route get 204.210.210.200 uid $(id -u backchannel)'
   ```
   Expected: `RTNETLINK answers: No route to host`, which is the `unreachable` floor answering, and never `dev eno1`. Within a minute the timer reconnects, and the tunnel is back within about 3 minutes.
2. **The friend's network vanishes.** Pull diamond's ethernet cable. From lightning, run `ssh -J rpc@192.168.50.128 -p 2201 rpc@localhost hostname`. Expected: `diamond`. The jump through riverboat still works, because riverboat is reached on the home LAN and diamond over LTE.
3. **Reboot.** Run `ssh -t rpc@192.168.50.48 'sudo reboot'`, wait 3 minutes, then repeat the `ssh -J` command. Expected: `diamond`.
4. **Key limits on riverboat**, run on diamond as the tunnel user:
   ```bash
   ssh -t rpc@192.168.50.48 'K="-i /var/lib/backchannel/.ssh/id_ed25519 -o UserKnownHostsFile=/etc/mesh-backchannel/known_hosts -p 2302"
    sudo -u backchannel ssh $K backchannel@204.210.210.200 id; echo "shell: $?"
    sudo -u backchannel ssh $K -N -o ExitOnForwardFailure=yes -L 9999:192.168.50.1:80 backchannel@204.210.210.200 & sleep 8; kill %1 2>/dev/null; echo "-L tried"
    sudo -u backchannel timeout 15 ssh $K -N -o ExitOnForwardFailure=yes -R localhost:2299:localhost:22 backchannel@204.210.210.200; echo "-R other port: $?"'
   ```
   Expected: no `id` output (the shell is refused); a `-L` connection to `localhost:9999` is refused when tried during those 8 s; and `-R` on 2299 exits with `remote port forwarding failed`.
5. **The unit limit holds with the fence gone.** This proves `RestrictNetworkInterfaces=~eno1` works on its own: with the routing rule deleted and the timer stopped, the tunnel's keepalives would route by main and leave by eno1 if anything let them.
   ```bash
   ssh -t rpc@192.168.50.48 'U=$(id -u backchannel)
    sudo systemctl stop mesh-backchannel-link.timer
    sudo ip rule del uidrange $U-$U lookup 4302 priority 1000
    echo "fence removed; 75 s capture on eno1 (expect 0 packets)"
    sudo timeout 75 tcpdump -ni eno1 tcp port 2302
    sudo systemctl start mesh-backchannel-link.service mesh-backchannel-link.timer
    ip rule show | grep uidrange'
   ```
   Expected: **0 packets** captured, a keepalive interval having passed, and the rule back at the end. The tunnel may then restart once and re-fence itself through `ExecStartPre`; the `ssh -J` command works again within about 3 minutes.

- [ ] **Step 8: Record it in the plan, and commit**

In `docs/phase-0-plan.md`, after the paragraph beginning "**A management jump host is not the rented infrastructure that was ruled out.**", add:

```markdown
**diamond's back channel is LTE, and it is fenced in both directions (2026-11-05).** A US Mobile SIM in a SIM7600G-H dongle carries one reverse SSH tunnel to `riverboat` (home port 2302) and nothing else. The tunnel user's traffic routes through a table of its own, with an `unreachable` floor, so the mesh cannot reach LTE and management cannot fall back to the friend's ethernet. Checked on the box with `tcpdump` on both interfaces: zero packets crossed in either direction. Because management goes through the carrier's NAT, its keepalives cannot hold the friend's router mappings open, which meets the "management quiescent" rule above by construction. Design: `docs/superpowers/specs/2026-11-05-lte-backchannel-design.md`. **The `tcpdump` checks must be run again on the friend's network before any measurement there counts.**
```

```bash
git add docs/phase-0-plan.md
git commit -m "diamond can be reached anywhere, over a channel the mesh cannot use

…reasoning and the measured results from steps 5-7, including any check
that did not come out as expected.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

If any check in steps 5–7 comes out differently from its expectation, the box is **not** ready for a friend's network. Record the result as it was, and don't adjust the expectation to match.
