# LTE back channel — design

**2026-11-05.** For `diamond`, the box that will leave the house first. The rules
it has to satisfy are in `../../phase-0-plan.md`, under "Volunteer machines and
remote management".

## What this is for

`diamond` is going to sit on a friend's network. Phase 0 needs it there:
it is the nearest thing available to step 5, a machine on a connection the
operator does not own. Once it is there, nothing on that network can be used to
reach it. The friend's router will not forward a port, and it should not be
asked to.

An LTE dongle with its own SIM gives `diamond` a second way out that belongs to
the operator and has nothing to do with the friend's network. This design
makes that second way **management only**. The friend's ethernet carries the
mesh and everything else the box does; LTE carries one SSH connection home and
nothing else.

**Success:** from home, `ssh` into `diamond` while it sits on someone else's
network. That keeps working when their internet is down or their router is
misconfigured, and the mesh never touches LTE.

## Why management has to be enforced, not just intended

The plan's contamination rule:

> The mesh must never route over the management overlay. Bind mesh sockets
> explicitly; never let a fallback route succeed silently. Otherwise the
> experiment reports that peer-to-peer works when what works is the management
> tool.

That rule cuts both ways, and both directions matter here:

- **Mesh over LTE** would measure the carrier's network instead of the
  friend's, and would spend a 2 GB/month plan in days. Run B's hotspot paid
  about 60 MB an hour.
- **Management over the friend's ethernet** would put a long-lived connection
  with keepalives through the router being measured, holding its mappings open.
  Holding mappings open is one of the behaviours Phase 0 measures.

So the design does not depend on getting a preference right. **Neither
direction can happen**: there is no route that allows it.

**This also meets the plan's third rule by construction.** The plan asks for
measurement windows with management traffic quiet, because keepalives hold NAT
mappings open. These keepalives go through the carrier's NAT, not the friend's
router, so they cannot hold the friend's mappings open whenever they run.

## What already exists

Read from both boxes on 2026-11-05, without changing anything:

| | |
|---|---|
| Modem | SIMCOM SIM7600G-H, USB `1e0e:9001`, drivers `qmi_wwan` + `option` |
| Network interface | `wwp0s20f0u8i5` today. The name encodes the USB port, so it changes if the dongle moves |
| SIM | active, US Mobile on Verizon's network (operator `311480`); APN `vzwinternet` confirmed 2026-11-05 |
| Modem state | `disabled`. Nothing brings it up; there is no NetworkManager |
| Network stack | `systemd-networkd` via netplan, plus `ModemManager`, both active |
| `eno1` | **static** `192.168.50.48/24`, gateway `192.168.50.1`, from `/etc/netplan/00-installer-config.yaml` |
| `riverboat` | always on (up 5 weeks), `192.168.50.128` |
| Home router | external TCP **2302** → `riverboat:22`, set up 2026-11-05. DHCP reservation `diamond` → `.48` |
| Home address | `204.210.210.200`. One ISP lease, which can change |
| riverboat sshd | password login turned off 2026-11-05, verified: `Permission denied (publickey)` |
| Tools | `mmcli`, `qmicli`, OpenSSH 10.2, Python 3.14 on `diamond`. No `autossh`, `wg`, `tailscale` |

**Static `eno1` is a blocker on its own.** On any other network `diamond` would
have no working internet: no mesh, no updates, nothing. The DHCP reservation
keeps `.48` at home, so switching to DHCP changes nothing here and fixes
everywhere else.

## The shape

```
 friend's network                                 home
 ────────────────                                 ────
 diamond ─ eno1 ─ their router ─ internet ─┐
   mesh, apt, DNS, everything              │
                                           │
 diamond ─ wwan ─ Verizon CGNAT ─ internet ┴─ 204.210.210.200:2302
   user `backchannel` only                     router forward
   ssh -N -R 2201:localhost:22                     │
                                               riverboat:22
                                               user `backchannel`: may only
                                               listen on localhost:2201
                                                   │
 operator ── ssh -J rpc@riverboat -p 2201 rpc@localhost ──► diamond:22
```

`diamond` dials out because nothing can dial in. Verizon puts the SIM behind
carrier NAT, so `diamond` holds a reverse tunnel open to `riverboat`, and the
operator reaches `diamond` back through it.

**No overlay (Tailscale, WireGuard).** Decided 2026-11-05. Tailscale tries
every interface and would punch through the friend's router, which is the
contamination this exists to prevent. WireGuard needs the same home port
forward as plain SSH, plus another moving part.

## Components

### 1. `eno1` on DHCP

