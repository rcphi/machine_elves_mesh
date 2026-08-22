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

**Duplicate execution is possible by design, and that changes what an effect is.** The second bug narrows the window but cannot close it: any node that has not yet heard from a peer believes it is alone. Since jobs are deterministic, two nodes continuing the same job produce *identical* effects — wasted work, which is acceptable. Widgets counted twice is not. **Whatever applies effects must treat `(job, tick)` as an effect's identity and ignore a repeat.** This is a requirement on the layer above, discovered here, and it would have been an unpleasant surprise later.

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
