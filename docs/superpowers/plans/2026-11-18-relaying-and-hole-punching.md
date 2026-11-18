# Relaying and Hole Punching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the mesh node keep unreachable members together through reachable ones, without rented infrastructure, and cheaply. It gets:
- coordinated hole punching over short introductions;
- UPnP beside NAT-PMP;
- reachability confirmed by members;
- a peer book on disk;
- a dial backoff.

**Architecture:**
- **Gossip still forwards every message,** as in Runs B–F.
- **libp2p 0.56's own relay**, at its default limits, runs on every node, and introduces two unreachable members so that DCUtR can time their punch.
- **AutoNAT v2, with members as the only servers,** decides who counts as reachable. Each heartbeat says so.
- **Unreachable nodes reserve introductions** on up to three reachable members, and announce the resulting relay addresses with the existing `Observed` gossip.
- **A per-address dial schedule** replaces the fixed 2 s retry.
- **Validation:** a namespace rig on lightning reproduces Run F's three connections, with a fake UPnP gateway, before any box is touched. The plan ends with Run G's handover.

**Tech Stack:** Rust (rustup, static musl via `./release.sh`). libp2p 0.56 with the added features `relay`, `dcutr` and `autonat`. `igd-next` 0.16 with `aio_tokio`. `rand_core` 0.6 with `getrandom`. Python 3, standard library only, for the provisioning tools. Bash with `ip netns` and `nft` for the rig.

**Spec:** `docs/superpowers/specs/2026-11-18-relaying-and-hole-punching-design.md` (approved 2026-11-18). Background: `docs/phase-0-plan.md`, "Run F: three connections, measured".

## Global Constraints

- **No server that is not a member.** No public STUN in the node, and no hosted relay or rendezvous. UPnP and NAT-PMP speak only to the node's own default gateway.
- **The relay keeps libp2p's defaults:** `max_circuit_duration` 120 s, `max_circuit_bytes` 131,072, `max_circuits` 16, `max_circuits_per_peer` 4.
- **`MAX_INTRODUCERS = 3`.**
- **The dial schedule:**
  - first retry 2 s, doubling to a cap of 60 s for configured addresses and 300 s for remembered and observed ones;
  - relay ("circuit") addresses start at 60 s and cap at 1,800 s;
  - an address that never answers costs under 1 MB/h.
- **AutoNAT v2 probes no more often than every 60 s, testing at most 4 candidates.**
- **Wire formats change only by appending.** A node built before this change must still decode every message, and a new node must decode an old heartbeat as "not reachable".
- **The thresholds are unchanged:** detection 2–3 s and resume 500 ms, at p95. `chaos-rig.sh` must still pass.
- **Every new event goes through the existing `emit()`,** and gets an entry in `provision/node_events.py`'s `NOTABLE`.
- **The management channel is untouched.**
- **Tests:** `cargo test` in `node/`, which needs `jobs/build.sh` to have built the jobs, and `python3 -m unittest discover -p 'test_*.py'` in `provision/`. Both stay green.
- **Commits:** every commit message carries the reasoning, and ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **An unreachable peer heard through a forwarder must not hurry its own redial.** This is exactly Run F's case: diamond heard yoinkerz every second through riverboat. A periodic re-announcement, or a heartbeat that arrives by gossip, must leave that address's backoff alone. *(Task 1, `a_known_address_announced_again_keeps_its_backoff`. The spec's line "or when its peer is heard from in any other way" is corrected in Task 1.)*
2. **A relayed connection ending at its 120 s limit must not start another punch at once.** That loop would spend 128 KiB of the introducer's budget every two minutes. *(Task 1, `resetting_a_circuit_address_does_not_hurry_it`, and Task 5's glue rule.)*
3. **A damaged or half-written peer book must never stop the node from starting.** *(Task 2, `a_book_of_garbage_starts_empty_and_says_so`.)*
4. **Old and new nodes in one mesh.** An old heartbeat reads as unreachable. A relay address announced to an old node fails its dial fast and harmlessly. *(Task 3, `a_heartbeat_from_before_reachability_was_said_is_unreachable`.)*
5. **No gateway, or a host that forbids multicast, as diamond's `isolate-lan` does,** leaves the port request failing quickly and quietly. It never delays the node. *(Task 6, `with_no_gateway_only_multicast_is_tried` and `the_whole_upnp_search_is_bounded`.)*

---

## File Structure

| File | Responsibility |
|---|---|
| `node/src/dialing.rs` (new) | The per-address dial schedule: when to try each address next |
| `node/src/peerbook.rs` (new) | Remembered peers on disk: load, atomic save, which addresses to dial on start |
| `node/src/upnp.rs` (new) | Asking the gateway for a port by UPnP: gateway first, then multicast |
| `node/src/portmap.rs` | Exposes `default_gateway`. Gains `Method` and `first_granted` |
| `node/src/main.rs` | Wiring: the swarm builder, heartbeat field, reachability, introducers, punches, events |
| `node/Cargo.toml` | The added features and dependencies |
| `node/nat-rig.sh` (new) | Run F's topology in network namespaces, in three scenarios |
| `node/fake-igd.py` (new) | A UPnP gateway for the rig that really installs the DNAT it grants |
| `node/rig.sh`, `node/migration-rig.sh`, `node/chaos-rig.sh` | Pass `--peer-book`, so no rig writes to `/var/lib/mesh-node` |
| `provision/timed-run.sh` | `EXTRA` gains `--peer-book /var/lib/mesh-node/peers-$MESH` |
| `provision/node_events.py` | `NOTABLE` entries for every new event |
| `provision/link-summary.py`, `provision/test_link_summary.py` (new) | Per run: links direct or relayed, punches, introducers, and ticks computed by two owners |
| `docs/phase-0-plan.md` | Run G's handover |

