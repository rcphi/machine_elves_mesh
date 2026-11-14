//! Running someone else's code on your machine, safely and repeatably.
//!
//! A job is not a program that starts, runs, and finishes. It is woken up,
//! handed everything it knows, does a small amount of work, hands everything
//! back, and sleeps again. The thing passed back and forth is an opaque blob of
//! state; each wake-up is a tick.
//!
//! ```text
//! tick(state, inputs) -> (state, effects)
//! ```
//!
//! That shape is what makes the rest of the design cheap. Checkpointing is
//! keeping the state between ticks. Migration is sending it elsewhere.
//! Speculative resume is another node simply running the next ticks. And
//! verification is re-running a tick and comparing — all of which need the same
//! property: identical inputs must give identical outputs.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimitsBuilder};

/// wasmtime carries its own `anyhow`, which is a different type from ours even
/// when the versions match. Flattening its errors to text here keeps that
/// detail from spreading through every call site.
fn wasm_err(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{error:#}")
}

/// CPU ceiling for one tick, in wasmtime's fuel units.
///
/// Fuel is consumed per instruction, so this is a bound on work rather than on
/// time — which is the point. A time limit would make execution depend on how
/// busy the host was, and two nodes running the same tick would disagree about
/// whether it finished. Fuel is identical everywhere.
pub const DEFAULT_FUEL: u64 = 50_000_000;

/// Memory ceiling for one tick.
///
/// Fuel bounds instructions and says nothing whatsoever about memory, so
/// without this a job can allocate until the host runs out — never escaping the
/// sandbox, and taking the machine down anyway. Two ceilings are needed because
/// there are two ways to consume a host.
pub const DEFAULT_MEMORY: usize = 64 * 1024 * 1024;

/// Largest state or effects blob a job may hand back.
///
/// Present because the host has to allocate whatever the guest claims, and an
/// unchecked length is how a sandboxed job exhausts the memory of the machine
/// hosting it without ever escaping the sandbox.
const MAX_BLOB: usize = 16 * 1024 * 1024;

/// Table entries a job may hold, across its one table.
///
/// Tables live outside linear memory, so `memory` does not bound them: a
/// module declaring a million-entry table instantiated happily under a 64 KiB
/// memory ceiling (review finding 2). A Rust job's table holds its indirect
/// call targets, a few hundred at most.
pub const MAX_TABLE_ELEMENTS: usize = 10_000;

/// The engine every job runs on, and the WebAssembly features it admits.
///
/// Importing nothing keeps clocks and randomness out, but WebAssembly has
/// sources of variation of its own, and determinism across machines is what
/// lets two nodes continuing one job produce the same things (review finding
/// 11). So the admitted features are stated, not inherited from the engine's
/// defaults:
///
/// - **NaNs are canonicalized.** Which bits a NaN carries is left to the
///   hardware; x86 produces a negative NaN where ARM produces a positive one,
///   so a job that stores a float's bits could diverge across machines.
/// - **Relaxed SIMD is off.** Its results are explicitly allowed to differ
///   between platforms.
/// - **Threads are off.** A job cannot start one (it imports nothing), and
///   shared memory buys nothing but a way to be nondeterministic.
/// - **GC is off.** Its heap lies outside linear memory and outside the
///   memory limit, and `array.new` allocates any length in one instruction,
///   for one unit of fuel.
///
/// What remains is the integer, float and SIMD core that compiled Rust uses,
/// with bulk memory, multi-value and reference types. Memory and table growth
/// are bounded by the store's limits; a growth refused there fails the same
/// way on every machine, so determinism holds up to those limits, not past a
/// host that runs out of memory first.
fn engine() -> Result<Engine> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_nan_canonicalization(true);
    config.wasm_relaxed_simd(false);
    config.wasm_threads(false);
    config.wasm_shared_everything_threads(false);
    config.wasm_gc(false);
    config.wasm_stack_switching(false);
    Engine::new(&config).map_err(wasm_err)
}

