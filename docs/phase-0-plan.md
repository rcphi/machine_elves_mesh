# Phase 0 — Mesh Spike

**Status:** Built and measured. Night A and Runs B–E are done; diamond has been at a friend's house since 2026-11-12, and Run E (2026-11-17), the first across two households, met every criterion fixed before it. Every result is an upper bound — see "Ubuntu Server, and what that costs". *(This line said "Planning. No code yet." until 2026-11-14.)*
**Language:** Rust — Wasmtime is Rust-native and provides the fuel metering §11.4 requires, libp2p's Rust implementation covers §11.6's overlay and gossip, and it produces a single static binary volunteers can run without setup.
**Started:** 2026-08-20
**Design document:** `../machine_elves/docs/superpowers/specs/2026-07-31-machine-elves-design.md` — §11 (compute mesh), §9 (persistence and degradation), §19 (MVP scope). This repo implements only what §19.5 calls Phase 0.

## What this answers

One question, stated as a go/no-go: **can ordinary people's machines, on ordinary home internet connections, form a mesh that genuinely runs each other's work — and keep doing it when machines disappear?**

Everything else in the design rests on this. If the answer is no, the architecture changes and the reveal (§12.1) becomes impossible, so it is answered before any art or game design effort.

**This is a spike, not a foundation.** It is expected to be discarded. Its output is a decision and a set of measurements, not a codebase.

## What it does not answer

- **Whether the game is any good.** No world, no avatars, no economy. Headless.
- **Whether the topology scales.** With five nodes, a full mesh works fine; §11.6's O(log n) structured overlay only matters in the hundreds. Deferred.
- **Anything about custom project code.** Template jobs only (§19.3).

## The two test rigs, and why both are needed

Neither rig can answer the other's question. Run both.

| | Local container rig | Volunteer rig |
|---|---|---|
| **Nodes** | Many, on one host | 3 home machines. No rented infrastructure. |
| **Tests** | Scheduling, checkpointing, migration on dropout, job correctness, load behavior, topology logic | NAT traversal, real latency, asymmetric upstream, router timeouts, genuine disconnection |
| **Cannot test** | Anything about real networks — containers on one host all reach each other trivially | Scale, or any behavior needing more than a handful of nodes |

**The volunteer rig is the one that decides go/no-go.** The local rig is where the work actually gets built and debugged.

## No rented infrastructure. This is a measurement decision, not only a principle.

**Phase 0 runs on player machines only.** No VPS, no rented relay, no hosted rendezvous.

The obvious argument is the design one: §12.1's reveal cannot survive a permanent dependency on servers anyone rents, §15 already rejects Tailscale as a service for exactly this reason, and infrastructure introduced as a convenience becomes load-bearing without anyone deciding that it should.

**The stronger argument is that a rented node would corrupt the measurement itself.** A public-address node in the mesh makes hole-punching succeed more often, gives relaying somewhere to land, and provides a rendezvous point for peers coordinating a connection. Phase 0 would then report that the architecture works, when what works is the architecture *plus a server* — which is precisely the finding the spike exists to prevent anyone from making.

**The three home machines are the whole rig**, and they carry the failure modes that matter: carrier-grade NAT, asymmetric upstream bandwidth, consumer routers dropping idle connections, real geographic latency.

Three is a small sample, but the relevant failures are **categorical rather than statistical** — a connection behind carrier-grade NAT shows up immediately and unmistakably. Three connections establish *whether* the problem class exists. They cannot estimate how common it is across a real player population, and no claim of that kind may be made from them.

### The consequence to accept up front

**Relay duty has to fall on a volunteer**, because there is nobody else. §11.6 already intends this — relays forward end-to-end encrypted packets and cannot read them, which is what makes distributing relay duty across citizens safe, and a player with a public address and spare upstream is the fifth contribution lever.

But relaying only works if the relaying node is itself reachable. **If none of the three volunteers can accept an inbound connection, the mesh cannot form at all.**

**That outcome is the finding, not a failed experiment.** It would mean the architecture as specified does not work on ordinary domestic connections without infrastructure — which is exactly what Phase 0 is for, arrived at in days rather than a year. Reaching for a VPS at that moment would convert a real answer into a comfortable one.

### One dependency that cannot be removed, and how the design already handles it

Discovering what your own public address looks like requires *someone else* to observe it and tell you. That is a third party by definition, and the probe below uses public STUN servers to do it.

**The probe is a diagnostic tool and is allowed to. The product is not.** §11.6's answer already exists: an invite carries live peer addresses, so once a client has any peer at all, that peer reports what address it observes — peer-observed addressing replaces STUN entirely. §9.5 flagged this same honesty risk for bootstrap and resolved it the same way.

The residual is coordination: two machines both behind NAT generally need something to tell them when to transmit simultaneously. With no third party, that role falls to whichever volunteer is directly reachable, or to exchanging invite blobs by hand — pasting a string into a chat window, which is what §11.6's invite is anyway.

## Step 0 — characterize the volunteers' connections first

**Before any mesh code exists**, have each volunteer run a small probe that reports what kind of network address translation sits between them and the internet, and whether they can accept an inbound connection at all. This is a well-understood measurement using public STUN servers and takes minutes.

This is deliberately the first deliverable because it is nearly free and can invalidate a great deal of work. If all three volunteers turn out to be behind carrier-grade NAT — increasingly common, and near-universal on mobile networks — then direct connections are the exception rather than the fallback, relaying becomes the normal case, and Phase 0's stop criterion is met on day one rather than month three.

Finding that out cheaply is worth more than any amount of careful implementation.

## Stop criteria

Carried from §19.5:

- Direct connections between home machines fail often enough that relaying becomes the common case rather than the fallback, **and** relay burden proves impractical to distribute across players.
- A machine dropping out produces a stall long enough that a player would read it as broken software rather than as the world breathing.

## Timing thresholds — fixed 2026-08-21, before measurement

These are recorded in advance deliberately. A number chosen after seeing results is not a criterion, it is a rationalisation.

**There are two thresholds, and conflating them is the usual mistake.**

| | Threshold | What it governs |
|---|---|---|
| **Detection** | 2–3 s of silence | How long a node may go quiet before it is presumed gone. This is the dip tolerance — set too low, the mesh thrashes, migrating work every time a wifi link stutters. **Measured thrashing on a phone hotspot — Run D, 2026-10-10: once every 14 minutes.** |
| **Resume** | **500 ms** | How long the visible gap may last once a takeover begins. This is the player-facing number. |

**500 ms is too slow for gameplay input and that is fine**, because this is not the input path. It is the recovery path for an unannounced disappearance, which should be rare. Anything a player does continuously — moving, interacting — runs locally and is never waiting on a remote node.

### Two consequences of choosing 500 ms

**1. It forces warm standbys, which is the right architecture anyway.** A transatlantic round trip is roughly 100–150 ms, so a takeover needing two or three exchanges plus a state transfer has already spent the budget. **500 ms is unachievable if the replacement node must fetch state after the failure, and comfortable if it already holds it.** §9.1's continuous replication is therefore not merely a durability feature — it is what makes the resume budget reachable at all.

**2. Detection and resume need not be added together.** The naive reading is that an unannounced loss costs 3 s of detection plus 500 ms of resume. It does not have to, because a standby that already holds recent state can **begin continuing from the last checkpoint immediately, while the original is still merely suspected of being gone.** If the original turns out to be alive, the speculative work is discarded. Briefly running two copies is cheap; a visible stall is not. This is the same tail-latency technique described by Dean and Barroso in *The Tail at Scale* (2013).

**Speculation is only valid where §9.3 says it is.** Deterministic simulation with no side effects may be run twice and one copy discarded. Anything committing to a ledger, an ownership record, or a governance record must have exactly one writer, and there correctness outranks latency — it may take longer than 500 ms, it may never be wrong, and the player is shown an honest waiting state rather than an invented one.

### A single global number is the wrong shape

§9.6 already tiers subsystems by criticality, and recovery should use the same tiers:

| Criticality | Acceptable gap |
|---|---|
| **Cosmetic** | Seconds. Nobody notices, and §12.2 already makes stalled ambience an honest signal. |
| **Supporting / Essential** | 500 ms, met by speculative resume from a warm standby. |
| **Core** (ledgers, ownership, governance) | Correctness first. Slower is acceptable; wrong is not. |

### What to actually measure

**Record the distribution, not the average.** A median of 90 ms with a 99th percentile of 8 s feels broken to every player who hits the tail, and reports as excellent in a summary statistic. Phase 0 reports p50, p95, and p99 for takeover gaps, and the stop criterion is evaluated against **p95**.

## What a job is

Settled 2026-08-21, and it decides whether checkpointing is straightforward or a research project.

**A job is not a program that starts, runs, and finishes.** It is woken up, handed everything it knows, does a small amount of work, hands everything back, and sleeps again. The thing handed back and forth is a serializable blob of state; each wake-up is a tick.

```
tick(state, inputs) -> (state, effects)
```

A factory's state holds what is in its input hoppers, what is on the line and how far along, what is in the output bin, wear, and staffing. A tick pulls some material, advances every item, moves finished ones to the bin, and adds a little wear.

**The rule underneath: nothing survives between ticks except the state.** That is the only thing that can be copied to another machine — anything held inside a running program is unreachable, and dies with the host.

Everything downstream then becomes easy rather than hard:

| | |
|---|---|
| **Checkpointing** | Keep the state between ticks. No snapshot machinery. |
| **Migration** | Send the state elsewhere and keep ticking. |
| **Speculative resume** | Another node with recent state just runs the next ticks. Identical inputs, identical outputs. |
| **Verification** (§11.4) | Same reason — anyone can re-run a tick and compare. |

**Time arrives as an input; jobs never read the clock.** Two machines running the same tick must agree, and a job that asked the operating system what time it is would disagree with itself. This costs nothing, because §5.9 already makes time a pure function every node computes identically.

**Jobs describe effects; they do not perform them.** A tick returns *"produce twelve widgets", "request forty units of steel"* and the host decides whether and how to apply them. This falls out of §11.4's sandbox — job code has no network and no capabilities — and it is also what makes speculation safe, since running the same tick twice yields the same requested effects and the host applies them once.

**The cost, stated plainly:** no job may start a long computation and hold its progress internally. Everything must be expressible as discrete resumable steps, with working notes in the state. Natural for a tick-based world simulation, and a genuine narrowing of what §11.2's player-authored code can ever be.

### Most things are not jobs

| Kind | Example | Runs? | Sandboxed? | Where |
|---|---|---|---|---|
| **Data at rest** | A house, possessions, ownership records | No | No — it is not code | Replicated across peers (§9.1) |
| **Simulation** | A factory, a farm, water treatment | Yes, tick by tick | Yes | Scheduled on the mesh |
| **Local interaction** | Walking through your own house | Yes, in real time | No | Only the owner's machine (§9.6) |

A house computes nothing. Peers store a copy so the city persists while its owner sleeps, but storing is not running and there is no code to contain. **The sandbox exists for exactly one purpose: running other people's code on your hardware.**

The line between the second and third rows is whether anyone else needs the thing while you are away. A living room, no. A factory where six people work, yes.

## What job execution settled — 2026-08-22

**Jobs get no imports at all.** Not a restricted set: none. A job that asks for anything is refused at load, and the refusal names what it wanted. Every route out of a sandbox — clock, randomness, network, filesystem — arrives as an import, so refusing all of them *is* the boundary, and §11.4's "receive input, compute, return output" becomes literally true rather than a policy someone must enforce. It is also what makes determinism reachable, since every source of variation a job could touch would have come through an import.

**A fresh instance is built every tick.** This looks wasteful and is the opposite. It means anything a job leaves in a global or on its heap is gone before the next tick, so *nothing survives between ticks except the state* stops being a rule a job could break and becomes a fact about how jobs are run. Making the violation impossible is cheaper than detecting it — the same move §17 describes as designing your way out of needing rules.

**The ceiling is on work, not on time.** Fuel is consumed per instruction, so a busy host and an idle one stop the same job at exactly the same point. A wall-clock limit would make two machines disagree about whether a tick completed, which would break both speculative resume and verification.

**State is opaque to the host.** The host moves bytes it never interprets. Only effects are parsed. A job may keep whatever it likes in whatever format it likes, and the host still checkpoints, migrates, and verifies it.

**The guest is treated as hostile at the boundary.** It chooses the pointer and length of its own output, so both are range-checked against its memory, and blobs are capped — an unchecked length is how a job exhausts the host's memory without ever escaping the sandbox.

### Verified, not asserted

| Property | Test |
|---|---|
| Determinism | The same tick from the same state gives identical state, effects, and fuel used |
| Nothing carries over | Ten unrelated ticks in between change nothing about repeating an earlier one |
| Checkpoint and resume | Twenty ticks straight through equals ten, drop the runner entirely, then ten more |
| Runaway containment | A job that never returns is stopped by fuel rather than taking the machine |
| Ceiling is work-based | The same job stops at every fuel level tried |
| No imports | A module importing `host::now` is refused at load |

The sample factory costs about 2,900 fuel and 36 bytes of state per tick, against a default ceiling of 50,000,000 — so the ceiling is nowhere near ordinary work, which is what a ceiling should look like.

### Determinism holds across machines — 2026-08-22

The same 22 KB job, byte-identical by checksum, ran 30 ticks on `lightning` and on `diamond`. Both produced the same effects in the same ticks, the same 36 bytes of state, and **the same 81,905 fuel consumed** — identical down to the instruction count, not merely to the visible result.

This is §11.4's independent verification working in practice rather than in principle: any node can re-run a tick and compare, with no coordination and nothing to trust. It is also what makes speculative resume sound, since a standby running the next ticks reaches exactly the state the original would have.

**A node never compiles anything.** Diamond had no WebAssembly toolchain, so the compiled job was copied there and run as-is — which is how the real thing works. A player's machine receives compiled jobs; only job *authors* need a compiler.

## Migration — 2026-08-22

A job runs on one node, which broadcasts its state to everyone after each tick. When that node goes away, a survivor continues from the last checkpoint it received.

| | Resumed after | |
|---|---|---|
| Announced departure | **4–9 ms** | the goodbye arrives; whoever should continue simply does |
| Unannounced loss | **3,263 ms** | the full detection window, then an immediate handover |

**The handover itself costs nothing.** In both cases the decision took 0 ms once the disappearance was known: the successor already held the state and needed to ask nobody's permission. The entire difference between the two rows is *finding out*.

That has a direct consequence for §19's 500 ms resume budget: the budget is comfortably met for the handover, and an unannounced loss is bounded by detection instead. The two are separate numbers and should be tuned separately, which is what §19 already argued and this now measures.

### Choosing a successor without agreeing on one

Every survivor picks the lowest peer identifier among everyone still present. There is no vote and no handshake — each node holds the same membership list, applies the same comparison, and reaches the same answer alone. A vote would cost more time than the takeover it was arranging.

**Checkpoints go to everyone, not to a designated successor**, because the successor is not known until the moment it is needed.

### Three bugs found by running it

**A node discovered itself.** Announcing on every interface, it received its own advertisement, dialled itself, and entered its own membership list. That was quietly fatal: the rule asks whether this node's identifier is lower than every member's, and it is never lower than its own — so a node that saw itself could never continue a job, with no error anywhere to show for it.

**Two nodes continued the same job.** Standbys learn about the owner from checkpoints every 200 ms but about each other from heartbeats every second, so both could be following the owner while neither knew the other existed. When the owner left, each saw an empty membership and concluded it was alone. Now a node answers a newly-seen peer immediately rather than waiting for the next interval.

**Duplicate execution is possible by design, and that changes what a produced thing is.** The second bug narrows the window but cannot close it: any node that has not yet heard from a peer genuinely believes it is alone, and no amount of faster messaging fixes that. Since jobs are deterministic, two nodes continuing the same job produce *identical* output — wasted work, which is acceptable. Widgets counted twice is not.

The first answer was a rule: whatever applies effects should remember what it has already applied and ignore repeats. That works and is worse, because it requires every applier to keep a record and to agree with every other applier about what counts as the same thing.

**The answer taken instead is to make the duplicate not exist.** See below.

## Things are identified by what made them — 2026-08-22

**A produced thing's identity is derived, never assigned.** A widget's serial number is a hash of the job's own code, the tick that made it, and which item of that tick it is. Nobody hands out serial numbers, because an authority handing them out is a thing that can be absent — and this has to work when every node is talking to a different half of the world.

**A job is identified by its code.** Its identity is the hash of its WebAssembly module, so two nodes holding the same bytes agree on which job it is without being told and without anyone maintaining a registry of names.

The consequence is the point: **when two nodes both continue a job, they do not each make a widget that someone must later reconcile. They make the same widget.** Recording it twice records it once. There is nothing to deduplicate because there is no duplicate.

**The ledger is a set, not a count.** Counting makes "add this widget" an operation whose result depends on how many times it happened; membership does not. Merging two nodes' views is set union — commutative, associative, idempotent — so nodes converge whatever order they hear things in and however often, and no node ever has to ask another what it has already seen.

Demonstrated with two nodes on separate meshes, unable to see each other, running the same job from scratch — which is exactly the state two nodes are in when both continue one:

```
tick  12: produced 2 widget, ledger now holds 2
tick  13: produced 2 widget, ledger now holds 4
tick  23: produced 2 widget, ledger now holds 6
```

Both produced identical records. A world merging their ledgers holds 14 widgets, not 28.

**What this does not solve, and must not be mistaken for solving it:** genuine contention. Two citizens wanting the last unit of steel is not a duplicate — it is a real conflict between different intentions, and no amount of content-derived identity resolves it. That needs ordering, which is what §9.3's consistency tiers are for and what the fair queue decides. The distinction is worth holding onto: **idempotent facts converge for free; contested decisions never will.**

### A mesh is now a named thing

Nodes only see each other through a topic named for their mesh, so two meshes on one network discover each other's addresses and then ignore each other entirely. That is what §5.1 means by city-states being separate shards, and it also stops a leftover process from an earlier test joining the next one — which it had been doing, silently changing results.

## Graceful drain is a performance feature, not a courtesy

Measured 2026-08-21 on the local rig, and it changed how departure is handled.

**An announced departure is noticed in about 3 ms. An unannounced one takes the full detection window — around 3,400 ms.** Three orders of magnitude.

The first implementation inferred orderliness from whether the transport connection closed cleanly. **That was wrong**, because a killed process still has its sockets closed tidily by the kernel, so a clean close says nothing about whether the departure was planned. §9.6's graceful drain is an *announcement* — only the node itself knows it is leaving, so it has to say so.

A node now publishes a goodbye on SIGTERM or SIGINT and waits briefly for it to propagate. Everything else — unplugged, powered off, network lost, hung — is silence, and silence costs the full window.

This is the strongest argument for making graceful drain the norm rather than the polite exception: it is the difference between a handover and a hole.

## First two-machine result — 2026-08-22

