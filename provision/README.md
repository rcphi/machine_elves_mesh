# Provisioning

Prepares the volunteer boxes so that a measurement happens whether or not
anyone is paying attention.

**The design goal is zero interaction.** The box boots, measures every half
hour, logs locally, and keeps doing so. Remote access exists for when something
breaks, not as the normal path — every step that needs a volunteer talked
through a terminal is a measurement that does not happen.

## Preparing a box

**Volunteer machines never compile anything.** Build once, centrally:

```
./release.sh                      # produces dist/ and a tarball
```

Then on the box:

```
sudo ./setup.sh --label vol-1 --dist dist
```

Idempotent. Re-run it to change the label or install an updated binary.

`setup.sh` verifies the checksums that travel with the files before installing
anything — a box quietly running a truncated copy would produce measurements
nobody could explain. It also records which build is installed, so a puzzling
result can be traced to a specific commit.

Both binaries build fully static, so neither cares what libraries the target
machine has. This needs `musl-tools` on the build machine — some dependencies
compile C, and without a C compiler targeting musl the build fails in a way
that looks like the dependency not supporting musl at all. `dist/VERSION`
records which target each binary was actually built for.

Compiling on the box still works — omit `--dist` — but it is the development
path, not the deployment one.

What it does:

| | |
|---|---|
| **Installs** | `mesh-probe` to `/usr/local/bin`, building it if no release binary exists |
| **Runs it** | connectivity every 30 minutes, mapping-lifetime every 25, both jittered |
| **Logs to** | `/var/log/mesh-probe/results.jsonl`, one JSON record per line, rotated weekly |
| **Isolates it** | unprivileged service account, no capabilities, read-only filesystem except its log |
| **Keeps it awake** | suspend, hibernate, and hybrid-sleep masked |
| **Stops surprise reboots** | security updates still apply; automatic reboot does not |
| **Persists logs** | journald storage made permanent so a power cut proves something |
| **Firewalls** | incoming denied except SSH; outgoing allowed |

### Why jitter, persistence, and no auto-reboot are not fussiness

**Jitter** stops three boxes measuring at the same instant, which would
correlate their results and point them at the same STUN servers together.

**`Persistent=true`** catches up after downtime rather than skipping silently. A
box unplugged for a day should leave a visible gap rather than pretend
continuity.

**No automatic reboot**, because an unannounced reboot mid-measurement is
indistinguishable from a node genuinely disappearing — which is precisely the
thing being measured. A box that reboots itself for updates will manufacture
exactly the failure Phase 0 is trying to observe.

**Suspend masked**, for the same reason: a box that sleeps at 3am produces
results identical to a network outage.

## Two measurements, two timers

**Connectivity** classifies the connection and finishes in under a minute.

**Mapping lifetime** measures how long the router remembers an idle connection,
and spends nearly all of its time deliberately sending nothing — a single run
lasts as long as the interval it drew, up to ten minutes. It therefore has its
own unit with a longer start timeout, and draws one interval per run so the
answer accumulates over days.

This is the measurement that most rewards shipping the boxes early: it costs an
hour to build and produces nothing but time, which a box in someone's house has
in abundance and a development machine does not.

## Isolating the box from the household

`--isolate-lan` additionally blocks the box from reaching the volunteer's other
devices, permitting only the gateway and the internet, with public DNS
configured so that name resolution survives losing the router's resolver.

**It is opt-in, and deliberately so.** A firewall rule that strands a box in
someone else's house is not remotely recoverable. The script therefore verifies
that the box can still reach a STUN server after applying the rules, and rolls
the whole change back automatically if it cannot. **Run it while the box is
still on your bench**, not after it has shipped.

It is worth doing anyway: a friend's other devices are not part of the
experiment, and their household should not have to think about what the box can
see.

## Running the mesh node too

The probe measures the network and joins nothing. To also run the node:

```
sudo ./setup.sh --label vol-1 --dist dist \
     --mesh shangri-la \
     --peer /ip4/203.0.113.9/udp/4001/quic-v1 \
     --job /usr/local/share/mesh-probe/jobs/factory_job.wasm
```

It runs under systemd with `Restart=always`, so a node that dies comes back —
and the restart itself is data. A node restarting hourly is a finding; one that
stays down leaves a gap in the record that looks exactly like a quiet network.

Settings live in `/etc/mesh-node/config` and can be changed without re-running
setup; `systemctl restart mesh-node` picks them up. Local discovery is off by
default and asking the router for a port is on, since a mesh with nobody
reachable cannot admit anyone it is not already connected to.

Two ways to read it:

```
sudo ./node-summary.py             # what happened, this run
sudo ./node-watch.sh               # what is happening, live
```

