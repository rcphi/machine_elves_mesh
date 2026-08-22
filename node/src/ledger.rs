//! What exists, and how two machines that both made it still made one of it.
//!
//! A job may briefly be continued by two nodes at once: a node that has not yet
//! heard from a peer genuinely believes it is alone, and no amount of faster
//! messaging fixes that. Being deterministic, both produce *identical* output.
//!
//! The tempting fix is a rule — "ignore an effect you have already applied" —
//! which requires every applier to remember what it has seen and to agree with
//! every other applier about what counts as the same. The better fix is to make
//! the duplicate not exist: **a thing's identity is derived from what produced
//! it**, so two nodes making the same widget make *the same widget*, and
//! recording it twice is recording it once.
//!
//! Nothing here needs coordination, ordering, or agreement. Merging two views
//! is set union, which gives the same answer in any order, however many times
//! it is applied — the property a world with no coordinator actually needs.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

/// The identity of one produced thing.
///
/// Derived, never assigned. There is no authority handing out serial numbers,
/// because an authority is a thing that can be absent — and this has to work
/// when every node is talking to a different half of the world.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Serial([u8; 32]);

impl Serial {
    /// `job` is the hash of the job's own code, so a job is identified by what
    /// it *is* rather than by a name someone gave it. Two nodes running the
    /// same code at the same tick therefore agree without being told.
    pub fn derive(job: &[u8; 32], tick: u64, kind: &str, ordinal: u32) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"machine-elves/serial/1");
        hasher.update(job);
        hasher.update(tick.to_le_bytes());
        // Lengths are hashed alongside the values so that a different split of
        // the same bytes cannot produce the same serial.
        hasher.update((kind.len() as u32).to_le_bytes());
        hasher.update(kind.as_bytes());
        hasher.update(ordinal.to_le_bytes());
        Serial(hasher.finalize().into())
    }

    pub fn short(&self) -> String {
        self.0[..6].iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Everything known to exist.
///
/// A set rather than a count. Counting would make "add this widget" an
/// operation whose result depends on how many times it happened; membership
/// does not.
#[derive(Default, Debug, Clone)]
pub struct Ledger {
    items: BTreeSet<(String, Serial)>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a production. Returns whether this was genuinely new.
    ///
    /// A repeat is not an error and not a special case; it simply does not
    /// change anything.
    pub fn record(&mut self, kind: &str, serial: Serial) -> bool {
        self.items.insert((kind.to_string(), serial))
    }

    /// Absorbs another node's view of the world.
    ///
    /// Union is commutative, associative, and idempotent, so nodes converge
    /// whatever order they hear things in and however often — which is why no
    /// node ever has to ask another what it has already seen.
    pub fn merge(&mut self, other: &Ledger) {
        self.items.extend(other.items.iter().cloned());
    }

    pub fn count(&self, kind: &str) -> usize {
        self.items.iter().filter(|(k, _)| k == kind).count()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: [u8; 32] = [7; 32];

    #[test]
    fn the_same_production_has_the_same_identity() {
        // Two nodes, same code, same tick, same item. This is the whole idea.
        assert_eq!(
            Serial::derive(&JOB, 12, "widget", 0),
            Serial::derive(&JOB, 12, "widget", 0)
        );
    }

    #[test]
    fn different_productions_have_different_identities() {
        let base = Serial::derive(&JOB, 12, "widget", 0);
        assert_ne!(base, Serial::derive(&JOB, 13, "widget", 0), "different tick");
        assert_ne!(base, Serial::derive(&JOB, 12, "widget", 1), "different item");
        assert_ne!(base, Serial::derive(&JOB, 12, "gear", 0), "different kind");
        assert_ne!(base, Serial::derive(&[9; 32], 12, "widget", 0), "different job");
    }

    #[test]
    fn a_kind_and_ordinal_cannot_be_confused_with_another_split() {
        // Without hashing the length, "ab" + 1 and "a" + something could collide
        // by running together into the same bytes.
        assert_ne!(
            Serial::derive(&JOB, 1, "ab", 0),
            Serial::derive(&JOB, 1, "a", 0)
        );
    }

    #[test]
    fn recording_the_same_thing_twice_records_it_once() {
        let mut ledger = Ledger::new();
        let serial = Serial::derive(&JOB, 5, "widget", 0);
        assert!(ledger.record("widget", serial), "first time is new");
        assert!(!ledger.record("widget", serial), "second time is not");
        assert_eq!(ledger.count("widget"), 1);
    }

    #[test]
    fn two_nodes_doing_the_same_work_produce_one_inventory() {
        // The case this exists for: a job continued by two nodes at once. They
        // are deterministic, so they make the same things, so the world ends up
        // with the right number of them — with nobody deduplicating anything.
        let mut alone = Ledger::new();
        let mut node_a = Ledger::new();
        let mut node_b = Ledger::new();

        for tick in 0..20u64 {
            for ordinal in 0..2u32 {
                let serial = Serial::derive(&JOB, tick, "widget", ordinal);
                alone.record("widget", serial);
                node_a.record("widget", serial);
                node_b.record("widget", serial);
            }
        }

        node_a.merge(&node_b);
        assert_eq!(
            node_a.count("widget"),
            alone.count("widget"),
            "duplicated work must not duplicate the widgets"
        );
        assert_eq!(node_a.count("widget"), 40);
    }

    #[test]
    fn merging_is_order_independent_and_repeatable() {
        // Nodes hear about the world in whatever order the network delivers it,
        // and may hear the same thing many times. Neither may change the answer.
        let mut first = Ledger::new();
        let mut second = Ledger::new();
        let mut third = Ledger::new();

        for tick in 0..10u64 {
            first.record("widget", Serial::derive(&JOB, tick, "widget", 0));
        }
        for tick in 5..15u64 {
            second.record("widget", Serial::derive(&JOB, tick, "widget", 0));
        }
        for tick in 12..20u64 {
            third.record("widget", Serial::derive(&JOB, tick, "widget", 0));
        }

        let mut one_way = first.clone();
        one_way.merge(&second);
        one_way.merge(&third);

        let mut other_way = third.clone();
        other_way.merge(&first);
        other_way.merge(&second);
        // And again, to show repetition changes nothing.
        other_way.merge(&second);
        other_way.merge(&first);

        assert_eq!(one_way.len(), other_way.len());
        assert_eq!(one_way.len(), 20);
    }
}