---

### Task 1: The dial schedule

**Files:**
- Create: `node/src/dialing.rs`
- Modify: `node/src/main.rs`. Replace `pending: Vec<Multiaddr>` (about 23 uses: the dial-retry arm near line 1352, the `Observed` handler near line 2100, `ConnectionEstablished` near line 1785, and `redial_lost_peer` near line 2488) with `dials: dialing::Schedule`.
- Modify: `docs/superpowers/specs/2026-11-18-relaying-and-hole-punching-design.md`, "Dial backoff"
- Test: `#[cfg(test)] mod tests` in `node/src/dialing.rs`

**Interfaces:**
- Produces, used by Tasks 2 and 5:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Configured, Remembered, Observed, Circuit }
pub const FIRST_RETRY: Duration = Duration::from_secs(2);
pub const CIRCUIT_FIRST_RETRY: Duration = Duration::from_secs(60);
pub fn cap(kind: Kind) -> Duration; // 60 s, 300 s, 300 s, 1800 s
pub struct Schedule { /* addr -> (kind, peer, next: Instant, interval) */ }
impl Schedule {
    pub fn new() -> Self;
    /// Due now if new (returns true). An address already scheduled keeps its
    /// backoff; only its kind may become Configured.
    pub fn add(&mut self, addr: Multiaddr, kind: Kind, peer: Option<PeerId>, now: Instant) -> bool;
    /// Due now, interval back to its first retry. A no-op for Circuit entries.
    pub fn reset(&mut self, addr: &Multiaddr, now: Instant);
    pub fn forget_peer(&mut self, peer: &PeerId);   // removes every entry tagged with it
    pub fn remove(&mut self, addr: &Multiaddr);
    pub fn contains(&self, addr: &Multiaddr) -> bool;
    /// The addresses due at `now`; each is pushed to now + interval, then its
    /// interval doubles up to cap(kind).
    pub fn due(&mut self, now: Instant) -> Vec<Multiaddr>;
    pub fn len(&self) -> usize;
}
```
  `Instant` is `std::time::Instant`, as `main.rs` uses.

- [ ] **Step 1: Write the failing tests.** Use a helper `fn gaps(kind: Kind, n: usize) -> Vec<u64>`, which adds one address at `t0`, then steps time one second at a time and records the seconds between successive `due()` returns.

```rust
#[test] fn a_new_address_is_due_at_once()                   // add at t0 -> due(t0) == [addr]
#[test] fn observed_retries_double_from_two_seconds_to_five_minutes() {
    assert_eq!(gaps(Kind::Observed, 10), vec![2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
}
#[test] fn configured_retries_cap_at_a_minute() {
    assert_eq!(gaps(Kind::Configured, 7), vec![2, 4, 8, 16, 32, 60, 60]);
}
#[test] fn circuits_start_at_a_minute_and_cap_at_half_an_hour() {
    assert_eq!(gaps(Kind::Circuit, 7), vec![60, 120, 240, 480, 960, 1800, 1800]);
}
#[test] fn a_known_address_announced_again_keeps_its_backoff()  // add at t0, due 3 times, add again: next gap is still 8
#[test] fn reset_makes_an_address_due_now_and_starts_again()  // after reset(t), due(t) contains it and the next gap is 2
#[test] fn resetting_a_circuit_address_does_not_hurry_it()   // reset on a Circuit entry: not due before its scheduled time
#[test] fn forgetting_a_peer_removes_all_its_addresses()     // two addrs tagged peer P, one untagged: after forget_peer(P), len()==1
#[test] fn a_dead_observed_address_is_tried_at_most_20_times_an_hour()  // count due() hits over 3600 s <= 20
#[test] fn a_dead_configured_address_is_tried_at_most_65_times_an_hour()
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cd node && cargo test dialing`. Expected: does not compile (`dialing` is not defined).

- [ ] **Step 3: Implement `dialing.rs`** to the interface above. Add `mod dialing;` to `main.rs`.

- [ ] **Step 4: Run the tests to verify they pass.** Run: `cd node && cargo test dialing`. Expected: 10 passed.

- [ ] **Step 5: Wire it into `main.rs`.** These rules bind:
  - **Configured peers** (`config.dial`) are added as `Kind::Configured` at start.
  - **The dial-retry arm** runs on `dial_retry.tick(), if dials.len() > 0`. It dials `dials.due(now)`. Addresses known in `reached_by` for an already-connected peer are removed first, as today. `dial_retry_interval()` and its test `a_quiet_mesh_does_not_store_up_dial_retries` stay as they are: the interval polls, and the schedule decides.
  - **`Observed`, for a peer not connected:** `dials.add(addr, Kind::Observed, Some(peer), now)`, then dial immediately **only if `add` returned true**. A re-announcement, every 30 s, must not dial.
  - **`ConnectionEstablished`:** call `dials.forget_peer(&peer_id)`. As dialer, also remove the dialled address.
  - **`redial_lost_peer`** calls `dials.reset(addr, now)` for each address in `reached_by` of a peer that had a **direct** connection and lost it. This path only.
  - **Heartbeats and other gossip never touch the schedule.**
  - **The `dialling` status line** keeps its fields `peers` (now `dials.len()`) and `attempts`.

- [ ] **Step 6: Correct the spec.** In "Dial backoff", replace "or when its peer is heard from in any other way" with: "or when a direct connection to its peer is lost. Hearing a peer through a forwarder must not reset it: that is Run F's case, where diamond heard yoinkerz every second through riverboat."

- [ ] **Step 7: Run the whole suite.** Run: `cd node && cargo test`. Expected: all pass, with 66 or more tests.

- [ ] **Step 8: Commit.**
```bash
git add node/src/dialing.rs node/src/main.rs docs/superpowers/specs/2026-11-18-relaying-and-hole-punching-design.md
git commit   # why: Run F spent ~13 MB/h per dead address; first retry stays 2 s so rejoin is not slower
```

---

### Task 2: The peer book

**Files:**
- Create: `node/src/peerbook.rs`
- Modify: `node/src/main.rs`. Add `Config.peer_book: String`, defaulting to `/var/lib/mesh-node/peers`. Add the `--peer-book <path>` flag and its line in the usage text. Load the book at start, and record into it.
- Modify: the `node/rig.sh:49`, `node/migration-rig.sh:68,183` and `node/chaos-rig.sh:58` invocations, adding `--peer-book "$OUT/<name>.peers"`. Modify `provision/timed-run.sh:90` so that `EXTRA="--ledger /var/lib/mesh-node/ledger-$MESH --peer-book /var/lib/mesh-node/peers-$MESH"`.
- Modify: `provision/node_events.py`, adding `NOTABLE` entries
- Test: `#[cfg(test)] mod tests` in `node/src/peerbook.rs`

