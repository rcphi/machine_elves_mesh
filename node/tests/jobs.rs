//! Runs real compiled jobs, rather than testing the runner against itself.
//!
//! Build the jobs first: `../jobs/build.sh`

use std::path::{Path, PathBuf};

// The runner is a module of the binary crate, so the test compiles it directly
// rather than depending on a library that does not exist yet.
#[path = "../src/job.rs"]
#[allow(dead_code)] // the job's id is used by the node, not by these tests
mod job;

use job::{Job, DEFAULT_FUEL};

/// A compiled job, which must exist.
///
/// These tests used to skip themselves, and pass, when a job had not been
/// built, so a checkout without the jobs reported every one of them as
/// passing coverage that never ran (review T1). A missing job is now a
/// failure that says how to fix it — not a bug in the runner, but not a pass.
fn wasm(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../jobs")
        .join(name)
        .join("target/wasm32-unknown-unknown/release")
        .join(format!("{}_job.wasm", name));
    assert!(
        path.exists(),
        "{} is not built, so the tests that run it cannot. Run ../jobs/build.sh first.",
        path.display()
    );
    path
}

macro_rules! job {
    ($name:expr, $fuel:expr) => {
        Job::load(wasm($name), $fuel).expect("loads")
    };
}

fn inputs(world_time: u64, steel: u32, workers: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&world_time.to_le_bytes());
    out.extend_from_slice(&steel.to_le_bytes());
    out.extend_from_slice(&workers.to_le_bytes());
    out
}

#[test]
fn a_factory_turns_steel_into_widgets_over_time() {
    let factory = job!("factory", DEFAULT_FUEL);

    let mut state = Vec::new();
    let mut produced = 0;
    for tick in 0..40u64 {
        let outcome = factory
            .tick(&state, &inputs(tick, 4, 4))
            .expect("tick succeeds");
        if String::from_utf8_lossy(&outcome.effects).contains("produce widget") {
            produced += 1;
        }
        state = outcome.state;
    }
    assert!(produced > 0, "the factory never finished anything");
    assert!(!state.is_empty(), "the factory kept no state");
}

#[test]
fn the_same_tick_from_the_same_state_gives_identical_bytes() {
    // The property the whole design rests on. Checkpoint-and-resume,
    // speculative execution on a second machine, and verification by re-running
    // are all only sound if this holds byte for byte.
    let factory = job!("factory", DEFAULT_FUEL);

    let mut state = Vec::new();
    for tick in 0..10u64 {
        state = factory.tick(&state, &inputs(tick, 3, 2)).expect("tick").state;
    }

    let first = factory.tick(&state, &inputs(99, 3, 2)).expect("tick");
    let second = factory.tick(&state, &inputs(99, 3, 2)).expect("tick");
    assert_eq!(first.state, second.state);
    assert_eq!(first.effects, second.effects);
    assert_eq!(first.fuel_used, second.fuel_used);
}

#[test]
fn a_job_cannot_smuggle_anything_between_ticks() {
    // A fresh instance every tick means globals and heap do not carry over, so
    // the rule that only state survives is a fact about how jobs are run rather
    // than something a job could choose to break.
    let factory = job!("factory", DEFAULT_FUEL);

    let empty = Vec::new();
    let a = factory.tick(&empty, &inputs(0, 5, 2)).expect("tick");
    // Ten unrelated ticks in between; if anything persisted inside the module,
    // the next result from the same state would differ.
    for tick in 0..10u64 {
        let _ = factory.tick(&a.state, &inputs(tick, 9, 4)).expect("tick");
    }
    let b = factory.tick(&empty, &inputs(0, 5, 2)).expect("tick");
    assert_eq!(a.state, b.state);
    assert_eq!(a.effects, b.effects);
}