**The summary shows the current run by default**, meaning everything since the
node last started. The log outlives every restart, so reading all of it mixes
together runs that had nothing to do with each other — and a summary that
silently includes yesterday is worse than one that says it is partial. Use
`--since 20m` or `--all` to widen it.

**There is no need to clear logs.** Restarting the service writes a `started`
event, which is the boundary the summary uses, so `systemctl restart mesh-node`
gives a clean view without losing the history.

Both report the same events; the summary counts them and the watcher narrates
them as they arrive. The watcher is for the minutes when something is being
deliberately broken and the question is what the node makes of it.

## Timing takeovers

`chaos.py` takes a box's node away at random so takeovers can be measured
against the thresholds fixed in `../docs/phase-0-plan.md`: 2–3 s detection and
500 ms resume, at p95.

On each box in the run, once:

```
sudo ./chaos.py install
```

Then, to start a run — on every box, at roughly the same time:

```
sudo mesh-chaos arm 8          # fires every 3–7 minutes for eight hours
mesh-chaos status              # armed or not, and what it has done
sudo mesh-chaos disarm         # stop early
```

`status` counts the current run only — the one armed most recently — and says
when it began. The log below it holds every earlier run too.

**It acts only when this node is running the job and has a peer present**,
and only once the last disruption has settled for a minute. Each fire picks
kill (60%), term (20%) or a 20–60 s partition (20%), and logs to
`/var/log/mesh-node/chaos.jsonl`. **It disarms itself** when the hours are up.

**Nothing needs to reach a box while a run is armed.** That is the point of it
running locally. Looking at a home box from another machine on the same LAN
is harmless, because LAN traffic never crosses the router being measured. A box
on another network — a laptop on a hotspot — is left alone until the run ends.

A partition drops every packet of the `mesh-node` unit's control group and
nothing else, so SSH to a partitioned box still works.

Afterwards, gather both logs from every box and read them together:

```
scp 'diamond:/var/log/mesh-node/*.jsonl*'   run-a/diamond/
scp 'riverboat:/var/log/mesh-node/*.jsonl*' run-a/riverboat/
./takeover-summary.py run-a/*/events.jsonl run-a/*/chaos.jsonl
```

**Take the rotated files too** — the `*` after `.jsonl` is not decoration. Logs
rotate daily, so a run that spans midnight leaves half its evidence in
`events.jsonl.1`. The summary reads the rotations beside each file it is given
and prints which files it read; check that list against the run's hours.

It reads only **the last armed period**, and prints which one that was, beside
the file list and checked the same way. A chaos log is not rotated per run, so
a box's holds every run it has ever done; reading from the first arm would
answer about an older night in the same shape and the same units as an answer
about this one. `--all` reads everything regardless.

Within that period it reports p50/p95/p99 for detection, resume and the visible
gap by cause and direction, gives the verdict against the thresholds, and
counts what should not happen: orphaned jobs, double takeovers, false alarms,
restarts nobody noticed.

**Every duration is measured on a single machine's clock.** Clocks between boxes
are used only to match an action to its takeover, to within seconds.

Before any of this reaches a box, `../node/chaos-rig.sh` plays two boxes on
one machine with compressed timings. It found the one-way gossip fault on its
first run.

## Collecting results

From the operator's machine:

```
./collect.sh vol-1 vol-2 vol-3
./summarise.py results/all.jsonl
```

`collect.sh` reads only; it writes nothing to the boxes. Hosts are whatever ssh
understands, so `~/.ssh/config` aliases are the tidiest way to name them.

`summarise.py` reports each machine's current classification, **whether that
classification has been stable across runs** — a connection that changes
behaviour between measurements cannot be planned around, and matters as much as
the current value — and then the verdict: whether any machine can accept an
incoming connection, since with no rented infrastructure a relay has nowhere
else to live.

## The management channel must stay outside the measurement

Remote-controlling machines behind NAT is *the same problem Phase 0 exists to
answer*, so however you reach these boxes:

- **The mesh must never route over the management overlay.** Bind mesh sockets
  explicitly and never let a fallback route succeed quietly, or the experiment
  will report that peer-to-peer works when what worked was the management tool.
- **The management overlay is never a rendezvous** for mesh peers.
- **Prefer measurement windows with management traffic quiescent**, since
  keepalives hold NAT mappings open — one of the behaviours being measured.

A management jump host is not the rented infrastructure ruled out in
`../docs/phase-0-plan.md`. That decision governs the architecture under test.
Ops tooling is a different category, provided it stays genuinely outside the
measured path.

## What to tell the volunteer

Plainly, and in advance:

- What the box does, and that it talks only to the internet.
- That the operator can access it remotely.
- That unplugging it at any time is fine and breaks nothing — an outage is data,
  not damage.
