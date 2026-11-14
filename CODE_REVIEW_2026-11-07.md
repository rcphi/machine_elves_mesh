# Machine Elves code review and proposed change specification

Original review: **2026-11-07**. Reconstructed into a persistent repository file: **2026-11-13**.

Reviewed baseline: **`5a7d71a` — Merge the LTE back channel for diamond**. On reconstruction, the checkout still has that HEAD, and the relevant implementation locations below remain present.

## Purpose and status

This file restores the complete proposed change list from the lost `/tmp/machine-elves-review-2026-11-07.md`. It is a review handoff for Claude Code, not an implementation patch. No application changes were implemented during the original review or this reconstruction. Therefore there is no application `git diff` to apply.

The original review covered the Rust node and probe, sample WASM jobs, iroh spike, provisioning scripts, systemd configuration, analysis tools, tests, and documentation. It identified 20 substantive findings, plus documentation and test cleanup. All are preserved below, with concrete proposed fixes and validation expectations.

Evidence labeled **reproduced** refers to the original November 7 review. Its temporary test sources were lost with `/tmp`; the scenarios are described here so they can be recreated. The full test suites were not rerun merely to reconstruct this document. Code-inspection findings are distinguished from live-machine behavior that still needs verification.

## Instructions for the implementing reviewer

1. Review each finding against the current implementation before changing it. Record any disagreement or accepted limitation rather than silently dropping an item.
2. Preserve Phase 0 scope. This is an experiment intended to produce measurements, not a request to build a production scheduler or general-purpose distributed database.
3. Preserve the existing decisions: no rented infrastructure for the measured mesh, no management traffic on the measured path, a standard-library-only probe, and timing thresholds fixed before measurement.
4. Keep raw historical measurements and experimental reasoning. Correct or qualify conclusions when their evidence is insufficient; do not rewrite history as though improved measurements had already been performed.
5. Implement fixes in reviewable groups with appropriate regression tests. Hardware, routing, firewall, SSH, and volunteer-machine changes need their own verification; this handoff does not assert that they were deployed or tested on those machines.
6. Do not change numeric thresholds just to make results pass. Where a behavior is intentionally out of scope, document and expose that limitation accurately.

## Priority and proposed implementation order

- **P1:** fix before relying on the affected persistence, resource boundaries, or measurements.
- **P2:** a functional defect or unsupported behavior that should be corrected or explicitly scoped out.
- **Cleanup:** documentation or test maintenance, subordinate to behavior fixes.

Recommended order:

1. Ledger recovery and guest-triggered resource bounds: findings 1–2.
2. Measurement validity and recovery attribution: findings 3–4, 12–14.
3. Compatible job membership, production identities, checkpoint transport, determinism, and persistence: findings 5–9 and 11.
4. Actual mDNS disabling, log history, installer workflows, isolation, rigs, and operator views: findings 10 and 15–20.
5. Documentation and test cleanup listed after the findings.

## 1. P1 — Recovery leaves a torn ledger record on disk

**Locations:** `node/src/ledger.rs`, especially `decode_items` around line 44, `Ledger::load` around line 283, and `append` around line 304.

Loading drops an incomplete final record from the in-memory inventory, but leaves its bytes in the file. Later appends go after that fragment. The next restart encounters it again and can discard later records or misinterpret their bytes. The existing truncation test checks decoding once, without a subsequent append and restart.

**Reproduced:** append one complete item; append `[6, 0]`, representing a partial next length header; load successfully with one item; append another complete item; reload. Only one item is recovered instead of two.

**Proposed change:** track the last valid byte offset and repair an incomplete tail before allowing further appends. Distinguish recoverable truncation from unexpected corruption and report recovery. Avoid a whole-file rewrite per tick.

**Acceptance:** a real file test covering write → torn append → load/recover → append → second load retains both complete items. Add boundary cases around header, kind, and serial truncation.

## 2. P1 — Guest-triggered resource use escapes the advertised limits

