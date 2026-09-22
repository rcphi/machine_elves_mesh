# Speculative Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standby continues a job as soon as its owner is *suspected* gone, so the player-visible stall falls from ~2.45 s to ~600 ms.

**Architecture:** One extra check inside the sweep that already runs every `detect/4`. When the owner has been silent that long and this node would be the successor, it starts ticking and publishing as if it owned the job, while `owner` still points at the suspect. Nothing new goes on the wire — speculative checkpoints and production are byte-identical to an owner's, because serials derive from `(job_id, tick, kind, ordinal)` and jobs are deterministic. Speculation ends as ownership when the loss is confirmed, or is abandoned when a checkpoint arrives that the existing two-owner rule says beats it.

**Tech Stack:** Rust (std + libp2p + wasmtime, built via rustup), Python 3 for the analysis tools, bash for the rigs.

**Spec:** `docs/superpowers/specs/2026-09-22-speculative-resume-design.md`

## Global Constraints

- **Trigger:** `speculate_after` defaults to `detect / 4` (500 ms at today's `detect` of 2 s). `--speculate-after-ms 0` disables speculation entirely.
- **No new message kinds.** Kind 6 is retired and must not be reused; this feature adds nothing to the wire protocol, so a node built before it interoperates with one built after it.
- **Only the designated successor speculates** — the existing lowest-peer-id rule, applied to membership *minus the suspect*.
- **`took-over` keeps its current name, timing and fields**, gaining only additions, so chaos-action pairing in `takeover-summary.py` keeps working.
- **Detection keeps its present meaning** — silence before a peer was declared gone — so the threshold stays comparable with Night A, its second run, and Run B.
- Rust is installed via rustup, not apt. Run `cargo` from `node/`.
- Tests accompany behaviour; `cargo test` must pass before any claim that something works.
- Commit messages carry the reasoning, not just the change.

---

### Task 1: The suspicion rule and its flag

The decision to speculate is a pure function so it can be tested without a swarm. Getting the "minus the suspect" part wrong makes speculation silently never fire in a two-node mesh, which no end-to-end test distinguishes from bad luck.

**Files:**
- Modify: `node/src/main.rs` — `Config` struct (~line 427), argument parsing (~line 646), `print_usage` (~line 765), `Work` impl (~line 500), tests module (~line 2100)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `Config.speculate_after: Option<Duration>`; `Work::lowest_among(me: &PeerId, peers: impl Iterator<Item = &PeerId>) -> bool`; `Work::should_speculate(me: &PeerId, owner: &PeerId, members: &HashMap<PeerId, Member>, now: Instant, after: Duration) -> bool`.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `node/src/main.rs`:

```rust
    fn member_last_heard(at: Instant) -> Member {
        Member { label: "peer".into(), joined: at, last_heard: at, disconnected: false, run: Some(1) }
    }

    #[test]
    fn speculation_waits_for_the_owner_to_go_quiet() {
        let now = Instant::now();
        let after = Duration::from_millis(500);
        let (me, owner) = (PeerId::random(), PeerId::random());
        let mut members = HashMap::new();
        members.insert(owner, member_last_heard(now - Duration::from_millis(200)));

        assert!(!Work::should_speculate(&me, &owner, &members, now, after),
                "200 ms of silence is one lost packet, not a missing machine");

        members.insert(owner, member_last_heard(now - Duration::from_millis(600)));
        assert!(Work::should_speculate(&me, &owner, &members, now, after));
    }

    #[test]
    fn only_the_successor_speculates_and_the_suspect_does_not_count() {
        // The election must exclude the node being suspected. Including it
        // would mean that in a two-node mesh the higher identifier is never
        // eligible, and speculation would appear simply not to work.
        let now = Instant::now();
        let after = Duration::from_millis(500);
        let quiet = now - Duration::from_secs(1);
        let mut ids = [PeerId::random(), PeerId::random(), PeerId::random()];
        ids.sort();
        let [low, middle, high] = ids;

        let mut members = HashMap::new();
        members.insert(low, member_last_heard(quiet));
        members.insert(high, member_last_heard(now));

        // `low` owns the job and has gone quiet. `middle` is the lowest of the
        // survivors, so it speculates and `high` does not.
        assert!(Work::should_speculate(&middle, &low, &members, now, after));

        let mut seen_by_high = HashMap::new();
        seen_by_high.insert(low, member_last_heard(quiet));
        seen_by_high.insert(middle, member_last_heard(now));
        assert!(!Work::should_speculate(&high, &low, &seen_by_high, now, after));
    }

    #[test]
    fn a_peer_that_is_not_a_member_is_not_speculated_about() {
        // Its silence cannot be measured, so there is nothing to suspect.
        let now = Instant::now();
        let (me, stranger) = (PeerId::random(), PeerId::random());
        assert!(!Work::should_speculate(&me, &stranger, &HashMap::new(), now,
                                        Duration::from_millis(500)));
    }

    #[test]
    fn suspicion_falls_strictly_inside_detection() {
        // Speculation that fired at or after detection would never happen: the
        // takeover it is meant to precede would already have occurred.
        let detect = Duration::from_millis(DEFAULT_DETECT_MS);
        assert!(detect / 4 < detect);
        assert!(detect / 4 >= Duration::from_millis(DEFAULT_HEARTBEAT_MS) / 2,
                "a trigger below half a heartbeat would suspect a node that is answering normally");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd node && cargo test speculat`
Expected: FAIL — `no function or associated item named 'should_speculate' found for struct 'Work'`

- [ ] **Step 3: Add the rule**

In `impl Work`, replace `should_take_over` with a delegating pair and add the new rule:

```rust
    /// Whether `me` is the lowest identifier among the peers given.
    ///
    /// `peer != me` matters as much as the comparison. If this node ever
    /// appears in its own membership — which self-discovery over the local
    /// network will cause — then without it the test is `me < me`, which is
    /// false, and this node silently becomes ineligible forever.
    fn lowest_among<'a>(me: &PeerId, peers: impl Iterator<Item = &'a PeerId>) -> bool {
        peers.filter(|peer| *peer != me).all(|peer| me < peer)
    }

    fn should_take_over(me: &PeerId, members: &HashMap<PeerId, Member>) -> bool {
        Self::lowest_among(me, members.keys())
    }

    /// Whether to start continuing a job whose owner has gone quiet but is not
    /// yet presumed gone.
    ///
    /// The same election the takeover uses, run early and with the suspect
    /// excluded — it is precisely the node whose absence is being assumed, and
    /// counting it would, in a two-node mesh, make the survivor ineligible
    /// whenever its identifier is the higher of the two.
    fn should_speculate(
        me: &PeerId,
        owner: &PeerId,
        members: &HashMap<PeerId, Member>,
        now: Instant,
        after: Duration,
    ) -> bool {
        members
            .get(owner)
            .is_some_and(|suspect| now.duration_since(suspect.last_heard) >= after)
            && Self::lowest_among(me, members.keys().filter(|peer| *peer != owner))
    }
```

- [ ] **Step 4: Add the config field**

In `struct Config`, after `detect`:

```rust
    /// How long an owner's silence starts a standby continuing its job, before
    /// anyone is presumed gone. `None` runs the node exactly as it ran before
    /// speculation existed.
    speculate_after: Option<Duration>,
```

In the `Config { .. }` initialiser, beside `detect`:

```rust
        speculate_after: None,
```

In the argument loop, beside `--detect-ms`:

```rust
            "--speculate-after-ms" => {
                let ms: u64 = args.next().context("--speculate-after-ms needs a number")?.parse()?;
                // Zero is off rather than instant. A standby that suspected an
                // owner the moment a packet was late would run the job
                // permanently alongside it.
                config.speculate_after = (ms > 0).then(|| Duration::from_millis(ms));
            }
```

After the argument loop ends, before the `config.listen.is_empty()` block, add the default. It has to be here rather than in the initialiser because `--detect-ms` may be parsed after this point:

```rust
    // A quarter of the detection window, which is also how often the sweep
    // runs, so suspicion is noticed on the first sweep that could notice it.
    // Derived rather than independent: retuning detection retunes this, and a
    // pair of numbers that must stay ordered would eventually be set out of
    // order.
    let speculating_by_default = !std::env::args().any(|a| a == "--speculate-after-ms");
    if speculating_by_default {
        config.speculate_after = Some(config.detect / 4);
    }
```

In `print_usage`, after the `--detect-ms` line:

```rust
    println!("  --speculate-after-ms <n>");
    println!("                       silence before a standby starts continuing the job, ahead");
    println!("                       of any decision that the owner is gone (default: a quarter");
    println!("                       of --detect-ms). 0 turns speculation off");
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd node && cargo test`
Expected: PASS, all tests, including the four new ones.

- [ ] **Step 6: Commit**

```bash
git add node/src/main.rs
git commit -m "The rule for suspecting an owner, before anything acts on it

A standby speculates when the owner has been silent a quarter of the detection
window and this node would be the successor were it already gone. The election
is the existing one, with the suspect excluded — counting it would make the
higher identifier of a two-node mesh ineligible for ever, and speculation would
look like it simply never fires.

Derived from detect rather than set beside it, so retuning detection retunes
this and the two cannot drift into the wrong order. 0 is off, not instant.

Nothing calls it yet.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: Start speculating, and tick while doing it

**Files:**
- Modify: `node/src/main.rs` — `Work` struct (~line 475), new `Speculating` struct beside `Resuming` (~line 589), the sweep arm (~line 1276), the job tick arm (~line 1187)

**Interfaces:**
- Consumes: `Work::should_speculate`, `Config.speculate_after` from Task 1.
- Produces: `struct Speculating { about: PeerId, since: Instant, from_tick: u64 }`; `Work.speculating: Option<Speculating>`; the `speculating` event; a `resumed` event carrying `cause: "speculated"`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_speculative_first_tick_is_timed_from_the_suspicion() {
        // The visible gap is what a player waits through, so it runs from the
        // last checkpoint heard to the first state produced — and under
        // speculation that first state is produced before anybody has decided
        // the owner is gone.
        let checkpoint = Instant::now();
        let suspected = checkpoint + Duration::from_millis(500);
        let resuming = Resuming {
            cause: "speculated",
            decided: suspected,
            last_checkpoint: Some(checkpoint),
        };
        let (resume, gap) = resuming.timings(suspected + Duration::from_millis(100));
        assert_eq!(resume, Duration::from_millis(100));
        assert_eq!(gap, Some(Duration::from_millis(600)),
                   "600 ms is the whole point: detection is no longer inside it");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd node && cargo test a_speculative_first_tick`
Expected: FAIL — the test compiles only once `cause: "speculated"` is a value the code produces; before Step 3 it fails to compile against the current `Resuming` usage only if fields changed. If it passes immediately, that is expected for this one test: `Resuming` already carries what is needed, and the test pins the arithmetic that Step 3's wiring depends on. Proceed.

- [ ] **Step 3: Add the state**

Beside `struct Resuming`:

```rust
/// A job being continued on suspicion alone.
///
/// The owner has gone quiet but has not been declared gone, so `Work::owner`
/// still points at it: nothing has been decided, and this node is running the
/// job only because a stall costs a player more than duplicate work costs the
/// mesh.
struct Speculating {
    /// Whose silence started it.
    about: PeerId,
    since: Instant,
    /// The tick reached when it began, so work done on a suspicion that turns
    /// out to be wrong can be counted rather than guessed at.
    from_tick: u64,
}
```

In `struct Work`, after `resuming`:

```rust
    /// Set while continuing a job whose owner is suspected gone but has not
    /// been declared so. See [`Speculating`].
    speculating: Option<Speculating>,
```

Add `speculating: None,` wherever `Work { .. }` is constructed (search for `resuming: None`).

- [ ] **Step 4: Start speculation in the sweep**

In the sweep arm, immediately after the existing election block (`if w.owner.is_none() && w.owner_lost_at.is_some() { take_over_if_ours(...) }`), add:

```rust
                // Suspicion, which is the same election run early. The owner is
                // quiet but not yet gone; this node starts continuing the job
                // anyway, and throws the work away if the owner proves alive.
                if let (Some(w), Some(after)) = (work.as_mut(), config.speculate_after) {
                    let owner = w.owner.filter(|owner| *owner != me);
                    if let Some(owner) = owner {
                        if w.speculating.is_none()
                            && Work::should_speculate(&me, &owner, &members, now, after)
                        {
                            let member = members.get(&owner);
                            let silent = member
                                .map(|m| now.duration_since(m.last_heard))
                                .unwrap_or_default();
                            let label = member.map(|m| m.label.clone()).unwrap_or_default();
                            w.speculating =
                                Some(Speculating { about: owner, since: now, from_tick: w.tick });
                            // The first tick reports itself as a resume, timed
                            // from here. Under speculation that tick happens
                            // before any decision, which is the whole gain.
                            w.resuming = Some(Resuming {
                                cause: "speculated",
                                decided: now,
                                last_checkpoint: w.last_checkpoint_at,
                            });
                            emit(
                                &config,
                                "speculating",
                                &format!(
                                    "{label} has said nothing for {:.0} ms — continuing its job \
                                     from tick {} without waiting to be sure",
                                    silent.as_secs_f64() * 1000.0,
                                    w.tick
                                ),
                                &[
                                    ("peer_id", &owner.to_string()),
                                    ("label", &label),
                                    ("silent_ms", &silent.as_millis().to_string()),
                                    ("tick", &w.tick.to_string()),
                                ],
                            );
                        }
                    }
                }
```

- [ ] **Step 5: Let a speculating node tick**

In the job tick arm, change the ownership guard:

```rust
                if work.owner != Some(me) { continue }
```

to:

```rust
                // A speculating node runs the job without owning it. What it
                // publishes is indistinguishable from an owner's because it is
                // identical — same job, same tick, same derived serials — so
                // nothing downstream needs to know which this is.
                if work.owner != Some(me) && work.speculating.is_none() { continue }
```

- [ ] **Step 6: Run the tests**

Run: `cd node && cargo test`
Expected: PASS, all tests.

- [ ] **Step 7: Check it compiles clean and runs**

Run: `cd node && cargo build --release && ./target/release/mesh-node --help | grep -A3 speculate`
Expected: the new flag appears in the usage text.

- [ ] **Step 8: Commit**

```bash
git add node/src/main.rs
git commit -m "Start the work on suspicion, and time it from there

The sweep already runs every detect/4 and already elects a successor once the
owner is gone. Suspicion is the same election run early: if the owner has been
quiet that long and this node would be the one, it starts ticking while owner
still points at the suspect, because nothing has been decided.

The first speculative tick reports itself as a resume timed from the suspicion,
so the visible gap it measures is suspicion plus resume rather than detection
plus resume. That is the number this feature exists to move, and it is measured
by the same field as before so every run stays comparable.

Ending speculation comes next; at this commit it starts and never stops.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: Confirmation turns speculation into ownership

**Files:**
- Modify: `node/src/main.rs` — `take_over_if_ours` (~line 1914)

**Interfaces:**
- Consumes: `Work.speculating` from Task 2.
- Produces: `took-over` events carrying `speculated` ("true"/"false") and `speculated_ms`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn confirming_a_speculation_does_not_resume_twice() {
        // The first tick already happened and already reported itself. Arming
        // another resume here would report two for one takeover, the second
        // measured from a decision that changed nothing.
        let me = PeerId::random();
        let config = test_config();
        let mut work = test_work();
        work.owner = None;
        work.owner_lost_at = Some(Instant::now());
        work.speculating = Some(Speculating {
            about: PeerId::random(),
            since: Instant::now(),
            from_tick: 10,
        });
        work.resuming = None;

        take_over_if_ours(&config, me, &mut work, &HashMap::new(), "vanished");

        assert_eq!(work.owner, Some(me));
        assert!(work.speculating.is_none(), "speculation has become ownership");
        assert!(work.resuming.is_none(), "the resume was reported at the speculative tick");
    }

    #[test]
    fn a_takeover_without_speculation_still_arms_a_resume() {
        let me = PeerId::random();
        let config = test_config();
        let mut work = test_work();
        work.owner = None;
        work.owner_lost_at = Some(Instant::now());

        take_over_if_ours(&config, me, &mut work, &HashMap::new(), "vanished");

        assert_eq!(work.owner, Some(me));
        assert!(work.resuming.is_some(), "nothing has ticked yet, so the resume is still owed");
    }
```

These need two helpers. Add them to the tests module:

```rust
    fn test_config() -> Config {
        Config {
            label: "test".into(),
            listen: vec![],
            dial: vec![],
            heartbeat: Duration::from_millis(DEFAULT_HEARTBEAT_MS),
            detect: Duration::from_millis(DEFAULT_DETECT_MS),
            speculate_after: Some(Duration::from_millis(DEFAULT_DETECT_MS / 4)),
            json: true,
            job: None,
            job_tick: Duration::from_millis(DEFAULT_JOB_TICK_MS),
            keepalive: Duration::from_millis(DEFAULT_KEEPALIVE_MS),
            mesh: "test".into(),
            port: DEFAULT_PORT,
            identity: String::new(),
            ledger: String::new(),
            map_port: false,
            mdns: false,
            own: false,
        }
    }

    fn test_work() -> Work {
        Work {
            job: job::Job::load(FACTORY_JOB, job::DEFAULT_FUEL).expect("the factory job builds"),
            ledger: ledger::Ledger::default(),
            owner: None,
            state: Vec::new(),
            tick: 10,
            owner_lost_at: None,
            last_checkpoint_at: None,
            resuming: None,
            speculating: None,
            claim_after: None,
        }
    }
```

`FACTORY_JOB` is the path the existing job tests already use — reuse the same constant or `job_or_skip!` macro the tests in `node/tests/jobs.rs` use. If loading a real job in a unit test proves awkward, give `test_work` a `job` built by the same helper the existing `mod tests` uses for serials; do not invent a second mechanism.

- [ ] **Step 2: Run to verify it fails**

Run: `cd node && cargo test speculation_does_not_resume_twice`
Expected: FAIL — `work.resuming` is `Some`, because the current code arms a resume unconditionally.

- [ ] **Step 3: Make confirmation absorb the speculation**

Rewrite the body of `take_over_if_ours` after the `should_take_over` check:

```rust
    let now = Instant::now();
    let gap = work
        .owner_lost_at
        .map(|at| now.duration_since(at))
        .unwrap_or_default();

    // Already running it on suspicion: this decision changes who owns the job,
    // not what is being computed. No tick is re-run and no second resume is
    // reported — the work is done and published, and the resume that matters
    // was measured from the suspicion.
    let speculated = work.speculating.take();
    let speculated_ms = speculated
        .as_ref()
        .map(|s| now.duration_since(s.since).as_millis().to_string());

    work.owner = Some(me);
    work.owner_lost_at = None;
    if speculated.is_none() {
        work.resuming =
            Some(Resuming { cause, decided: now, last_checkpoint: work.last_checkpoint_at });
    }

    emit(
        config,
        "took-over",
        &format!(
            "continuing the job from tick {} — {:.0} ms after noticing{}",
            work.tick,
            gap.as_secs_f64() * 1000.0,
            match &speculated_ms {
                Some(ms) => format!(", having already been running it for {ms} ms"),
                None => String::new(),
            }
        ),
        &[
            ("tick", &work.tick.to_string()),
            ("cause", cause),
            ("decision_ms", &gap.as_millis().to_string()),
            ("state_bytes", &work.state.len().to_string()),
            ("survivors", &(members.len() + 1).to_string()),
            ("speculated", if speculated_ms.is_some() { "true" } else { "false" }),
            ("speculated_ms", speculated_ms.as_deref().unwrap_or("0")),
        ],
    );
```

- [ ] **Step 4: Run the tests**

Run: `cd node && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add node/src/main.rs
git commit -m "A confirmed suspicion is a change of owner, not a restart of work

take_over_if_ours armed a resume unconditionally. Under speculation the first
tick has already happened and already reported itself, so arming another would
report two resumes for one takeover — the second measured from a decision that
changed nothing about what was being computed.

took-over keeps its name, its timing and every field it had, so pairing chaos
actions to takeovers is untouched. It gains speculated and speculated_ms, which
is how a run can later say how much of the takeover had already happened before
anyone decided anything.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: Abandoning a wrong suspicion

**Files:**
- Modify: `node/src/main.rs` — the `Message::Checkpoint` arm (~line 1704)

**Interfaces:**
- Consumes: `Work.speculating`, `Work::theirs`.
- Produces: the `speculation-abandoned` event with `peer_id`, `label`, `ticks_wasted`, `wasted_ms`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_returning_owner_decides_by_the_same_rule_as_any_two_owners() {
        // A speculator resumed from an older checkpoint, so an owner that never
        // stopped is normally ahead and keeps the job. Behind, and it yields —
        // the rule does not care which of the two was speculating.
        let (me, owner) = (PeerId::random(), PeerId::random());
        assert!(Work::theirs((12, &me), (15, &owner)), "they are further along");
        assert!(!Work::theirs((15, &me), (12, &owner)), "we are");
    }

    #[test]
    fn wasted_ticks_are_counted_from_where_speculation_began() {
        let spec = Speculating { about: PeerId::random(), since: Instant::now(), from_tick: 40 };
        assert_eq!(52u64.saturating_sub(spec.from_tick), 12);
        // A speculation that never got a tick in wasted none.
        assert_eq!(40u64.saturating_sub(spec.from_tick), 0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd node && cargo test wasted_ticks_are_counted`
Expected: FAIL — `Speculating` has no `from_tick` in scope of the tests module until Task 2 is merged; if Tasks 2–3 are already in, this compiles and passes, and the real verification is Step 4's test run plus the rig in Task 6.

- [ ] **Step 3: Resolve speculation when a checkpoint arrives**

At the top of the `Message::Checkpoint` arm, after `w.claim_after = None;` and before the `if w.owner == Some(me)` branch:

```rust
                            // A checkpoint means somebody else is running this
                            // job, which is the answer to the suspicion that
                            // started this node running it. Resolved by the
                            // same rule two owners always use, so a speculator
                            // is not a special case: whoever is further along
                            // keeps it, and the other stops.
                            if let Some(spec) = w.speculating.as_ref() {
                                if Work::theirs((w.tick, &me), (tick, &source)) {
                                    let wasted = w.tick.saturating_sub(spec.from_tick);
                                    let wasted_ms =
                                        Instant::now().duration_since(spec.since).as_millis();
                                    emit(&config, "speculation-abandoned",
                                         &format!("{label} is running it after all — {wasted} ticks \
                                                   computed on suspicion are discarded"),
                                         &[("peer_id", &source.to_string()),
                                           ("label", &label),
                                           ("ticks_wasted", &wasted.to_string()),
                                           ("wasted_ms", &wasted_ms.to_string())]);
                                    w.speculating = None;
                                    // Nothing ticked after this point, so a
                                    // resume still owed is a resume that will
                                    // never happen.
                                    w.resuming = None;
                                } else {
                                    // Further along than the node that came
                                    // back, so this one keeps the job and that
                                    // one yields when it hears this node's next
                                    // checkpoint. Speculation is over either
                                    // way; what differs is who owns the work.
                                    w.speculating = None;
                                    w.owner = Some(me);
                                    continue;
                                }
                            }
```

The `continue` matters: the code below this point adopts the sender's tick and state, which is exactly what must not happen when this node is the one further along.

- [ ] **Step 4: Run the tests**

Run: `cd node && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add node/src/main.rs
git commit -m "A suspicion that was wrong ends the way two owners always end

A checkpoint arriving is the answer to the suspicion that started this node
running the job, so it is resolved by the rule two owners already share:
whoever is further along keeps it. A speculator is not a special case. It is
normally the one behind — it resumed from an older checkpoint while the owner
never stopped — so it normally stops, and says how many ticks it computed for
nothing.

Nothing is undone when it does. Items are named by what made them and the job
is deterministic, so the discarded ticks produced the same items the owner
produced; the ledger merges them as a union of equal sets. The cost of being
wrong is arithmetic, which is why this is worth doing at all.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: Teach the summary to read a resume that comes first

Without this the tool reports `resume_ms` and `gap_ms` as absent for every speculated takeover — silently, for the exact number the feature exists to move.

**Files:**
- Modify: `provision/takeover-summary.py` — `match()` (~line 156), `report()` (~line 234)
- Test: `provision/test_takeover_summary.py`

**Interfaces:**
- Consumes: the `speculating`, `resumed` (`cause: "speculated"`), `took-over` (`speculated`) and `speculation-abandoned` events from Tasks 2–4.
- Produces: a `SPECULATION` block in the report; `result["speculated"]` on each row.

- [ ] **Step 1: Write the failing tests**

Add to `provision/test_takeover_summary.py`:

```python
def kill_with_speculation(at, silent=2200, suspected=500, resume=100, gap=600,
                          victim="diamond", successor="riverboat"):
    """A takeover where the work restarted before anyone decided anything."""
    actions = [act(victim, at, "kill")]
    events = [
        ev(successor, at + suspected, "speculating", label=victim,
           silent_ms=str(suspected), tick="10"),
        ev(successor, at + suspected + resume, "resumed", cause="speculated",
           resume_ms=str(resume), gap_ms=str(gap)),
        ev(successor, at + silent, "vanished", label=victim, silent_ms=str(silent)),
        ev(successor, at + silent, "took-over", tick="13", speculated="true",
           speculated_ms=str(silent - suspected)),
    ]
    return actions, events


class Speculation(unittest.TestCase):
    def test_a_resume_before_its_takeover_is_still_that_takeovers_resume(self):
        actions, events = kill_with_speculation(1000)
        [row] = summary.match(actions, events)
        self.assertEqual(row["resume_ms"], 100)
        self.assertEqual(row["gap_ms"], 600,
                         "the gap is what the player waited: suspicion plus resume")
        self.assertEqual(row["detect_ms"], 2200,
                         "detection still means the silence before the peer was declared gone")
        self.assertTrue(row["speculated"])

    def test_an_unspeculated_takeover_reads_exactly_as_before(self):
        actions, events = kill_and_takeover(1000)
        [row] = summary.match(actions, events)
        self.assertEqual(row["resume_ms"], 150)
        self.assertEqual(row["gap_ms"], 2400)
        self.assertFalse(row["speculated"])

    def test_the_report_counts_suspicions_that_were_wrong(self):
        actions, events = kill_with_speculation(1000)
        events += [
            ev("riverboat", 5000, "speculating", label="diamond", silent_ms="500", tick="60"),
            ev("riverboat", 5600, "speculation-abandoned", label="diamond",
               ticks_wasted="3", wasted_ms="600"),
        ]
        out = io.StringIO()
        summary.report(summary.match(actions, events), events, actions, out=out)
        text = out.getvalue()
        self.assertIn("SPECULATION", text)
        self.assertIn("started", text)
        self.assertIn("abandoned", text)
        self.assertIn("3", text, "the wasted ticks are reported, not just the count")
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd /home/ubuntu/workspace/machine_elves_mesh && python3 -m unittest provision.test_takeover_summary -v 2>&1 | tail -20`
Expected: FAIL — `KeyError: 'speculated'`, and `row["resume_ms"]` is `None` for the speculated case.

- [ ] **Step 3: Match a resume that precedes its takeover**

In `match()`, replace the `resumed` lookup and add the field:

```python
        first = takeovers[0] if takeovers else None
        resumed = None
        if first:
            # A speculated resume happens BEFORE the takeover it belongs to:
            # the standby started the work on suspicion and only later was told
            # it had been right. Requiring the usual order here would report
            # this feature's whole effect as missing data.
            candidates = [e for e in window
                          if e.get("event") == "resumed" and e.get("node") == first["node"]]
            resumed = next((e for e in candidates
                            if e["ts_unix_ms"] >= first["ts_unix_ms"]), None)
            if resumed is None:
                resumed = next((e for e in candidates
                                if e.get("cause") == "speculated"), None)
```

And in the dict appended to `results`, after `"gap_ms"`:

```python
            "speculated": bool(first and first.get("speculated") == "true"),
```

- [ ] **Step 4: Add the speculation block to the report**

In `report()`, after the `THINGS THAT SHOULD NOT HAPPEN` block and before the final disruption count:

```python
    started = [e for e in events if e.get("event") == "speculating"]
    abandoned = [e for e in events if e.get("event") == "speculation-abandoned"]
    confirmed = [r for r in results if r["speculated"]]
    if started:
        say("\nSPECULATION")
        say(f"  started                   {len(started):>4}  suspicions acted on before any decision")
        say(f"  confirmed                 {len(confirmed):>4}  the owner really had gone")
        say(f"  abandoned                 {len(abandoned):>4}  the owner came back and kept the job")
        wasted = [num(e.get("ticks_wasted")) for e in abandoned
                  if num(e.get("ticks_wasted")) is not None]
        if wasted:
            say(row("ticks wasted", wasted))
        say("  A wrong suspicion costs ticks, not correctness: the same job from the same")
        say("  state makes the same items, so discarded work merges to nothing.")
```

- [ ] **Step 5: Run the tests**

Run: `cd /home/ubuntu/workspace/machine_elves_mesh && python3 -m unittest provision.test_takeover_summary provision.test_chaos -v 2>&1 | tail -5`
Expected: PASS, including the pre-existing tests.

- [ ] **Step 6: Re-read Run B with the new tool, to prove nothing regressed**

Run: `cd provision && python3 takeover-summary.py results/night-b/*/events.jsonl results/night-b/*/chaos.jsonl | head -45`
Expected: identical numbers to the committed Run B section — detection p95 2456 ms, resume p95 200 ms, gap p95 2566 ms, 21 disruptions, four zeros. No `SPECULATION` block, since those logs contain no such events.

- [ ] **Step 7: Commit**

```bash
git add provision/takeover-summary.py provision/test_takeover_summary.py
git commit -m "The resume can now arrive before the takeover it belongs to

match() accepted a resumed event only at or after its took-over. Speculation
inverts that order — the standby starts the work on suspicion and is told it was
right afterwards — so every speculated takeover would have reported resume and
gap as absent. Silently, and for the exact number the feature exists to move.

That is the third time this project has met a tool answering about the wrong
thing in the same shape as a right answer: twice blinded by a log rotation, once
by reading the wrong armed window. The fix is the same shape too — state the
rule, and test the case that breaks it.

Detection keeps its meaning, the silence before a peer was declared gone, so
Night A, its second run and Run B stay comparable with whatever comes next. Run
B's logs re-read identically under this change.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: Prove it on the rigs

The state machine is now complete. This task runs it against the two rigs that have historically caught this class of bug, before any real box sees it.

**Files:**
- Modify: `node/chaos-rig.sh` — only if the rig needs the new flag passed explicitly; the default is on, so most likely no change.
- Modify: `docs/phase-0-plan.md` — build order note.

**Interfaces:**
- Consumes: everything from Tasks 1–5.
- Produces: evidence, and a documented state of the feature.

- [ ] **Step 1: Build**

Run: `cd node && cargo build --release && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 2: The migration rig, which exercises the returning-node race**

Run: `cd node && ./migration-rig.sh`
Expected: PASS. Then run it with speculation off to confirm the flag really disables it:

Run: `cd node && MESH_NODE_EXTRA="--speculate-after-ms 0" ./migration-rig.sh`

If `migration-rig.sh` has no such hook, read it first and add one in the same style as its existing arguments; a flag that cannot be turned off in the rig cannot be A/B tested on a box either.

- [ ] **Step 3: The chaos rig, which is where the takeover state machine gets stressed**

Run: `cd node && sudo ./chaos-rig.sh 15`
Expected: the summary at the end reports zero orphaned jobs, zero double takeovers, and a `SPECULATION` block whose `confirmed` count is close to the number of kills.

- [ ] **Step 4: Read what the rig produced, and judge the trigger**

The question the rig can answer on loopback: does suspicion fire when nothing is wrong? Count `speculating` events with no matching disruption.

Run: `cd node && grep -c speculating "$(ls -dt /tmp/tmp.* | head -1)"/*.jsonl`

Expected: suspicions roughly equal to disruptions. **If suspicion fires repeatedly on an idle loopback mesh, stop.** That means the trigger is wrong in kind rather than in value, and the spec says so: this comes back to design rather than being tuned.

- [ ] **Step 5: Note the state in the plan document**

In `docs/phase-0-plan.md`, in the build-order section, record that speculative resume is built and awaiting Run C, with the local evidence from Steps 2–4. Do not write the outcome of Run C; it has not happened.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Speculative resume survives both rigs

The migration rig covers the race this touches most directly — a node killed and
restarted inside the transport's 10 s idle timeout, now with a speculator
mid-flight — and the chaos rig stresses the takeover state machine over a few
dozen disruptions. Both with speculation on, and the migration rig again with it
off, because a flag that cannot be turned off in a rig cannot be A/B tested on a
box either.

What loopback cannot answer is the one that matters: how often suspicion fires
when nothing is wrong. Nothing is ever slow or lost here. Run C measures it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: Run C

Not code. This is the handover, because every privileged step belongs to the person with the boxes.

**Files:**
- Create: `provision/results/run-c/` (gitignored, as every run's logs are)
- Modify: `docs/phase-0-plan.md` — the results section, after the run

**Interfaces:**
- Consumes: a built release from Task 6.
- Produces: the measurement the spec set as success.

- [ ] **Step 1: Build and stage**

```bash
cd /home/ubuntu/workspace/machine_elves_mesh && ./release.sh
scp dist/mesh-node rpc@192.168.50.48:~/runc/
scp dist/mesh-node rpc@192.168.50.128:~/runc/
```

- [ ] **Step 2: Hand over the run commands**

diamond and riverboat are both wired, so `--not-a-hotspot` is honest:

```
ssh -t rpc@192.168.50.48  'cd ~/runc && sudo ./timed-run.sh --label diamond   --mesh run-c --peer 192.168.50.128 --own --hours 8 --not-a-hotspot'
ssh -t rpc@192.168.50.128 'cd ~/runc && sudo ./timed-run.sh --label riverboat --mesh run-c --peer 192.168.50.48        --hours 8 --not-a-hotspot'
```

`timed-run.sh` needs `chaos.py`, `mesh-chaos.service` and `mesh-chaos.timer` beside it; copy them into `~/runc` alongside the binary.

- [ ] **Step 3: Collect and read**

```bash
mkdir -p provision/results/run-c/{diamond,riverboat}
scp 'rpc@192.168.50.48:/var/log/mesh-node/*.jsonl*'  provision/results/run-c/diamond/
scp 'rpc@192.168.50.128:/var/log/mesh-node/*.jsonl*' provision/results/run-c/riverboat/
cd provision && python3 takeover-summary.py results/run-c/*/events.jsonl results/run-c/*/chaos.jsonl
```

The `*` after `.jsonl` is not decoration — an eight-hour run crosses the 00:02 UTC rotation.

- [ ] **Step 4: Judge against the spec's table**

| | Baseline (Night A run 2, n=104) | Run C |
|---|---|---|
| Detection p95 | 2461 ms | expect unchanged |
| Resume p95 | 200 ms | expect unchanged |
| **Visible gap p95** | **2651 ms** | **target: under 500 ms** |
| Orphaned / doubled / false alarm / unnoticed | 0 | must stay 0 |
| False suspicion rate | n/a | record it; it will not be zero |

- [ ] **Step 5: Write it up and commit**

Add a section to `docs/phase-0-plan.md` in the style of the Night A and Run B sections: what was measured, against what, what moved, what did not, and what remains an upper bound. **Retract the superseded claim** — the plan currently says the player waits detection plus resume — and say plainly whether speculation changed that, including if it did not.

---

## Self-Review

**Spec coverage.** Trigger at `detect/4` — Task 1. Publish immediately with nothing new on the wire — Task 2, Step 5. Successor rule minus the suspect — Task 1. Three exits — Task 3 (confirmed), Task 4 (returns ahead, returns behind). The flag — Task 1. Events table — Tasks 2, 3, 4. Summary changes — Task 5. Unit tests, rig tests, summary test, Run C — Tasks 1–7. Not-in-scope items appear nowhere, as intended.

**Interfaces.** `Speculating { about, since, from_tick }` is defined in Task 2 and used by Tasks 3 and 4 with those field names. `Config.speculate_after: Option<Duration>` is defined in Task 1 and read in Task 2. `Work::lowest_among` and `Work::should_speculate` are defined once, in Task 1. The `speculated` / `speculated_ms` fields are emitted in Task 3 and read in Task 5 as the string `"true"`.

**One known rough edge, left deliberately.** Task 3's tests need a `Work` value, which needs a loaded job, and the existing unit tests have not had to build one. The step says to reuse whatever the current tests use rather than invent a second mechanism; if that proves impossible, the honest fallback is to test `take_over_if_ours`'s resume-arming decision by extracting it as a small pure function, and the implementer should do that rather than skip the test.
