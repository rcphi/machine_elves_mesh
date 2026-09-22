# Speculative resume — design

**2026-09-22.** For the Phase 0 mesh node in this repo. The measurements it
sets out to move are in `../../phase-0-plan.md`; §11 and §9 references point at
the design document in the `machine_elves` repo.

## What this is for

Both timed runs met both thresholds and left the same number untouched: **a
player waits about 2.45 s** when a machine disappears without warning.

That wait is detection plus resume. Resume is already fast — 200 ms at p95
across 119 unannounced losses on two rigs — so the wait is detection, and
detection cannot simply be shortened. It is the time spent being *sure*, and
buying certainty faster means false alarms.

Speculative resume removes detection from the wait instead of shortening it. A
standby holding a recent checkpoint begins continuing the job as soon as the
owner is **suspected** gone, and its work is thrown away if the owner turns out
to be alive. Briefly running two copies is cheap; a visible stall is not. This
is the tail-latency technique from Dean and Barroso, *The Tail at Scale* (2013),
and the plan has named it as the reason the 500 ms resume budget was ever
reachable.

**The expected effect on a player:** a 2.45 s freeze becomes a ~600 ms hitch.

## Why it is safe here, which is not obvious

The usual hard part of speculation is undoing work that turned out to be
unnecessary. **There is nothing to undo.**

Items are named by what made them — `Serial::derive(job_id, tick, kind,
ordinal)` — and jobs are deterministic to the instruction, verified across
machines at identical fuel consumption. Two nodes running the same tick from
the same checkpoint therefore produce *the same items*, and merging them is a
union of equal sets. `main.rs` already says so where effects are published:

> Two nodes may briefly continue the same job — a node that has not yet heard
> from a peer believes it is alone — and being deterministic, they produce
> identical effects. Wasted work is acceptable; widgets counted twice is not.

Run B demonstrated the property at length: two machines ran the same job alone
for 40 minutes, producing some 4,500 items each with no contact, and
reconciling moved **12 items in 8 cells**.

**The constraint this rests on, stated so it is not lost:** speculation is
valid only where a duplicate is a no-op. Deterministic simulation qualifies.
Ledgers, ownership and governance do not — they need exactly one writer, and
there correctness outranks latency. Phase 0 runs one deterministic job whose
sandbox makes side effects impossible, so no per-job gate is built now; a
system with more than one kind of job needs one before this goes near a
ledger.

## The state machine

The sweep already runs every `detect/4` and already removes members silent
longer than `detect` before electing a successor. Speculation adds one earlier
check inside the same sweep. **No new timer.**

**Entering.** If the job's owner has been silent longer than
`speculate_after` (default `detect/4`), and this node would be the successor
were the owner already gone, the node starts speculating. `owner` keeps
pointing at the suspect, because nothing has been decided; a new
`speculating: Option<Speculating>` records when it began and whom it concerns.
The tick arm's guard becomes `owner == Some(me) || speculating.is_some()`.

The successor rule is the existing one — lowest peer id — applied to
**membership minus the suspect**. Applying it to full membership would, with
two nodes, make the higher-id node ineligible forever and speculation would
appear simply not to work.

Only the designated successor speculates, so at most one extra copy of a job
runs regardless of how many nodes stand by.

**Leaving, three ways:**

1. **Confirmed gone.** The existing `vanished` path fires at `detect` and
   `take_over_if_ours` sets `owner = me`. Speculation becomes ownership; no
   tick is re-run, because the work is already done and published.
2. **Owner returns ahead.** Its checkpoint arrives and the existing two-owner
   rule `Work::theirs` applies unchanged — whoever is further along keeps the
   job. A returning owner is almost always ahead, having never stopped while
   the speculator resumed from an older checkpoint.
3. **Owner returns behind.** The same rule, opposite answer: the speculator
   keeps the job and the owner yields. This is already how the code resolves
   two owners after a partition, and needs no new policy.

**One flag:** `--speculate-after-ms`, default `detect/4`, disabled at 0, so a
node can be run exactly as today without rebuilding.

## What goes on the wire

**Nothing new.** No message kind, no flag, no protocol change.

A speculating node publishes ordinary checkpoints and ordinary production,
indistinguishable from an owner's — because they are identical. Marking them
would invite receivers to treat them differently, and the one thing receivers
must not do is treat them differently.