**Locations:** `node/src/job.rs`, `StoreLimitsBuilder` around line 143; `node/src/main.rs`, `record_production` around lines 2082–2096.

The runner limits linear memory, but leaves table elements unbounded and does not select a small table count. WASM table storage is separate from linear memory. Also, a tiny effect such as `produce widget 4294967295` causes billions of hashes, set operations, and host allocations after WASM fuel metering has ended. Guest instruction and linear-memory limits do not bound that host work.

**Reproduced:** a module with a million-entry table successfully ticks under a 64 KiB configured linear-memory ceiling and 1,000 fuel. The unbounded production path was established by inspection; no resource-exhausting production count was executed.

The table test can be recreated from this small WAT module, loaded through `Job::load_with(path, 1000, 65536)` and ticked with empty state and inputs:

```wat
(module
  (memory (export "memory") 1)
  (table 1000000 funcref)
  (func (export "alloc") (param i32) (result i32) i32.const 16)
  (func (export "tick") (param i32 i32) (result i64) i64.const 8))
```

**Proposed change:** bound table count/elements and validate effect syntax, kind length, production count, and aggregate host work per tick before applying effects. Reject invalid numeric counts explicitly instead of silently defaulting them to one. State precisely what each configured memory limit covers.

**Acceptance:** small, bounded tests cross each configured limit and fail cleanly without excessive allocation or partial application. Existing factory jobs continue to run.

## 3. P1 — Mapping-lifetime results do not prove mapping survival

**Locations:** `probe/src/main.rs`, `probe_mapping_lifetime`, especially the second request and comparison around lines 751–769; downstream claims in `provision/summarise.py`, `probe/README.md`, and `docs/phase-0-plan.md`.

After idling, the probe sends another request from the socket under test. That outbound request can recreate an expired NAT mapping at exactly the old external endpoint, especially on a port-preserving router. The code calls the unchanged endpoint `Survived`. It has actually established endpoint reuse, not survival throughout the idle interval.

This affects the documented long survival observations on the port-preserving home router and any keepalive recommendation derived from them. It does not by itself establish what that router's real timeout is.