Two physically separate machines: `lightning` (a VM behind its host's address translation) and `diamond` (a dedicated box on a home LAN). `lightning` could reach `diamond`; `diamond` could not reach back at all.

| | Detected after | |
|---|---|---|
| Announced departure | **6 ms** | the goodbye arrived; no waiting |
| Frozen without warning | **3,608 ms** | full detection window, transport never dropped |

**These match the single-host rig almost exactly** (3 ms and 3,450 ms), which is the useful part: crossing a real network boundary cost essentially nothing. Round-trip time between the machines was about 1 ms, so this says nothing yet about behaviour over real distance.

**A node that cannot accept incoming connections still participated fully.** `lightning` sits behind translation and was unreachable from `diamond`, yet it joined, was seen, exchanged heartbeats, and had its departure noticed — because it could reach *one* peer that was reachable, and the resulting connection carries traffic both ways.

That is encouraging for the no-rented-infrastructure decision, and it must not be overstated. It demonstrates **unreachable node + reachable peer works.** It says nothing about **unreachable node + unreachable node**, which is the case needing hole punching or a relay, and the case that decides Phase 0.

Note also that `diamond` was reachable *on the LAN* while its own probe reports `relay_capable: false` — meaning it is not reachable from the internet. Those are different questions and the test only answered the first.

**One provisioning bug found this way:** `setup.sh` had set the firewall to deny all incoming except SSH. Correct for the probe, which only dials out; silently wrong the moment a node existed. Now fixed, and a reminder that provisioning written for one phase should be re-read at the start of the next.

## First results from a second internet connection — 2026-08-22

A laptop on a phone hotspot, which is a genuinely different path from the home line and the harshest network expected to appear in this population.

| | `diamond` (home broadband) | laptop (mobile hotspot) |
|---|---|---|
| IPv4 | endpoint-independent NAT | endpoint-independent NAT |
| IPv6 | **none** | **global address** |
| Idle mapping survives | **over 600 s** | **under 300 s** |

**Mobile carriers run IPv6; this home connection does not.** That was the opposite of what was expected — a hotspot was thought most likely to yield carrier-grade NAT. The population will be *mixed* rather than uniformly IPv4, which is a better situation than either uniform case, because a peer with a global address is exactly what peers without one need.

**Reachability is a candidate, not a fact.** Holding a global address means packets *could* arrive; whether a firewall permits them is a separate question the probe cannot answer alone, and carrier and device firewalls routinely block incoming IPv6. The probe's summary field was renamed from `relay_capable` to `relay_candidate` on discovering it had been overstating exactly this. **Confirming it needs a cooperating peer that also has IPv6, and neither existing machine has any.**

**The idle timeout, now bracketed.**

| Idle | `diamond` (home broadband) | laptop (mobile hotspot) |
|---|---|---|
| 30 s | survived | survived |
| 60 s | survived | survived |
| 120 s | survived | survived |
| 300 s | survived | **forgotten** |
| 600 s | survived, 6 of 6 | — |

Diamond has now run 45 measurements over about a day without a single expiry, so its router's limit is somewhere past ten minutes and has never been found. The hotspot's sits **between 120 and 300 seconds**.

> **Corrected 2026-11-14: diamond's column proves nothing.** The test asks a STUN server again after the silence, and that question is itself outbound. If the mapping had expired, it makes a new one, and a router that preserves port numbers gives the new one the old number, so "unchanged" cannot tell survival from expiry. The home router preserves ports: the probe's fixed port 41999 came back as 41999 in 3,251 of 3,380 readings. So none of the "survived" rows above, and none of the ~3,650 that followed, is evidence, and "past ten minutes" is unproven. The hotspot's column stands, because its finding is an *expiry*, and a changed port is expiry on any router. So does the 55 s keepalive drawn from it. Raised by the external review of 2026-11-07 (finding 3). I first disputed it from a truncated count, and was wrong. Fixed in `319307e`; see "Measuring idle timeouts properly" below.

The keepalive the mesh may use is governed by the worst connection among the players rather than the average, so the mobile figure decides it: **120 s confirmed safe, halved to 60 s**, because the measured value is where the mapping was still alive rather than where it dies, and a keepalive that only just makes it is one dropped packet away from not making it.

**Shipped: 55 s**, leaving better than a twofold margin on the shorter of the two connections measured.

> **Corrected 2026-11-16: a host firewall that admits only replies is now the shortest path, and 55 s fails this rule there.** At the friend's house the path into diamond kept an idle flow through 90 s and lost it at 120 s, and the 120 s was diamond's own firewall: Linux's connection tracking forgets an idle two-way UDP flow at 120 s by default, and a firewall that admits only replies then refuses the next packet (see "Measuring idle timeouts properly"). The friend's router itself kept the flow through 120 s, which halves to 60 s and would have let 55 s stand. But a player's node behind a default firewall meets the 120 s first. Halving 90 s gives **45 s**. At 55 s, one lost keepalive leaves 110 s of silence, 10 s from that limit: the "one dropped packet away" case the rule exists to prevent. In Run E it changes nothing, because heartbeats cross the connection every second, and both boxes admit 4001 by rule. **Shipped for Run E: 40 s**, a little under 45 s, as 55 s sat under 60. The test that encodes this rule now holds the node to 90 s, and failed at 55 s before the change. *(First written as "the friend's router is now the shorter connection", which the kernel log disproved the same evening. The decision survives the correction; its stated reason did not.)*

The residual risk is worth stating: these are one carrier and one home router. Carriers vary, and the common industry figure is nearer 25 seconds — WireGuard's persistent keepalive defaults to 25 s for that reason. A carrier more aggressive than the one measured here would break connections silently rather than loudly. It is a one-line change (`--keepalive-ms`) if a volunteer's connection turns out to be shorter, and the probe will say so before it becomes a mystery.

### The measurement exposes a mechanism doing two jobs at once

Nodes currently heartbeat once a second, which is **roughly 120 times more often than keeping a mapping alive requires**. That is not a bug, because the heartbeat is not a keepalive: it is what makes an unannounced disappearance visible within about three seconds, and slowing it would slow detection in exactly the same proportion.

But it means one mechanism is serving two needs with opposite appetites. **Failure detection wants frequent messages. Keeping a mapping alive wants rare ones. A metered or battery-powered connection wants as few as possible** — and the mobile connection measured here is precisely the case where that matters, being the one machine likely to be on a phone tethering plan.

**Now separated.** Keepalives are an explicit setting with their own interval, carried by ping rather than inherited from gossip, so a path stays open because something is deliberately holding it open rather than because gossip happens to be chatty enough. The heartbeat rate remains a detection choice and can be changed without silently breaking connectivity, which was the trap.

## Identity and location are already separate

Raised as a question about the Host Identity Protocol, which splits the two jobs an IP address was never meant to do at once: *who you are* and *where you are*. HIP makes the identity a public key and demotes the address to a locator that may change freely.

**This design already has that property.** A peer is a `PeerId` — a hash of its public key — and addresses are separate, plural, and disposable; connections are made to a peer, with addresses being merely how it is currently reached. §8.1 pushes the same split a level higher, where identity belongs to the *person* rather than the host.

**HIP itself is not adopted.** It wants kernel support or a shim, deployment has stayed thin, and it resolves "where is this identity now" with a **rendezvous server** — precisely the infrastructure §11.6 excludes. Adopting it would mean inheriting its hardest dependency to obtain an architecture already in hand.

**Roaming, concretely.** A node moving between networks keeps its identity; QUIC's connection migration survives some path changes outright; and a longer gap simply reads as a disappearance, whereupon its job migrates to a survivor and, on return, it receives checkpoints and stands down. That path is already correct. What a roamed node cannot do is *be found again* at its new address with nothing to announce it to — which is the same reachability problem as everything else rather than a separate one.

### What this makes possible, and what still blocks it

The two machines cannot currently reach each other at all. Over IPv6 diamond has no address; over IPv4 both sit behind translation with neither publicly reachable, so a direct dial has nowhere to land.

Both being *endpoint-independent* is the encouraging half: each uses the same external port whatever it is talking to, which is the condition under which hole punching works. What is missing is the coordination — two peers must learn each other's live addresses and dial at roughly the same moment, and the mapping expires, so the exchange has to be recent. That is the piece a relay normally provides and the reason §11.6 counts relay bandwidth as a contribution.

**This is now the concrete form of the Phase 0 question.** Not "does peer-to-peer work" in the abstract, but: can two machines behind ordinary translation, with no rented infrastructure and no third party, arrange a meeting? An answer either way is the finding.

## The Phase 0 question is answered — 2026-08-22

**Two machines behind ordinary address translation reached each other directly, with no rented infrastructure and no third party in the path.**

`diamond` on home broadband and a laptop on a mobile hotspot — different networks, different carriers, different routers, neither able to be dialled from outside. Both sent outward at once. Contact was mutual and immediate:

```
<- their packet arrived from 69.224.155.159:2101 after 0.0s
-> they answered ours after 0.1s
PUNCH v0.1 | me=204.210.210.200:51650 | peer=69.224.155.159:2101 | result=two-way
```

This is what the whole phase existed to find out. The architecture does not need a server to let players find each other, which means §12.1's reveal can be true and §11.6's refusal of rented infrastructure is affordable rather than merely principled.

### Corrected 2026-08-22, later the same day

**The claim above stands, but its explanation was wrong, and the correction matters more than the result.**

Contact was reported at 0.0 s, which was read as packets having already been arriving and buffering — implying the router admitted strangers. It did not. The tool was not reading its socket while it waited for an address to be typed in, so an arriving packet would have sat unread and been indistinguishable from an absent one. A second fault did the same thing again: the twenty-second address refresh read the socket and discarded anything not from a STUN server, silently eating the very packets being measured.

With both fixed, the test was repeated. **Nothing arrived during the wait.** Not one packet, over minutes, while the far side sent every 250 ms. The moment this side sent, contact was mutual within milliseconds.

**So the finding is:** this router admits packets only from an address it has itself already sent to — address-dependent filtering, in RFC 4787's terms. The 0.0 s was never buffering; it is how quickly a reply arrives once the path opens.

### The filtering is per address *and* port — 2026-08-22

Two explanations survived the node's failure, and they needed separating rather than guessing between. Either the router filters per address-and-port, or the node was not sending from the socket it listens on, leaving its listening port with no mapping at all.

Tested with one variable: the raw tool, where the sending socket is certainly the listening socket, aimed at **the right address and a deliberately wrong port**. If the filter were per address, that would have been enough to open it.

**Nothing arrived.** So the filter is per address *and* port. Having sent to `69.224.18.54:9999` does not admit packets from `69.224.18.54:7021`.

### The consequence, stated plainly

**Neither of these machines can accept an unsolicited connection.** One has a predictable address and a closed filter; the other has an open enough filter and an unpredictable address. Between the two of them there is no first packet that can ever land.

So **these two machines cannot connect without a third party**, no matter what is built. That is not a gap in the implementation — it is what these two routers do, and no amount of retrying or cleverness reaches past it.

**This makes the invite structural rather than convenient.** §11.6 chose invite-as-bootstrap for reasons of theme and honesty — that a world should not be findable without a citizen. It turns out to be the only thing that works: an introducer already connected to both parties can observe each one's live address and tell the other, and nothing else can. The mesh rendezvous is not an optimisation; it is the mechanism.

**And a city must contain at least one member reachable without punching** — a public address, or IPv6 with a permissive firewall — or it can never admit anyone it is not already connected to. The laptop measured here has global IPv6 and would serve, if the other machine had any. It does not. That is now the most concrete argument for IPv6 in this project: not performance, not tidiness, but whether a stranger can be let in at all.

### What this changes

**Hole punching works, and requires both sides to know each other's live address first.** Neither can open a path alone. That is not a limitation of the implementation; it is what these two routers do.

**Therefore two strangers behind such routers cannot meet unaided.** Something must tell each of them where the other is, at a moment when both addresses are still valid. Three things can be that something, and none is rented infrastructure:

- **A peer already connected to both** — which is what §11.6's mesh rendezvous provides, and why a newcomer needs one existing contact rather than none.
- **An invite carrying a live address**, used promptly. §11.6 already says live; this is why.
- **One member reachable without punching** — a public address, or IPv6.

**A city therefore cannot bootstrap from nothing.** At least one member must be reachable, or every pair needs an introducer already connected to both. That is a real constraint on what a volunteer population must contain, and it was invisible until the instrumentation stopped lying.

**It also explains the node's failure.** The node was dialling `69.224.155.159` while the laptop had moved to `69.224.18.54`, so it never sent to the laptop's real address, its router never opened, and the laptop's packets were dropped exactly as this filtering predicts. The node behaved correctly on stale information.

**The address changed because the hotspot was switched off and on**, not on its own — so a mobile address is stable within a session and reassigned whenever the connection cycles. That is a weaker claim than "mobile reassigns addresses" and a more useful one: the events that invalidate a cached address are the ones a person causes, and they are frequent. A laptop closing, a tunnel, a train, a hotspot toggled — each ends a session, and the address after it is unrelated to the address before.

### What it does not prove, and the distinctions matter

**Addresses were discovered through public STUN servers.** A third party observed each machine and told it what it looked like from outside. §11.6's answer — a peer already in the mesh reports what address it sees — is not what was used here, and is untested.

**A human carried the addresses across.** In production that is the invite, and the invite would have to carry a *live* address: mappings expire, and the mobile one expires within five minutes. A hand-copied address that outlives its mapping is worse than useless. **This remains the unsolved half.** Punching works; arranging the meeting at scale does not yet.

**It happened suspiciously fast.** Their packet arrived at 0.0 s — before this side could plausibly have opened a path by sending. The likely explanation is that `diamond`'s router uses the most permissive filtering there is, admitting anyone once the socket has sent anywhere at all, which the STUN traffic had already done. A stricter router would not behave this way and the result would look different. **Two routers is not a sample**, and this one may be the friendly case.

**Nothing here tested sustained use** — only first contact. Whether the path survives, and for how long without traffic, is what the mapping-lifetime measurements bound.

### What follows from it

The remaining work is no longer "can this work at all" but "how do two peers exchange live addresses without a server". That is a narrower question with known shapes: an invite that carries a currently-valid address and is used promptly; a peer already in the mesh acting as introducer for two who are not yet connected; or a player with a genuinely reachable address doing the same job §11.6 already calls the fifth contribution lever.

**The introducer need not be infrastructure.** Any citizen already connected to both parties can perform it, which is precisely what a friend-to-friend invite (§11.6) makes available: the person who invites you is, by construction, already connected to you and to the world.

## Remembering peers — 2026-08-22

**A player who has joined a city keeps a list of peers it has seen, and uses it to get back in.** The invite is then needed exactly once, ever: after first contact, a machine bootstraps from its own memory, and whoever is still online brings it back. This is what Bitcoin's address manager and BitTorrent's persisted routing table do, and it removes the repeated-bootstrap problem entirely.

**The objection is staleness, and one measurement decides it.** A remembered address is worthless if it has changed. Every observation so far showed a different external port each run — but every run had also bound a *different local port*, because the socket was opened on port zero and the kernel chose. That confounded the two.

Bound to a fixed local port instead, across five separate runs:

```
local 41999 -> 204.210.210.200:41999
local 41999 -> 204.210.210.200:41999
local 41999 -> 204.210.210.200:41999
local 41999 -> 204.210.210.200:41999
local 41999 -> 204.210.210.200:41999
```

**The router preserves the port.** A node that always binds the same local port has the same external address every time it starts. That is the difference between a peer cache that works and a list of dead ends, and it costs nothing to arrange.

**Not yet confirmed elsewhere.** This is one router. Mobile carriers commonly do *not* preserve ports, and the hotspot's observed ports did vary — though those runs also used random local ports, so the question is still open there. Running `--port` on `diamond` and on the hotspot would settle it, and the answer changes how much the cache can be relied on for which peers.

**Prefer remembering peers that are reachable.** A peer with a public address or working IPv6 is worth far more in a cache than one behind translation, because it can be reached without arranging anything. §11.6 already counts that as a contribution.

### Measured on both machines — 2026-08-22

| | `diamond` (home broadband) | laptop (mobile hotspot) |
|---|---|---|
| Same local port each run | 41999 | 41999 |
| External address | `…:41999`, unchanged across readings | IP unchanged, **port different every reading** |
| Cacheable over IPv4 | **yes** | **no** |

The home router preserves the port outright: bind 41999, be reachable at 41999, every time. The carrier does not — it hands out a fresh external port whenever a mapping is created, which happens on every restart and after every idle expiry, and those come every few minutes.

**Only the port moves; the IP holds.** That is not enough to cache an address, since an address is both, but it does mean a mobile peer is not *unrecognisable* — merely undiallable.

### The asymmetry that makes this work

A cache does not have to contain everyone.

**You only need to remember peers you can dial. Peers who cannot be dialled remember you instead.**

A mobile peer keeps a cache of stable ones — `diamond`, or anything with global IPv6 — and dials them on startup. It is never found; it finds. Nothing is lost, because a peer that must always initiate is still a full member the moment it does: connections carry traffic both ways once open, which the two-machine test already demonstrated when a node behind translation participated fully while being unreachable itself.

What this does require is that **a city always contains some peers worth remembering.** A city of nothing but mobile connections has no stable address anywhere in it, and no member could rejoin after everyone went offline at once — which is §9.5's cold shard problem arriving by a different road. Another reason the health measures to watch are how many members can be reached and how many can bridge, rather than how many there are.

## Address families, and the peers that bridge them — 2026-08-22

**IPv4 and IPv6 hosts cannot address each other at all.** Not slowly, not partially — an IPv4-only machine has no way to write down an IPv6 destination. Two peers with no family in common have no path.

| Both ends | Result |
|---|---|
| Both IPv4 | Works; needs hole punching, which is proven |
| Both IPv6 | Works, and more easily — no translation, no mapping, no keepalive |
| One IPv4-only, one IPv6-only | **No path at all** |
| Either end dual-stack | Works over whichever family they share |

**The node now listens on both families.** It had been binding IPv4 only, which made a peer's most valuable property — a global IPv6 address, with nothing in front of it to punch through and no mapping to expire — invisible to the mesh. Failing to bind is reported rather than swallowed: "nobody can reach me" and "I never opened the door" look identical from outside.

### Relaying across the families, which costs nothing new

An IPv4-only peer reaches an IPv6-only peer through a dual-stack peer that holds a connection to each and carries bytes between them.

**This is not tunnelling.** Nothing wraps IPv6 packets inside IPv4 ones, and no protocol translation happens. The relay simply holds two ordinary connections and forwards a payload from one to the other, which is the same circuit-relay mechanism §11.6 already requires for peers that cannot punch through translation. **The relays are already there; the address-family bridge is a second job for them, not a second mechanism.** End-to-end encryption survives, since the relay carries bytes it cannot read — which is what made player-operated relays safe in the first place.

**The wrinkle: a bridge relay is permanent in a way a NAT relay is not.** A relayed connection between two translated peers is a stopgap — it gets upgraded to a direct one the moment hole punching succeeds. A relayed connection between an IPv4-only peer and an IPv6-only peer can *never* be upgraded, because there is no direct path to upgrade to. The relay carries that traffic for as long as the two want to talk, so bridging is a standing cost on whoever provides it rather than a brief one.

**It mitigates the partition risk without removing it.** A city whose last dual-stack member leaves does not degrade — it splits into two meshes that cannot see each other, each convinced the other vanished. Dual-stack membership is therefore worth watching the way §11.6 watches relay bandwidth: not reachability, but the ability to be a bridge at all.

## The node reaches peers by address, not only by broadcast — 2026-08-22

Discovery had been mDNS only, which finds peers on the same local network and nothing else. That is not merely a limitation: **many machines have mDNS disabled**, so the node's only route to a peer was one an operator may have deliberately switched off.

Two changes make a node reachable across the internet.

**A fixed listening port.** The node bound whatever the system offered, so its external address changed every restart and was worth nothing to remember. Bound to a known port, a router that preserves port numbers — as the home connection measured here does — gives the same external address every time, and a peer's address survives restarts.

**Persistent dialling.** A peer is retried every two seconds until reached, rather than attempted once at startup. **This is not politeness, it is the mechanism.** Two nodes behind translation cannot be dialled; the only thing that opens a path is both sending outward at once, and each attempt is a packet outward. A single attempt succeeds only if the other side happened to already be listening, which neither can usefully be. Whoever started second would fail permanently if the first had given up.

Verified across two hosts, with the statically linked binary copied to a machine that never compiled it:

```
event=connected  addr=/ip4/192.168.50.48/udp/4001/quic-v1  after_attempts=1  still_pending=0
event=all-reached
```

**A bug worth recording, because the wrong version looked right.** A reached peer was first matched against the address the connection *ended up* on. A peer dialled at one address commonly answers from another — a different interface, or the public address its translation assigned — so the entry was never cleared and the peer stayed on the dial list for the rest of the run, retried forever. It is now matched against the address that was *asked for*. The mistake was invisible on a single host, where broadcast discovery reached the peer by another route first and disguised the symptom as something else entirely.

## The mesh is its own rendezvous — 2026-08-22

**Nobody advertises themselves. Everyone reports what they observe of others.**

A node behind address translation cannot know its own address; only the peer receiving its packets can see where they came from. So when a connection is established, the receiving node announces *"I can see this peer at X"* to the whole mesh, and a node learns where it lives by hearing itself described.

Three things fall out of one message.

**Simultaneous dialling, without a coordinator.** The announcement reaches every member at once, and every member dials the newcomer immediately while the newcomer dials back. Two machines neither of which can be called only ever meet by both sending outward at the same moment, and the thing that makes the moment shared is the gossip layer they were already running. **§11.6 can refuse rented infrastructure without refusing what it would have provided.**

**Transitive discovery.** A newcomer needs one address, not a roster. Verified with three nodes and local discovery switched off, where the third knew only the first's address:

```
gamma is connected to: 2 peers
gamma discovered its own address: /ip4/…/udp/4203/quic-v1  (seen by beta)
```

Gamma never learned beta's address at all. Beta heard gamma announced and dialled *it* — the mesh reached out rather than waiting to be found.

**Self-address discovery with no server.** The last STUN dependency in the node is gone. STUN remains in the probe, which is a diagnostic and allowed one; the node itself asks nobody.

**Local discovery is now optional** (`--no-mdns`) and the node is tested without it, because many machines disable it and a node that depends on it has one route to a peer that an operator may have deliberately closed.

### Two costs, accepted deliberately

**A burst.** Every member dialling one newcomer at once is a thundering herd. At the district scale §5.7 describes it is nothing; in the thousands it would need to be a subset — the best-connected members rather than everyone.

**Addresses become known city-wide.** A citizen's address would be known to every member rather than only to those they actually talk to. Any peer-to-peer system reveals an address to whoever you connect to, so this widens an existing exposure rather than creating one — but it is a real widening, and it sits a little awkwardly beside §5.7's decision that people are not searchable. Recorded as a deliberate choice rather than an inherited one.

## A mesh across the internet — 2026-08-22

`diamond` on home broadband and `yoinkerz` on a mobile hotspot, two carriers, no rented infrastructure of any kind, connected on the first attempt:

```
event=connected   peer=diamond
event=my-address  /ip4/69.224.18.54/udp/5732/quic-v1  observed_by=diamond
```

The second line is the one that matters. **A machine behind carrier translation learned its own address because a peer told it what it could see** — no STUN server, no third party, nothing asked of anyone outside the mesh.

### What made it work was not punching

Hole punching does work between these two connections, and it needs both sides to already know each other's live address — which needs an introducer, which needs somebody reachable. That is a chain with a hole in the middle when a city has no reachable member yet.

**The router opened a port on request.** NAT-PMP, offered by most consumer routers under a setting usually labelled UPnP, and it granted exactly the port asked for:

```
PORTMAP v0.1 | supported=yes | mapped=yes | external=4001
```

`diamond` was then genuinely listening on the internet. `yoinkerz` dialled it. No punching, no coordination, no timing window, no address exchanged by hand.

**This should have been the first thing tried.** It is how peer-to-peer software has always become reachable, it costs nothing, and it was reached only after a long detour through hole punching — which was worth understanding, but was never the shortest path.

### The shape this settles

**One reachable member is enough.** Not everyone. `yoinkerz` sits behind an unpredictable carrier address and is a complete member: it dials out, is observed, is announced, and can be introduced to anyone. What it cannot do is be dialled first, and it never has to be.

That is the same asymmetry as everything else here — not everyone bridges address families, not everyone has a stable address, not everyone relays. **A city needs some members who can, and mechanisms that let the rest participate fully without.** A player who forwards a port contributes reachability, which §11.6 already counts alongside compute and relay bandwidth.

**Phase 0's question is answered, and by a plainer route than expected.** Ordinary home machines form a mesh across the internet with no rented infrastructure, provided one of them asks its router for a door — and consumer routers say yes.

### Still owed — *settled 2026-08-23*

**The mapping expires in an hour.** The node must renew it, the way it already keeps NAT mappings alive with keepalives. Until it does, a long-running node silently stops being reachable, which is precisely the failure that is hardest to notice. ***Built** in `3362a50` and `c04a118`: `portmap::RENEW_EVERY` renews every fifteen minutes, spawned rather than awaited so a failing renewal fits inside the detection window, and a renewal that stops succeeding is reported rather than swallowed. The table below already said "held open by renewal"; this section had not been updated to match. Field evidence: `diamond` has held port 4001 across four multi-hour runs, including 8.7 hours on 2026-09-13 and two eight-hour chaos runs.*

## Work migrated across two carriers — 2026-08-23

A factory ran on `diamond` (home broadband) for 1,487 ticks, producing 270 batches of widgets. `diamond` was killed outright — no goodbye, no graceful drain. `yoinkerz` (a laptop on a mobile hotspot, behind an unpredictable carrier address, reachable by nobody) noticed the silence, found itself the only survivor, resumed at **tick 1497**, and carried on producing.

**Nothing was lost.** The takeover tick is *past* the last visible effect, because the factory kept ticking after its last widget and checkpointed every one. The gap was the detection window, not lost work.

Everything in this had been tested separately. None of it had been tested with two carriers between the parts:

| | |
|---|---|
| Reachability | asked of a consumer router and granted, then held open by renewal |
| Address discovery | `yoinkerz` learned its own address from `diamond` observing it — no STUN |
| Job execution | deterministic WebAssembly, metered on CPU and memory |
| Checkpointing | broadcast on gossip every tick |
| Failure detection | silence, no announcement, ~3 s |
| Takeover | decided alone, by a rule every survivor applies identically |
| Identity of produced things | derived from the job's code and the tick, so continuing is not duplicating |

**No server. No relay. No rented anything.** One machine asked its own router for a door; the other never needed one.

### Phase 0's question, answered

> Can ordinary people's machines, on ordinary home internet connections, form a mesh that genuinely runs each other's work — and keep doing it when machines disappear?

**Yes**, with one condition worth stating precisely: **at least one member must be reachable.** Not everyone. `yoinkerz` is behind a carrier address that changes whenever the connection cycles, cannot be dialled by anyone, and was nonetheless a complete member — it dialled out, was observed, was announced, and took over the work when the reachable machine died.

That condition is met by any member whose router will open a port on request, which consumer routers do, or by anyone with working IPv6. It is not a dependency on infrastructure; it is a property some members have and others need not.

## What running it for real found — 2026-08-23

Five faults, none of which any rig here had caught. Every one needed two machines that keep working while unable to see each other, and then meet again.

**A lost peer was never dialled again.** An address came off the list once reached and nothing put it back, so the first disconnection was permanent. Waiting did not help because nothing was trying.

**The redial hung off the wrong signal.** It fired when the transport reported a closed connection — but a machine that vanishes closes nothing. A killed process, a closed laptop, a switched-off hotspot: the connection stays open in the transport's opinion until an idle timeout minutes away. It now hangs off silence, which is the only warning a vanished machine gives.

**Two owners deadlocked.** Each deferred to the other's checkpoint, so both stopped and the job stalled with nobody running it and no disappearance to notice. Settled now by a rule both apply identically: whoever is further along keeps it, ties to the lower identifier.

**`--own` meant claim always**, so a node returning from a restart took work somebody was already doing. It now listens for three detection windows and claims only if nobody answered.

**A connection storm.** A connection prompted an announcement, an announcement prompted every peer to dial, and each dial made another connection — dozens within a second, between two machines.

**And identity was not persistent**, against §8.1: generated fresh every start, so a node returning from a restart was a stranger who happened to be at a familiar address. Nothing could recognise it and no peer cache could ever contain it. Now written to disk on first use and read thereafter.

**The lesson is about testing rather than about any of the bugs.** All six live in the same place: what happens *after* something goes wrong and then comes back. A rig that starts a connection and tears it down never visits there. A person turning a phone off and on visits immediately.

## The timing criterion was never measured — 2026-09-12

**"Phase 0's question, answered" rests on one takeover.** The thresholds fixed on 2026-08-21 are distributions evaluated at p95, and a single run has no p95. It showed that work *can* move between carriers, not that it moves fast enough. The second stop criterion — a stall long enough to read as broken software — is open until it is measured.

**The node could not have met the detection threshold.** Detection was set to 3 s, and the sweep that notices silence runs every quarter window, so a loss was noticed 3.0–3.75 s after the last word: over 2–3 s by construction. It is now 2 s, landing at 2.0–2.5 s. **Changed before the timed runs, not after them** — the threshold stayed fixed and only the setting moved. The cost of a shorter window is false alarms on a jittery link, and the runs count them rather than assume there are none.

**Meeting 500 ms resume as written will still leave a player waiting ~2.5 s.** The node takes over only after detection. The 500 ms budget was premised on speculative resume from a warm standby (above, "Two consequences of choosing 500 ms"), which is not built. So the runs report the **visible gap** — last state heard to first state produced — beside the thresholds, and the baseline is taken before speculation is built, so the difference it makes is measured rather than assumed.

**How:** `provision/chaos.py` runs on each box and, every 3–7 minutes, takes the node away if and only if it is running the job with a peer present — kill (60%), announced stop (20%), or a 20–60 s partition (20%). It needs nothing from outside the box once armed. Every duration is measured on the survivor's own clock; clocks between machines are used only to pair an action with its takeover. Two runs, so the network's contribution can be separated from the node's:

| Run | Mesh | Isolates |
|---|---|---|
| A | diamond + riverboat, same LAN | the node's own takeover logic |
| B | diamond + yoinkerz on the phone hotspot | the same, plus a real internet path |

### A returning node was never heard again

**Found by the local chaos rig on its first run, before anything touched a box.** A node killed and restarted within ten seconds was connected to, and then never heard from: the survivor logged no `joined`, and would have held the job alone indefinitely while the returned node stood by believing all was well.

The survivor had declared the node gone but kept its QUIC connection, which the transport holds until its own 10 s idle timeout. The restarted node's connection therefore arrived as a *second* connection to a peer already connected, and gossipsub exchanges topic subscriptions only on the first. The returning node never learned the survivor was subscribed, and published into nothing.

**A box restarts its node after exactly ten seconds**, so this was not an edge case: it was a race the ordinary restart path runs every time, and would have corrupted every takeover after the first in a timed run.

**The first fix was incomplete, and the second dry run said so.** Dropping a peer's connections when declaring it gone fixed the unannounced case. An announced departure reopened it by a different route: the survivor, having dropped the connection, redialled at once — and reached the departing node during the half second it spends making sure its goodbye is delivered. The node then exited without closing that brand-new connection, and the stale connection was back.

So there are three layers now, each covering what the one above can miss:

- A node drops a peer's connections when it declares the peer gone, whether by silence or by announcement.
- A departing node closes its own connections before it exits.
- **Whatever the route, a stale connection has one signature** — a former member connected at the transport and silent in gossip. Two detection windows of that, and the connection is dropped so a fresh one can form.

`node/migration-rig.sh` checks both kill and announced return inside the idle timeout. It failed one run in three with only the first layer; twenty of twenty with all three.

## A night on a phone hotspot cost 7.54 GB — 2026-09-15

`yoinkerz` spent 2026-09-13 01:00–10:00 UTC on the phone hotspot, connected to `diamond`'s forwarded port. **The connection itself held for 8.7 hours** across carriers, which is the good news. The first twenty minutes were not the mesh flapping but the laptop itself, rejoining the home wifi and returning to the hotspot six times; each switch was a clean lose-both-peers, take over alone, yield on return.

**The bad news was the bill.** The phone recorded 7.54 GB. diamond's log accounts for it: inventory repair ran **537 times in nine hours**, once a minute, each time offering the entire ledger — about 1.3 million items — to fix a gap that sat at **exactly 88 items for the whole night**.

Two faults, compounding:

- **A whole-set digest can only say "different".** So every difference, however small, was answered with everything.
- **The repair never delivered.** Each one published ~2,600 messages in one burst. The gap never moved, so the same 88 were lost every time — consistent with a send queue dropping the same tail of a sorted set, though that mechanism is inferred, not observed.

For a player on mobile data this is not a performance problem, it is a reason to uninstall. It also sits on the same gossip channel as heartbeats: in the first twenty minutes yoinkerz declared `riverboat` gone 34 times at 51 seconds past the minute, which is when diamond's digest landed. That did not persist all night, so the claim is that repair *can* starve detection, not that it did throughout.

**Repair now sends what differs.** Each node keeps 65,536 cells — count and XOR, updated as items arrive — addressed by a serial's first two bytes. A summary of 256 buckets (3 KB) goes out each minute; a differing bucket draws that bucket's 256 cells; only the items in differing cells cross, both ways, 32 buckets and 4,096 items a bucket per round at most. Whole cells or nothing, so a round that does not finish makes progress anyway.

**Replayed on diamond's real ledger** — 1,733,312 items, one copy missing the newest 88: converged in three rounds (88 → 48 → 13 → 0), about 1.6 MB in all, against 218 MB the old scheme would have offered in the same three minutes and never finished. At rest the cost is one 3 KB summary a minute per node.

**Hotspot runs are now time-boxed**, never overnight, and their data use estimated before they start.

## Night A: the takeover criterion, measured — 2026-09-15/16

`diamond` and `riverboat`, both on home broadband on the same LAN, on mesh `night-a` with a ledger of its own. Chaos armed for eight hours on each. **46 disruptions, every one continued by the other machine.** Nothing was orphaned, nothing was taken over twice, and no false alarm occurred.

| Unannounced loss (kill and partition, n=33) | p50 | p95 | max | Threshold |
|---|---|---|---|---|
| **Detection** | 2164 ms | 2436 ms | 2438 ms | 2–3 s — **met** |
| **Resume** (decision to first tick) | 100 ms | 200 ms | 200 ms | 500 ms — **met** |
| **Visible gap** (last state heard to first produced) | 2264 ms | 2533 ms | 2539 ms | none set |

An announced departure (n=13) costs a resume of 14–81 ms at p50 and a visible gap of 177–236 ms: a different order of magnitude, as it should be, because nothing has to be detected.

**Both thresholds are met, and the player still waits about 2.5 s.** The gap is detection plus resume, and this plan said from the start that the 500 ms budget only holds if a standby begins continuing while the original is merely *suspected* gone. Speculation is not built. So the honest statement of this result is: **the mechanism works and is within its own budget; the experience it produces is not yet the one the budget was chosen to deliver.** Building speculative resume and re-running the same measurement is the way to close that, and the before is now on record. *(Superseded 2026-09-24: speculative resume is built and Run C measured the visible gap at 1060 ms p95, 763 ms p50 — see "Run C: the player's wait, measured". This paragraph is left as written, because the distance between what was predicted and what was measured is the point.)*

**The sample is smaller than intended, and the reason matters more than the number.** 46 disruptions arrived in 2h45m, and then the run went quiet for five hours — not because the mesh failed, but because both chaos tools stopped firing:

- **`chaos.py` was blinded by log rotation**, the same fault found in `node-summary.py` the day before, in a second tool. At 00:26 `riverboat`'s log rotated; its claim on the job was in yesterday's file; it concluded it was a standby and never fired again. `diamond`, correctly, would not disturb a job it did not hold. Ownership had settled on `riverboat`, so nothing moved for five hours.
- **`takeover-summary.py` did not read rotated files either**, and reported 24 orphaned takeovers on the first pass — evidence sitting in `events.jsonl.1`, unread, looking exactly like evidence that did not exist. It now gathers the rotations beside each file it is given and prints which files it read.

Both are fixed, with tests. **n=33 makes a p95 worth quoting and a p99 worth nothing**, and the run should be repeated for a fuller distribution.

**The repair fix held in the field.** Over fifteen hours the two nodes exchanged **1,608 items, about 70 KB**, across 92 rounds — against the 706 million items offered in nine hours on the hotspot. At rest, the cost is a 3 KB summary a minute.

**And the mesh is still running.** Twelve hours after the last disruption, the job is advancing on `riverboat`, 271,000 ticks in, with `diamond` following at 1 ms.

## Night A, second run: the same criterion at n=104 — 2026-09-17

The same two boxes, the same mesh and ledger, chaos armed eight hours on each. **120 disruptions, every one continued by the other machine.** Nothing orphaned, nothing taken over twice, no false alarm, no restart that went unnoticed — 2.6 times Night A's sample with the same four counts at zero.

| Unannounced loss (kill and partition, n=104) | p50 | p95 | p99 | max | Threshold |
|---|---|---|---|---|---|
| **Detection** | 2347 ms | 2461 ms | 2480 ms | 2486 ms | 2–3 s — **met** |
| **Resume** (decision to first tick) | 100 ms | 200 ms | 201 ms | 212 ms | 500 ms — **met** |
| **Visible gap** (last state heard to first produced) | 2450 ms | 2651 ms | 2659 ms | 2680 ms | none set |

**This is the run where the p99 means something.** Night A's n=33 gave a p99 that was one sample; at n=104 it is the 103rd of 104, and it sits 19 ms above the p95. The distribution is not merely inside the threshold, it is tight against it: 2347 ms at p50 to 2486 ms at the worst of 104, a spread of 139 ms.

An announced departure (n=16) resumes in 23–103 ms at p50 with a visible gap of 150–292 ms, the same order of magnitude Night A found.

**The conclusion is unchanged, and it is the one that matters.** Both thresholds are met and **the player still waits about 2.45 s**, because the gap is detection plus resume and speculative resume from a warm standby — which this plan always said the 500 ms budget depended on — is still not built. A second, larger measurement of the same mechanism does not move that; it only removes "the sample was too small" as a reason to doubt it. The before is on record twice now. *(Superseded 2026-09-24: speculative resume is built and Run C measured the visible gap at 1060 ms p95, 763 ms p50 — see "Run C: the player's wait, measured". This paragraph is left as written, because the distance between what was predicted and what was measured is the point.)*

### What Night A blamed on rotation, this run proved fixed

At 20:01, four and three-quarter hours in, **`riverboat`'s log rotated mid-run** — the exact event that ended Night A's measurement five hours early. Its ownership was now in `events.jsonl.1`.

It fired three times in the next sixteen minutes, correctly naming itself the job owner each time. The run continued to its end. `takeover-summary.py` read the boundary as continuous, gathering nine files without being told to.

That is the field proof the rotation fixes never had. The regression test failed against the copy installed on the boxes with the same words as the live log; this is the same fix working on a real machine, across a real rotation, unattended, while nobody was watching.

### A third tool was reading the wrong period

Twenty-five minutes into this run, the command this document gives for reading a run reported **47 disruptions, all of them from Night A and the dud run of the 16th, none from this one** — in the same units, the same verdicts and the same shape as a true answer. A chaos log is not rotated per run, so a box's holds every run it has ever done, and a window from the first arm to the last disarm names an older night. `mesh-chaos status` had it too: this run's two kills read as sixteen.

Both now take the last armed period, by one rule defined once and shared, and both print which period that was. **That is three tools blinded by reading the wrong span of a log — two by a rotation, one by the absence of one.** The lesson is not about rotation. It is that a tool which silently answers about the wrong period produces a result indistinguishable from a correct one, and only a printed window makes it checkable.

### Repair cost, and a guess that was wrong

Over the run the two nodes repaired **about 26,800 items, roughly 1.2 MB** across 244 comparisons — an order more than Night A's 1,608, on 2.6 times the disruptions.

The obvious explanation is partitions: a partitioned node keeps running and keeps producing, so both sides advance and the divergence to reconcile should be large. **The measurement says otherwise.** Attributing repairs to the disruption they follow, within 150 s (windows overlap slightly, so these are approximate):

| | n | items per disruption |
|---|---|---|
| kill | 75 | ~315 |
| term | 16 | ~383 |
| partition | 29 | ~68 |

**A partition is the cheapest of the three to reconcile, not the dearest.** The cost tracks the restart, not the divergence: a killed or stopped node is away about eight seconds and comes back needing everything produced meanwhile, while a partitioned node never stops, yields to whichever peer is further along, and needs little. Night A's smaller total is consistent — it had seven partitions and most of its quiet fifteen hours had no restarts at all.

**All of this remains an upper bound.** Two dedicated always-on wired boxes on one LAN are the best case on every axis a real player's machine is worse.

## Run B: the same criterion across two carriers — 2026-09-20

`diamond` on home broadband, reachable because its router granted NAT-PMP port 4001; `yoinkerz` on a phone hotspot behind a carrier address nobody can dial. Two carriers, no rented infrastructure. Chaos armed two hours on each.

**21 disruptions, every one continued by the other machine**, with the same four counts at zero: nothing orphaned, nothing taken over twice, no false alarm, no restart that went unnoticed.

| Unannounced loss (kill and partition, n=15) | p50 | p95 | max | Threshold |
|---|---|---|---|---|
| **Detection** | 2265 ms | 2456 ms | 2456 ms | 2–3 s — **met** |
| **Resume** (decision to first tick) | 100 ms | 200 ms | 200 ms | 500 ms — **met** |
| **Visible gap** (last state heard to first produced) | 2428 ms | 2566 ms | 2566 ms | none set |

An announced departure (n=6) resumes in 50–101 ms with a visible gap of 99–139 ms.

### The internet path contributes nothing measurable

Run A's second night, on one LAN, gave detection p95 **2461 ms** and resume p95 **200 ms**. Run B, across a mobile carrier and a home broadband connection, gives **2456 ms** and **200 ms** — five milliseconds apart on detection and identical on resume.

**That is the result Run B existed to produce.** The two rigs were separated so the network's contribution could be told apart from the node's, and the answer is that for takeover timing there is no network contribution worth reporting. The gap a player waits through is the detection window and almost nothing else, which is what the plan said when it fixed 2 s and predicted a ~2.5 s wait until speculative resume exists.

Both directions are symmetric: diamond→yoinkerz detects in 2173 ms at p50, yoinkerz→diamond in 2345 ms.

**n=15, against Run A's 104.** A usable p95; the p99 is one sample and the summary says so. The window was armed 2.1 h but paired for about 92 minutes — the laptop was carried out of hotspot range near the end, and 26 fires were skipped for want of a peer. That is the operator's fault, not the carrier's.

### What the phone paid

**121.6 MB for the run** — 89.3 MB in, 32.3 MB out, on the laptop's own interface counters, including an unrelated snapd refresh in the first minutes.

Scaled to the nine hours of 2026-09-13, that is roughly 550 MB against **7.54 GB**: about a fourteenfold reduction, and the repair rewrite is what did it.

**The two ends do not agree, and the discrepancy is unexplained.** diamond's NIC recorded 108.0 MB sent while yoinkerz's recorded 89.3 MB received, and the laptop was offline for 40 of those minutes. Candidates are QUIC retransmission into a lossy mobile path, the 1,746 dial attempts diamond aimed at the LAN address the laptop no longer had, and the ssh used to watch the run. Interface counters cannot separate them. `IPAccounting=yes` on `mesh-node.service` would attribute bytes to the unit alone; it needs a restart, so it is a change for the next run rather than a correction to this one.

**The estimate made before the run was wrong by about four times** — a few tens of MB predicted, ~122 MB paid. It was built from payload sizes: a 36-byte checkpoint five times a second looks like nothing. At roughly ten messages a second the wire cost is about 1.4 KB each once gossipsub signing and QUIC framing are counted. **Steady state costs about 60 MB an hour while a job is running**, which is 1.4 GB a day for an always-on player, and the standby pays a quarter of what the owner does. Fine on broadband; a real constraint on anything metered, and a number Phase 0 did not have before tonight.

### Repair cost, and the ordering that replicates

1,576 items crossed in repair over the whole run, against 18,188 produced. Attributing them to the disruption they follow:

| | n | items per disruption |
|---|---|---|
| kill | 10 | ~37 |
| term | 6 | ~53 |
| partition | 5 | ~11 |

**Same ordering as Night A's second run** — a partition is the cheapest to reconcile, an announced stop the dearest — at a tenth the magnitude, because this run produced items an order slower than a LAN pair does. Ledgers sat 16 items apart at the median comparison.

### A forty-minute partition cost twelve items

After the armed window closed, the laptop left hotspot range for **40 minutes**. Both machines declared the other gone and ran the job alone, each producing some 4,500 items with no contact.

It came back on a different carrier address — `69.223.229.91:5409` became `69.224.92.117:1806` — without restarting, and dialled until diamond answered on attempt 1,746. Reconciling the two divergent histories moved **12 items in 8 cells**, over 8 rounds:

```
{"ledger-comparing","their_held":"19504","our_held":"19508","buckets":"4"}
```

**Two nodes that diverged for forty minutes were four items apart.** That is content-derived identity paying for itself ("Things are identified by what made them", above): items are named by what made them, the job is deterministic, so two nodes running the same job from the same checkpoint independently produce *the same items*. A partition does not create divergence to reconcile; it creates duplicate work whose results are identical and merge to nothing. This is outside the measured window and is not part of the criterion, but it is the strongest evidence yet for the mechanism Night A's repair-cost table first hinted at.

### Two things the setup found

**A peer on the wrong mesh is indistinguishable from a broken one.** Reverting the laptop restored its old binary and its old mesh name while leaving it pointed at diamond. It connected over the internet, was ignored by gossip, was dropped, and redialled — logging `silent-connection` every 4.5 s indefinitely, with nothing anywhere naming the cause. Fifteen minutes went into diagnosing what a single line — *connected, never subscribed, mesh name differs* — would have said outright. Worth fixing before volunteers can run mismatched versions.

**diamond's log rotated at 00:02 UTC, mid-run, and the run did not notice.** This is the box whose logrotate config had drifted and was corrected the day before; this was its first rotation since. The node wrote through it and `takeover-summary.py` read the 8 files without being told to. Ad-hoc `grep` over `events.jsonl` alone, used while watching, made the resume count appear to fall from 8 to 1 — the same blindness as Night A, in a hand-typed command rather than a tool.

**Still an upper bound.** A dedicated laptop on a good hotspot beside its phone is better than a player's machine on a bus.

## Volunteer machines and remote management

Volunteers are not technically inclined. Machines are prepared centrally — Ubuntu 26.04, Rust and dependencies preinstalled — and shipped ready to plug in.

**Design for zero interaction first, remote access as the fallback.** The box should boot, start its service, run its measurements, log locally, and report results without anyone touching it. Remote access exists for when something breaks, not as the normal path. Every requirement to talk a friend through a terminal is a measurement that does not happen.

### The contamination rule

Remote management of machines behind NAT is *the same problem Phase 0 exists to answer*, so the management path must be provably separate from the measured path.

- **The mesh must never route over the management overlay.** Bind mesh sockets explicitly; never let a fallback route succeed silently. Otherwise the experiment reports that peer-to-peer works when what works is the management tool.
- **The management overlay may never serve as rendezvous** for mesh peers.
- **Prefer measurement windows with management traffic quiescent**, since a keepalive-heavy overlay holds NAT mappings open that would otherwise expire — which is itself one of the behaviours being measured.

**A management jump host is not the rented infrastructure that was ruled out.** The no-VPS decision is about the architecture under test, not about operations tooling. Something outside the measured path is a different category — but it must be genuinely outside it, which is what the rule above enforces.

### diamond's LTE back channel — built and verified 2026-11-05/06

`diamond` now carries its own management path: a USB LTE modem (US Mobile on Verizon, 2 GB a month) holding a reverse SSH tunnel to `riverboat`. The router forwards external port 2302 to riverboat's port 22. The operator reaches diamond with `ssh -J rpc@riverboat -p 2201 rpc@localhost`. There is no WireGuard, no Tailscale and no rented host. Design: `docs/superpowers/specs/2026-11-05-lte-backchannel-design.md`. Code: `provision/backchannel/`.

**The contamination rule is enforced, not intended.** Only the `backchannel` user can reach LTE, through a uid rule into table 4302, which has an `unreachable` floor. Nothing is ever added to the main table, so the mesh, and everything else on the box, can only use the friend's ethernet. The tunnel unit is also barred from eno1 (`RestrictNetworkInterfaces=~eno1`), so either limit alone keeps the tunnel off the friend's network. What was measured on the box:

- **Five minutes, both directions:**
  - **0 packets** of tunnel traffic on eno1;
  - **0 packets** on LTE other than the tunnel. *(Not the whole story; see "The mesh node was announcing itself over LTE" below.)*
- **Fence removed, unit limit alone:** 0 tunnel packets on eno1 in 75 s.
- **What riverboat lets the key do:** no shell, no `-R` on any other port, and an `-L` that opens but carries nothing ("administratively prohibited").
- **LTE down:** the tunnel user gets `No route to host` and never falls through to eno1.
- **Ethernet pulled:**
  - the main table is **empty**;
  - ordinary processes get "Network is unreachable";
  - the operator still logs in.
- **Rebooted with the ethernet still out:** it came back over LTE alone, with:
  - the fence in place;
  - the modem unmanaged by networkd;
  - no LTE DNS.
- **Ethernet back:** DHCP on eno1. The tunnel user still routes via LTE, and everyone else via eno1.

**Three things only the box could show, each now fixed:**

1. **networkd was configuring the modem.** dracut leaves a catch-all `.network` file in `/run` that runs DHCP on every link. It put an LTE default route into main at metric 1024, one eno1 failure from carrying everything, and sent some DNS over LTE. The modem is now `Unmanaged=yes`.
2. **riverboat kept dead sessions.** When the modem changed address, riverboat's sshd (`ClientAliveInterval 0`) held port 2201 for the dead session for about two hours, so every reconnect was refused. It now drops dead sessions in about 90 s.
3. **The bearer was connected but useless.** After a reconnect, the bearer passed pings, yet SYNs to home went unanswered for 15 minutes. The cause is unknown: not ufw, and UniFi logged nothing. A fresh bearer cleared it. The link script now reconnects the bearer when the tunnel has been down for 5 minutes, doubling the wait up to an hour. Tested by blocking the tunnel on purpose: it reconnected at exactly 300 s, with a new address. That wait was later cut to 2 minutes; see below.

**Unplugging the modem, and booting without it.** Both recover with nobody at the box.

- **With no modem,** the fence stays up and the script logs "no modem: the fence holds" every 30 s.
- **When the modem is plugged in,** in any port:
  - ModemManager takes about 24 s to find it, under a new index and a new interface name.
  - The script, which reads both fresh every run, connects it on its next run and restarts the tunnel at once.
  - networkd's `Type=wwan` match keeps it unmanaged.
- **Measured from plug-in to login:**
  - **about 35 s** when the bearer is good;
  - **2 min 38 s** when the carrier black-holes it, because the watchdog now gives a fresh bearer 2 minutes before reconnecting.
- **The black-hole hit 3 of the 9 fresh bearers seen,** each time the first bearer after the modem attached. All four watchdog reconnects worked straight away.
- **Bugs this testing found, now fixed:**
  - the script read mmcli's no-modem reply, `modem-list : 0`, as a modem path;
  - a blocking `systemctl restart` from inside the link oneshot deadlocked for 90 s on the tunnel unit's `After=`.

**At the friend's house, a failure the bench never showed — found and fixed 2026-11-13/14.** diamond went to the friend's house on 2026-11-12 and was out of reach for **17.5 hours** before a hand reboot brought it back.

- **What happened.** At 04:40 UTC on 2026-11-13 the modem hit a USB protocol error (`qmi_wwan: Unexpected error -71`) and reset itself. ModemManager reprobed it, timed out, logged `fatal error initializing: modem is unusable`, and **never tried again.** The modem never left USB, so nothing told ModemManager to look again. The link script logged "no modem" every 30 s, 2,074 times, until the friend rebooted the box.
- **Why the bench missed it.** The unplug test recovered because a replug makes ModemManager start over. Here ModemManager gave up on a modem that was still present, which is a different state from the script's point of view. This was the first such error in about 24 hours at the friend's, and there were none in the earlier boots at home.
- **The fix.** If ModemManager lists no modem while the modem is still on USB (`1e0e:9001`), the link script now:
  1. restarts ModemManager after 2 minutes;
  2. if that fails, resets the USB device 2 minutes later (deauthorize, then reauthorize, which the kernel treats as a replug);
  3. repeats both, doubling the wait each round, up to an hour.

  A modem that is not on USB at all has been unplugged, and nothing is done. Each step goes to the journal.
- **Tested on the box, 2026-11-14.** `mmcli --inhibit-device` made ModemManager let go of the modem while it stayed plugged in, which the script sees exactly as it saw the 04:40 failure. The test unit had a 10-minute limit as a safety net.

  | UTC | |
  |---|---|
  | 00:38:12 | modem released, tunnel down |
  | 00:38:16 | script: "modem on USB at 1-7 but ModemManager lists none" |
  | 00:40:20 | script restarts ModemManager |
  | 00:40:25 | modem listed again |
  | 00:40:55 | fresh bearer; tunnel restarted |
  | 00:41:26 | operator logged in |

  **3 min 15 s from losing the modem to login, with nobody at the box**, against 17.5 hours before. The restart was enough, so the USB reset never ran.
- **What this does not prove.** The test released the modem; it did not reproduce the USB error that started the real failure. If a ModemManager restart does not clear that error, the USB reset follows 2 minutes later, but that step has only been tested in code. The next real occurrence will show which step clears it.

**The next real occurrence showed that neither step clears it — found 2026-11-16, fixed the same day.** diamond's link was down for **48 hours**, from 2026-11-14 23:04 to 2026-11-16 23:23 UTC. The measurement was unaffected: diamond's idle-timeout tests kept reaching riverboat over the friend's internet throughout, every half hour or so.

| UTC | |
|---|---|
| Sat 22:54 | The modem's QMI side stopped answering. There was no USB error this time, and the signal was fine. |
| Sat 22:58 | ModemManager marked the modem invalid after 10 timeouts. The new recovery noticed. |
| Sat 23:00 | It restarted ModemManager; the modem stayed unlisted. |
| Sat 23:02 | It reset the USB device. The modem came back, but its QMI probe timed out, so ModemManager built it from the AT ports alone: listed, registered and "connected", with no interface and no address. All three of the modem's lights were on. |
| Sat 23:04 → Mon 23:22 | The link script asked it to connect every 30 s and got "no usable IPv4 bearer" each time. The recovery acted only on a modem that was *not* listed, and this one was. |
| Mon 23:22 | The friend unplugged and replugged the modem. It had a bearer 44 s later. |

- **Two gaps.** A listed modem was taken to be a working one. And the USB reset re-enumerates the modem without cutting its power, so a hang inside the modem survives it; the replug cut the power, and that is what cleared it.
- **The fix.** A listed modem that gives no usable link now goes through the same steps as an unlisted one. Between the ModemManager restart and the USB reset there is a new step: stop ModemManager, send the modem `AT+CFUN=1,1` on its AT port, and start ModemManager again. That command makes the modem restart its own firmware. The AT ports kept answering on Saturday while QMI did not. A modem briefly off USB while it reboots keeps the backoff, so a modem with no service is not rebooted every few minutes for the whole outage. `mesh-backchannel-link --reboot-modem` runs the step by hand.
- **Under the new code, Saturday would have gone:** ModemManager restarted at 23:00, the modem rebooted at 23:02, and a bearer by about 23:03. That is minutes, not two days.
- **Tested on the box, 2026-11-16**, with `--reboot-modem` run under `systemd-run` so it outlived the session it cut:

  | UTC | |
  |---|---|
  | 23:44:42 | ModemManager stopped; `AT+CFUN=1,1` sent; modem answered `OK`; ModemManager started |
  | 23:44:51 | modem left USB |
  | 23:44:58 | modem back on USB |
  | 23:45:10 | ModemManager built it from all 7 ports, QMI included |
  | 23:45:22 | bearer up, 40 s after the command |
  | 23:47:04 | riverboat timed out the dead session that still held port 2201 |
  | 23:47:27 | tunnel up |

  The last two minutes were riverboat, not the modem; see the operator limitation below. The watchdog also reconnected the bearer at 23:47:23, which was not needed: the tunnel was being refused by riverboat, not black-holed, and its own retry would have got in at the same moment. That cost a second bearer, and nothing else.
- **What this does not prove.** The test rebooted a healthy modem; it could not hang one on demand. That the reboot clears a QMI hang rests on what the replug did on 2026-11-16, and the reboot is the software version of a replug. Two modem failures in four days at the friend's, and none on record before the move, mean the next real one will probably come during Run E. If the reboot does not clear it, the fallback is a USB hub that can switch off a port's power.

**The first real failures under the new recovery — 2026-11-17/18.** There were none during Run E. In the 27 hours after it there were five. Each began with `qmi_wwan 1-7:1.5: Unexpected error -71`, the USB protocol error that began Friday's, and each recovered with nobody at the box:

| began (UTC) | link back | took | what cleared it |
|---|---|---|---|
| Tue 14:25:00 | 14:36:17 | 11 min | see below |
| Wed 01:43:54 | 01:49:06 | 5 min | the modem reboot |
| Wed 06:14:33 | 06:25:50 | 11 min | see below |
| Wed 06:26:22 | 06:32:02 | 6 min | the modem reboot |
| Wed 16:55:56 | 17:07:06 | 11 min | see below |

In each of the three 11-minute cases the steps ran like this:
1. Restarting ModemManager did nothing.
2. The modem rebooted and returned to USB, and the same error came back 12–21 s later.
3. The USB reset left the modem listed with no bearer.
4. Restarting ModemManager again, 240 s later, cleared it.

Separately, at 08:10 on Wednesday the network dropped the bearer (`connected -> registered`), and the link reconnected it in 29 s. That was not a hang.

- **The recovery works.** The same failure took 48 hours and a trip to the box before `0020abb`. It now takes 5–11 minutes and nobody.
- **Something physical is probably wrong.** The error came back within seconds of a freshly rebooted modem reaching USB, which a hang inside the modem's firmware would not do. There were five failures in 27 hours, against two in the first five days at the friend's, with the same error each time. That points at the USB link itself: power, the cable, the port, or the modem. Software cannot fix any of those. A powered hub with per-port power switching would rule out power and would also be the fallback named above.
- **The ladder's slow path costs about 6 minutes.** In the three slow cases, the step that worked was a ModemManager restart after the USB reset, reached only after the doubled 240 s wait. Nothing has been changed; this is a note.

**One limitation for the operator.** sshd only checks for a dead session when that session has been idle for 30 s, and a login attempt through 2201 counts as activity. So retrying the login in a tight loop keeps a dead session alive. After a reboot this held recovery to 9 minutes instead of about 1. **After diamond drops off, wait 2 minutes before trying.**

**Data cost.** It's small: tens of KB an hour while idle, and about 25 MB a month of keepalives against the 2 GB plan.

**The mesh node was announcing itself over LTE — found and fixed 2026-11-14.** `--no-mdns` kept libp2p's mDNS behaviour with a year-long query interval, on the theory that it would never speak. It still joined the mDNS group on every interface, the LTE link included, and probed at 0.5 s, 1 s, 2 s, 4 s and so on after every node start and every new address on an interface, which is what a fresh bearer gives the modem.

- **Before** (tcpdump on the LTE interface while the node restarted): 20 packets in 56 s. These were queries, plus 1,400-byte answers listing the node's own addresses.
- **After** (`de6ec47`, the behaviour is absent under `--no-mdns`): 0 packets in 70 s. No sockets on 5353, and no membership of 224.0.0.251 on any interface.

The 5-minute capture above could not have seen it: nothing restarted and no bearer changed inside it. The data was trivial, but it broke the contamination rule, which this section had said was "enforced, not intended". It is now, for this path. Raised by the external review (finding 10) and confirmed live on the box.

### On the volunteer's household

The box sits on someone else's home network. **Firewall it away from their LAN**, permitting only outbound internet and the mesh ports. This is partly courtesy and partly correctness: a friend's other devices are not part of the experiment, and their household should not have to think about what the box can see.

**Applied to diamond at the friend's house, 2026-11-14,** by `provision/isolate-lan`. The first, hand-applied version blocked four IPv4 ranges only. The friend's LAN carries two unique-local IPv6 prefixes, one of them behind a second router that looks like a smart-home hub, and multicast was open, which is how the node's mDNS reached their network. It now rejects outbound traffic to:

- the IPv4 private, link-local and multicast ranges;
- IPv6 unique-local and link-local addresses, and the mDNS and LLMNR groups;
- any other route on the LAN interface.

The gateway is allowed first. Applying it proves it: one test packet goes into every rejected destination, and the rule meant to catch it must count it. On the box: "verified: the internet answers, and every rejected destination was caught".

Volunteers should be told plainly what the machine does, that it can be remotely accessed by the operator, and that unplugging it at any time is fine and breaks nothing.

### Ubuntu Server, and what that costs

**The boxes run Ubuntu Server.** Desktop carries update checks, telemetry, and indexing that add noise to a network measurement and compete for the CPU that job metering is supposed to govern. Nobody logs into these machines, so the graphical stack buys nothing.

**The cost is representativeness, and it must not be forgotten later.** Real players will run Windows, macOS, or a desktop Linux, on machines they are also using for other things — machines that sleep when the lid closes, that lose wifi when someone microwaves lunch, that share bandwidth with a video call. A dedicated always-on server on a wired connection is the *best* case in every one of those dimensions.

**Phase 0 therefore measures the network, not the player's machine.** Its results are an upper bound on how well the real thing will behave. Desktop-OS behaviour, contention with other applications, and sleep/wake cycles are deferred to a phase where there is a game to run on them, and any conclusion drawn here should be stated as "at best."

## Measuring idle timeouts properly — 2026-11-14

`--mapping-lifetime` cannot see expiry on a router that preserves port numbers, and both routers measured so far do (see the correction under "First results from a second internet connection"). So `--mapping-inbound` never speaks after the silence. A cooperating peer, one of our own machines and never a rented server, sends a packet in once the silence is over. If it arrives, the mapping survived without being refreshed. That is the property a peer-to-peer connection needs, because it is the peer's packet that must get through.

**How a run goes, on one socket:**
- The responder (riverboat, `mesh-probe-responder.service`, UDP 3479 forwarded by the home router) reports the address it observes, so no STUN server is involved.
- The two sides exchange a 3-second two-way flow, so the router treats it like a mesh connection rather than a single request.
- After the silence, the responder sends three probes, a second apart. If none arrives, a check confirms that the path itself still works before the run counts as an expiry.

**riverboat's own router must not forget its half meanwhile.** That router usually keeps port numbers, but not always: 128 of diamond's 3,380 readings from home came back on another port. If it forgot the flow and gave the probes a new port, they would be filtered out at the client: a false expiry. *(Corrected 2026-11-16: this said the router randomises ports, the error retracted above.)* So during the silence the responder sends keepalives with TTL 2. They refresh its own router and die one hop later, long before the client's router. If one ever arrives, the run is inconclusive, so the assumption checks itself.

**Checked against real Linux NAT in namespaces** (`probe/inbound-rig.sh`), with a port-preserving client router timing out at 20 s and a port-randomising responder router at 12 s:

| Case | Result |
|---|---|
| 8 s silence | survived |
| 30 s silence | **expired**, which `--mapping-lifetime` could never report on that router |
| 16 s silence, keepalives on | survived |
| 16 s silence, keepalives off | expired, falsely |
| Keepalive TTL of 5 | caught, reported inconclusive |

**First result from the friend's router, 2026-11-14 01:53 UTC:** 60 s of silence, survived. The probe arrived 60,008 ms after the last packet diamond heard; all 3 flow echoes came back; local port 59228 came back unchanged as 75.118.77.81:59228. `summarise.py` now counts only evidence that can tell survival from expiry, and reports separately for each network a machine has been on. Until each site has confirmed evidence, it gives no mesh-wide keepalive interval.

**Final result at the friend's house — 138 runs, 2026-11-14 01:53 to 2026-11-16 23:38 UTC. The path into diamond forgets an idle two-way flow between 90 and 120 s, and the 120 s is diamond's own firewall, not the router. The router keeps the flow for at least 120 s and loses it by 180 s.**

| Silence | Survived | Expired | Where the probes stopped |
|---|---|---|---|
| 15 s | 17 | 0 | |
| 30 s | 10 | 0 | |
| 45 s | 12 | 0 | |
| 60 s | 15 | 0 | |
| **90 s** | **12** | **0** | |
| **120 s** | **0** | **11** | **at diamond's own firewall**, in all 11 |
| 180 s | 0 | 8 | before diamond: none arrived |
| 240 s | 0 | 16 | before diamond |
| 300 s | 0 | 14 | before diamond |
| 420 s | 0 | 12 | before diamond |
| 600 s | 0 | 11 | before diamond |

- **The correction, found the same evening.** For every 120 s expiry, diamond's kernel log holds three `[UFW BLOCK]` lines from `204.210.210.200:3479` to that run's port, a second apart, 6–9 s before the run's record: the three probes, delivered by the friend's router and refused by diamond. diamond's `nf_conntrack_udp_timeout_stream` is 120 s, Linux's default, and nothing in its ufw rules admits the test's random ports, so 120 s after the last packet the reply-only rule no longer matched. For every silence of 180 s or more, no probe reached diamond in any of 61 runs. That expiry is the router's. *(First recorded here, an hour earlier, as the router forgetting between 90 and 120 s. The "reading, not a finding" that 120 s is Linux's default was right about the number and wrong about whose Linux.)*
- **So the test measures the whole path in**, the host's own firewall included, and the probe cannot see where along the path a packet stopped. Only the kernel log showed it. A follow-up for the probe: record the client's `nf_conntrack_udp_timeout_stream`, so the summary can say when the host's limit, not the router's, is what was found. Measuring the router alone, between 120 and 180 s, needs the client's port admitted by its firewall; nothing now waits on that number.
- **Both limits are real for a player.** A node behind a host firewall that admits only replies, which is the default posture, meets the 120 s first. diamond's mesh port is admitted by rule (`setup.sh` opens 4001), so its node meets only the router's.
- **No run contradicts another, and none needed excusing.** Every run had all three flow echoes and no note. Every expiry was followed by a check that got through, from the same public address and port, so the path was up and only the flow's state was gone. No keepalive leaked.
- **The same at every time of day.** Each quarter of the UTC day shows the same split, so the household's evening traffic does not shorten it.
- **On time.** Every probe that got in arrived within 90 ms of the silence ending. The public address stayed 75.118.77.81, and the router kept the local port number, in all 138.
- **The outbound test, alongside, shows what it could not see.** Before the new probe went in, `--mapping-lifetime` on this router said "survived" 24 times at silences of 120 to 600 s, and the router does not survive 180 s or more.

`summarise.py` reports this site as forgetting "somewhere between 90s and 120s". That is true of the path the test used, and it is not the router. It still gives no mesh-wide keepalive: the home router has no confirmed evidence, because it is the responder's end and has never been measured from outside. Run E does not need it, because riverboat's port 4001 is held open by a NAT-PMP mapping rather than by traffic.

## Before Run E: what the review's second group changed — 2026-11-14

The external review of 2026-11-07 (`CODE_REVIEW_2026-11-07.md`, with its completion record) found faults that would have shaped Run E's numbers. They are fixed, and none of the earlier runs' numbers moved.

**Changes that matter for Run E:**

- **Only nodes holding the same job are candidates in the election.** Heartbeats now carry the job's code hash, and checkpoints name their job. Before this, a lower-identifier member with no job, or a different one, won every election and did nothing.
  - **Both boxes must run this build.** Checkpoints are a new message kind (9; kind 3 is retired), so an older node ignores them rather than misreading a job id as a tick. Two builds in one mesh would each run the job: no items are duplicated, because serials repeat, but no checkpoints are exchanged.
- **Every takeover names the owner it continued.** The summary matches each takeover to one disruption. Older logs fall back to timing, and are flagged when another disruption could have caused the takeover.
- **A takeover with no previous owner names nobody.** A claim at tick 0 by a node whose ledger is not empty says `cold-start`, because progress is not kept on disk: a Phase 0 limitation, said rather than hidden.
- **Production, checkpoints and ledger writes have stated limits and report their failures.** Before this, publish failures were dropped silently, a standby's failed ledger write was forgotten, and a torn ledger record corrupted every later append.
- **On diamond, the mesh node can use only ethernet and loopback** (`RestrictNetworkInterfaces=lo eno1`). This is the mirror of the tunnel's ban on eno1. Routing alone had not kept the node's multicast off LTE.

**The tools were re-checked against every earlier run.** The takeover summary now counts "kept working elsewhere" only when the tick count keeps its previous pace, uses each takeover once, and reads every rotation. On Night A, Night A run 2, Night B, Run C and Run D, the only difference in any output is the list of files read.

**Re-checked on the local rigs with the new build:**

| Rig | Result |
|---|---|
| `migration-rig.sh`, which now requires a resumed tick and fails on two takeovers | announced departure: took over in 2 ms, first tick at 150 ms; unannounced loss: took over in 1,942 ms, first tick at 542 ms |
| `chaos-rig.sh`, 15 min | 20 disruptions; the four counts that must stay zero all stayed zero; detection p95 2,499 ms; resume p95 201 ms; visible gap p95 1,199 ms (September's figure); 13 suspicions, all confirmed |

**The probe log had never rotated, on either box.** systemd creates the log as root, and the logrotate policy switched to `meshprobe` before opening it. On diamond, logrotate logged "Permission denied" every week. The policy is fixed, and `collect.sh` now reads rotations and adds to its archive, so the first real rotation cannot drop history from the dataset.

**To install before Run E:**
- the new node and probe on both boxes;
- the logrotate policy on both;
- the `lan-only` drop-in on diamond;
- a UDP port forward for 4001 to riverboat. riverboat has to be the reachable member, because the friend's router does not answer NAT-PMP. *(Not needed, found 2026-11-16: the home router answers NAT-PMP, and riverboat's node already holds 204.210.210.200:4001 that way, as diamond's did while it was at home. A mapping is also what a real player's node would get, where a hand-made forward is not.)*

The Run E handover will list these as commands.

## Provisioning

Implemented in `../provision/`. The design goal is **zero interaction**: the box boots, measures every thirty minutes, logs locally, and continues without anyone touching it. Remote access is for when something breaks.

Three configuration choices exist to protect the measurement rather than the machine, and should not be tidied away later:

- **Jitter on the timer**, so three boxes do not measure at the same instant and correlate their results.
- **No automatic reboot** for updates, because an unannounced reboot mid-measurement is indistinguishable from a node genuinely disappearing — the exact thing being measured.
- **Suspend masked**, for the same reason: a box asleep at 3am produces results identical to a network outage.

Household isolation (`--isolate-lan`) is opt-in, verifies internet reachability after applying its rules, and rolls itself back automatically on failure. A firewall rule that strands a box inside someone else's home is not remotely recoverable.

## What gets built, in order

1. **Connection probe** (above) — *done*. Ship to volunteers, collect results.
1a. **Mapping lifetime** — *done, then found blind on port-preserving routers (2026-11-14), and replaced by an inbound test.* How long a consumer router holds an idle mapping open before dropping it. Each run tests one idle interval chosen from a list, and the answer accumulates over days. Determines the keepalive interval the mesh will need, which is set by the **worst** router among the players rather than the average one. Both routers measured so far preserve ports, so only `--mapping-inbound` results count for them; see "Measuring idle timeouts properly".
2. **Local rig** — *done*. Several nodes forming an overlay, gossiping membership, and noticing departures. `node/rig.sh` runs it and checks the result. Processes rather than containers for now: they share one host and reach each other trivially, so this tests the logic and nothing about real networks.
3. **Job execution** — *done*. Sandboxed WebAssembly with a CPU ceiling, in `node/src/job.rs`. Sample jobs in `jobs/`, built by `jobs/build.sh`.
4. **Checkpoint and migrate** — *done*. A job whose host disappears is continued by a survivor, from the last checkpoint. `node/migration-rig.sh` runs it and checks the result.
4a. **Speculative resume** — *done, and measured on real boxes.* Run C (2026-09-24) put the visible gap at **1060 ms p95** against a 2651 ms baseline, with detection and resume unchanged. A standby begins continuing a suspected-gone owner's job from `detect/4` (500 ms) of silence, rather than waiting out the full detection window, so the visible gap should fall from the ~2.45 s Night A measured toward the resume budget alone. `node/migration-rig.sh` passes with speculation on (the default) and, now that `MESH_NODE_EXTRA` plumbs flags through to every node it starts, with `--speculate-after-ms 0` forcing it off — a flag that could not be turned off in the rig could not have been A/B tested on a box either. `sudo node/chaos-rig.sh 15` ran 17 disruptions (10 kill, 4 partition, 3 announced) with the same four counts at zero (orphaned, doubled, false alarms, unnoticed restarts) it has always required, plus a `SPECULATION` block of 14 started, 14 confirmed, 0 abandoned — exactly one suspicion per unannounced disruption and none during the announced ones, which is the answer to "does suspicion fire when nothing is wrong?" that loopback *can* give. The visible gap fell to p50 842 ms / p95 1199 ms: a real improvement on 2.45 s, but not the clean ~600 ms a 500 ms threshold implies, because the sweep that raises suspicion is periodic and can lag the threshold by up to its own interval before it fires. **These figures were re-derived from the same logs after a bug was fixed in the tool that produced them** (a takeover could borrow a later disruption's resume, which would have biased the gap optimistic). They did not move — not one number in this paragraph — because the rig fired its disruptions 33 s apart at the closest and the tool matches within a 30 s window, so no takeover here could reach the next one's evidence. That is the rig's pacing saving it, not the old tool being right. What loopback cannot answer — how often suspicion fires when nothing is wrong, *on a connection where things are sometimes slow or lost* — is what Run C was for. It ran on 2026-09-24: **0 false suspicions in 93**, but on two boxes sharing a switch, which has neither loss nor jitter, so that is the condition Run C could not make adverse. **Run D took it to a phone hotspot on 2026-10-10 and the condition became real: 36 wrong suspicions in 73**, exited by `checkpoint` 28 and `heard-again` 8 — the latter being the cause `a6cdc78` was written for, firing in the field for the first time. The gap held at 1076 ms p95 across two carriers against Run C's 1060 ms on one switch, and the whole of the wrong suspicion cost 0.29% of the run's ticks.
5. **Volunteer run** — *not run, and not currently possible.* The same binary on connections the operator does not own, measured against the stop criteria. **No volunteers are available** (2026-09-25), so this step has no path as written, and nothing that follows substitutes for it: three machines in one household, two of them on the same switch, are not ordinary people's machines on ordinary connections. Every conclusion Phase 0 reaches must say so.
5a. **Run D** — *done, 2026-10-10.* The nearest the available rig reaches: `diamond` on broadband and `yoinkerz` on a phone hotspot, two carriers, speculation on. It answered the one question Run C could not — false suspicion on a link that genuinely loses and delays packets — and re-tested the visible gap across a real internet path against Run B's pre-speculation 2566 ms, which held at **1076 ms p95**. It also **met two of its own pre-declared failure conditions**: 17 false alarms and 19 yields the partitions do not explain, at a measured cost of 0.29% of the run's ticks and nothing a player could see. **It does not answer step 5**, and no result in it should be read as if it did.
5b. **Run E** — *done, 2026-11-17.* `riverboat` at home against `diamond` at a friend's house, two households on cable broadband, 8.1 h, 120 disruptions. Every condition fixed before the run held: detection p95 2496 ms, resume p95 200 ms, visible gap p95 **1118 ms**, the four counts at zero, **0 false alarms**, 1 yield beyond the partitions, 4 wrong suspicions in 96, and 0.06% of ticks wasted. One prediction missed: RTT p99 was 276 ms against under 200 ms, nearly all from one 25-minute stretch on the path. Mesh traffic was 14 MB/h per box. **It does not answer step 5 either**: both machines are still the operator's.

## Run C: handover (2026-09-22) — run on 2026-09-24, results in the section after this one

**What this run tests.** Speculative resume passed both local rigs (4a, above) but loopback cannot answer the question that matters: how often suspicion fires when nothing is wrong, on a connection where things are sometimes slow or lost rather than perfectly reliable. Run C is that answer — the same eight-hour chaos run, on the same two wired boxes (diamond and riverboat), that Night A's second run (n=104) used, with speculation on, so the visible-gap measurement can be compared directly against that run's pre-speculation baseline. (Run B was two hours, and one of its two boxes was a phone hotspot — a different comparison, not this one.)

**What would count as failure.** Any of: a false suspicion rate so high it would visibly disrupt an unbroken connection; any orphaned, doubled, false-alarmed, or unnoticed-restart count above zero (the four counts every prior run has held at zero); detection or resume moving off their established p95s, which would mean speculation changed behaviour it was not supposed to touch. A visible gap that fails to improve on 2651 ms would not be a clean failure of the code — see the correction below — but it would be a failure of the run's reason for existing.

Judge against the spec's table, carried over verbatim:

| | Baseline (Night A run 2, n=104) | Run C |
|---|---|---|
| Detection p95 | 2461 ms | expect unchanged |
| Resume p95 | 200 ms | expect unchanged |
| **Visible gap p95** | **2651 ms** | **target: under 500 ms** |
| Orphaned / doubled / false alarm / unnoticed | 0 | must stay 0 |
| False suspicion rate | n/a | record it; it will not be zero |

**Correction to the 500 ms target — do not measure this away.** That number is the design document's target, carried into this table unchanged, and the local chaos rig (4a, above) has already shown it will not be met as written: `node/chaos-rig.sh 15` — a 15-minute run, not 15 disruptions — produced 17 disruptions (10 kill, 4 partition, 3 announced) with a visible gap of p50 842 ms / p95 1199 ms, not the ~500–600 ms the threshold implies — the same numbers after the summary tool was corrected and the run re-read through it (see 4a). The reason is structural, not a bug to fix before Run C: suspicion is raised by a sweep that runs every `detect/4` (the same 500 ms), not continuously, so the silence already elapsed when a disruption happens to fall right after a sweep can be anywhere from just past 500 ms to just under 1000 ms before the *next* sweep catches it and speculation begins. A 500 ms budget on a periodic check whose own period is 500 ms was never going to land on 500 ms at p95 — it lands somewhere in the sweep's own window, and the local rig already shows where. Run C is expected to reproduce that shape on real hardware, not to somehow meet the original number. The target stays as written above because this project fixes criteria before measuring.

**Decided 2026-09-23, before Run C: the ~1.2 s is accepted and the sweep is not being changed.** The alternative was to raise suspicion every `detect/8` and halve the jitter, and it was declined. So Run C is expected to miss the design's 500 ms gap target and is being run anyway, which is worth stating precisely because it is the kind of thing that later reads as a failure nobody noticed:

- **The threshold fixed on 2026-08-21 is resume, and it is met** — 200 ms at p95 across 119 unannounced losses, and unchanged by speculation. **The visible gap has never had a threshold**; every run so far has reported it beside the thresholds precisely because none was set for it.
- **The 500 ms gap figure is a target this design set for itself**, written the day before the rig ran. Missing it is a result about the design, not a failed criterion, and the number above is left exactly as it was written so the gap between what was predicted and what was measured stays legible.
- **What is being bought by not changing the sweep**: the shape Run C measures is the shape that has already been through review and both rigs. A trigger change on the eve of the run would mean measuring something nobody has exercised, and the run is expensive — eight hours, on the boxes, once.

So the honest summary of the expected outcome, before the run: **a player's stall goes from about 2.45 s to somewhere near 1.2 s**, which is most of the available win and not the whole of it, and the remainder is a known, cheap, deliberately deferred change.

**Re-staged 2026-09-24, because the first staging went stale underneath the handover.** `./release.sh` at commit `4435451`, built with `rustc 1.98.0 (88d9e12ae 2026-08-18)`, static (`x86_64-unknown-linux-musl`). `~/runc/` on both boxes holds `mesh-node`, `chaos.py`, `mesh-chaos.service`, `mesh-chaos.timer` and `timed-run.sh`; the staged `mesh-node`'s sha256 (`26c5697ab716586f379a3dcb15e44bd792db7993d2958f68b269fb72b66072c8`) matches `dist/mesh-node` on both, `--help` runs on both, and `chaos.py` matches the repo's by checksum.

**What the first staging would have measured.** It was built at `cf3f161` on 2026-09-22 at 20:49 EDT (sha256 `b0b9ea75…`), and three commits touched `node/src/main.rs` after it: `a6cdc78`, `0f286c5` and `60d75b8`. The binary confirms it rather than the log — `strings` finds `speculat` 16 times in it and `heard-again` and `not-ours` not at all. **`a6cdc78` is the one that matters**: it gives speculation an exit when a suspect is heard from again, and its own message says the lossy-link case it fixes is "the case a real home connection produces and the one loopback can never show, which makes it precisely what the next run exists to find." That next run is this one. Run C would have spent eight hours measuring a node without the fix written for the condition Run C exists to detect, and `takeover-summary.py` would have broken abandonment down by causes that binary cannot emit. The new binary carries both (`heard-again` 3, `not-ours` 1).

Nothing was wrong when this paragraph was first written at 20:51 — the node changed at 21:18 and again the following day. **The lesson is that a staged artefact records a commit, not a promise that the commit is still HEAD**, and the gap is invisible unless something compares them. Before any future run: check `dist/VERSION`'s commit against `git rev-parse HEAD`, and `git diff --stat <that commit>..HEAD -- node/`.

**State of the boxes at re-staging** (read-only; neither was reconfigured): both `active`, both still on `MESH=night-a` with matching `PEERS=` pointing at each other, both still running `/usr/local/bin/mesh-node` at sha256 `1f3d2344…`, an older build left in place rather than reverted. `mesh-chaos` is disarmed on both, and the mesh has been advancing the same job uninterrupted for over 40 hours. `timed-run.sh` keeps the current config and binary and restores them on `--revert`, so none of this blocks the run.

**A defect the run's first 86 seconds exposed, fixed in the tree but deliberately not staged.** When `timed-run.sh` stopped diamond's node to switch it to `run-c`, riverboat — still running the old build — logged `left  label=diamond  was_present_ms=148031679`: the two had been connected for **41.1 hours**, the longest undisturbed stretch any run has begun from. It then opened **16,873 connections in 86 seconds**, about 197 a second, writing 7.5 MB of log.

The cause is not speculation, and not new: the burst came from the *pre-speculation* binary. `dial_retry` is a 2-second `tokio::time::interval` whose arm is guarded by `!pending.is_empty()`, so while every peer is reachable nothing polls it, and tokio's default `MissedTickBehavior::Burst` keeps every tick that elapsed unwatched. 148,031,679 ms of health is **74,015 stored ticks**, released the instant a peer left. The healthier the mesh, the bigger the burst — which is why Night A and Run B looked clean at 1–3 connections: they began after runs full of disruptions, with backlogs seconds deep rather than days. Their low numbers were never evidence of a healthier code path.

On this LAN it cost log volume and one real blindness: the burst pushed the run's `started` line outside `chaos.py`'s 2 MB tail, so diamond misread `settled` as 41 hours until its first restart. On a volunteer's connection it would be worse — 74,000 dials at a NAT resembles a port scan and can exhaust a cheap router's connection table, and on a metered link an estimated 60–70 MB of QUIC handshakes is about an hour of the measured hotspot budget spent in 86 seconds.

The fix is one line — `set_missed_tick_behavior(Delay)`, so a lost peer is redialled once immediately and then every 2 seconds however long the mesh was healthy first — with a test that moves the clock 41.1 hours rather than waiting for it. **It is committed but not staged**: Run C finishes on the binary it started with, and `dist/VERSION` is deliberately left behind `HEAD` so the check above catches it before the next run.

**Commands the human runs** (both machines are wired, so `--not-a-hotspot` is honest — see the hotspot data-budget note, which is why this is never left running unattended past its `--hours`):

```
ssh -t rpc@192.168.50.48  'cd ~/runc && sudo ./timed-run.sh --label diamond   --mesh run-c --peer 192.168.50.128 --own --hours 8 --not-a-hotspot'
ssh -t rpc@192.168.50.128 'cd ~/runc && sudo ./timed-run.sh --label riverboat --mesh run-c --peer 192.168.50.48        --hours 8 --not-a-hotspot'
```

Collect and read afterward:

```
mkdir -p provision/results/run-c/{diamond,riverboat}
scp 'rpc@192.168.50.48:/var/log/mesh-node/*.jsonl*'  provision/results/run-c/diamond/
scp 'rpc@192.168.50.128:/var/log/mesh-node/*.jsonl*' provision/results/run-c/riverboat/
cd provision && python3 takeover-summary.py results/run-c/*/events.jsonl results/run-c/*/chaos.jsonl
```

The `*` after `.jsonl` is not decoration — a run this long can cross a mid-run log rotation, which has already bitten two runs on two different boxes at two different times: riverboat's at 20:01 in Night A's second run, diamond's at 00:02 UTC in Run B.

**The claim this run retracts, or confirms.** This plan has twice said the player waits detection plus resume — about 2.5 s after Night A's first run, about 2.45 s after its second — because speculative resume was not built. It is now built. Once Run C's numbers exist, this section (or the results section beside it) must say plainly whether the visible gap actually fell, and by how much, rather than leaving the old detection-plus-resume claim standing unqualified. That has not happened yet: no result from Run C appears anywhere in this document until the run has actually run.

## Run C: the player's wait, measured — 2026-09-24

`diamond` and `riverboat`, the same two wired boxes as Night A's second run, on mesh `run-c` with a ledger of its own, speculation on, chaos armed eight hours each. **118 disruptions, every one continued by the other machine**, and the four counts that must not move stayed at zero.

| | Baseline (Night A run 2, n=104) | **Run C (n=92)** | Judged |
|---|---|---|---|
| Detection p95 | 2461 ms | **2439 ms** | unchanged, as required — **2–3 s met** |
| Resume p95 | 200 ms | **200 ms** | unchanged, as required — **500 ms met** |
| **Visible gap p95** | **2651 ms** | **1060 ms** | target was under 500 ms — **missed, as predicted** |
| Visible gap p50 | 2450 ms | **763 ms** | |
| Orphaned / doubled / false alarm / unnoticed | 0 | **0** | held |
| False suspicion rate | n/a | **0 of 93 suspicions** | recorded |

**The claim this plan made twice is now retracted with a number.** It said the player waits detection plus resume — about 2.5 s after Night A's first run, about 2.45 s after its second — *because speculative resume was not built*. It is built, and the wait is **1060 ms at p95, 763 ms at p50**: the gap fell by **60%**, and it no longer contains detection at all. What a player waits through is now the silence before suspicion plus the resume, and the takeover arrives about 1.5 s *after* the work has already restarted.

**The 500 ms target was missed and the reason is the one written down before the run.** Suspicion is raised by a sweep running every `detect/4` — the same 500 ms as the budget — so the silence already elapsed when a disruption lands just after a sweep runs anywhere from 500 ms to just under 1 s before the next one catches it. A 500 ms budget on a 500 ms periodic check was never going to land at 500 ms at p95. The decision of 2026-09-23 not to sweep at `detect/8` was taken before the run and is unchanged by it; the remaining win is still a known, cheap, deferred change.

**The prediction was slightly conservative, and the rig slightly pessimistic.** Before the run this section said the stall would go "from about 2.45 s to somewhere near 1.2 s." Measured: 1.06 s. The local chaos rig had predicted p50 842 / p95 1199 ms; real hardware returned p50 763 / p95 1060. Loopback with compressed timings was the *harsher* estimate of the two, which is worth knowing the next time the rig is used to anticipate a box run.

**An announced departure is in a different class entirely**: resume p50 28–102 ms and a visible gap of p50 166–240 ms, because nothing has to be suspected or detected. The gap only exists when a node vanishes without saying so.

**Direction is not symmetric.** `diamond → riverboat` gaps run faster than the reverse at p50 (644 vs 859 ms on kills, 630 vs 810 ms on partitions) while the p95s are close. Both nodes sweep on the same 500 ms period but not in the same phase, and where a disruption lands in the victim's successor's sweep cycle decides how much of that window it waits. It is the jitter the section above describes, seen from one side.

### What zero false suspicions does and does not establish

93 suspicions, 92 confirmed by a real departure, **none abandoned**: speculation never once started work because of a peer that was fine. The single `unaccounted` is the startup stagger, where riverboat raised a suspicion one second into its own life while the two boxes were being armed 83 seconds apart.

**The bound this puts on the false-suspicion rate is weak, and the link is the easiest one this design will ever run on.** Zero events across 16 node-hours gives, by the rule of three, a 95% upper bound of roughly **one false suspicion per 5 node-hours** — not a small number for a player. And two boxes on one switch have essentially no loss and no jitter, while the case `a6cdc78` was written for is a suspect on a *lossy* link whose heartbeats arrive and whose checkpoints do not. **Run C could not produce that condition, so the honest reading is "not measurable on this rig," not "does not happen."** *(Run D produced it: 36 of 73, including the 8 `heard-again` exits this rig could not reach. See two sections down.)*

### What the next run should do differently

Three of the four things Run C measured were settled long before it ended. The visible gap p95 was within 35 ms of its final value after **forty minutes** (n=5) and moved between 1037 and 1135 ms for the remaining seven hours; detection and resume p95 never moved at all. Only the false-suspicion bound kept improving, and it improved on the wrong link.

- **Iterate on `node/chaos-rig.sh`, not the boxes.** It produced 17 disruptions in 15 minutes — about 68/hour against the box run's measured **14.9/hour** — needs no boxes, and its gap figures turned out to bracket the real ones.
- **When the boxes are used, make them show what only they can.** If the false-suspicion rate is the question, impair the link deliberately (`tc netem` loss and jitter on the mesh port) rather than waiting longer on a clean one. An hour of a degraded link answers what eight hours of a clean one cannot.
- **If more samples are wanted, shorten the fire interval.** Of 187 fires, 118 acted; 29 skips were the standby's own timer, 7 fell during partitions, and only **13 came from the settling gate** — so the 60-second settle is not the constraint, the 3–7 minute timer is.
- **A long run is still worth doing occasionally as a soak**, for drift that only appears over hours — memory, descriptors, ledger growth — but that does not need a disruption every four minutes.

## Run D: handover (2026-10-10) — run the same day, results in the section after this one

**What this run tests, and it is two things.** Run C put the visible gap at 1060 ms p95 but on two boxes sharing a switch: a link with no loss and no jitter, which is exactly the condition `a6cdc78`'s speculation exit was written for and exactly what Run C could not produce. Run D puts `yoinkerz` on a phone hotspot and `diamond` on home broadband, two carriers and no rented infrastructure, so that:

1. **The gap is re-measured across a real internet path**, against Run B's pre-speculation **2566 ms** on the same two machines and the same two carriers. Run B found the internet contributes nothing measurable to *detection*; whether that holds for a trigger that fires on 500 ms of silence is a different question, because 500 ms of silence is something a mobile path can produce on its own.
2. **False suspicion is measured on a link that genuinely loses and delays packets.** The hotspot is not a simulation of an impaired link, it is one. This is the only remaining question that could still change a conclusion.

**What would count as failure.** Any of the four counts leaving zero (orphaned, doubled, false-alarmed, unnoticed restart). Detection or resume moving off their established p95s. Or false suspicion causing **ownership churn** — yields beyond those the partitions explain — which is the form in which a wrong suspicion stops being free.

**There is no threshold for the false-suspicion rate and one is not being invented now.** A wrong suspicion costs ticks and not correctness: the same job from the same state makes the same items, so discarded work merges to nothing. What is recorded is the rate, the ticks wasted, and whether any of it reached the player as a change of owner.

| | Baseline | Run D | Prediction, fixed before the run |
|---|---|---|---|
| Detection p95 | 2456 ms (Run B) | | unchanged |
| Resume p95 | 200 ms (Run B) | | unchanged |
| **Visible gap p95** | **2566 ms** (Run B) | | **near Run C's 1060 ms** if the path contributes nothing |
| Orphaned / doubled / false alarm / unnoticed | 0 | | must stay 0 |
| **False suspicion** | 0 of 93 (Run C, on a switch) | | **expected non-zero** — and that is the point |
| Data on the phone | 121.6 MB / 2.1 h (Run B) | | ~120 MB at the measured 60 MB/h |

**Predicting a non-zero rate is deliberate.** If the hotspot returns zero too, that is a stronger result for the design than Run C's zero was, because the link had every opportunity. If it returns a high rate, the sweep's 500 ms trigger is too eager for a mobile path and the number says by how much.

**Staged and verified 2026-10-10.** `./release.sh` at commit `95d3723`, `rustc 1.98.0 (88d9e12ae 2026-08-18)`, static musl. `~/rund/` on `diamond` and `yoinkerz` holds `machine-elves/` (binaries, `SHA256SUMS`, `VERSION`, jobs) and `provision/`, with `mesh-node` in **both** places: `timed-run.sh` requires it beside itself. On both boxes the staged `mesh-node` is sha256 `18e3d3a3807104cc5306d2cd7d079753f35671f8a0141314cfa639ffb0efc747`, matching `dist/mesh-node`; checksums verify, `--help` runs, and `IPAccounting=yes` is present in the staged unit. **`yoinkerz` was still running `1f3d2344`, the pre-speculation build** — without this step Run D would have measured nothing new.

**Two setup faults from earlier runs, not to be repeated.**

- **Do not give `diamond` an address for `yoinkerz`.** Run B aimed **1,746 dial attempts** at the LAN address the laptop no longer had once it moved to the hotspot. `yoinkerz` behind carrier NAT cannot be dialled by anyone; it dials out and is observed. `diamond` needs no `PEERS` at all.
- **Start `diamond` first and read its address from its own log.** `timed-run.sh` restarts the node, which re-emits `port-mapped` because the event fires only when the mapping changes. Take the address from that line rather than from this document — it was `204.210.210.200:4001` on 2026-09-27 and an ISP may change it at any time.

**Two faults in the first version of this handover, found by running it.** Recorded because both were mine and both were silent until a command failed.

- **`setup.sh` is not needed and does not do what was claimed.** Its entire node section — unit file, config, logrotate, `mesh-node-run` — is gated on `--mesh`, which was not passed, so `--dist` installed the binaries and `VERSION` and nothing else. Passing `--mesh` would have been worse: it rewrites `/etc/mesh-node/config`, which `timed-run.sh` owns, so `--revert` would then restore a config `setup.sh` had invented rather than the one that was there. **`timed-run.sh` installs the binary itself**, so the only thing `setup.sh` was wanted for is the one line below.
- **`timed-run.sh` requires `mesh-node` beside itself** and says so, but the staging put the binary in `machine-elves/` to suit `setup.sh`. It failed with `missing beside me: mesh-node`. Run C's flat `~/runc/` had this right; the layout changed to serve a tool that turned out to be unnecessary.

A follow-up worth doing *after* this run, not before it: `timed-run.sh` could install `mesh-node.service` when one sits beside it, which would remove the manual step below. Changing that tool in the hour before a run is the same mistake in a different coat.

**Commands the human runs.** `riverboat` is stopped first so it stops dialling `diamond` on a mesh `diamond` has left; different meshes ignore each other, but its connection attempts land in the log being read.

```
ssh -t rpc@192.168.50.128 'sudo systemctl stop mesh-node'

ssh -t rpc@192.168.50.48 'sudo install -m 0644 ~/rund/provision/mesh-node.service /etc/systemd/system/mesh-node.service && sudo systemctl daemon-reload'
ssh -t rpc@192.168.50.48 'cd ~/rund/provision && sudo ./timed-run.sh --label diamond --mesh run-d --peer 127.0.0.1 --own --hours 2 --not-a-hotspot'
ssh rpc@192.168.50.48 'grep port-mapped /var/log/mesh-node/events.jsonl | tail -1'
```

The `install` line is the whole of what `IPAccounting` needs; on both boxes it is the only difference between the installed unit and the staged one. `timed-run.sh` restarts the unit, so the new setting takes effect without a separate restart.

The last command prints the address the laptop needs. `--peer 127.0.0.1` is a placeholder because `timed-run.sh` requires `--peer`: `diamond` is the reachable end and dials nobody. **Then move the laptop to the hotspot** and, from the laptop:

```
sudo install -m 0644 ~/rund/provision/mesh-node.service /etc/systemd/system/mesh-node.service && sudo systemctl daemon-reload
cd ~/rund/provision && sudo ./timed-run.sh --label yoinkerz --mesh run-d --peer <diamond's external IP> --hours 2
```

`--not-a-hotspot` is deliberately absent on `yoinkerz`: it *is* the hotspot, and `--hours 2` is inside the limit that flag guards. **This run is never left unattended past its hours** — 60 MB an hour is real money on a phone, and the two-hour box is the budget, not a target.

Afterwards:

```
mkdir -p provision/results/run-d/{diamond,yoinkerz}
scp 'rpc@192.168.50.48:/var/log/mesh-node/chaos.jsonl*'  provision/results/run-d/diamond/
scp 'rpc@192.168.50.48:/var/log/mesh-node/events.jsonl*' provision/results/run-d/diamond/
# yoinkerz once it is back on the LAN, chaos logs first, then events
cd provision && python3 takeover-summary.py results/run-d/*/events.jsonl results/run-d/*/chaos.jsonl
systemctl show mesh-node -p IPIngressBytes -p IPEgressBytes   # on each box, before reverting
```

Read `IPAccounting` **before** anything restarts the unit, because the counters reset with it. That is the number Run B could not get.

## Run D: false suspicion on a real link, measured — 2026-10-10

`diamond` on home broadband and `yoinkerz` on a phone hotspot, two carriers, no rented infrastructure, mesh `run-d`, speculation on, chaos armed 14:36–16:44 UTC. **23 disruptions, 2.1 h, and the job advanced 36,674 ticks without ever stopping.** Both questions the handover set were answered, and the second one answered against the prediction's hope rather than its fear.

| | Baseline | **Run D** | Prediction, fixed before the run | Judged |
|---|---|---|---|---|
| Detection p95 | 2456 ms (Run B) | **2463 ms** (n=20) | unchanged | held — **2–3 s met** |
| Resume p95 | 200 ms (Run B) | **200 ms** (n=18) | unchanged | held — **500 ms met** |
| **Visible gap p95** | **2566 ms** (Run B) | **1076 ms** (n=18) | near Run C's 1060 ms | **landed, 16 ms off** |
| Visible gap p50 | 2460 ms (Run B) | **831 ms** | | |
| Orphaned / doubled / unnoticed restart | 0 | **0** | must stay 0 | held |
| **False alarms** | 0 (Run B, Run C) | **17** | not predicted | **left zero** |
| **Yields beyond the partitions** | 0 (Run B), 1 (Run C) | **19** of 23 | the failure condition | **met** |
| False suspicion | 0 of 93 (Run C, on a switch) | **36 of 73** | expected non-zero | as predicted |
| Data on the phone | 121.6 MB / 2.1 h (Run B) | **not measurable** — see below | ~120 MB | not obtained |

### The 58% cut in the player's wait is not an artefact of the switch

Run C measured the gap at 1060 ms p95 on two boxes sharing a switch. Run D measures **1076 ms p95 across two carriers** — 16 ms apart, on a path whose round trip is 85 times longer. Against the same two machines' own pre-speculation 2566 ms, the player's wait fell by **58%**, and as in Run C it no longer contains detection at all: the takeover arrives about 1.4 s *after* the work has already restarted elsewhere.

This is the second time this plan has found the internet path contributing nothing measurable, and it is the stronger of the two. Run B's finding was about *detection*, a 2–3 s window that a 100 ms path disappears into. The gap is triggered by **500 ms of silence**, which is well inside what a mobile path can manufacture by itself — so there was a real reason to expect it to degrade, and it did not.

### Round trip, for the record

| | Run C (one switch) | **Run D (two carriers)** |
|---|---|---|
| RTT p50 | ~1 ms | **85 ms** |
| RTT p95 | | **192 ms** |
| RTT p99 | | **599 ms** |
| RTT max | | **666 ms** |

n=470 across both nodes inside the window. The tail matters more than the median: a single round trip at p99 is longer than the entire resume budget, which is why §"Two consequences of choosing 500 ms" insists the standby already hold the state. A design that fetched state after the failure would have missed the budget on this link in one hop.

### Two of the pre-declared failure conditions were met

The handover named three. It is worth quoting, because the temptation after a run is to re-read the condition until the result passes it:

> **What would count as failure.** Any of the four counts leaving zero (orphaned, doubled, false-alarmed, unnoticed restart). Detection or resume moving off their established p95s. Or false suspicion causing **ownership churn** — yields beyond those the partitions explain — which is the form in which a wrong suspicion stops being free.

**False alarms: 17.** A false alarm is a peer that went silent long enough to be declared gone and then came back *without having restarted* — it was never broken, the mesh only thought so. Chaos explains eight of the 25 returns in this run (4 partitions seen from both sides); the other 17 have no chaos action behind them at all. Time away, per observer: **p50 2546 ms, 11 of 17 over 2 s, 5 over 3 s, max 14,475 ms**, against a detection threshold of 2–3 s.

**Yields beyond the partitions: 19.** 23 yields where 4 are explained. A yield is how two nodes that both believe they own the job settle it, so this is the churn condition, by name.

**These are one phenomenon counted twice.** The hotspot produced dropouts of a third of a second to fourteen and a half seconds that no chaos action caused. Each long enough dropout is seen from both ends, both ends suspect the other, both ends start the job, and one of them yields. 17 false alarms and 19 excess yields are the two faces of roughly nine link events.

### What the churn actually cost, which is the number the decision needs

The condition is met and the mesh still never lost or doubled a tick. Every yield was settled by tick number, and the node that gave up was behind:

| | Run B (no speculation) | Run C (switch) | **Run D (hotspot)** |
|---|---|---|---|
| Yields | 5 | 27 | **23** |
| Tick deficit of the yielding node, p50 | 11 | 3 | **3** |
| Tick deficit, max | 12 | 5 | **5** |

- **Duplicated by yields:** 52 ticks.
- **Discarded by wrong suspicions:** 55 ticks across 36 abandonments (p50 **1**, p95 5, max 8).
- **Total wasted: 107 ticks of 36,674 — 0.29%.**

Speculation also *reduced* the cost of a yield, which was not something this plan predicted: Run B's five yields each threw away 9–12 ticks, Run C's and Run D's throw away 0–5. A standby that has been advancing the job all along is close behind when the two owners meet, so there is less to discard.

**So the honest statement is narrow and it is not reassuring by itself.** On a phone hotspot, the 2–3 s detection threshold thrashes — it migrates work for a peer that never left, about nine times in two hours, or **once every fourteen minutes**. What it cost here was 0.29% of the work and nothing a player could see, because this job is a deterministic simulation and §9.3 permits exactly this. The cost would not be 0.29% for work §9.3 excludes from speculation, and nothing in Run D measures that case.

### The first real false suspicions, and what they validate

Run C returned 0 abandoned of 93 suspicions and this plan said plainly that this was "not measurable on this rig," not "does not happen." Run D makes it measurable:

| | |
|---|---|
| started | **73** suspicions acted on before any decision |
| confirmed | **18** the owner really had gone |
| abandoned | **36** the suspicion was wrong |
| — `checkpoint`: the owner came back and kept the job | 28 |
| — `heard-again`: the suspect was alive, its checkpoints lost | 8 |
| ended by a yield | **12** two owners resolved instead |
| unaccounted | **7** ended without an event |

**`a6cdc78` has now run in the field in the condition it was written for.** That change gave speculation an exit for a suspect on a lossy link whose heartbeats arrive while its checkpoints do not — the `heard-again` cause, which fired 8 times here and could not fire once on a switch. The 28 `checkpoint` exits are the ordinary case: the owner reappeared with a newer checkpoint and the speculation stood itself down.

**A node-side gap the tool now papers over.** The 12 "ended by a yield" speculations emit no abandonment event — a yield stands the speculation down without one. The tool infers them by looking forward for the node's next decision; the node should say so itself. Until `535afb8` these 12 sat in `unaccounted`, where they read as 19 speculations that ended in silence.

### What the data figures are, and why there is no total

**There is no total for this run and the method is at fault, not the measurement.** `IPAccounting=yes` was added to the unit for exactly this, but systemd's counters reset when the unit restarts, and chaos restarted the node **10 times on `diamond` and 11 on `yoinkerz`**. The figures read after the run cover only each box's last segment:

| | bytes in | bytes out | segment | implies |
|---|---|---|---|---|
| `diamond` | 3,280,812 | 1,171,604 | last 27.0 min | ~9.9 MB/h |
| `yoinkerz` | 1,794,533 | 3,701,839 | last 30.8 min | ~10.7 MB/h |

Extrapolated, ~21 MB per node for the run — well under the 60 MB/h that Run B's phone-side figure implied, but **an extrapolation from a quarter of the run is not a measurement**, and the two cannot be compared anyway: Run B's 121.6 MB came from the phone's own counter, which totals everything the handset sent, while these count one service's sockets. The phone-side figure was not captured this time.

The fix is already written down as a follow-up: count on something that outlives a restart — a parent slice, or `nft` counters on UDP 4001 — rather than per-service accounting on a unit that chaos exists to kill.

### Two counts in this section were wrong when first read, and the correction moved nothing else

First read, Run D reported **2 orphaned jobs** and **19 unaccounted speculations**. Both were reporting faults in `takeover-summary.py`, fixed with tests in `535afb8`:

- The two "orphans" were disruptions of a node that believed it owned the job *while the other machine was already advancing it* — 300 ticks a minute, unbroken, straight through both. No takeover was needed because nothing had stopped. The summary now names it: `kept going: kill of yoinkerz — diamond never stopped working`.
- 12 of the 19 unaccounted speculations were the yields described above.

**Re-read through the corrected tool, Night A run 2, Run B and Run C did not move by one number.** That is the check `277f534` exists for: a tool fix that moves only the case it was written for is a fix, and one that moves everything is a new bug.

**One suspicion of mine about this section's largest number was wrong.** I thought the false-alarm count was inflated — that the partition excuse matched on *duration*, forgiving Run B's 37-second outages while counting Run D's 5-second ones. It does not; the excuse requires an actual partition action overlapping the return. Run B had 5 partitions × 2 sides = exactly its 10 returns, which is why its count is 0. Run D's 17 are real. The finding is larger than I first reported, not smaller.

### What Run D is not

**It is not step 5.** Two internet connections in one household, both machines owned and administered by the operator, one of them a laptop carried to a hotspot for the afternoon. It is the nearest the available rig reaches to a volunteer run and it is not one. **Phase 0 results remain an upper bound**, and this run moves the bound on one axis only: the link is now genuinely lossy, while the machines are still dedicated and still the operator's.

## Run E: handover (2026-11-16) — two households; run that night, results in the section after this one

**What this run tests.** The same takeover criterion as Runs B–D, for the first time between two households on ordinary broadband. `riverboat` is at home behind the home router (`204.210.210.200`); `diamond` is at a friend's house behind theirs (`75.118.77.81`). Run C had a switch between the boxes, and Run D had a phone hotspot. This is the case in between, and the one a real player is in. It is also:
- the first run with one box unattended in someone else's home, reachable only over its LTE back channel;
- the first with the review's second group live: the election counts only holders of the same job, checkpoints name their job, and every takeover names the owner it continued;
- the first with the 40 s keepalive. That should change nothing here, because heartbeats cross every second.

**What would count as failure,** unchanged from Run D:
- any of the four counts leaving zero (orphaned, doubled, false alarm, unnoticed restart);
- detection or resume moving off their p95s;
- ownership churn: yields beyond those the partitions explain.

| | Baseline | Run E | Prediction, fixed before the run |
|---|---|---|---|
| Detection p95 | 2463 ms (Run D) | | unchanged, inside 2–3 s |
| Resume p95 | 200 ms (Run D) | | unchanged, about 200 ms |
| **Visible gap p95** | 1060 ms (Run C), 1076 ms (Run D) | | **near 1060–1076 ms**: a third path, the same answer |
| Orphaned / doubled / unnoticed restart | 0 | | must stay 0 |
| **False alarms** | 0 (Runs B, C), 17 (Run D) | | **0**. Broadband should not produce Run D's multi-second dropouts. A real outage of either household's internet would show here as both sides losing each other with no chaos action behind it, and would be a finding about the link. |
| Yields beyond the partitions | 0 (Run B), 1 (Run C), 19 (Run D) | | **0–2** |
| **False suspicion** (abandoned of started) | 0 of 93 (Run C), 36 of 73 (Run D) | | **under 10%**, far nearer Run C than Run D |
| RTT p50 / p99, from the node's own `rtt` events | 85 / 599 ms (Run D) | | **p50 under 60 ms, p99 under 200 ms.** Two cable networks; the home router does not answer ICMP, so there is no earlier figure. |
| Mesh bytes, UDP 4001, per box | Run B ~60 MB/h (phone-side); Run D not measurable | | **10–60 MB/h**: well under 1 Mbit/s of either uplink |

About a hundred disruptions are expected: chaos fires every 3–7 minutes on each box, and acts only on the box running the job with a peer present.

**Staged and verified 2026-11-16.**
- **The build:** `./release.sh` at `974cf23`, `rustc 1.98.0 (88d9e12ae 2026-08-18)`, static musl. `mesh-node` sha256 `62ae4c69fefa42b87e24da349cbad275028f2a7620f44c0973533aafc7dfba1d`, `mesh-probe` `3226fdea…`.
- **What `~/rune/` holds on both boxes:** the two binaries, `timed-run.sh` and its chaos files, `mesh-node.service`, `logrotate.mesh-probe` and `mesh-node-lan-only.conf`, with a `SHA256SUMS` checked on each box. diamond's copy went over LTE in 24 s (11 MB compressed).
- **The job:** `factory_job.wasm` is already in place on both, sha256 `9300437c…`, the same as `dist/`.
- **Rigs on this build:** `migration-rig.sh` passes. `chaos-rig.sh`, 15 min: 20 disruptions, and the four counts that must stay zero all stayed zero. Detection p95 2,494 ms, resume p95 201 ms, visible gap p95 1,143 ms (1,199 ms on `d4739fb`). 17 suspicions, all confirmed. 3 yields, the 3 its partitions explain.

**Found while staging, and handled in the commands below:**
- **diamond's config had `JOB=""`** from the solo phase. `timed-run.sh` does not set the job. Under the new election a node without the job is never a candidate, so diamond would have been a spectator: every disruption of riverboat would have been an orphan. The commands save diamond's config first with `cp -n`, which `timed-run.sh`'s own `cp -n` then leaves alone, so `--revert` restores the solo config.
- **diamond's config has no `OWN=` line**, so `timed-run.sh`'s edit of it does nothing. The runner defaults to `no`, which is what diamond should be.
- **riverboat's unit predated `IPAccounting=yes`**: Run D's unit update reached diamond and yoinkerz only. It is installed here, though Run D showed its counters reset with every restart. **The byte count this run relies on is an `nft` counter table on UDP 4001**, the follow-up Run D wrote down. It only counts, its chains accept everything, and it is separate from chaos's `mesh_chaos` table. *(The first version written here put each chain on one line without a `;` before its closing brace, and `nft` rejected it on riverboat: untested, my error. Nothing was applied, because `nft -f` is all or nothing. The version below was checked with `nft -c`, applied, and seen to count on lightning first.)*
- **No router change is needed.** riverboat holds `204.210.210.200:4001` by NAT-PMP.

**Commands the human runs.** riverboat first, because it is the reachable end, and its address must be read from its own log.

On **riverboat** (`ssh -t rpc@192.168.50.128`):
```
cd ~/rune
sudo install -m 0755 mesh-probe /usr/local/bin/mesh-probe
sudo install -m 0644 logrotate.mesh-probe /etc/logrotate.d/mesh-probe
sudo install -m 0644 mesh-node.service /etc/systemd/system/mesh-node.service && sudo systemctl daemon-reload
sudo nft -f - <<'EOF'
table inet rune_bytes {
    chain in {
        type filter hook input priority 0; policy accept;
        udp dport 4001 counter
    }
    chain out {
        type filter hook output priority 0; policy accept;
        udp sport 4001 counter
    }
}
EOF
sudo ./timed-run.sh --label riverboat --mesh run-e --peer 127.0.0.1 --own --hours 8 --not-a-hotspot
grep port-mapped /var/log/mesh-node/events.jsonl | tail -1
```
- `--peer 127.0.0.1` is a placeholder, as in Run D: `timed-run.sh` requires a peer, and the reachable end dials nobody.
- The last line must say `"external":"204.210.210.200:4001"`. If the port is not 4001, stop: diamond dials 4001.

Then on **diamond** (from riverboat, `ssh -p 2201 rpc@127.0.0.1`):
```
cd ~/rune
sudo systemctl disable --now mesh-probe-inbound.timer
sudo install -m 0755 mesh-probe /usr/local/bin/mesh-probe
sudo install -m 0644 logrotate.mesh-probe /etc/logrotate.d/mesh-probe
sudo install -D -m 0644 mesh-node-lan-only.conf /etc/systemd/system/mesh-node.service.d/lan-only.conf && sudo systemctl daemon-reload
sudo cp -n /etc/mesh-node/config /etc/mesh-node/config.before-run-e
sudo sed -i 's|^JOB=.*|JOB=/var/lib/mesh-node/jobs/factory_job.wasm|' /etc/mesh-node/config
sudo nft -f - <<'EOF'
table inet rune_bytes {
    chain in {
        type filter hook input priority 0; policy accept;
        udp dport 4001 counter
    }
    chain out {
        type filter hook output priority 0; policy accept;
        udp sport 4001 counter
    }
}
EOF
sudo ./timed-run.sh --label diamond --mesh run-e --peer 204.210.210.200 --hours 8 --not-a-hotspot
```
- Its wait for the peer should end in `joined`.
- The idle-timeout test is switched off because it has finished its job.
- The drop-in confines the node to ethernet and loopback, and takes effect when `timed-run.sh` restarts it.

**During the run** nothing needs to be done at either box. Reading diamond's logs over the back channel is allowed, because management stays off the measured path. If the modem hangs, the recovery added in `0020abb` meets its first real failure.

**Afterwards** — collection, the summary and the byte counts are mine:
```
mkdir -p provision/results/run-e/{diamond,riverboat}
# chaos logs, then events, from each box; diamond's compressed for the LTE link
cd provision && python3 takeover-summary.py results/run-e/*/events.jsonl results/run-e/*/chaos.jsonl
sudo nft list table inet rune_bytes      # on each box, then: sudo nft delete table inet rune_bytes
```
What each box runs after the run is a separate decision. `--revert` would put riverboat back on Run C's config, which dials diamond's old home address, and put diamond back on its pre-review binary. Neither is wanted.

## Run E: two households, measured — 2026-11-17

`riverboat` at home and `diamond` at a friend's house, two households on ordinary cable broadband, no rented infrastructure, mesh `run-e`. Chaos was armed from 00:50 to 08:56 UTC (8.1 h). **There were 120 disruptions, and the job advanced 145,009 ticks without stopping.** None of the failure conditions fixed in the handover was met. One of my own predictions missed: the tail of the round-trip times.

| | Baseline | **Run E** | Prediction, fixed before the run | Judged |
|---|---|---|---|---|
| Detection p95 | 2463 ms (Run D) | **2496 ms** (n=92) | unchanged, inside 2–3 s | held — **2–3 s met** |
| Resume p95 | 200 ms (Run D) | **200 ms** (n=92) | unchanged, about 200 ms | held — **500 ms met** |
| **Visible gap p95** | 1060 ms (Run C), 1076 ms (Run D) | **1118 ms** (n=92) | near 1060–1076 ms | **landed**, 42 ms above Run D |
| Visible gap p50 | 831 ms (Run D) | **823 ms** | | |
| Orphaned / doubled / unnoticed restart | 0 | **0 / 0 / 0** of 120 | must stay 0 | held |
| False alarms | 17 (Run D) | **0** | 0 | held |
| Yields beyond the partitions | 19 (Run D) | **1** (23 yields, 22 explained) | 0–2 | held |
| False suspicion | 36 of 73 (Run D) | **4 of 96** (4%) | under 10% | held |
| RTT p50 | 85 ms (Run D) | **41 ms** (n=2,361) | under 60 ms | held |
| **RTT p99** | 599 ms (Run D) | **276 ms** | under 200 ms | **missed** |
| Mesh bytes, UDP 4001, per box | not measurable (Run D) | **14.1 and 14.5 MB/h** | 10–60 MB/h | held, at the low end |

- **The disruptions:** 70 kills, 22 partitions and 28 clean shutdowns. diamond took over 63 times and riverboat 57.
- **Skipped:** 68 chaos fires, because the box was standby, had no peer, or had not settled.
- **Worst cases:** every category in both directions met both thresholds. The worst single detection was 2,499 ms, and the worst resume 200 ms.

### The third path gives the same answer

The visible gap p95 on each path so far:
- 1060 ms on a switch (Run C);
- 1076 ms across two carriers, one of them a phone hotspot (Run D);
- **1118 ms between two households** (Run E).

`chaos-rig.sh` on this same build, on loopback, gave 1143 ms. So the internet between two houses added nothing that the build does not already show without a network. The 42 ms over Run D comes with the build, not the path. That rig sample was small (n=20).

### The round trip, and the stretch that missed

| | Run D (two carriers, hotspot) | **Run E (two households)** |
|---|---|---|
| RTT p50 | 85 ms | **41 ms** |
| RTT p95 | 192 ms | **53 ms** |
| RTT p99 | 599 ms | **276 ms** |
| RTT max | 666 ms | **881 ms** |

These are the node's own `rtt` events, n=2,361 across both nodes inside the armed window.

**The p99 miss comes almost entirely from one stretch, 01:15–01:41 UTC** (20:15–20:41 at home):
- Both nodes measured round trips of 200–881 ms at the same moments. The median inside the stretch stayed at 43 ms.
- 31 of the run's 33 samples over 200 ms fall inside it.
- Outside it, p99 is **59 ms** (n=2,236).

**The prediction missed, and the split does not change that.** It said p99 under 200 ms over the run, and the run's p99 is 276 ms. The split explains the miss; it does not undo it. An evening on household broadband contains stretches like this one, which is part of why a real evening was worth measuring.

- **The mesh did not cause it.** Repair traffic inside those 25 minutes was in the same range as at any other time: 0–82 items per node per 10 minutes. The run averaged 31 kbit/s per box.
- **The logs cannot say which household's link it was.** All three of the run's abandoned suspicions fell inside the stretch, at 01:38–01:40, and all three were riverboat suspecting diamond. So at least diamond's messages were arriving late, which means either the friend's upload or the home download. One guess is household traffic queueing on someone's router, but it is only a guess.
- **What it cost:** three wrong suspicions, 3 ticks discarded, and no false alarm. No peer was declared gone that had not gone. Every takeover inside the stretch met both thresholds.

### What wrong suspicion cost

| | |
|---|---|
| started | **96** suspicions acted on before any decision |
| confirmed | **92** the owner really had gone |
| abandoned | **3**, all `checkpoint`: the owner came back and kept the job. 1, 2 and 0 ticks discarded |
| ended by a yield | **1** |
| unaccounted | **0** |

**The one yield without a partition behind it was a one-second hiccup on the path.** At 08:20:46, 138 s after the last chaos action and with nothing restarting, diamond heard nothing from riverboat for 997 ms. It started a speculative run at tick 135,043 and yielded 153 ms later, when riverboat's tick 135,045 arrived. riverboat's `working` line for that minute shows 300 ticks, unbroken.

**The other 22 yields each came within a second of a partition ending.** The yielding node was behind by 3 ticks at p50 and 5 at most, the same as Run D.

- **Duplicated by yields:** 80 ticks.
- **Discarded by wrong suspicions:** 3 ticks.
- **Total wasted: 83 ticks of 145,009, which is 0.06%.** Run D's was 0.29%.

**On household broadband, the 2–3 s window did not thrash.** Run D found it migrating work for a peer that never left about once every fourteen minutes on a phone hotspot. Here there were no false alarms in 8.1 hours. One night between one pair of households cannot set a rate, but it does place Run D's thrashing on the hotspot, not on internet paths in general.

### Data

| | in | out | slice | per box |
|---|---|---|---|---|
| `riverboat` | 54.8 MB | 58.0 MB | 7.99 h | **14.1 MB/h** (31 kbit/s) |
| `diamond` | 57.9 MB | 57.1 MB | 7.94 h | **14.5 MB/h** (32 kbit/s) |

**This is the first total any run has produced. It comes from the `nft` counter table, which outlives restarts.**
- **When the counters ran:** the tables were created at 00:49:34 (riverboat) and 00:54:05 (diamond), and read at about 17:30 on 2026-11-18.
- **How the run's share is separated:** the counters kept counting after the run. So the run's share is the counter minus systemd's `IPAccounting` for the node's last start, interpolated to the minute the counters were read. Those last starts were at 08:48:58 and 08:50:28, four and six minutes before each box's chaos disarmed (08:53:03 and 08:56:24).
- **What the slice covers:** it runs from each counter's creation to that last start, 7.99 h and 7.94 h of the 8.1 h armed.
- **After the run**, with one steady owner and nothing disrupting it, each box used **10.8 MB/h** over 32.7 h. The owner sent 8.3 MB/h (18 kbit/s) and the standby 2.5 MB/h. Chaos adds about a third.
- **The two ends agree to within 4%.** riverboat sent 58.0 MB, and diamond counted 57.9 MB in. diamond sent 57.1 MB, and riverboat counted 54.8 MB in. Part of the gap is how the counters are placed: chaos's partition table drops at priority −10, ahead of the counter at 0, so packets a partition dropped were never counted on the partitioned side. riverboat was partitioned 14 times, diamond 8. The rest is whatever the path lost. The per-box rates do not depend on this.
- **Against the uplinks:** about 16 kbit/s each way is far below a cable uplink, and the friend's connection is uncapped. **Peaks were not measured.** The counters give totals, not bursts.

### The back channel

The link stayed up through the run and logged nothing from 00:40 to 14:25 UTC. In the 27 hours after the run, the modem failed five times. Each recovered with nobody at the box. See "diamond's LTE back channel", where they are recorded.

### What Run E is not

**It is not step 5.** It is the first run between two households, and the link is the kind a real player has. But both machines are the operator's own dedicated, wired, always-on Ubuntu Server boxes, and the data is one night on one pair of cable connections 41 ms apart. **Phase 0 results remain an upper bound.** This run moves the bound on one axis: the mesh met every criterion fixed before the run on an ordinary path between two homes.

## Run F: handover (2026-11-18) — three connections, and the reachable one leaves; run the same day, results below

**What this run tests.** Every internet run so far had two machines, and one of them was reachable. "One reachable member is enough" holds only while that member stays. Run F has three machines on three connections, only one of them reachable:
- **`riverboat`, at home:** reachable. It holds `204.210.210.200:4001` by NAT-PMP.
- **`diamond`, at the friend's house:** the router preserves ports, does not answer NAT-PMP, and admits nothing unsolicited.
- **`yoinkerz`, on the phone hotspot:** carrier NAT. A new mapping gets a new port, but while a mapping lasts it uses the same port for every destination.

**The node has no relay and no hole-punching protocol.** What it has is cruder. Every node announces the address it sees each peer at: once on first connection, and every 30 s after. A node that hears of a peer it is not connected to dials it at once, then every 2 s. So diamond and yoinkerz each learn the other's public address through riverboat, and both dial. Both NATs keep one public port per socket, which is the condition simultaneous dialling needs.

The run asks two questions:
1. Is that enough to connect the two unreachable machines directly?
2. What happens to them when riverboat leaves?

**Three parts, 2 h in all**, the hotspot limit `timed-run.sh` enforces:

| | from yoinkerz's start | |
|---|---|---|
| 1 | 0:00–0:45 | All three, chaos on all three. The first three-node run across the internet: when the owner goes, two survivors must settle on one successor. |
| 2 | 0:45–1:40 | **riverboat leaves:** its chaos is disarmed and its node stopped. Chaos continues on diamond and yoinkerz. |
| 3 | 1:40–2:00 | riverboat returns, with chaos left off on it. Chaos disarms itself on the other two at 2:00. |

**Two facts about the node decide the second question:**
- **Memory across a restart:** a node keeps no record of its peers. It restarts knowing only its configured peer, which for both diamond and yoinkerz is riverboat.
- **Claiming:** only riverboat runs with `--own`. So a node that comes up and finds nobody waits; it never starts a second copy of the job.

| Question | Prediction, fixed before the run |
|---|---|
| **Do diamond and yoinkerz connect directly** while riverboat is up? | **Yes**, within a minute of yoinkerz starting. The evidence is a `connected` event on diamond naming the carrier's address. `joined` is not evidence: heartbeats reach a node through riverboat without any direct link. |
| Part 1: orphaned, doubled, false alarm, unnoticed restart | 0 |
| Part 1: detection and resume p95 | inside 2–3 s and 500 ms |
| When riverboat leaves, does the pair stay connected? | Yes. The link is direct, so losing riverboat does not touch it. |
| **After a restart in part 2, does the restarted node find its peer again?** | **Mostly.** The restarted node knows only riverboat, so re-meeting is up to the survivor. The survivor redials any address it once dialled successfully. Both routers should still hold the old connection's state, since a restart takes seconds and the routers keep idle flows for minutes. **Where the survivor only ever accepted the connection, it has no address to redial.** The restarted node then waits, idle, until riverboat returns. |
| Part 2: two owners at once | 0, because only riverboat claims on its own |
| Part 2: a partition of one of the pair | survives: the connection's idle timeout (120 s) outlasts the longest partition (60 s) |
| riverboat returns | rejoins within seconds, because both nodes are still dialling its configured address. It stands by, and does not claim. |
| yoinkerz's mesh bytes (UDP 4001, nft) | **40–70 MB for the run.** With three nodes, each standby receives the owner's messages twice, once directly and once forwarded. Run E's 14 MB/h for two nodes becomes about 25 MB/h here, and about 14 MB/h in part 2. |
| The phone's own counter | **under 150 MB**, the mesh plus the laptop's own traffic, with snap refreshes held |

**What would mean building relaying and coordinated punching before Phase 1:**
- **the pair never connecting directly.** Then the mesh hangs off riverboat, and losing riverboat ends the pair's redundancy at once;
- **a restarted node that never finds its peer again** while riverboat is away. That is a mesh that heals only while it has a reachable member, which is the case the plan calls "the case that decides Phase 0".

The four counts leaving zero would be a failure in their own right, as in every earlier run.

**Staged 2026-11-18.**
- **The build:** the same `974cf23` build as Run E, on all three boxes (`mesh-node` sha256 `62ae4c69…`). yoinkerz's `~/rune` was copied from riverboat's staged set and checked against its `SHA256SUMS`.
- **yoinkerz's node:** its installed unit already matches the staged one (`IPAccounting=yes`, no interface restriction, which the hotspot needs). Its job file is the same `9300437c…`. It was still running Run D's config and older binary, and is stopped before it leaves home Wi-Fi.
- **Found while staging: yoinkerz's clock.** At boot it was **five hours behind**, at 13:01 UTC against 18:01. Its hardware clock is kept in local time, and NTP corrected the system clock four minutes after boot. The summary matches a chaos action on one machine to a takeover on another within 30 s, so a run started before that sync would have matched nothing. The command block checks for sync first.
- **The bytes:** the `rune_bytes` nft table goes back on all three boxes, as in Run E. riverboat's and diamond's were deleted after Run E was read.

**Started 2026-11-18:** riverboat armed at 18:27:39 UTC, diamond at 18:29:24 and yoinkerz at 18:31:39. riverboat holds `204.210.210.200:4001` again (`as_asked` true). yoinkerz appears on the carrier as `70.242.158.221:5465`: the carrier does not preserve the port.

### Corrected before part 2, at 18:39 UTC

**The first prediction is failing, and the table's reasoning about two owners was wrong.** Both corrections are written down before riverboat leaves, so that part 2 tests them rather than being explained by them.

- **No direct link has formed.** diamond heard yoinkerz's address from riverboat at 18:31:38 and has dialled it every 2 s since, about 180 attempts in ten minutes. Not one connected. diamond's node has had no incoming connection at all, and its kernel log has no packet from the carrier's address. diamond "sees" yoinkerz only because riverboat forwards its messages, which is what `joined` without `connected` means. Why is not yet known. One possibility is a carrier that gives each destination its own port while the friend's router admits only the exact address and port it sent to. The other is that yoinkerz never dialled diamond. yoinkerz's log will say which.
- **"Two owners at once: 0, because only riverboat claims on its own" confused claiming with taking over.** `--own` governs only the claim a node makes at startup when nobody answers. Any node holding the job takes over when the owner goes silent, and that is the whole mechanism Runs B–E measured. So with no direct link, losing riverboat splits the mesh in two:
  - each side hears nothing from the other;
  - whichever of the pair is standby takes over;
  - **the job runs twice, once on each side, for all of part 2.**
- **This has already happened in miniature.** At 18:32:28 yoinkerz's link to riverboat dropped for about a second. yoinkerz lost diamond with it, because diamond's messages reach yoinkerz only through riverboat. riverboat then logged `job-owner yoinkerz` at 18:32:29.680 and `job-owner diamond` 50 ms later: yoinkerz briefly ran the job as owner and stood down when diamond's state reached it again.

**Revised predictions for parts 2 and 3:**

| Question | Revised prediction |
|---|---|
| When riverboat leaves | diamond and yoinkerz each see the other vanish within about 2.5 s of riverboat's departure, and **both run the job**. The standby of the two takes over. |
| Chaos on the pair in part 2 | almost nothing fires: chaos acts only on an owner with a peer present, and neither will have one |
| The restart questions in the first table | not reached, for the same reason |
| riverboat returns | both dial it, so it connects both sides again. **One of the two owners yields to the other**, by tick, within seconds. The job's items merge to nothing duplicated, because the same job from the same state makes the same serials. |
| What part 2 costs | 55 minutes of the job computed twice. That is free for this deterministic job and would not be free for work §9.3 excludes from speculation. |

If this is what happens, it is the first of the two outcomes the handover named: **the pair never connecting directly, so the mesh hangs off its one reachable member.** That means building relaying and coordinated punching before Phase 1.

## Run F: three connections, measured — 2026-11-18

riverboat at home (reachable), diamond at the friend's house, yoinkerz on the phone hotspot. Mesh `run-f`, chaos armed 18:27–20:31 UTC. riverboat left at 19:16:40 and came back at 20:14:08. There were 13 disruptions. **The two unreachable machines never connected to each other. When riverboat left, the mesh split and ran the job twice for 57 minutes, and it healed 1.3 s after riverboat returned.** That is the first of the two outcomes the handover named as meaning relaying and coordinated punching must be built before Phase 1.

| | Prediction | Run F | |
|---|---|---|---|
| diamond and yoinkerz connect directly | yes | **never, in over two hours** | **missed** |
| Detection / resume p95 | inside 2–3 s / 500 ms | **2489 / 200 ms** (n=9 / 8) | held, on a small sample |
| Visible gap p95 | | 1188 ms (n=8) | |
| Nobody took over / more than one took over | 0 / 0 | **0 of 13 / 0** | held |
| False alarms | 0 | **27** | **missed**: the hotspot, and the split (below) |
| Yields | | **21**, of which the partitions explain 3 | churn, as in Run D |
| When riverboat leaves | the pair stays connected; corrected before part 2 to **the mesh splits and both run the job** | **split: both ran the job for 57 min** | the correction held |
| Re-meeting after a restart in part 2 | "mostly"; corrected to not reached | not reached: chaos found no peer at every fire | as corrected |
| riverboat returns | one owner yields within seconds | **1.3 s** | held |
| yoinkerz's mesh bytes | 40–70 MB | **65.4 MB**, about two-thirds of it failed dialling | inside the range, for the wrong reason |
| The phone's own counter | under 150 MB | not read: no reading was taken before the run | not obtained |

### The two unreachable machines never met, and neither NAT's type explains it

- **Both learned the other's address within 21 s.** riverboat's announcements reached diamond at 18:31:38 and yoinkerz at 18:31:59. Both dialled every 2 s from then until the end.
- **Both sent correctly, and neither received anything.** Packet captures at about 20:42 UTC on both ends show each node sending from its listening port 4001 to the other's correct public address: diamond to `70.242.158.221:5465`, yoinkerz to `75.118.77.81:4001`. Each sent about 6 packets of 1,228 bytes every 2 s, and neither capture holds a single packet from the other side. yoinkerz's firewall admits 4001, and diamond's does too.
- **Both NATs are the friendly kind.** The probe, run on each after the run, reports both as `nat-endpoint-independent`: the same public port for every destination. That is the condition hole punching needs, and here it was not sufficient. **The packets were lost between the two routers.** From the two ends it is not possible to say whose equipment dropped them.

Two corrections to things said during the run:
- **"Corrected before part 2" cited diamond's kernel log** as showing no packet from the carrier. That log could not have shown one: ufw admits 4001 and logs only what it blocks. The capture is the evidence, and it says the same thing.
- **I suggested during the run that the probe's August hotspot result might be the old comparison of ports alone.** It was not: the corrected probe gives the same answer, endpoint-independent.

### Part 2: the mesh split in two, and both halves ran the job

| UTC | |
|---|---|
| 19:16:40.155 | riverboat leaves, announcing it |
| 19:16:41.103 | yoinkerz hears nothing from diamond for 979 ms and starts the job speculatively |
| 19:16:42.603 | yoinkerz **takes over**, while diamond is still running the job |
| 19:17–20:14 | **both run the job for 57 minutes**: diamond tick 15,203 → 32,003, yoinkerz 15,347 → 32,147. About 16,800 ticks were computed twice. |
| 20:14:08.764 | riverboat starts. diamond reaches it 45 ms later, yoinkerz 0.9 s later. |
| 20:14:10.027 | yoinkerz hears diamond through riverboat and **yields**, 5 ticks behind |

- **For this job the split cost compute and nothing else.** The same job from the same state makes the same serials, so the two halves' inventories merge without duplicates. That holds for a deterministic simulation and would not hold for work §9.3 excludes from speculation, nor for anything a player can see happening in two places at once.
- **Chaos could not act on the pair in part 2.** diamond's chaos found "no peer to take the job over" at all 13 of its fires.
- **Healing took 1.3 s** once a reachable member was back, with no intervention.

### Part 1: three nodes, and a hub that is also a single point of failure

The takeover thresholds held, though on 9 unannounced losses rather than Run E's 92. Nobody failed to take over, and no disruption was taken over twice. The churn is the hotspot's, as in Run D, made worse by the topology:
- **27 false alarms and 18 yields beyond the partitions.** Two of the false alarms are the part-2 split itself, seen from each side. Run D's hotspot gave 17 false alarms in 2.1 h.
- **Each drop of the hotspot's one link cut yoinkerz off from both peers at once**, because it reached diamond only through riverboat. So each drop briefly made two owners, as at 18:32:28, and each ended in a yield.
- **One disruption hit a second owner.** The kill of yoinkerz at 19:33:26 hit a node that believed it owned the job while diamond was also running it.
- **Speculation:** 43 started, 8 confirmed, 17 abandoned (13 `checkpoint`, 4 `heard-again`), 9 ended by a yield, and **9 unaccounted**. Run E had none unaccounted, and these 9 are not yet investigated.

### The bytes, and a cost nobody predicted

| | in | out | counted over |
|---|---|---|---|
| `riverboat` | 46.7 MB | 29.7 MB | 2.3 h, including 55 min stopped while both others dialled it |
| `diamond` | 14.5 MB | 52.8 MB | 2.2 h |
| `yoinkerz` (hotspot) | 14.4 MB | 50.9 MB | 2.2 h; **65.4 MB** in all |

**A node dials every address it cannot reach every 2 s, indefinitely.** At the captured rate, 6 packets of 1,228 bytes every 2 s, that is **about 13 MB/h for each unreachable address**.
- **How much of each box's traffic it was:** the node's own `dialling` lines put diamond and yoinkerz at 3.2 address-hours each. At that rate that is about 42–43 MB, roughly four-fifths of each box's outgoing bytes, against 14.5 MB of real traffic received.
- **How firm the figure is:** the rate rests on two 3-second captures, so it is approximate. That the retries dominated is not in doubt.
- **On a metered link it matters:** about two-thirds of the laptop's 65.4 MB was spent dialling an address that never answered.

### What Run F settles

- **"One reachable member is enough" is true only while that member stays.** Without a relay, two unreachable members reach each other only through it. When it leaves, the mesh splits, and every split is a second owner. That held here for 57 minutes, and in miniature at every drop of the hotspot.
- **Hole punching cannot be assumed, even between two endpoint-independent NATs.** So relaying through a reachable member is not an optimisation. It is the fallback the mesh needs, with coordinated punching as the upgrade when a punch works.
- **Healing is fast once a reachable member returns:** 1.3 s, by tick number, with nothing lost.
- **Retries need a backoff.** 13 MB/h per unreachable address, for as long as the node runs, is a real cost on a metered link and noise at a router.

**What Run F is not.** It is one pair of routers and one carrier, on one afternoon. It shows that punching *can* fail between friendly NATs, not how often it does. The machines are still the operator's, and Phase 0 results remain an upper bound.

Logs: `provision/results/run-f/`, not committed, as for every run.

## Open decisions

- **The rendezvous fallback**, if no volunteer turns out to be directly reachable. Manual invite exchange covers address distribution but not simultaneous-transmission timing. **Run F (2026-11-18) made this concrete:** two unreachable members never connected to each other, even though both NATs are endpoint-independent and both dialled the right address for two hours. With no relay, losing the one reachable member split the mesh and ran the job twice for 57 minutes. A relay on a reachable member, with coordinated punching as the upgrade, is now a requirement rather than a fallback to decide on.
- ~~**The threshold for an acceptable stall**~~ — settled 2026-08-21: 2–3 s detection, 500 ms resume, at p95.
- **How many volunteers** the go/no-go can honestly rest on, given three is enough to detect a problem class but not to estimate its frequency.
- **Whether the detection window should be fixed or per-link.** Run D showed 2–3 s thrashing on a phone hotspot — work migrating for a peer that never left, once every fourteen minutes. The threshold stays as fixed on 2026-08-21, because changing a criterion after seeing the measurement is the thing this plan refuses to do. But the measurement is now in hand, and the question it raises is open: a single global window is set by the worst link any player has, the same shape of problem as the keepalive interval in step 1a. Deciding it needs a case where wrong suspicion is *not* free — work §9.3 excludes from speculation — and Run D did not measure that. Run E found no thrashing on household broadband: 0 false alarms in 8.1 h. On the evidence so far, then, the link that sets a global window is a mobile one.
