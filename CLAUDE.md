# Machine Elves — mesh spike

This repo implements **Phase 0 only**: a headless spike answering one go/no-go
question before any game exists.

> Can ordinary people's machines, on ordinary home internet connections, form a
> mesh that genuinely runs each other's work — and keep doing it when machines
> disappear?

**It is expected to be discarded.** Its output is a decision and a set of
measurements, not a codebase to build on.

## Where the design lives

`../machine_elves/docs/superpowers/specs/2026-07-31-machine-elves-design.md` —
~31,000 words, self-contained, written so a reader with no prior context can
pick it up cold. Section references throughout this repo (§11, §9.6, §19) point
there. **That repo is design exploration only; no code goes in it.**

§19 is the MVP scope and phasing. This repo is its Phase 0.

## Constraints that are decisions, not preferences

Violating any of these silently invalidates the experiment.

- **No rented infrastructure.** No VPS, no hosted relay, no hosted rendezvous.
  Not only because the product cannot depend on one, but because a
  public-address node makes hole punching succeed more often and gives relaying
  somewhere to land — so the spike would report that the architecture works when
  what works is the architecture *plus a server*.
- **The management channel stays outside the measured path.** Remote-controlling
  machines behind NAT is the same problem being measured. Bind mesh sockets
  explicitly; never let a fallback route succeed quietly.
- **The probe depends on nothing outside the standard library.** Volunteers run
  one static binary on machines they own.
- **Thresholds were fixed before measurement** (2026-08-21): 2–3 s detection,
  500 ms resume, evaluated at p95. A number chosen after seeing results is a
  rationalisation, not a criterion.
- **Phase 0 results are an upper bound.** Dedicated always-on wired Ubuntu Server
  boxes are the best case on every axis a real player's machine is worse. State
  conclusions as "at best."

## Layout

| | |
|---|---|
| `docs/phase-0-plan.md` | Scope, the two test rigs, thresholds, stop criteria, build order |
| `probe/` | The connection probe, including the inbound idle-timeout test. Rust, std-only |
| `node/` | The mesh node, and the local rigs (`rig.sh`, `migration-rig.sh`, `chaos-rig.sh`) |
| `jobs/` | Sample WASM jobs; `build.sh` builds them, and the node's tests need them |
| `provision/` | Unattended box setup, systemd units, the LTE back channel, collection and analysis |

## State

Done: the connection probe, the provisioning, and the mesh node — overlay and
gossip, sandboxed WASM jobs with fuel and memory limits, checkpointing, and
takeover with speculative resume — measured on real boxes in Night A and Runs
B–D. diamond has lived at a friend's house since 2026-11-12, managed over an
LTE back channel; the inbound idle-timeout test runs there against riverboat.
An external review (`CODE_REVIEW_2026-11-07.md`) is being worked through; its
completion record says what is fixed.

Next: Run E — diamond at the friend's house against riverboat at home, the
first run across two households. `docs/phase-0-plan.md` is the record.

Available: 3 home connections and one friend's. No VPSs, by decision.

## Conventions

- Rust, **installed via rustup, not apt**. Ubuntu 26.04 ships rustc 1.93 and
  wasmtime needs 1.95 or newer. `apt install rust-all` is enough for the probe
  and was enough for the node before wasmtime; it is not enough now.
- Wasmtime for sandboxing (Rust-native, provides the fuel metering §11.4 needs);
  libp2p for §11.6's overlay and gossip.
- **A node never compiles anything.** Jobs arrive as compiled `.wasm`, and both
  binaries build fully static, so a volunteer machine needs no toolchain and no
  matching libraries. `./release.sh` produces everything; `setup.sh --dist`
  installs it. Static builds need `musl-tools` on the *build* machine.
- Tests accompany behaviour. `cargo test` before claiming anything works.
- Commit messages carry the *reasoning*, not just the change — much of this
  project's thinking lives there.
