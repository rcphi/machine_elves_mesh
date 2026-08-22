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

pub struct Job {
    engine: Engine,
    module: Module,
    fuel: u64,
    memory: usize,
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
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(wasm_err)?;
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
        })
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
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.memory)
            // One instance and one memory: a job has no legitimate reason to
            // create more, and each one it could create is more host memory.
            .instances(1)
            .memories(1)
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

    #[test]
    fn rejects_an_absurd_length() {
        let mut frame = vec![];
        frame.extend_from_slice(&(u32::MAX).to_le_bytes());
        assert!(decode_frame(&frame).is_err());
    }
}