Two consequences:

- **A node from before this change interoperates with one after it.** Retiring
  kind 6 forced all three boxes to be redeployed together; this does not.
- **Traffic rises only while suspicion is active**, by one node's share. A
  standby that suspects nothing publishes nothing, so the ~60 MB/h steady
  state measured in Run B is unchanged.

## Events, and the tool this breaks

`takeover-summary.py`'s `match()` accepts a `resumed` event only **at or after**
the `took-over` it belongs to. Speculation inverts that order: the first tick
precedes confirmation. Left alone, the tool finds no `resumed` and reports
`resume_ms` and `gap_ms` as absent — silently, and for the exact measurement
this feature exists to move. **That is the third variation of this project's
recurring bug**: a tool that answers about the wrong thing in the same shape
as a right answer.

| Event | When | Carries |
|---|---|---|
| `speculating` | suspicion starts | `peer_id`, `label`, `silent_ms`, `tick` |
| `resumed` | first speculative tick | `cause: "speculated"`, `resume_ms`, `gap_ms` |
| `took-over` | confirmation at `detect` | as today, plus `speculated`, `speculated_ms` |
| `speculation-abandoned` | owner returns and keeps the job | `peer_id`, `ticks_wasted`, `wasted_ms` |

`took-over` keeps its current shape and timing so that chaos-action pairing in
the summary continues to work untouched.

**Summary changes:** accept a `resumed` that precedes its `took-over` when it
is a speculated one from the same node; add a speculation block reporting
started, confirmed, abandoned, wasted ticks at p50 and p95, and the
false-suspicion rate. **Detection keeps its present meaning** — the silence
before a peer was declared gone — so that threshold stays comparable with
Night A, Night A's second run, and Run B.

**A claim in the plan will need retracting.** It says the player waits
detection plus resume. Under speculation the wait is suspicion plus resume and
detection drops out of it. Run C tests that, and the plan says so afterwards
either way.

## Testing

**Unit tests**, against a pure suspicion function mirroring `should_take_over`:

- Only the owner's silence triggers suspicion; a silent standby triggers none.
- Only the designated successor speculates, with the rule applied to
  membership minus the suspect. The wrong answer here makes speculation never
  fire, which no end-to-end test distinguishes from bad luck.
- `speculate_after` sits strictly inside `detect`, so speculation always
  precedes confirmation.
- Abandonment when the returning owner is ahead; continuing when it is behind.
- `--speculate-after-ms 0` reproduces today's behaviour exactly.

**Rig tests**, which is where this class of bug has actually been caught:

- `node/chaos-rig.sh` — kills under speculation: one confirmed takeover per
  disruption, no duplicate ownership.
- `node/migration-rig.sh` — the returning-node race, which speculation touches
  directly: a node killed and restarted inside the transport's 10 s idle
  timeout while a speculator is mid-flight.

**A summary test** in `provision/test_takeover_summary.py`: a fixture where
`resumed` precedes `took-over`, asserting the gap is read rather than dropped.

**Then Run C** on diamond and riverboat — same rig, same mesh shape,
speculation on — against Night A's second run as the baseline.

| | Baseline (n=104) | Run C target |
|---|---|---|
| Detection p95 | 2461 ms | unchanged |
| Resume p95 | 200 ms | unchanged |
| **Visible gap p95** | **2651 ms** | **under 500 ms** |
| Orphaned / doubled / false alarm / unnoticed | 0 | 0 |

Also recorded: wasted ticks per disruption, and how often false suspicion
fires. Both runs saw zero false alarms at 2 s; at 500 ms that number will not
be zero, and the cost of being wrong is a player's CPU.

**What stops this and returns it to design:** false suspicion firing
constantly on an idle LAN. That would mean the trigger is wrong in kind rather
than in value, and patching the number would be fitting the design to the
measurement.

## Not in scope

- **Per-job speculation gates.** One job kind, deterministic by construction.
  Needed before anything speculates near a ledger.
- **Speculating on more than one standby.** One extra copy is the whole
  benefit; N copies is N times the waste for nothing.
- **Tuning `detect` itself.** Fixed at 2 s before measurement and unchanged
  here.