`provision/backchannel/eno1-dhcp.yaml` replaces
`/etc/netplan/00-installer-config.yaml`. It has to *replace* it, not sit beside
it, because netplan merges files and appends address lists, so a second file
would keep the static address. The original is kept as
`00-installer-config.yaml.pre-dhcp`, which netplan ignores because of the
suffix.

```yaml
network:
  version: 2
  ethernets:
    eno1:
      dhcp4: true
      dhcp6: true
```

Applied with **`netplan apply` under a dead-man switch**: a `systemd-run`
timer that restores the `.pre-dhcp` copy and re-applies it after 180 s unless
it is cancelled. The change is made over SSH on `eno1`, the link it changes,
and the automatic rollback is what makes that safe. With the reservation the
box should come back on `.48`.

*Corrected before use: this first said `netplan try`.* `netplan try` backs up
`/etc/netplan` when it starts and restores that backup on revert, so with the
file already replaced its "revert" would restore the new config. Passing the
file with `--config-file` instead adds it beside the installer's, and the
merge keeps the static address. The switch below depends on neither.

### 2. The fence

A fence means traffic can only leave by the routes it allows. These are
installed by `mesh-backchannel-link --fence`, idempotently, and **before every
start of the tunnel**:

| | |
|---|---|
| `ip rule add uidrange U-U lookup 4302 priority 1000` | the `backchannel` user's IPv4 traffic uses table 4302 and nothing else |
| `ip route replace unreachable default metric 65535 table 4302` | **if LTE is down, the tunnel fails here instead of falling through to the main table and leaving by `eno1`** |
| `ip -6 rule add uidrange U-U prohibit priority 1000` | the tunnel never uses IPv6 at all |

`U` is the `backchannel` user's numeric uid, resolved at run time.

**The `unreachable` route is the important line.** Without it, a lookup in an
empty table 4302 falls through to the next rule and then to the main table, and
the tunnel quietly redials over the friend's ethernet. That is exactly the
silent fallback the contamination rule names.

**Nothing is ever added to the main table.** The mesh, `apt`, `systemd-resolved`
and every other process route as they do today, and they cannot reach LTE
because no route they will ever look up points at it.

### Two things on the box that would undo the fence

*Added 2026-11-05 after the branch review, before anything was installed.*

- **Reverse-path filtering.** The kernel checks an arriving packet's source against the main table *as uid 0*, so the tunnel user's rule never applies to that check. While eno1 has a default route the check passes, but with no route home in main — the friend's network down, the one case this exists for — the tunnel's replies are dropped as martians. The reviewer reproduced this in a network namespace on diamond's kernel. Fix: `rp_filter=0` on the LTE interface, set every run, plus `net.ipv4.conf.all.rp_filter = 0` in `/etc/sysctl.d/60-mesh-backchannel.conf`, because the effective value is max(all, interface) and Ubuntu sets all=2. eno1 keeps its own 2.
- **networkd deletes foreign rules and routes.** With its defaults (`ManageForeignRoutingPolicyRules=yes`, `ManageForeignRoutes=yes`), systemd-networkd removes every rule and route it did not create whenever it starts or reconfigures: a package upgrade, a `netplan apply`. That deletes the fence, and until the next minute's run the tunnel user routes by main and leaves by eno1. Fix: `/etc/systemd/networkd.conf.d/60-mesh-backchannel.conf` turns both off, installed and applied before the fence first exists.

**And a third limit, independent of routing:** `RestrictNetworkInterfaces=~eno1` on the tunnel unit, so whatever happens to the fence, the tunnel process cannot use the friend's ethernet. This first draft rejected `RestrictNetworkInterfaces=` because the LTE name changes. A deny-list of the stable name `eno1` avoids that objection, and it covers exactly the direction the networkd finding threatened.

### 3. The link

`provision/backchannel/mesh-backchannel-link` (Python, standard library only,
like `chaos.py`). It runs as root, at boot and every 60 s from a timer. Each run
is idempotent:

1. Install the fence (above).
2. Find the modem: `mmcli -L -K`. If there is none, stop. The fence already
   makes the tunnel fail cleanly.
3. If no bearer is connected, connect one:
   `mmcli -m M --simple-connect="apn=$APN,ip-type=ipv4"`. IPv4 only, because
   home has no IPv6 and the tunnel targets an IPv4 address.
4. Read the bearer: `mmcli -b B -K` gives `bearer.status.interface`, and
   `bearer.ipv4-config.address`, `.prefix`, `.mtu`.
5. Apply it to that interface: `ip link set IFACE up mtu MTU`,
   `ip addr replace ADDR/PREFIX dev IFACE`, and flush any stale address from an
   earlier lease.
6. `ip route replace default dev IFACE metric 100 table 4302`.

