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
| `probe/` | The connection probe. Rust, std-only, 13 tests |
| `provision/` | Unattended box setup, systemd units, collection and analysis |

## State

Done: the connection probe (classification plus mapping lifetime) and the
provisioning to run it unattended on volunteer boxes.

Next: the mesh node — overlay formation and gossip, sandboxed WASM job
execution with fuel metering, checkpointing, and resume when a host vanishes.
Start on the local container rig, which exercises the logic but tells you
nothing about real networks.

Available: 3 home connections. No VPSs, by decision.

## Conventions

- Rust, **installed via rustup, not apt**. Ubuntu 26.04 ships rustc 1.93 and
  wasmtime needs 1.95 or newer. `apt install rust-all` is enough for the probe
  and was enough for the node before wasmtime; it is not enough now.
- Wasmtime for sandboxing (Rust-native, provides the fuel metering §11.4 needs);
  libp2p for §11.6's overlay and gossip.
- **A node never compiles anything.** Jobs arrive as compiled `.wasm`. The
  toolchain is only needed on machines that author jobs, which real volunteer
  machines are not. Build jobs with `jobs/build.sh` and copy the output.
- Tests accompany behaviour. `cargo test` before claiming anything works.
- Commit messages carry the *reasoning*, not just the change — much of this
  project's thinking lives there.