**Interfaces:**
- Consumes: `dialing::Kind::Remembered` and `Schedule::add` (Task 1)
- Produces, used by Task 4:
```rust
pub struct Remembered { pub label: String, pub addrs: Vec<Multiaddr>, pub reachable: bool, pub last_heard_unix_ms: u64 }
pub struct PeerBook { /* BTreeMap<PeerId, Remembered> */ }
impl PeerBook {
    pub fn load(path: &Path) -> io::Result<(PeerBook, usize)>; // missing file: (empty, 0); usize = lines skipped
    pub fn save(&self, path: &Path) -> io::Result<()>;          // write <path>.tmp, sync, rename
    pub fn dialled(&mut self, peer: PeerId, addr: &Multiaddr, now_unix_ms: u64) -> bool; // true if changed
    pub fn named(&mut self, peer: PeerId, label: &str) -> bool;
    pub fn set_reachable(&mut self, peer: PeerId, reachable: bool) -> bool;
    pub fn addresses_to_dial(&self) -> Vec<Multiaddr>;          // every address of every peer remembered reachable
    pub fn len(&self) -> usize;
}
pub const MAX_ADDRS_PER_PEER: usize = 4; // the most recently dialled are kept
```
  The file has one line per peer, separated by tabs: `peer_id`, `reachable` (0 or 1), `last_heard_unix_ms`, `label`, then the addresses separated by spaces. Tabs and newlines in a label become spaces.

- [ ] **Step 1: Write the failing tests:**
```rust
#[test] fn a_book_survives_a_round_trip()                 // save then load: equal entries, 0 skipped
#[test] fn a_missing_file_is_an_empty_book()               // load(nonexistent) == Ok((empty, 0))
#[test] fn a_damaged_line_is_skipped_and_counted()         // 2 good lines + 1 bad: len()==2, skipped==1
#[test] fn a_book_of_garbage_starts_empty_and_says_so()    // random bytes: Ok((empty, n>0)), never Err
#[test] fn a_leftover_temporary_file_does_not_matter()     // <path>.tmp present before save: save succeeds, .tmp gone after
#[test] fn only_peers_remembered_as_reachable_are_dialled_on_start()
#[test] fn a_peer_keeps_its_four_most_recent_addresses()
#[test] fn recording_what_is_already_known_changes_nothing() // dialled() of a known addr returns false
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd node && cargo test peerbook`. Expected: does not compile.
- [ ] **Step 3: Implement `peerbook.rs`.**
- [ ] **Step 4: Run them to verify they pass.** Run: `cd node && cargo test peerbook`. Expected: 8 passed.
- [ ] **Step 5: Wire it into `main.rs`.**
  - **At start:** load the book. Emit `peer-book-loaded` with fields `peers` and `skipped`. Add each of `addresses_to_dial()` as `Kind::Remembered`.
  - **On a successful dial,** `ConnectionEstablished` as dialer and not relayed: call `dialled`.
  - **On a member's first heartbeat with a label:** call `named`.
  - **Save** only when a call returned true, at most once every 10 s. On failure, emit `peer-book-not-saved` (field `error`), at most once a minute through the existing `Quiet`.
  - **`node_events.py` entries:**
    - `"peer-book-loaded": "remembered {peers} peers ({skipped} unreadable lines skipped)"`
    - `"peer-book-not-saved": "could not write the peer book: {error}"`
  - **The rigs and `timed-run.sh`,** as listed under Files.