**The interface name is read from ModemManager every run, never written
down,** because `wwp0s20f0u8i5` encodes the USB port.

**Its logic is pure functions, tested without a modem:**
- parsing `mmcli -K` output;
- choosing the bearer;
- turning a bearer into a list of `ip` commands;
- deciding which rule commands are still needed, given `ip -j rule show`.

A thin runner executes the commands. The fixtures are real `mmcli` output,
captured from this modem on its first manual connect.

`systemd-networkd` must leave the LTE interface alone, and **on this box it
does not by default.** *Corrected 2026-11-05 in Task 8.* This first said no
`.network` file matches the interface. The install found otherwise: dracut
leaves a catch-all, `/run/systemd/network/zzzz-dracut-default.network`, which
runs DHCP on every link that is not loopback. networkd took a DHCP lease on
the modem and put an LTE default route into the main table at metric 1024,
one eno1 failure away from carrying everything. It also handed the carrier's
DNS servers to resolved, which then sent some name lookups over LTE.
`/etc/systemd/network/10-mesh-backchannel-wwan.network` matches `Type=wwan`
and sets `Unmanaged=yes`. It sorts first, and networkd uses the first file
that matches. ModemManager connects the bearer; this script alone configures
its IP.

**A connected bearer is not the same as a working one.** *Added 2026-11-06,
from Task 8.* After a forced reconnect, the bearer said `connected` and pinged
8.8.8.8 fine. For 15 minutes, though, every SYN to 204.210.210.200:2302 went
unanswered. No ufw block, no UniFi threat and no riverboat log explains it. A
fresh bearer with a new address cleared it at once. So the script also watches
the tunnel, through `ss` for an established socket to `HOME_ADDR:HOME_PORT`.
It does this only while `mesh-backchannel.service` wants to run (`active` or
`activating`, which includes waiting between restarts):

- When the tunnel has been down for WAIT, the script disconnects the bearer and
  reconnects it at once.
- WAIT starts at 5 minutes and doubles after each forced reconnect, up to an
  hour, so a home that is down does not churn the modem.
- A healthy tunnel, or a stopped unit, resets WAIT.
- The state lives in `/run/mesh-backchannel-link.state`, so a reboot starts
  clean. It is timed on `CLOCK_BOOTTIME`, so a wrong or jumping wall clock
  cannot move it.
- Each forced reconnect is written to the journal.

### 4. The tunnel

`mesh-backchannel.service` on `diamond`:

```ini
[Unit]
Description=Management back channel to riverboat over LTE
After=mesh-backchannel-link.service

[Service]
User=backchannel
ExecStartPre=+/usr/local/sbin/mesh-backchannel-link --fence
ExecStart=/usr/bin/ssh -N \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=60 -o ServerAliveCountMax=3 \
  -o StrictHostKeyChecking=yes \
  -o UserKnownHostsFile=/etc/mesh-backchannel/known_hosts \
  -o BatchMode=yes \
  -i /var/lib/backchannel/.ssh/id_ed25519 \
  -R 2201:localhost:22 -p 2302 backchannel@204.210.210.200
IPAddressDeny=any
IPAddressAllow=localhost 204.210.210.200
Restart=always
RestartSec=30

[Install]
WantedBy=multi-user.target
```

- **The `+` on `ExecStartPre` runs the fence as root before every start,** so
  the tunnel never runs without a fence, whatever order boot happens in.
- **`IPAddressDeny`/`IPAddressAllow`** add a second, independent limit: the
  tunnel process may talk to home and to loopback, where its forwarded
  connections land, and nothing else.
- **Host key pinned** with riverboat's ed25519 key, recorded as
  `[204.210.210.200]:2302` in `/etc/mesh-backchannel/known_hosts`, so the
  first connection does not trust whatever answers.
- **A dead tunnel is noticed in about 3 minutes** (60 s × 3) and redialled 30 s
  later. A dial with LTE down costs nothing, because it stops at the
  `unreachable` route without sending a packet.
- **No DNS.** An address literal means the tunnel works when the friend's DNS,
  or the friend's network as a whole, does not.

The address, port and listen port live in `/etc/mesh-backchannel/config`,
together with `APN=vzwinternet`. The install script writes the unit from that
config, so the address is entered once.

The `backchannel` user on `diamond` is a system user with home
`/var/lib/backchannel`, shell `/usr/sbin/nologin`, and no password. Its key is
generated **on `diamond`**, and the private half never leaves it.

### 5. riverboat's side

User `backchannel`: system user, shell `/usr/sbin/nologin`, password locked.
Its `authorized_keys` holds one line:

```
restrict,port-forwarding,permitlisten="localhost:2201" ssh-ed25519 AAAA… backchannel@diamond
```

And `/etc/ssh/sshd_config.d/40-backchannel.conf`:

```
Match User backchannel
    AllowTcpForwarding remote
    PermitListen localhost:2201
    PermitTTY no
    X11Forwarding no
    AllowAgentForwarding no
    ForceCommand /usr/sbin/nologin
```

**What that key can do, and only that:** hold `localhost:2201` open on
riverboat's loopback. It cannot get a shell, cannot open a pty, cannot `-L` into
the home LAN, cannot listen on any other port, and cannot listen on a public
address. The limits are set twice, in the key line and in sshd, so either alone
holds. `sshd -t` checks the config before any reload.

**riverboat drops dead sessions** (`ClientAliveInterval 30`,
`ClientAliveCountMax 3`). This was found on the box. When the modem reconnects
with a new CGNAT address, diamond's socket dies with no FIN. riverboat's sshd
default (`ClientAliveInterval 0`) leaves only TCP keepalive to notice, which
takes about two hours. Until then the dead session holds `localhost:2201`, and
every new tunnel fails its forward and exits. With the setting, the port frees in
about 90 s. Idle keepalives cost a few hundred bytes a minute, roughly 25 MB a
month of the 2 GB.

### 6. Install scripts

`install-riverboat.sh` and `install-diamond.sh`, idempotent, each with
`--remove`. They follow `timed-run.sh` and `setup.sh`: privileged steps are run
by the operator, never from a script on this workstation.

**Order matters:**
1. `diamond` creates its user and key and prints the public key.
2. `riverboat` installs that key.
3. `diamond` pins riverboat's host key and starts the tunnel.

## Data

At 60 s keepalives, each exchange is a request, a reply and their ACKs: a few
hundred bytes a minute, **under 20 MB a month, about 1% of the plan.** SSH
sessions through the tunnel cost whatever they move. A log tail is cheap;
copying `results/` back is not, and should wait until the box is on a network
that can carry it, or come home in person.

`ip -s link show` on the LTE interface reads the actual figure. Its counters
reset when the modem re-enumerates, the same trap `IPAccounting` fell into in
Run D, so readings are taken daily rather than trusted as a running total.

## Verifying it

**Unit tests** for the link's pure functions, using captured `mmcli` fixtures.
Watch them fail first.

**On the box**, each check is a command with an expected answer:

| Check | Command | Expect |
|---|---|---|
| main table untouched | `ip route get 1.1.1.1` | `dev eno1` |
| tunnel user fenced onto LTE | `ip route get 204.210.210.200 uid $(id -u backchannel)` | `dev wwp…` |
| fence holds with LTE down | same, modem unplugged | `unreachable` |
| no management on the measured path | `sudo tcpdump -ni eno1 tcp port 2302` for 5 min | nothing |
| no mesh on LTE | `sudo tcpdump -ni wwp… not tcp port 2302` for 5 min | nothing |
| survives the friend's network vanishing | unplug `eno1`, `ssh -J … -p 2201 rpc@localhost` | logs in |
| survives a reboot | `sudo reboot`, wait, same `ssh` | logs in |
| riverboat key limits | as `backchannel`, try a shell, `-L`, `-R` on another port | all refused |

**The two `tcpdump` checks are the contamination rule as a test.** They are
run once at home and again on the friend's network, before any measurement
there counts.

## Not in scope

- **Firewalling `diamond` off the friend's LAN.** The plan requires it before
  the box goes to their house ("permitting only outbound internet and the mesh
  ports"). It is separate work with its own design, and it must leave the
  back channel and DHCP working.
- **The home address changing.** Accepted for now. If the ISP reassigns
  `204.210.210.200`, the tunnel stops until the config is edited, and editing it
  needs a way in. Dynamic DNS would fix this, but it brings DNS into the tunnel
  path. Revisit if the address moves.
- **`riverboat` or home internet being down.** That leaves no way in. With one
  jump host this is unavoidable, and accepted.

## Unknowns the first manual connect answers

- **The APN.** `vzwinternet` is Verizon's. Some resellers on Verizon's network
  use the same operator ID with a different APN. The first `--simple-connect`
  settles it, or the provider's documentation does.
- **The carrier's idle timeout for TCP.** It is unknown. 60 s keepalives are
  well inside any timeout reported for US carriers. If the tunnel still drops
  on a schedule, the interval comes down; the cost scales linearly and stays
  small.
- **Whether the bearer reports an MTU.** If it does not, use 1428. LTE paths
  commonly carry less than 1500, and a too-large MTU shows up as an SSH
  session that hangs on large output.
