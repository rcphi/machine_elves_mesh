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

### What it does not prove, and the distinctions matter

**Addresses were discovered through public STUN servers.** A third party observed each machine and told it what it looked like from outside. §11.6's answer — a peer already in the mesh reports what address it sees — is not what was used here, and is untested.

**A human carried the addresses across.** In production that is the invite, and the invite would have to carry a *live* address: mappings expire, and the mobile one expires within five minutes. A hand-copied address that outlives its mapping is worse than useless. **This remains the unsolved half.** Punching works; arranging the meeting at scale does not yet.

**It happened suspiciously fast.** Their packet arrived at 0.0 s — before this side could plausibly have opened a path by sending. The likely explanation is that `diamond`'s router uses the most permissive filtering there is, admitting anyone once the socket has sent anywhere at all, which the STUN traffic had already done. A stricter router would not behave this way and the result would look different. **Two routers is not a sample**, and this one may be the friendly case.

**Nothing here tested sustained use** — only first contact. Whether the path survives, and for how long without traffic, is what the mapping-lifetime measurements bound.

### What follows from it

The remaining work is no longer "can this work at all" but "how do two peers exchange live addresses without a server". That is a narrower question with known shapes: an invite that carries a currently-valid address and is used promptly; a peer already in the mesh acting as introducer for two who are not yet connected; or a player with a genuinely reachable address doing the same job §11.6 already calls the fifth contribution lever.

**The introducer need not be infrastructure.** Any citizen already connected to both parties can perform it, which is precisely what a friend-to-friend invite (§11.6) makes available: the person who invites you is, by construction, already connected to you and to the world.

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
5. **Volunteer run** — the same binary, on real connections, measured against the stop criteria.

## Open decisions

- **The rendezvous fallback**, if no volunteer turns out to be directly reachable. Manual invite exchange covers address distribution but not simultaneous-transmission timing.
- **The threshold for an acceptable stall**, in milliseconds, written down before step 5.
- **How many volunteers** the go/no-go can honestly rest on, given three is enough to detect a problem class but not to estimate its frequency.
