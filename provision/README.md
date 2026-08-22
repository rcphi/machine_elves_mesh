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

The probe is built fully static, so it does not care what libraries the target
machine has. The node currently is not: wasmtime and libp2p pull in C libraries
that do not build against musl, so it needs a machine resembling the build host.
`dist/VERSION` says which is which.

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