The reference inbound method uses a second source port to request a response to the idle binding without first refreshing it: [RFC 5780 §4.6](https://www.rfc-editor.org/rfc/rfc5780.html#section-4.6).

**Proposed change:** distinguish endpoint reuse from confirmed mapping survival in results and summaries. Stop deriving confirmed-safe lifetimes from the current method. Implement an inbound test using a cooperating participant or a server supporting the required STUN behavior, within the project's infrastructure constraints. Qualify historical conclusions while preserving raw data.

**Acceptance:** a simulated expired mapping recreated at the same port cannot be reported as confirmed survival. A genuine inbound lifetime test distinguishes expiration without an outbound refresh from the tested socket.

## 4. P1 — The takeover report can hide a real outage

**Location:** `provision/takeover-summary.py`, `kept_working_elsewhere` around line 158.

Two `working` summaries within 90 seconds before and after a disruption are treated as proof that another node worked continuously through it. The function ignores intervening yields, restarts, failures, ownership changes, and gaps. A `working` event is itself an aggregate over the preceding minute.

**Reproduced:** a survivor works, yields, stops, then claims the job 20 seconds after the owner is killed. Summaries on either side cause the function to report that the survivor kept working. The report excludes the disruption from its orphan count despite the interruption.

**Proposed change:** reconstruct ownership and process lifetimes, honor intervening loss events, and mark insufficient evidence as unknown. Do not infer continuous execution solely from two summaries.

**Acceptance:** fixtures with a yield, failure, or restart between summaries cannot establish uninterrupted work. Preserve the legitimate case where another owner demonstrably continued through the disruption.

## 5. P2 — Job compatibility and successor eligibility are assumed

**Locations:** `node/src/main.rs`: message definitions around line 229, `Work::should_take_over` around line 552, and checkpoint handling around line 1947.

Checkpoints do not identify the job whose state they contain. Nodes supplied different `--job` files in one mesh can accept each other's state and ownership claims. The election also considers members launched without any job: a lower-ID member that cannot execute the job can block compatible survivors indefinitely.

The intended single-common-job assumption is documented but is not enforced at the protocol boundary.

**Proposed change:** advertise job identity and execution eligibility, reject incompatible checkpoints, and elect compatible holders only. Enforce the current single-job scope rather than expanding this into a multi-job scheduler. Document any protocol compatibility implications.

**Acceptance:** test a heartbeat-only member with the lowest ID and two members holding incompatible job files. Neither can corrupt or prevent recovery of a compatible job.

## 6. P2 — Repeated production lines collide on item identity

**Location:** `node/src/main.rs`, `record_production` around line 2082.

The ordinal restarts at zero on every effect line. Multiple lines producing the same kind at one tick derive overlapping serials.

**Reproduced:** `produce widget 2\nproduce widget 3\n` records three widgets instead of five. The current factory emits one line per kind, hiding the problem in its ordinary tests.

**Proposed change:** allocate ordinals across the whole tick for each kind, or aggregate validated counts before serial derivation. Consider the effect of identity changes on interoperability with existing jobs and ledgers.

**Acceptance:** repeated same-kind lines produce five distinct items, and replaying the same tick remains idempotent. Different kinds remain independent.

## 7. P2 — Standbys silently discard ledger write errors

**Location:** `node/src/main.rs`, `Message::Produced` around line 1816; compare `publish_production` around line 2107.

Local production reports `ledger-not-saved`, but additions received from peers ignore `ledger::append` errors. Those items have already entered the in-memory set, so receiving them again will not trigger another append. A standby can look healthy, then forget replicated inventory on restart.

**Proposed change:** use consistent persistence/error handling for local and remote additions. Track unsaved records and retry them or otherwise make the degraded durability state explicit and recoverable.

**Acceptance:** inject a temporary write failure, restore writability, and restart. Successfully recovered records persist, and failure reporting is consistent for local and remote items.

## 8. P2 — A complete cold restart loses job progress

**Location:** `node/src/main.rs`, `Work` initialization around lines 1173–1174.

The inventory persists, but state and tick always initialize empty/zero. If every holder restarts, there is no surviving peer to supply a checkpoint. The designated owner starts from zero and repeats old work; serial deduplication suppresses repeated inventory until execution catches up.

**Proposed change:** atomically persist and recover a checkpoint keyed to job and mesh identity. If total-outage recovery is intentionally outside Phase 0, document that limitation and emit an explicit cold-start indication rather than implying recovered progress. Review this scope choice before implementing a broader persistence subsystem.

**Acceptance:** either a full-holder restart recovers compatible progress, including a torn-checkpoint case, or the unsupported recovery case is explicitly observable and accurately documented.

## 9. P2 — Valid job output can exceed gossip transport limits

**Locations:** `node/src/job.rs`, `MAX_BLOB` around line 52; `node/src/main.rs`, checkpoint publication around line 1409 and production publication around line 2123.

The runner accepts blobs up to 16 MiB while gossip retains its much smaller default message limit. Checkpoint publish errors are discarded. A job can execute successfully while no standby receives its state. Limiting production chunks by item count likewise does not bound encoded bytes when kind names are long.

**Proposed change:** reconcile execution and wire limits, validate state size before accepting an unreplicable checkpoint, split production by encoded size, and expose actionable publication failures. Do not classify normal publication with no peers as an exceptional failure flood.

**Acceptance:** exercise states just below/above the configured wire limit and long kind names. Every accepted checkpoint is transferable, or a clear supported failure is emitted.

## 10. P2 — `--no-mdns` leaves mDNS sockets and responses active

**Location:** `node/src/main.rs`, behavior construction around lines 1046–1055.

The flag changes the query interval to a year and ignores discovery events, but still creates the mDNS behavior. Inspection of the locked libp2p-mdns implementation during the original review showed multicast sockets, an immediate initial probe timer, and replies to incoming queries independently of that interval. The comment claiming it is "never speaking" is incorrect.

**Proposed change:** make the behavior genuinely optional, for example using libp2p's behavior toggle. Do not represent disabled behavior using a large timer interval.

**Acceptance:** the disabled configuration creates no mDNS behavior sockets or responses. Local discovery still functions when enabled.

## 11. P2 — Import-free WASM does not guarantee deterministic execution

**Location:** `node/src/job.rs`, engine configuration around line 80.

The engine enables fuel but not NaN canonicalization or deterministic relaxed SIMD, and does not reject those features. Removing imports excludes external clocks/randomness but does not remove these WASM sources of variation. The integer-only factory avoids them; its tests establish a narrower property than the runner's comments imply.

Reference: [Wasmtime deterministic execution documentation](https://docs.wasmtime.dev/examples-deterministic-wasm-execution.html), including qualifications around memory and table growth.

**Proposed change:** define and enforce a deterministic admitted feature subset. Canonicalize NaNs and disable relaxed SIMD or make it deterministic. State allocation/growth limits accurately rather than promising unconditional determinism under host allocation failure.

**Acceptance:** tests cover the admitted numeric features and rejected features. Documentation distinguishes the verified factory behavior from the runner's general guarantee.

## 12. P2 — Keepalive advice omits the least safe participants

**Location:** `provision/summarise.py`, `safe_per_machine` and recommendation calculation around lines 95–105.

Machines with no successful interval are omitted from the calculation. A mesh-wide interval is then derived only from machines with successes. The 15-second floor can also erase the claimed twofold margin when 15 seconds is the shortest safe observation.

**Reproduced:** one machine reports survival at 600 seconds; another reports expiry at 15 seconds. The script recommends 300 seconds for the mesh.

**Proposed change:** require adequate evidence for every participating machine and report missing/unsafe evidence. Remove the floor where it contradicts the safety margin. First address finding 3: the present observations are not confirmed-safe lifetimes.

**Acceptance:** the above input cannot produce a safe mesh-wide recommendation; neither can inconclusive-only evidence for a participant. Boundary tests preserve the stated margin.

## 13. P2 — The aggregate report converts candidates into confirmed reachability

**Locations:** `provision/summarise.py` around lines 231–280; `probe/README.md`.

The probe deliberately renamed its result to `relay_candidate` because STUN alone cannot prove that inbound packets pass a firewall. The summary consumes that field but prints "can accept incoming (relay-capable)" and makes proceed/stop judgments from it. Three labels without candidates are treated as meeting a stop criterion without actual peer-connectivity/port-mapping/relay-cost evidence or establishing that the labels represent distinct networks.

**Proposed change:** preserve candidate, confirmed, and unknown states through all tooling. Use actual peer evidence for inbound reachability and the documented relay measurements for the stop criterion. Do not infer CGNAT layers, inbound reachability, or impossibility solely from ordinary external STUN observations.

**Acceptance:** global/public candidate fixtures remain unconfirmed until peer evidence exists. Endpoint-independent NAT and successful port mapping cannot cause a false stop verdict merely because the original STUN candidate flag is false.

## 14. P2 — Overlapping disruptions can borrow unrelated recoveries

**Location:** `provision/takeover-summary.py`, matching around lines 228–244.

After a loss event is selected, any later takeover in the time window is eligible. `took-over` lacks previous-owner identity. An unrelated takeover can be selected, one takeover can satisfy multiple disruptions, and detection/resume evidence can come from different observers without an explicit causal association.

**Proposed change:** add job, previous-owner, process-run, and recovery identifiers where needed, then correlate events to the appropriate action/recovery. Retain old-log support but mark ambiguous matches explicitly. Preserve the distinction between speculative resume before confirmation and ordinary resume after takeover.

**Acceptance:** fixtures with overlapping victims, multiple observers, and adjacent speculative/ordinary recoveries cannot double-use or borrow a recovery. Ambiguous historical evidence is not presented as a precise sample.

## 15. P2 — Log history is still lost or reconstructed incorrectly

**Locations:** `provision/collect.sh` around line 38; `takeover-summary.py::with_rotations` around line 85; `chaos.py::read_events` around line 128 and `situation` around line 155; `node-summary.py` around line 145; `provision/logrotate.mesh-probe` around line 9.

Related problems:

- Collection reads only the current probe log and overwrites the last local snapshot. After remote rotation, older measurements disappear from the merged dataset.
- Takeover rotation discovery accepts single-digit suffixes only, omitting `.10` through `.14` despite retaining 14 rotations. **Reproduced:** `.10.gz` is not discovered.
- Chaos searches at most seven rotations; node-summary reads only `.1`. Long-lived runs can lose initial joins, claims, and start boundaries.
- `chaos.situation` does not reset ownership at `started` when called across multiple process runs by the takeover summary. **Reproduced:** `claimed` followed by `started` still reports ownership.
- The probe logrotate policy switches to `meshprobe`, while the node policy explicitly records that systemd append output creates root-owned files. On that ownership layout the account cannot copy-truncate the probe log. **This subfinding requires confirmation against actual installed probe-file ownership.**

**Proposed change:** share rotation discovery/parsing, accept every retained numeric suffix and gzip, preserve/deduplicate collected history, and reset reconstructed state at process boundaries. Make logrotate permissions agree with observed service-created file ownership. Show partial history explicitly.

**Acceptance:** cover multi-digit rotations, compressed history, long runs spanning retained rotations, repeated collection across rotation, and restarts between claims. Verify production ownership before changing its rotation policy.

## 16. P2 — Re-running setup does not apply changes to an active node

**Location:** `provision/setup.sh`, especially around line 257.

Setup overwrites the binary/config and invokes `systemctl enable --now`. Starting an active service does not restart it. Setup can report success while the previous process still uses its old binary, label, mesh, peers, and job. Re-running setup also regenerates defaults, overwriting manual settings the README tells operators they may edit.

**Proposed change:** define update semantics, retain settings not explicitly overridden, and restart an active node when its executable or effective configuration changes. Preserve the intentional fresh-install defaults.

**Acceptance:** an already-installed fixture retains unspecified manual settings and applies explicit changes to a restarted process. An unchanged rerun avoids unnecessary disruption where practical.

## 17. P2 — The fresh-machine release archive workflow is incomplete

**Location:** `release.sh`, archive creation/instructions around lines 128–137.

The archive contains binaries, jobs, checksums, and a version file, but no setup script, wrapper, systemd units, or rotation policies. Its printed installation command refers to a sibling repository the workflow never supplies. The separately documented "repository already on the box" path does not repair the fresh-machine path.

**Proposed change:** include the provisioning files needed by the standalone workflow and print a valid packaged installation command, or explicitly require and document obtaining a checkout first.

**Acceptance:** inspect an extracted archive on a fixture with no checkout and verify that every referenced installation asset exists. Avoid installing services on the development host to test packaging.

## 18. P2 — Optional household isolation does not cover IPv6

**Location:** `provision/setup.sh`, `--isolate-lan`, particularly the IPv4 range loop around line 326.

The option promises to prevent access to the household's other devices, but only blocks four IPv4 destination ranges. IPv6 neighbors remain reachable, including neighbors using globally addressed on-link prefixes. A successful STUN query checks retained internet access, not household isolation.

**Proposed change:** specify isolation for actual connected prefixes and both address families, with gateway/DHCP/neighbor-discovery exceptions. Validate blocking as well as internet access, and make rollback handle partial failures. Keep the management channel usable and outside the measured path.

**Acceptance:** isolated-rig tests demonstrate household blocking on both families, retained intended internet/management access, and rollback on a mid-application failure. Physical deployment behavior remains a separate verification step.

## 19. P2 — The migration rig can pass without resumed execution

**Location:** `node/migration-rig.sh`, takeover checks around lines 115–135 and cleanup around line 47.

The rig times `took-over` and prints "resumed" without requiring a first successful tick or `resumed` event. Claiming ownership then failing the first tick can pass. The script describes exactly one survivor, but multiple takeovers only produce a note. `pkill -x mesh-node` can terminate unrelated processes owned by the invoking user.

**Proposed change:** assert post-recovery successful progress and the intended timing metric; distinguish election from execution; explicitly enforce/document the chosen duplicate-owner policy. Clean up only PIDs/units launched by this rig. Preserve unique mesh names and per-node files.

**Acceptance:** a takeover followed immediately by job failure fails the rig. Actual successful recovery passes. An unrelated mesh-node process is left alone. Duplicate takeover behavior matches the stated experiment criterion.

## 20. P2 — Operator views consume retired events

**Locations:** `provision/node-summary.py` around lines 221–223 and `provision/node-watch.sh` event mapping.

The summary counts `effect` and `produced`, while the node now emits aggregated `working` records. A healthy log with 300 ticks and 120 held items reports "0 effects, 0 productions". Both operator views omit important newer events, including speculation and its outcome, contrary to the claim that they report the same events.

**Reproduced:** feed node-summary a `started`, `job-loaded`, and `working` event carrying `ticks="300"`, `tick="300"`, and `ledger="120"`; it prints the obsolete zero work counts.

**Proposed change:** document/share the event mapping, read current `working` fields, and represent resumed/speculation/failure events consistently. Do not equate current ledger inventory with production during a selected window.

**Acceptance:** representative current-node logs show actual tick activity and inventory accurately, with explicit scope. Watcher and summary expose the same supported important event categories.

## Documentation and unused-code changes

### D1. Refresh the current project entry point

`CLAUDE.md` says the mesh node is next. `docs/phase-0-plan.md` starts with "Planning. No code yet." The node module header says jobs/migration come later. These conflict with implemented and measured behavior.

Add a concise root README or equivalent current entry point covering structure, prerequisites, build/test/run commands, and present limitations. Update the stale state descriptions and link to historical evidence. Explain that the iroh spike deliberately uses infrastructure outside the primary mesh experiment's constraints.

### D2. Separate historical implementation plans from current instructions

Both files under `docs/superpowers/plans/` retain unchecked steps for completed work. Mark historical plans accordingly and point to current files. Preserve their reasoning and original targets.

In the LTE design, earlier five-minute watchdog/60-second timer descriptions and older SSH examples coexist with later corrections and the current two-minute/30-second behavior. Make current values and canonical deployed files unambiguous without deleting the chronology. Do not copy older permissive authorized-key examples over the actual restricted configuration.

### D3. Correct probe terminology and coverage

`probe/README.md` prints `relay-capable`, but the program prints `relay-candidate`. It omits `nat-unclassified` and newer diagnostic modes. Qualify mapping-lifetime claims per finding 3 and inbound/CGNAT claims per finding 13.

### D4. Reconcile static-build guarantees with fallback behavior

Documentation guarantees static binaries, but `release.sh` deliberately falls back to host-linked builds. Either make deployment releases require static artifacts or clearly document and validate the fallback's target requirements. Keep actual build targets visible in the manifest.

### D5. Correct diagnostic conclusions

The punch probe's `outbound-only` branch can include receipt of a PONG: that packet already demonstrates a request and response crossing both directions. Missing an independent HELLO does not prove a one-way network path. Report protocol evidence and bidirectional request/response accurately.

IPv4 mapping classification compares only external ports, not complete external endpoints. Stable ports with changing public addresses can be mislabeled endpoint independent. Compare the relevant address-and-port mappings, and distinguish them from proven peer reachability.

### D6. Remove genuinely unused helpers and fix the timeout description

`Job::short_id` in `node/src/job.rs` and `Serial::short` in `node/src/ledger.rs` have no callers. Remove them unless a concrete supported caller is introduced.

Keep `Job::id`: production serial derivation uses it. `portmap::WORST_CASE` is used only by tests; gate or relocate it appropriately rather than misclassifying it as wholly unused. Its description/formula covers one exchange, but `request` performs two exchanges that can each spend retry time. Correct the claimed full-request bound and related comments.

### D7. Retain intentional fixtures and exploratory code

The runaway/glutton jobs actively exercise fuel and allocation behavior. The iroh spike is a documented experiment with a recorded result. They are not orphaned merely because the main node does not call them during normal operation.

## Test changes

### T1. Make missing WASM integration fixtures visible

`node/tests/jobs.rs::job_or_skip!` and several direct early returns let missing compiled jobs appear as successful tests. Require fixtures in the integration verification workflow, build them as an explicit prerequisite, or provide an unmistakably optional suite that reports omissions honestly. Do not silently count unexecuted behavior as passing coverage.

### T2. Avoid running the runner's five unit tests twice

`node/tests/jobs.rs` includes `job.rs` by path, recompiling and executing its five unit tests a second time. A small library boundary can let integration tests exercise the real runner without duplicating its unit suite. Keep this refactor proportionate to the disposable spike.

### T3. Replace weak guards with meaningful behavior checks

`a_gateway_is_read_from_the_routing_table` can pass without assertions when discovery fails. Extract route parsing for fixture-based tests, keeping platform discovery separate.

The port-renewal regression checks that a timeout constant exceeds detection; it would still pass if renewal were accidentally awaited in the event loop again. Add a delayed fake-router test demonstrating that heartbeat/event-loop progress continues during renewal. Preserve useful configuration assertions, but do not treat them as proof of runtime scheduling behavior.

### T4. Cover the missing failure sequences

Keep useful historical regressions and add the acceptance cases for findings 1–20. In particular: recover/append/restart, host-side effect bounds, repeated production kinds, incompatible/non-executing members, current log schemas, multi-digit rotations, and ambiguous recovery matching. Avoid tests that merely reproduce implementation expressions without checking externally meaningful behavior.

## Original verification record — November 7, not a fresh November 13 test run

| Check | Original result |
| --- | --- |
| Node unit suite | 66 passed |
| Node integration test executions | 15 passed |
| Probe | 13 passed |
| Factory job | 6 passed |
| Provisioning Python suite | 133 passed |
| Existing total | 233 test executions passed, including the five duplicated runner tests |
| iroh spike | `cargo check --locked --offline` passed after Cargo could unpack cached dependencies |
| Shell syntax | All 14 Bash scripts/wrappers passed `bash -n` |
| Committed prebuilt artifacts | Probe and factory-job hashes matched `prebuilt/SHA256SUMS` |

Three additional Rust checks in a temporary copy failed as expected, demonstrating torn-ledger recovery loss, same-kind production collisions, and unrestricted WASM table allocation. Python examples demonstrated outage misclassification, unsafe keepalive advice, missing `.10.gz`, ownership surviving `started`, and obsolete zero work counts.

Clippy and ShellCheck were not installed during that review. No live volunteer machines were contacted, no installed service/firewall/routing configuration was changed, and the privileged systemd/network rigs were not run. Actual modem behavior, household isolation, installed log ownership, and historical WAN results were not independently reverified.

The temporary harnesses and their outputs did not survive `/tmp`. Recreate the described regressions in the project tests when implementing fixes; do not cite the historical passing totals as validation of new code.

## Completion record for Claude Code

For each numbered finding and cleanup item, record whether it was fixed, superseded, disputed with evidence, or accepted as an explicit Phase 0 limitation. Include changed paths, tests actually run and their results, any protocol/data compatibility impact, and remaining physical-rig validation. Keep implementation diffs separate from this original review specification.
