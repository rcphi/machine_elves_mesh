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
2. **Local container rig** — N nodes, forming an overlay, gossiping membership.
3. **Job execution** — a sandboxed WASM job with fuel metering, so a job cannot consume unbounded CPU (§11.4).
4. **Checkpoint and migrate** — a running job's state captured periodically, and resumed elsewhere when its host vanishes (§11.3, §9.4).
5. **Volunteer run** — the same binary, on real connections, measured against the stop criteria.

## Open decisions

- **The rendezvous fallback**, if no volunteer turns out to be directly reachable. Manual invite exchange covers address distribution but not simultaneous-transmission timing.
- **The threshold for an acceptable stall**, in milliseconds, written down before step 5.
- **How many volunteers** the go/no-go can honestly rest on, given three is enough to detect a problem class but not to estimate its frequency.