pub struct Job {
    engine: Engine,
    module: Module,
    fuel: u64,
    memory: usize,
    id: [u8; 32],
}

#[derive(Debug, PartialEq)]
pub struct Outcome {
    /// The job's new state. Opaque: the host moves these bytes and never
    /// interprets them, so a job may keep whatever it likes in whatever format
    /// it likes without the host needing to know.
    pub state: Vec<u8>,
    /// What the job wants the world to do, which the host may apply or refuse.
    pub effects: Vec<u8>,
    /// Fuel actually consumed, so an over-budget job can be seen coming before
    /// it starts failing.
    pub fuel_used: u64,
}

impl Job {
    pub fn load(path: impl AsRef<Path>, fuel: u64) -> Result<Self> {
        Self::load_with(path, fuel, DEFAULT_MEMORY)
    }

    pub fn load_with(path: impl AsRef<Path>, fuel: u64, memory: usize) -> Result<Self> {
        let engine = engine()?;
        // A job is identified by its code and nothing else. Two nodes holding
        // the same bytes agree on which job it is without being told, and
        // without anyone maintaining a registry of names — which matters
        // because the identity is what produced things are derived from.
        let bytes = std::fs::read(path.as_ref())
            .with_context(|| format!("reading {}", path.as_ref().display()))?;
        let id: [u8; 32] = Sha256::digest(&bytes).into();

        let module = Module::from_file(&engine, path.as_ref())
            .map_err(wasm_err)
            .with_context(|| format!("loading {}", path.as_ref().display()))?;

        // A job that imports nothing cannot call anything: no clock, no
        // randomness, no network, no filesystem, no host functions of any kind.
        // §11.4's "receive input, compute, return output" is then literally
        // true rather than a policy someone has to enforce. It is also what
        // makes determinism achievable, since every source of variation a job
        // could reach would have arrived through an import.
        let imports: Vec<String> = module
            .imports()
            .map(|i| format!("{}::{}", i.module(), i.name()))
            .collect();
        if !imports.is_empty() {
            bail!(
                "job wants {} import(s) and jobs get none: {}",
                imports.len(),
                imports.join(", ")
            );
        }

        Ok(Self {
            engine,
            module,
            fuel,
            memory,
            id,
        })
    }

    /// The hash of this job's code.
    pub fn id(&self) -> &[u8; 32] {
        &self.id
    }

    /// Runs one tick.
    ///
    /// A fresh instance is built every time, and that is deliberate rather than
    /// wasteful. It means anything a job leaves in a global, or on its heap, is
    /// gone before the next tick — so "nothing survives between ticks except
    /// the state" is not a rule a job could break, it is a fact about how the
    /// job is run. Making the violation impossible is cheaper than detecting it.
    pub fn tick(&self, state: &[u8], inputs: &[u8]) -> Result<Outcome> {
        // `Store<StoreLimits>` rather than `Store<()>`: wasmtime asks the store's
        // own data whether a growth request is allowed, so the limits have to
        // live there.
        // `memory_size` bounds linear memory and nothing else. Tables are
        // separate host allocations with their own limit; GC heaps are
        // excluded by the engine (see `engine`).
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.memory)
            .table_elements(MAX_TABLE_ELEMENTS)
            // One instance, one memory, one table: a job has no legitimate
            // reason to create more, and each one is more host memory.
            .instances(1)
            .memories(1)
            .tables(1)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(self.fuel).map_err(wasm_err)?;

        let instance = Instance::new(&mut store, &self.module, &[])
            .map_err(wasm_err)
            .context("instantiating the job")?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .context("job exports no memory")?;
        let alloc = instance
            .get_typed_func::<u32, u32>(&mut store, "alloc")
            .map_err(wasm_err)
            .context("job exports no alloc(len) -> ptr")?;
        let tick = instance
            .get_typed_func::<(u32, u32), u64>(&mut store, "tick")
            .map_err(wasm_err)
            .context("job exports no tick(ptr, len) -> packed")?;

