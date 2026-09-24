# Phase 0 — Mesh Spike

**Status:** Planning. No code yet.
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
| **Detection** | 2–3 s of silence | How long a node may go quiet before it is presumed gone. This is the dip tolerance — set too low, the mesh thrashes, migrating work every time a wifi link stutters. |
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

The keepalive the mesh may use is governed by the worst connection among the players rather than the average, so the mobile figure decides it: **120 s confirmed safe, halved to 60 s**, because the measured value is where the mapping was still alive rather than where it dies, and a keepalive that only just makes it is one dropped packet away from not making it.

**Shipped: 55 s**, leaving better than a twofold margin on the shorter of the two connections measured.

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

### Still owed

**The mapping expires in an hour.** The node must renew it, the way it already keeps NAT mappings alive with keepalives. Until it does, a long-running node silently stops being reachable, which is precisely the failure that is hardest to notice.

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

**Both thresholds are met, and the player still waits about 2.5 s.** The gap is detection plus resume, and this plan said from the start that the 500 ms budget only holds if a standby begins continuing while the original is merely *suspected* gone. Speculation is not built. So the honest statement of this result is: **the mechanism works and is within its own budget; the experience it produces is not yet the one the budget was chosen to deliver.** Building speculative resume and re-running the same measurement is the way to close that, and the before is now on record.

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

**The conclusion is unchanged, and it is the one that matters.** Both thresholds are met and **the player still waits about 2.45 s**, because the gap is detection plus resume and speculative resume from a warm standby — which this plan always said the 500 ms budget depended on — is still not built. A second, larger measurement of the same mechanism does not move that; it only removes "the sample was too small" as a reason to doubt it. The before is on record twice now.

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

### On the volunteer's household

The box sits on someone else's home network. **Firewall it away from their LAN**, permitting only outbound internet and the mesh ports. This is partly courtesy and partly correctness: a friend's other devices are not part of the experiment, and their household should not have to think about what the box can see.

Volunteers should be told plainly what the machine does, that it can be remotely accessed by the operator, and that unplugging it at any time is fine and breaks nothing.

### Ubuntu Server, and what that costs

**The boxes run Ubuntu Server.** Desktop carries update checks, telemetry, and indexing that add noise to a network measurement and compete for the CPU that job metering is supposed to govern. Nobody logs into these machines, so the graphical stack buys nothing.

**The cost is representativeness, and it must not be forgotten later.** Real players will run Windows, macOS, or a desktop Linux, on machines they are also using for other things — machines that sleep when the lid closes, that lose wifi when someone microwaves lunch, that share bandwidth with a video call. A dedicated always-on server on a wired connection is the *best* case in every one of those dimensions.

**Phase 0 therefore measures the network, not the player's machine.** Its results are an upper bound on how well the real thing will behave. Desktop-OS behaviour, contention with other applications, and sleep/wake cycles are deferred to a phase where there is a game to run on them, and any conclusion drawn here should be stated as "at best."

## Provisioning

Implemented in `../provision/`. The design goal is **zero interaction**: the box boots, measures every thirty minutes, logs locally, and continues without anyone touching it. Remote access is for when something breaks.

Three configuration choices exist to protect the measurement rather than the machine, and should not be tidied away later:

- **Jitter on the timer**, so three boxes do not measure at the same instant and correlate their results.
- **No automatic reboot** for updates, because an unannounced reboot mid-measurement is indistinguishable from a node genuinely disappearing — the exact thing being measured.
- **Suspend masked**, for the same reason: a box asleep at 3am produces results identical to a network outage.

Household isolation (`--isolate-lan`) is opt-in, verifies internet reachability after applying its rules, and rolls itself back automatically on failure. A firewall rule that strands a box inside someone else's home is not remotely recoverable.

## What gets built, in order

1. **Connection probe** (above) — *done*. Ship to volunteers, collect results.
1a. **Mapping lifetime** — *done*. How long a consumer router holds an idle mapping open before dropping it. Each run tests one idle interval chosen from a list, and the answer accumulates over days. Determines the keepalive interval the mesh will need, which is set by the **worst** router among the players rather than the average one.
2. **Local rig** — *done*. Several nodes forming an overlay, gossiping membership, and noticing departures. `node/rig.sh` runs it and checks the result. Processes rather than containers for now: they share one host and reach each other trivially, so this tests the logic and nothing about real networks.
3. **Job execution** — *done*. Sandboxed WebAssembly with a CPU ceiling, in `node/src/job.rs`. Sample jobs in `jobs/`, built by `jobs/build.sh`.
4. **Checkpoint and migrate** — *done*. A job whose host disappears is continued by a survivor, from the last checkpoint. `node/migration-rig.sh` runs it and checks the result.
4a. **Speculative resume** — *built and passing both local rigs; awaiting Run C on real boxes.* A standby begins continuing a suspected-gone owner's job from `detect/4` (500 ms) of silence, rather than waiting out the full detection window, so the visible gap should fall from the ~2.45 s Night A measured toward the resume budget alone. `node/migration-rig.sh` passes with speculation on (the default) and, now that `MESH_NODE_EXTRA` plumbs flags through to every node it starts, with `--speculate-after-ms 0` forcing it off — a flag that could not be turned off in the rig could not have been A/B tested on a box either. `sudo node/chaos-rig.sh 15` ran 17 disruptions (10 kill, 4 partition, 3 announced) with the same four counts at zero (orphaned, doubled, false alarms, unnoticed restarts) it has always required, plus a `SPECULATION` block of 14 started, 14 confirmed, 0 abandoned — exactly one suspicion per unannounced disruption and none during the announced ones, which is the answer to "does suspicion fire when nothing is wrong?" that loopback *can* give. The visible gap fell to p50 842 ms / p95 1199 ms: a real improvement on 2.45 s, but not the clean ~600 ms a 500 ms threshold implies, because the sweep that raises suspicion is periodic and can lag the threshold by up to its own interval before it fires. **These figures were re-derived from the same logs after a bug was fixed in the tool that produced them** (a takeover could borrow a later disruption's resume, which would have biased the gap optimistic). They did not move — not one number in this paragraph — because the rig fired its disruptions 33 s apart at the closest and the tool matches within a 30 s window, so no takeover here could reach the next one's evidence. That is the rig's pacing saving it, not the old tool being right. What loopback cannot answer — how often suspicion fires when nothing is wrong, *on a connection where things are sometimes slow or lost* — is exactly what Run C is for, and it has not happened yet.
5. **Volunteer run** — the same binary, on real connections, measured against the stop criteria.

## Run C: handover — not yet run (2026-09-22)

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

## Open decisions

- **The rendezvous fallback**, if no volunteer turns out to be directly reachable. Manual invite exchange covers address distribution but not simultaneous-transmission timing.
- ~~**The threshold for an acceptable stall**~~ — settled 2026-08-21: 2–3 s detection, 500 ms resume, at p95.
- **How many volunteers** the go/no-go can honestly rest on, given three is enough to detect a problem class but not to estimate its frequency.
