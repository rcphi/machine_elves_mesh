//! A factory, as a job.
//!
//! Steel arrives, items move along an assembly line over several ticks, and
//! finished widgets accumulate in the output bin. It exists to demonstrate the
//! job contract with something recognisable rather than to be a good factory.
//!
//! Three rules the whole design rests on, all visible here:
//!
//! 1. **Nothing survives between ticks except the state.** There are no globals
//!    and no statics; everything the factory knows is decoded from the incoming
//!    state and encoded back out. The host builds a fresh instance every tick,
//!    so anything kept elsewhere would vanish anyway.
//! 2. **Time arrives as an input.** Nothing here reads a clock, because two
//!    machines running this tick must agree, and a clock is the fastest way to
//!    make them disagree.
//! 3. **Effects are described, not performed.** The factory says it produced
//!    widgets and wants steel. Whether either happens is the host's business.
//!
//! Arithmetic is integer throughout. Floating point on WebAssembly is
//! deterministic for ordinary values but has awkward corners around NaN bit
//! patterns, and none of this needs fractions.

const LINE_SLOTS: usize = 4;
/// Progress units an item needs before it is finished.
const WORK_PER_ITEM: u32 = 100;
/// Progress one worker contributes to one item per tick.
const WORK_PER_WORKER: u32 = 12;
/// Steel consumed to start one item.
const STEEL_PER_ITEM: u32 = 2;
/// Below this, the factory asks for more steel.
const STEEL_REORDER_AT: u32 = 10;
const STEEL_REORDER_QTY: u32 = 40;

struct State {
    tick: u64,
    steel: u32,
    widgets: u32,
    wear: u32,
    line: [u32; LINE_SLOTS],
}