        let frame = encode_frame(state, inputs);
        let len = u32::try_from(frame.len()).context("input frame too large")?;

        let ptr = alloc
            .call(&mut store, len)
            .map_err(wasm_err)
            .context("job's alloc failed")?;
        memory
            .write(&mut store, ptr as usize, &frame)
            .map_err(wasm_err)
            .context("job's alloc returned a buffer that does not fit its claim")?;

        let packed = match tick.call(&mut store, (ptr, len)) {
            Ok(packed) => packed,
            Err(error) => {
                // Fuel exhaustion arrives as a trap like any other, so the
                // remaining fuel is what distinguishes "this job is a runaway"
                // from "this job has a bug".
                let remaining = store.get_fuel().unwrap_or(0);
                if remaining == 0 {
                    bail!("job exhausted its {} fuel and was stopped", self.fuel);
                }
                return Err(wasm_err(error)).context("job trapped");
            }
        };

        let fuel_used = self.fuel - store.get_fuel().unwrap_or(0);

        // Everything past this point treats the guest as hostile: it chose
        // these numbers, and they address the host's own allocation.
        let out_ptr = (packed >> 32) as usize;
        let out_len = (packed & 0xFFFF_FFFF) as usize;
        if out_len > MAX_BLOB {
            bail!("job returned {out_len} bytes, over the {MAX_BLOB} limit");
        }

        let data = memory.data(&store);
        let end = out_ptr
            .checked_add(out_len)
            .context("job returned a buffer that overflows its own address space")?;
        if end > data.len() {
            bail!("job returned a buffer running past the end of its memory");
        }

        let (state, effects) = decode_frame(&data[out_ptr..end])?;
        Ok(Outcome {
            state,
            effects,
            fuel_used,
        })
    }
}

/// `[u32 len][bytes][u32 len][bytes]`, little-endian.
///
/// Deliberately dull. The host has to parse whatever crosses this boundary
/// while assuming the other side is hostile, and a format with no options is
/// the one with the fewest ways to be wrong.
fn encode_frame(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + first.len() + second.len());
    out.extend_from_slice(&(first.len() as u32).to_le_bytes());
    out.extend_from_slice(first);
    out.extend_from_slice(&(second.len() as u32).to_le_bytes());
    out.extend_from_slice(second);
    out
}

