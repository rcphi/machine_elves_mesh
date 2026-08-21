# Phase 0 — Mesh Spike

**Status:** Planning. No code yet.
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
| **Nodes** | Many, on one host | 3 home machines + VPSs |
| **Tests** | Scheduling, checkpointing, migration on dropout, job correctness, load behavior, topology logic | NAT traversal, real latency, asymmetric upstream, router timeouts, genuine disconnection |
| **Cannot test** | Anything about real networks — containers on one host all reach each other trivially | Scale, or any behavior needing more than a handful of nodes |

**The volunteer rig is the one that decides go/no-go.** The local rig is where the work actually gets built and debugged.

## What the available nodes can and cannot represent

**The three home machines are the real subjects.** They carry the failure modes that matter: carrier-grade NAT, asymmetric upstream bandwidth, consumer routers dropping idle connections, real geographic latency.

Three is a small sample, but the relevant failures are **categorical rather than statistical** — if a connection sits behind carrier-grade NAT, that shows up immediately and unmistakably. Three connections establish *whether* the problem class exists. They cannot estimate how common it is across a real player population, and no claim of that kind should be made from them.

**The VPSs are not test subjects.** A rented server has a public address, no NAT, and symmetric bandwidth — it is exactly the case that was never in doubt. Including VPS-to-VPS results in a traversal success rate would produce a flattering and meaningless number.

**What the VPSs are for is relaying.** §11.6 adopts DERP-style relays that forward end-to-end encrypted packets and cannot read them, which is what makes relay duty safe to distribute. A VPS is a legitimate stand-in for *a player with a public address and spare upstream* — which §11.6 counts as the fifth contribution lever.

**The standing caveat:** rented infrastructure is acceptable in a spike and is not acceptable in the product. §12.1's reveal cannot survive a permanent dependency on servers anyone rents, and §15 already rejects Tailscale as a service for exactly this reason. Any VPS here is a stand-in for a citizen, and the moment it becomes load-bearing the design has drifted.

## Step 0 — characterize the volunteers' connections first

**Before any mesh code exists**, have each volunteer run a small probe that reports what kind of network address translation sits between them and the internet, and whether they can accept an inbound connection at all. This is a well-understood measurement using public STUN servers and takes minutes.

This is deliberately the first deliverable because it is nearly free and can invalidate a great deal of work. If all three volunteers turn out to be behind carrier-grade NAT — increasingly common, and near-universal on mobile networks — then direct connections are the exception rather than the fallback, relaying becomes the normal case, and Phase 0's stop criterion is met on day one rather than month three.

Finding that out cheaply is worth more than any amount of careful implementation.

## Stop criteria

Carried from §19.5, unchanged:

- Direct connections between home machines fail often enough that relaying becomes the common case rather than the fallback, **and** relay burden proves impractical to distribute across players.
- A machine dropping out produces a stall long enough that a player would read it as broken software rather than as the world breathing.

**Write down the threshold for "long enough" before measuring it.** A number chosen after seeing results is not a criterion.

## What gets built, in order

1. **Connection probe** (above). Ship to volunteers, collect results.
2. **Local container rig** — N nodes, forming an overlay, gossiping membership.
3. **Job execution** — a sandboxed WASM job with fuel metering, so a job cannot consume unbounded CPU (§11.4).
4. **Checkpoint and migrate** — a running job's state captured periodically, and resumed elsewhere when its host vanishes (§11.3, §9.4).
5. **Volunteer run** — the same binary, on real connections, measured against the stop criteria.

## Open decisions

- **Implementation language.** Not yet chosen.
- **The threshold for an acceptable stall**, in milliseconds, written down before step 5.
- **How many volunteers** the go/no-go can honestly rest on, given three is enough to detect a problem class but not to estimate its frequency.