- [ ] **Step 6: Run both suites.** Run: `cd node && cargo test && cd ../provision && python3 -m unittest discover -p 'test_*.py'`. Expected: all pass.
- [ ] **Step 7: Commit.** The reasoning: "remember the peers you can dial; the ones you cannot will remember you" (the plan's rule), and a restarted node that knows only a departed peer is Run F's re-meeting failure.

---

### Task 3: Heartbeats say whether the sender is reachable

**Files:**
- Modify: `node/src/main.rs`:
  - `encode_heartbeat`, at about line 330;
  - the `KIND_HEARTBEAT` decode, at about line 429;
  - `Message::Heartbeat`;
  - `struct Member`, at about line 773;
  - the heartbeat handler.
- Test: `node/src/main.rs` `mod tests`

**Interfaces:**
- Produces, used by Tasks 4 and 5:
  - `fn encode_heartbeat(label: &str, counter: u64, run: u64, job: Option<&[u8; 32]>, reachable: bool) -> Vec<u8>`. The byte is appended after the job field: 1 if reachable, 0 if not.
  - `Message::Heartbeat { label, run, holding, reachable: Option<bool> }`. It is `None` when the byte is absent.
  - `Member.reachable: bool`. It is false until a heartbeat says otherwise.

- [ ] **Step 1: Write the failing tests:**
```rust
#[test] fn reachability_survives_a_round_trip()   // encode(.., true) decodes to reachable: Some(true); likewise false
#[test] fn a_heartbeat_from_before_reachability_was_said_is_unreachable() // old 3-field encoding -> reachable None; Member.reachable false
```
  Then extend `messages_survive_a_round_trip` with the new field.
- [ ] **Step 2: Run them to verify they fail.** Run: `cd node && cargo test reachab`. Expected: does not compile, because `encode_heartbeat` has the wrong arity.
- [ ] **Step 3: Implement.** Callers pass `false` until Task 4 supplies the real value.
- [ ] **Step 4: Run the whole suite.** Run: `cd node && cargo test`. Expected: all pass.
- [ ] **Step 5: Commit.**

---

### Task 4: Relay, DCUtR and AutoNAT in the swarm, and knowing whether this node is reachable

**Files:**
- Modify: `node/Cargo.toml`. libp2p features gain `"relay", "dcutr", "autonat"`. Add `rand_core = { version = "0.6", features = ["getrandom"] }`.
- Modify: `node/src/main.rs`:
  - `MeshBehaviour`, at about line 505;
  - extract `fn build_swarm(config: &Config, keypair: identity::Keypair) -> Result<Swarm<MeshBehaviour>>` out of `run()`, at about line 1153;
  - the swarm event handling;
  - the heartbeat publish.
- Modify: `provision/node_events.py`
- Modify: the spec, "Introductions and punching", "Who runs what"
- Test: `node/src/main.rs` `mod tests`

**Interfaces:**
- Consumes: `encode_heartbeat(.., reachable)` (Task 3), `PeerBook::set_reachable` (Task 2)
- Produces, used by Task 5:
  - `MeshBehaviour` gains `relay: relay::Behaviour`, `relay_client: relay::client::Behaviour`, `dcutr: dcutr::Behaviour`, `autonat_client: autonat::v2::client::Behaviour` and `autonat_server: autonat::v2::server::Behaviour`.
  - The builder chain is `.with_quic().with_relay_client(noise::Config::new, yamux::Config::default)?.with_behaviour(|key, relay_client| ..)`.
  - `fn relay_config() -> relay::Config`. It returns `relay::Config::default()` and exists so a test can pin the limits.
  - `const AUTONAT_PROBE_EVERY: Duration = Duration::from_secs(60); const AUTONAT_MAX_CANDIDATES: usize = 4;`
  - `struct Reachability { confirmed: BTreeSet<Multiaddr> }` with:
    - `fn confirm(&mut self, a: Multiaddr) -> bool` — true when the node becomes reachable;
    - `fn expire(&mut self, a: &Multiaddr) -> bool` — true when it stops being reachable;
    - `fn is_reachable(&self) -> bool`.

- [ ] **Step 1: Write the failing tests:**
```rust
#[tokio::test] async fn the_swarm_builds_with_relaying_and_punching()  // build_swarm(&test_config(), Keypair::generate_ed25519()) is Ok
#[test] fn introductions_keep_the_relays_default_limits() {
    let c = relay_config();
    assert_eq!(c.max_circuit_duration, Duration::from_secs(120));
    assert_eq!(c.max_circuit_bytes, 131_072);
}
#[test] fn reachable_once_any_address_is_confirmed_and_until_the_last_expires()
#[test] fn autonat_probes_no_more_than_once_a_minute() { assert!(AUTONAT_PROBE_EVERY >= Duration::from_secs(60)); }
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd node && cargo test swarm_builds reachab introductions autonat`. Expected: does not compile.
- [ ] **Step 3: Implement.**
  - **The relay server runs on every node,** not only on confirmed-reachable ones. libp2p fixes the behaviours when the swarm is built, while reachability changes at runtime. Who gets *used* as an introducer is decided in Task 5, by the heartbeat flag. Write that into the spec's "Who runs what" in the same commit.
  - **AutoNAT client:** `autonat::v2::client::Behaviour::new(OsRng, Config::default().with_probe_interval(AUTONAT_PROBE_EVERY).with_max_candidates(AUTONAT_MAX_CANDIDATES))`.
  - **Swarm events:**
    - `SwarmEvent::ExternalAddrConfirmed { address }` → `confirm`. On true, emit `reachable` (field `addr`).
    - `ExternalAddrExpired { address }` → `expire`. On true, emit `unreachable`.
  - **The heartbeat** publishes `reachability.is_reachable()`.
  - **The heartbeat handler** sets `Member.reachable` and calls `PeerBook::set_reachable` (Task 2).
  - **`node_events.py` entries:**
    - `"reachable": "a member reached this node at {addr}: it can introduce others"`
    - `"unreachable": "no member can reach this node at {addr} any more"`
- [ ] **Step 4: Run the whole suite.** Run: `cd node && cargo test`. Expected: all pass.
- [ ] **Step 5: Check the static build.** Run: `cd .. && ./release.sh`. Expected: `dist/mesh-node` builds, `file dist/mesh-node` says "statically linked", and it runs `--help`.
- [ ] **Step 6: Commit.**

---

### Task 5: Introducers, relay addresses, and punches

**Files:**
- Modify: `node/src/main.rs`:
  - the sweep, or membership change, that maintains introducers;
  - the `announce` tick, at about line 1473;
  - the `Observed` handler;
  - `ConnectionEstablished`;
  - the DCUtR and relay-client events.
- Modify: `provision/node_events.py`
- Test: `node/src/main.rs` `mod tests`

**Interfaces:**
- Consumes:
  - `Member.reachable` (Task 3);
  - `Reachability::is_reachable` (Task 4);
  - `dialing::Kind::Circuit`, `Schedule::{add, forget_peer}` (Task 1).
- Produces:
  - `const MAX_INTRODUCERS: usize = 3;`
  - `fn choose_introducers(me_reachable: bool, members: &HashMap<PeerId, Member>, directly_connected: &HashSet<PeerId>, held: &BTreeSet<PeerId>, max: usize) -> BTreeSet<PeerId>`
  - `fn is_relayed(addr: &Multiaddr) -> bool`. True if it contains `Protocol::P2pCircuit`.
  - `fn circuit_listen_addr(relay_addr: &Multiaddr, relay: PeerId) -> Multiaddr`. That is `relay_addr` plus `/p2p/<relay>/p2p-circuit`, without a duplicate `/p2p`.
  - `fn dialable(addr: &Multiaddr, peer: PeerId) -> Multiaddr`. It appends `/p2p/<peer>` if the address does not already end in it.
  - The `connected` event gains a field `relayed`, `"true"` or `"false"`.

- [ ] **Step 1: Write the failing tests:**
```rust
#[test] fn a_reachable_node_holds_no_introducers()
#[test] fn introducers_are_reachable_members_this_node_is_directly_connected_to()
#[test] fn at_most_three_introducers_and_the_ones_already_held_are_kept()
#[test] fn an_introducer_that_stops_being_reachable_is_dropped()
#[test] fn a_circuit_address_is_recognised_and_a_direct_one_is_not()
#[test] fn a_circuit_listen_address_names_the_relay_once()
#[test] fn a_dialable_address_names_its_peer_once()
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd node && cargo test introducer circuit dialable`. Expected: does not compile.
- [ ] **Step 3: Implement the pure functions.** For `choose_introducers`: keep every member in `held` that is still reachable and directly connected, then add the lowest `PeerId`s among the other such members, up to `max`. Return the empty set if `me_reachable`.
- [ ] **Step 4: Run them to verify they pass.** Same command. Expected: 7 passed.
- [ ] **Step 5: Wire it in.** These rules bind:
  - **Maintaining introducers.** On every sweep, recompute `choose_introducers`. For each new one, call `swarm.listen_on(circuit_listen_addr(addr_it_is_connected_on, relay))`, and keep the `ListenerId`. For each one dropped, call `swarm.remove_listener`.
    - `relay::client::Event::ReservationReqAccepted { renewal: false, .. }` → emit `introducer-held` (fields `peer_id`, `label`).
    - A circuit listener closing with an error → emit `introducer-lost` (field `error`).
  - **Announcing.** `NewListenAddr` with a circuit address goes into `circuit_addrs`. The `announce` tick also publishes `encode_observed(&label, &me, addr)` for each of them.
  - **Hearing an announcement.** In the `Observed` handler, for an address with `is_relayed` and a peer not connected: `dials.add(dialable(&addr, peer), Kind::Circuit, Some(peer), now)`, and keep it in `circuits_of: HashMap<PeerId, Vec<Multiaddr>>`. A relay address is **never** stored in `reached_by`, so `redial_lost_peer` never hurries a punch.
  - **Connections.** `ConnectionEstablished` emits `connected` with `relayed`, and calls `dials.forget_peer`.
    - A direct connection's address goes into `reached_by` and the peer book.
    - When a peer whose direct link was lost has entries in `circuits_of`, they are re-added as `Kind::Circuit`. Each is new, so one punch is tried at once and the backoff then applies.
  - **Punch results.** `dcutr::Event { remote_peer_id, result }` → emit `punch-succeeded` on `Ok`, or `punch-failed` (field `error`) on `Err`.
  - **`node_events.py` entries:**
    - `"introducer-held": "{label} will introduce this node to members it cannot reach"`
    - `"introducer-lost": "an introducer stopped: {error}"`
    - `"punch-succeeded": "PUNCHED THROUGH to {label}: a direct link, no longer through anyone"`
    - `"punch-failed": "could not punch through to {label}: {error}"`
    - Extend `"connected"` with `(relayed: {relayed})`.
- [ ] **Step 6: Run the whole suite.** Run: `cd node && cargo test && cd ../provision && python3 -m unittest discover -p 'test_*.py'`. Expected: all pass.
- [ ] **Step 7: Commit.** The reasoning: introductions use the relay only for the 2 minutes a punch needs, and gossip still carries everything else. Run F's two endpoint-independent NATs failed a blind simultaneous dial, and DCUtR times it.

---

### Task 6: UPnP beside NAT-PMP

**Files:**
- Create: `node/src/upnp.rs`
- Modify: `node/src/portmap.rs`. Change `fn default_gateway` to `pub(crate) fn default_gateway`. Add `Method` and `first_granted`.
- Modify: `node/Cargo.toml`, adding `igd-next = { version = "0.16", default-features = false, features = ["aio_tokio"] }`
- Modify: `node/src/main.rs`. The `renew_mapping` task, at about line 1386, calls `first_granted`. `port-mapped` gains `method`. Shutdown releases the mapping by the method that granted it.
- Modify: `provision/node_events.py`
- Test: `mod tests` in `node/src/upnp.rs` and `node/src/portmap.rs`

**Interfaces:**
- Produces:
```rust
// portmap.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Method { NatPmp, Upnp }
impl Method { pub fn name(self) -> &'static str } // "nat-pmp" / "upnp"
pub async fn first_granted<A, B, FB>(nat_pmp: A, upnp: FB) -> Result<(Mapping, Method), String>
where A: Future<Output = Result<Mapping, String>>, FB: FnOnce() -> B, B: Future<Output = Result<Mapping, String>>;
// upnp.rs
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(2); // per search, unicast then multicast
pub(crate) fn search_options(gateway: Option<Ipv4Addr>) -> Vec<igd_next::SearchOptions>;
pub async fn request(port: u16, lifetime: u32) -> Result<portmap::Mapping, String>; // UDP, internal port == external port
pub async fn release(port: u16);
```
- [ ] **Step 1: Write the failing tests:**
```rust
// portmap.rs
#[tokio::test] async fn a_nat_pmp_grant_means_upnp_is_never_asked() // the upnp closure panics if called
#[tokio::test] async fn upnp_is_asked_when_nat_pmp_refuses()          // returns (mapping, Method::Upnp)
#[tokio::test] async fn both_refusals_are_reported_together()          // Err contains both messages
// upnp.rs
#[test] fn the_gateway_is_asked_directly_before_multicast() {
    let o = search_options(Some(Ipv4Addr::new(192, 168, 1, 1)));
    assert_eq!(o[0].broadcast_address, "192.168.1.1:1900".parse().unwrap());
    assert_eq!(o[1].broadcast_address, "239.255.255.250:1900".parse().unwrap());
}
#[test] fn with_no_gateway_only_multicast_is_tried()                   // len()==1, multicast
#[test] fn the_whole_upnp_search_is_bounded() {                        // each timeout is SEARCH_TIMEOUT, total <= 6 s
    assert!(search_options(Some(Ipv4Addr::LOCALHOST)).iter().map(|o| o.timeout.unwrap()).sum::<Duration>() <= Duration::from_secs(6));
}
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd node && cargo test upnp first_granted nat_pmp both_refusals`. Expected: does not compile.
- [ ] **Step 3: Implement.**
  - **`request`** tries each `search_options` entry in turn, setting `timeout` and `single_search_timeout` to `SEARCH_TIMEOUT`. On the first gateway found, it calls `get_external_ip` and then `add_port(UDP, port, (local_ip, port), lifetime, "machine elves mesh")`. `local_ip` is the address the host uses to reach the gateway.
  - **A send that fails** with `EPERM`, as from `isolate-lan`, is a failure of that search, not of the node.
  - **The caller** stays a spawned task, so nothing here blocks the loop. The existing test `asking_the_router_takes_longer_than_a_peer_takes_to_be_declared_dead` must still hold.
  - **`node_events.py`:** `"port-mapped": "the router opened {external} by {method}"`.
- [ ] **Step 4: Run the whole suite, then the static build.** Run: `cd node && cargo test && cd .. && ./release.sh`. Expected: all pass, and `dist/mesh-node` is static.
- [ ] **Step 5: Commit.** The reasoning, in the operator's words: "work on as many computers as possible with the least router configuration". Also: the friend's router answers neither protocol, and the gateway is asked first because `isolate-lan` forbids multicast.

---

### Task 7: The link summary

**Files:**
- Create: `provision/link-summary.py` and `provision/test_link_summary.py`
- Uses: `takeover-summary.with_rotations` and `chaos.armed_window`, for the same reading of logs and windows

**Interfaces:**
- Produces:
```python
def owner_spans(events) -> dict[str, list[tuple[int, int]]]
    # per node: (from_tick, to_tick) it computed as owner. Starts at `claimed`/`took-over` tick;
    # ends at `yielded` our_tick, else the last `working` tick before that node's next `started`, else its last `working`.
def duplicated_ticks(spans) -> int          # ticks inside spans of two or more nodes at once
def links(events) -> dict[tuple[str, str], dict]  # per label pair: direct, relayed, punches_tried, punches_ok
def main(argv) -> int                        # prints LINKS, INTRODUCERS, REACHABLE, DUPLICATED WORK for the armed window
```
- [ ] **Step 1: Write the failing tests:**
```python
def test_run_f_part_two_shape_counts_the_overlap(self):  # diamond owns 100-200, yoinkerz took over at 150, yielded at 190 -> 40
def test_one_owner_at_a_time_duplicates_nothing(self)
def test_a_killed_owner_ends_at_its_last_working_line(self)
def test_a_direct_link_and_a_relayed_one_are_counted_apart(self)  # connected relayed=false vs true, per unordered pair
def test_punches_are_counted_per_pair_with_their_outcome(self)
```
- [ ] **Step 2: Run them to verify they fail.** Run: `cd provision && python3 -m unittest test_link_summary -v`. Expected: `ModuleNotFoundError`, or a load error.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to verify they pass, then check it against Run F.** Run: `python3 -m unittest test_link_summary -v && python3 link-summary.py results/run-f/*/events.jsonl results/run-f/*/chaos.jsonl`. Expected: 5 passed. Run F's `DUPLICATED WORK` is roughly 16,800 ticks or more (part 2 alone was about 16,800), and there are no direct diamond–yoinkerz links.
- [ ] **Step 5: Commit.**

---

### Task 8: The NAT rig: Run F in namespaces

**Files:**
- Create: `node/nat-rig.sh`, `node/fake-igd.py`

**The topology.** It follows `probe/inbound-rig.sh`'s style, with every namespace removed on exit:
```
mer-a 10.1.0.2 ── mer-nata (203.0.113.1) ──┐
mer-b 10.2.0.2 ── mer-natb (198.51.100.1) ─┼── mer-inet ── mer-pub 192.0.2.10 (public, --own)
```
- `mer-a` and `mer-b` pass `--peer /ip4/192.0.2.10/udp/4001/quic-v1`, keep their port mapping on, and run with `--no-mdns --json`.
- Every node gets its own `--identity`, `--ledger` and `--peer-book` under `$OUT`, and runs the factory job.
- `mer-pub` runs with `--no-map-port`.
- The nodes run as background processes inside `ip netns exec`, and are killed by PID.
- An `nft` counter in `mer-b` and in `mer-a` on output UDP to the other side's public address measures dial cost.

**The scenarios,** run as `sudo ./nat-rig.sh preserving|randomised|upnp`. Each prints `PASS` or `FAIL` for every check, and exits non-zero on any failure.

| Scenario | NATs | Checks |
|---|---|---|
| `preserving` | both `masquerade` | within 180 s, `punch-succeeded`, and a and b `connected relayed=false` to each other. Kill pub: over 60 s, no `vanished` between a and b, and exactly one of a and b logs `working`. Restart pub: it logs `joined` within 20 s. |
| `randomised` | natb `masquerade random,fully-random` | within 180 s, `introducer-held` on a and b, and at least one `punch-failed`, with no direct a–b link. Kill pub: both a and b become owners, as in Run F. The dial cost from b to a's address and from a to b's, over 120 s and extrapolated, is **under 1 MB/h**. Restart pub: one `yielded` within 10 s. |
| `upnp` | as `randomised`, plus `fake-igd.py` in mer-nata | within 120 s, a logs `port-mapped` by `upnp` and then `reachable`, and b gains a direct link to a. Kill pub: no `vanished` between a and b, and exactly one owner. |

All three scenarios also show **transitive discovery** working under the backoff: a and b are given only pub's address.

**What `fake-igd.py` does** (standard library only):
- It answers SSDP M-SEARCH, unicast and multicast, on 10.1.0.1:1900, with `LOCATION: http://10.1.0.1:5000/rootDesc.xml` and `ST`/`USN` for `InternetGatewayDevice:1`.
- It serves a description with one `WANIPConnection:1` service whose control URL is `/ctl/IPConn`.
- It handles four SOAP actions, chosen by the `SOAPAction` header:
  - `GetExternalIPAddress` returns 203.0.113.1;
  - `AddPortMapping` runs `nft add rule ip nat pre iif <mer-nata's outside interface> udp dport <ext> dnat to <client>:<int>` and keeps the rule's handle;
  - `DeletePortMapping` deletes that rule;
  - `GetSpecificPortMappingEntry`.

- [ ] **Step 1: Write the rig and `fake-igd.py`.** The checks are the test here: each scenario's table row, read from the JSON event logs with `jq` or `python3 -c`.
- [ ] **Step 2: Run `sudo node/nat-rig.sh preserving`.** Expected: every check passes. If `punch-succeeded` never appears, stop, and record the DCUtR or QUIC error from `punch-failed`. The spec's last builder item ("whether a punch over QUIC works in libp2p 0.56") is answered here. Fix the node, not the rig.
- [ ] **Step 3: Run `sudo node/nat-rig.sh randomised`.** Expected: every check passes, with the dial cost under 1 MB/h. If the configured cap of 60 s breaks the limit for pub's address while pub is down, raise `cap(Kind::Configured)` to 120 s in Task 1's module and its test, and say so in the commit.
- [ ] **Step 4: Run `sudo node/nat-rig.sh upnp`.** Expected: every check passes.
- [ ] **Step 5: Run the existing rigs.** Run: `node/rig.sh && node/migration-rig.sh && sudo node/chaos-rig.sh 15`. Expected: each passes as before. For the chaos rig that means the four must-be-zeros at 0, and detection and resume p95 inside 3 s and 500 ms.
- [ ] **Step 6: Commit.** Record each scenario's result line in the message.

---

### Task 9: Run G's handover

**Files:**
- Modify: `docs/phase-0-plan.md`. Add "## Run G: handover", before "## Open decisions".
- Stage: `dist/` built by `./release.sh`, copied into `~/rune` on riverboat, diamond (over LTE, compressed) and yoinkerz, with `SHA256SUMS` checked on each.

- [ ] **Step 1: Write the handover.** Use the same three parts as Run F (0:00–0:45, then riverboat leaves until 1:40, then it returns until 2:00; the hotspot limit is 2 h). It covers:
  - **The test's own scope,** from the spec: the friend's router answers neither NAT-PMP nor UPnP, so Run G tests whether DCUtR's coordinated punch connects diamond and yoinkerz.
  - **Predictions, fixed before the run:**
    - riverboat `reachable`, and diamond and yoinkerz not;
    - both hold an introducer on riverboat;
    - **a direct diamond–yoinkerz link: not predicted.** Run F's packets were lost in both directions despite continuous sending, and timing is not what failed;
    - punches retried no more than the circuit schedule allows (60 s doubling to 1,800 s);
    - when riverboat leaves without a punch having worked, the split recurs, and heals within seconds of its return;
    - yoinkerz's mesh bytes at **20–35 MB** for the run, against Run F's 65.4 MB, because failed dialling was about two-thirds of that;
    - the four counts and the thresholds as in Run F.
  - **What would mean the punch is not worth keeping:** relay traffic per hour, computed from the counters, above the dial cost it replaced.
  - **Commands for the human,** as Run F's, plus `--peer-book` arriving through `timed-run.sh`'s `EXTRA`. The nft counter table goes on all three boxes.
- [ ] **Step 2: Stage and verify.** Run: `sha256sum -c SHA256SUMS` on each box, run by me over ssh, with no sudo needed. Expected: every file OK.
- [ ] **Step 3: Commit** the handover. The predictions are committed before the run, as for every run.

---

## Self-review

- **Spec coverage:**
  - Goals 1–2 (UPnP, introducers): Tasks 6 and 5.
  - Goal 3 (punch): Task 5.
  - Goal 4 (peer book): Task 2.
  - Goal 5 (backoff): Task 1.
  - Goal 6 (observability): events in Tasks 2, 4, 5 and 6, and the summary in Task 7.
  - Goal 7 (nothing worse): Tasks 1 and 8.
  - Validation: Tasks 1–8, with Run G in Task 9.
  - The reachability decision: Task 4.
  - Unicast first, then multicast: Task 6.
  - Three introducers: Task 5.
- **Corrections the plan makes to the spec:**
  - The backoff reset rule (Task 1, step 6), because hearing a peer through a forwarder must not reset its dial.
  - "Who runs what" (Task 4), because the relay server is on every node, with the choice made by the heartbeat flag.
- **Type consistency:** `Kind` and `Schedule` are used by Tasks 2 and 5 as Task 1 defines them. `Member.reachable` comes from Task 3, and `Reachability` from Task 4. `portmap::Mapping` is unchanged and reused by `upnp::request`.