fn decode_frame(buf: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let read_block = |buf: &[u8], at: usize| -> Result<(Vec<u8>, usize)> {
        let header_end = at.checked_add(4).context("truncated length")?;
        if header_end > buf.len() {
            bail!("truncated length header");
        }
        let len = u32::from_le_bytes(buf[at..header_end].try_into()?) as usize;
        if len > MAX_BLOB {
            bail!("block claims {len} bytes, over the {MAX_BLOB} limit");
        }
        let end = header_end.checked_add(len).context("length overflows")?;
        if end > buf.len() {
            bail!("block claims {len} bytes but only {} remain", buf.len() - header_end);
        }
        Ok((buf[header_end..end].to_vec(), end))
    };

    let (first, next) = read_block(buf, 0)?;
    let (second, _) = read_block(buf, next)?;
    Ok((first, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let frame = encode_frame(b"state", b"inputs");
        let (state, inputs) = decode_frame(&frame).expect("decodes");
        assert_eq!(state, b"state");
        assert_eq!(inputs, b"inputs");
    }

    #[test]
    fn empty_blocks_are_valid() {
        // A job's first tick has no prior state, and a quiet tick produces no
        // effects. Neither is an error.
        let frame = encode_frame(b"", b"");
        assert_eq!(decode_frame(&frame).expect("decodes"), (vec![], vec![]));
    }

    #[test]
    fn rejects_a_length_that_runs_past_the_buffer() {
        // The guest picks these numbers, so believing them is how a sandboxed
        // job reads memory that was never handed to it.
        let mut frame = encode_frame(b"abc", b"");
        frame[0] = 0xFF;
        assert!(decode_frame(&frame).is_err());
    }

    #[test]
    fn rejects_a_truncated_header() {
        assert!(decode_frame(&[1, 2]).is_err());
    }

    fn wat_job(name: &str, wat: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("me-job-{name}-{}.wat", std::process::id()));
        std::fs::write(&path, wat).unwrap();
        path
    }

    /// The review's reproduction of finding 2: a million-entry table ticked
    /// under a 64 KiB memory ceiling and 1,000 fuel.
    #[test]
    fn a_table_past_the_limit_is_refused() {
        let path = wat_job("table", r#"(module
            (memory (export "memory") 1)
            (table 1000000 funcref)
            (func (export "alloc") (param i32) (result i32) i32.const 16)
            (func (export "tick") (param i32 i32) (result i64) i64.const 8))"#);
        let job = Job::load_with(&path, 1000, 65536).expect("loads");
        assert!(job.tick(b"", b"").is_err(), "a million-entry table was allowed");
    }

    #[test]
    fn a_table_within_the_limit_still_runs() {
        let path = wat_job("small-table", r#"(module
            (memory (export "memory") 1)
            (table 100 funcref)
            (func (export "alloc") (param i32) (result i32) i32.const 16)
            (func (export "tick") (param i32 i32) (result i64)
                ;; an empty state and empty effects: two zero lengths at 1024
                i64.const 1024
                i64.const 32
                i64.shl
                i64.const 8
                i64.or))"#);
        let job = Job::load_with(&path, 10_000, 65536).expect("loads");
        assert_eq!(job.tick(b"", b"").expect("ticks").state, Vec::<u8>::new());
    }

    /// A NaN's bits must not depend on the machine: x86 makes 0/0 a negative
    /// NaN (0xffc00000) where other hardware makes it positive.
    #[test]
    fn nans_come_out_the_same_on_every_machine() {
        let path = wat_job("nan", r#"(module
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) i32.const 16)
            (func (export "tick") (param i32 i32) (result i64)
                ;; state = the 4 bytes of 0.0/0.0; effects empty
                (i32.store (i32.const 1024) (i32.const 4))
                (i32.store (i32.const 1028)
                    (i32.reinterpret_f32 (f32.div (f32.const 0) (f32.const 0))))
                (i32.store (i32.const 1032) (i32.const 0))
                i64.const 1024
                i64.const 32
                i64.shl
                i64.const 12
                i64.or))"#);
        let job = Job::load_with(&path, 10_000, 65536).expect("loads");
        let state = job.tick(b"", b"").expect("ticks").state;
        assert_eq!(state, 0x7fc0_0000u32.to_le_bytes(), "NaN was not canonicalized");
    }

    #[test]
    fn gc_and_relaxed_simd_are_not_admitted() {
        let gc = wat_job("gc", r#"(module
            (type $a (array (mut i8)))
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) i32.const 16)
            (func (export "tick") (param i32 i32) (result i64)
                (drop (array.new_default $a (i32.const 1000000000)))
                i64.const 0))"#);
        assert!(Job::load_with(&gc, 1000, 65536).is_err(), "a GC array, outside the memory limit");
        let relaxed = wat_job("relaxed", r#"(module
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) i32.const 16)
            (func (export "tick") (param i32 i32) (result i64)
                (drop (f32x4.relaxed_madd (v128.const i32x4 0 0 0 0)
                                          (v128.const i32x4 0 0 0 0)
                                          (v128.const i32x4 0 0 0 0)))
                i64.const 0))"#);
        assert!(Job::load_with(&relaxed, 1000, 65536).is_err(), "relaxed SIMD");
    }

    #[test]
    fn rejects_an_absurd_length() {
        let mut frame = vec![];
        frame.extend_from_slice(&(u32::MAX).to_le_bytes());
        assert!(decode_frame(&frame).is_err());
    }
}