#[test]
fn resuming_from_a_checkpoint_matches_never_having_stopped() {
    // Migration in miniature: carry the state to a freshly loaded runner and
    // the world continues exactly as if nothing happened.
    let path = wasm("factory");

    let uninterrupted = Job::load(&path, DEFAULT_FUEL).expect("loads");
    let mut straight = Vec::new();
    for tick in 0..20u64 {
        straight = uninterrupted
            .tick(&straight, &inputs(tick, 2, 3))
            .expect("tick")
            .state;
    }

    let first_half = Job::load(&path, DEFAULT_FUEL).expect("loads");
    let mut checkpointed = Vec::new();
    for tick in 0..10u64 {
        checkpointed = first_half
            .tick(&checkpointed, &inputs(tick, 2, 3))
            .expect("tick")
            .state;
    }
    drop(first_half);

    let second_half = Job::load(&path, DEFAULT_FUEL).expect("loads");
    for tick in 10..20u64 {
        checkpointed = second_half
            .tick(&checkpointed, &inputs(tick, 2, 3))
            .expect("tick")
            .state;
    }

    assert_eq!(straight, checkpointed);
}

#[test]
fn a_runaway_job_is_stopped_rather_than_taking_the_machine() {
    let runaway = job!("runaway", 5_000_000);

    let error = runaway
        .tick(b"", b"")
        .expect_err("a job that never returns must not be allowed to");
    let message = format!("{error:#}");
    assert!(
        message.contains("exhausted"),
        "stopped for the wrong reason: {message}"
    );
}

#[test]
fn the_ceiling_is_on_work_rather_than_on_time() {
    // Fuel counts instructions, so a busy host and an idle one stop the same
    // job at the same point. A wall-clock limit would make two machines
    // disagree about whether a tick completed.
    let path = wasm("runaway");
    for fuel in [1_000_000u64, 3_000_000, 9_000_000] {
        let job = Job::load(&path, fuel).expect("loads");
        assert!(job.tick(b"", b"").is_err(), "{fuel} fuel was not a ceiling");
    }
}

#[test]
fn a_job_that_wants_imports_is_refused_at_load() {
    // Every route out of the sandbox — clock, randomness, network, filesystem —
    // would arrive as an import, so refusing all of them is the whole boundary.
    let dir = std::env::temp_dir().join("machine-elves-import-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("importer.wasm");

    // (module (import "host" "now" (func)) (memory (export "memory") 1))
    let wat = br#"(module
        (import "host" "now" (func $now))
        (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "tick") (param i32 i32) (result i64) i64.const 0))"#;
    std::fs::write(&path, wat).expect("write");

    // `Job` holds a wasmtime engine and is not Debug, so the failure is
    // matched rather than unwrapped.
    let message = match Job::load(&path, DEFAULT_FUEL) {
        Ok(_) => panic!("a job importing host::now was accepted"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        message.contains("host::now") || message.contains("import"),
        "refused for the wrong reason: {message}"
    );
}

#[test]
fn a_greedy_job_is_stopped_before_it_exhausts_the_host() {
    // Fuel bounds instructions and nothing else. Without a separate memory
    // ceiling a job can stay well inside its CPU budget and still take the
    // machine down, which for a volunteer running this on their own hardware is
    // the same outcome as a successful attack.
    let path = wasm("glutton");

    // Generous fuel on purpose: this must fail for running out of memory, not
    // for running out of instructions, or the test proves the wrong thing.
    let job = job::Job::load_with(&path, 5_000_000_000, 16 * 1024 * 1024).expect("loads");
    let error = job.tick(b"", b"").expect_err("must be stopped");
    let message = format!("{error:#}");
    assert!(
        !message.contains("exhausted its"),
        "stopped by the fuel ceiling rather than the memory ceiling: {message}"
    );
}

#[test]
fn the_memory_ceiling_holds_at_several_sizes() {
    let path = wasm("glutton");
    for limit in [8 * 1024 * 1024usize, 32 * 1024 * 1024, 64 * 1024 * 1024] {
        let job = job::Job::load_with(&path, 5_000_000_000, limit).expect("loads");
        assert!(job.tick(b"", b"").is_err(), "{limit} bytes was not a ceiling");
    }
}

#[test]
fn an_ordinary_job_is_untroubled_by_the_ceiling() {
    // A limit that also stops honest work is not a limit, it is a bug.
    let path = wasm("factory");
    let job = job::Job::load_with(&path, DEFAULT_FUEL, 2 * 1024 * 1024).expect("loads");
    let mut state = Vec::new();
    for tick in 0..30u64 {
        state = job.tick(&state, &inputs(tick, 4, 3)).expect("honest work").state;
    }
    assert!(!state.is_empty());
}