impl State {
    fn decode(bytes: &[u8]) -> Self {
        // An absent or malformed state means a factory that has not run yet.
        // Refusing to start would make the first tick a special case for the
        // host to handle, and there is nothing to recover from.
        let mut state = State {
            tick: 0,
            steel: 0,
            widgets: 0,
            wear: 0,
            line: [0; LINE_SLOTS],
        };
        if bytes.len() < 8 + 12 + LINE_SLOTS * 4 {
            return state;
        }
        state.tick = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        state.steel = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        state.widgets = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        state.wear = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        for (slot, chunk) in state.line.iter_mut().zip(bytes[20..].chunks_exact(4)) {
            *slot = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        state
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(20 + LINE_SLOTS * 4);
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.extend_from_slice(&self.steel.to_le_bytes());
        out.extend_from_slice(&self.widgets.to_le_bytes());
        out.extend_from_slice(&self.wear.to_le_bytes());
        for slot in &self.line {
            out.extend_from_slice(&slot.to_le_bytes());
        }
        out
    }
}

struct Inputs {
    delivered_steel: u32,
    workers: u32,
}

impl Inputs {
    fn decode(bytes: &[u8]) -> Self {
        // World time is the first eight bytes. This factory does not need it,
        // but every job receives it, because a job that wanted the time and
        // could not ask the host for it would have no other way to get it.
        let at = |i: usize| -> u32 {
            bytes
                .get(i..i + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .unwrap_or(0)
        };
        Inputs {
            delivered_steel: at(8),
            workers: at(12),
        }
    }
}

fn run(state: &mut State, inputs: &Inputs) -> String {
    let mut effects = String::new();

    state.tick += 1;
    state.steel = state.steel.saturating_add(inputs.delivered_steel);

    // Nobody present means nothing moves. The line keeps its progress, because
    // half-built items do not un-build themselves when a shift ends.
    if inputs.workers > 0 {
        let per_slot = WORK_PER_WORKER.saturating_mul(inputs.workers) / LINE_SLOTS as u32;

        let mut finished = 0u32;
        for slot in state.line.iter_mut() {
            if *slot == 0 {
                continue;
            }
            *slot = slot.saturating_add(per_slot.max(1));
            if *slot >= WORK_PER_ITEM {
                *slot = 0;
                finished += 1;
            }
        }

        if finished > 0 {
            state.widgets = state.widgets.saturating_add(finished);
            effects.push_str(&format!("produce widget {finished}\n"));
        }

        // Start new items in whatever slots are now free, while steel lasts.
        for slot in state.line.iter_mut() {
            if *slot == 0 && state.steel >= STEEL_PER_ITEM {
                state.steel -= STEEL_PER_ITEM;
                *slot = 1;
            }
        }

        state.wear = state.wear.saturating_add(1);
    }

    if state.steel < STEEL_REORDER_AT {
        effects.push_str(&format!("request steel {STEEL_REORDER_QTY}\n"));
    }

    effects
}

// ------------------------------------------------------------------- the ABI

/// Hands the host a buffer to write the input frame into.
///
/// The host cannot allocate inside this module's memory, so the module has to
/// offer it a place to write.
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let mut buf: Vec<u8> = Vec::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr as u32
}

/// One tick. Returns the output buffer as `(pointer << 32) | length`.
///
/// # Safety
/// The host guarantees `ptr..ptr+len` is a buffer it wrote through `alloc`.
#[no_mangle]
pub unsafe extern "C" fn tick(ptr: u32, len: u32) -> u64 {
    let frame = core::slice::from_raw_parts(ptr as *const u8, len as usize);
    let (state_bytes, input_bytes) = match split_frame(frame) {
        Some(parts) => parts,
        None => (&[][..], &[][..]),
    };

    let mut state = State::decode(state_bytes);
    let inputs = Inputs::decode(input_bytes);
    let effects = run(&mut state, &inputs);

    let mut out = Vec::new();
    let encoded = state.encode();
    out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
    out.extend_from_slice(&encoded);
    out.extend_from_slice(&(effects.len() as u32).to_le_bytes());
    out.extend_from_slice(effects.as_bytes());

    let ptr = out.as_ptr() as u64;
    let len = out.len() as u64;
    core::mem::forget(out);
    (ptr << 32) | len
}

fn split_frame(frame: &[u8]) -> Option<(&[u8], &[u8])> {
    let first_len = u32::from_le_bytes(frame.get(0..4)?.try_into().ok()?) as usize;
    let first = frame.get(4..4 + first_len)?;
    let at = 4 + first_len;
    let second_len = u32::from_le_bytes(frame.get(at..at + 4)?.try_into().ok()?) as usize;
    let second = frame.get(at + 4..at + 4 + second_len)?;
    Some((first, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(steel: u32, workers: u32) -> Inputs {
        Inputs {
            delivered_steel: steel,
            workers,
        }
    }

    #[test]
    fn state_survives_a_round_trip() {
        let state = State {
            tick: 7,
            steel: 40,
            widgets: 3,
            wear: 12,
            line: [1, 50, 0, 99],
        };
        let back = State::decode(&state.encode());
        assert_eq!((back.tick, back.steel, back.widgets, back.wear), (7, 40, 3, 12));
        assert_eq!(back.line, [1, 50, 0, 99]);
    }

    #[test]
    fn an_absent_state_starts_an_empty_factory() {
        let fresh = State::decode(&[]);
        assert_eq!((fresh.tick, fresh.steel, fresh.widgets), (0, 0, 0));
    }

    #[test]
    fn nobody_present_means_nothing_moves() {
        let mut state = State::decode(&[]);
        state.steel = 100;
        state.line = [50, 0, 0, 0];
        run(&mut state, &inputs(0, 0));
        // The tick still counts and the delivery still lands, but no work
        // happens and half-built items keep their progress.
        assert_eq!(state.line[0], 50);
        assert_eq!(state.wear, 0);
        assert_eq!(state.steel, 100);
    }

    #[test]
    fn steel_becomes_widgets_over_several_ticks() {
        let mut state = State::decode(&[]);
        let mut produced = 0;
        for _ in 0..40 {
            let effects = run(&mut state, &inputs(4, 4));
            if effects.contains("produce widget") {
                produced += 1;
            }
        }
        assert!(produced > 0, "nothing was ever finished");
        assert!(state.widgets > 0);
        assert!(state.tick == 40);
    }

    #[test]
    fn asks_for_steel_when_low_and_stops_when_stocked() {
        let mut state = State::decode(&[]);
        assert!(run(&mut state, &inputs(0, 1)).contains("request steel"));

        let mut stocked = State::decode(&[]);
        stocked.steel = 500;
        assert!(!run(&mut stocked, &inputs(0, 1)).contains("request steel"));
    }

    #[test]
    fn the_same_tick_twice_gives_the_same_answer() {
        // The property everything downstream depends on: checkpoint and resume,
        // speculative execution on a second machine, and verification by
        // re-running all assume this.
        let mut a = State::decode(&[]);
        let mut b = State::decode(&[]);
        a.steel = 20;
        b.steel = 20;
        for _ in 0..25 {
            assert_eq!(run(&mut a, &inputs(1, 3)), run(&mut b, &inputs(1, 3)));
            assert_eq!(a.encode(), b.encode());
        }
    }
}
