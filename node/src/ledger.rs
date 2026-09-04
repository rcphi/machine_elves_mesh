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
use std::io::{self, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

/// One item on the wire and on disk: the length of its kind, the kind, the
/// serial. The same shape in both places on purpose — a chunk of ledger sent
/// to a peer and a chunk of ledger written to a file are the same thing, and
/// giving them one encoding means there is only one of them to get wrong.
pub fn encode_items<'a>(items: impl Iterator<Item = (&'a str, &'a Serial)>) -> Vec<u8> {
    let mut out = Vec::new();
    for (kind, serial) in items {
        out.extend_from_slice(&(kind.len() as u32).to_le_bytes());
        out.extend_from_slice(kind.as_bytes());
        out.extend_from_slice(serial.as_bytes());
    }
    out
}

/// Reads back what `encode_items` wrote.
///
/// A trailing partial record is dropped rather than refused. The file is
/// appended to as things are made, so a power cut lands mid-record eventually;
/// losing the last item is the correct outcome, and refusing to start because
/// of it is not.
pub fn decode_items(bytes: &[u8]) -> Vec<(String, Serial)> {
    let mut items = Vec::new();
    let mut at = 0usize;
    while at + 4 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
        at += 4;
        // A length that runs past the end is the truncated-record case.
        let Some(kind) = bytes.get(at..at + len) else { break };
        let Ok(kind) = std::str::from_utf8(kind) else { break };
        at += len;
        let Some(serial) = bytes.get(at..at + 32) else { break };
        at += 32;
        items.push((
            kind.to_string(),
            Serial::from_bytes(serial.try_into().expect("32 bytes")),
        ));
    }
    items
}

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

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Serial(bytes)
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
        self.absorb(other.items.iter().cloned().collect());
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

    /// Everything, in an order every node agrees on.
    ///
    /// The set is ordered, so two nodes holding the same items iterate them
    /// identically no matter what order they arrived in. Everything below
    /// depends on that: a digest computed from this is comparable between
    /// machines, and a file written from it can be read back into the same set.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Serial)> {
        self.items.iter().map(|(kind, serial)| (kind.as_str(), serial))
    }

    /// A short stand-in for the whole set, for asking "do we agree?" cheaply.
    ///
    /// Two nodes exchange 32 bytes instead of megabytes, and learn whether
    /// they hold the same things. It says nothing about *what* differs — only
    /// that something does, which is enough to decide whether to look.
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"machine-elves/ledger-digest/1");
        for (kind, serial) in self.iter() {
            hasher.update((kind.len() as u32).to_le_bytes());
            hasher.update(kind.as_bytes());
            hasher.update(serial.as_bytes());
        }
        hasher.finalize().into()
    }

    /// Absorbs items from a peer or a file, returning those that were new.
    ///
    /// Returns the items themselves rather than a count because the caller has
    /// to write them down, and the set is sorted — new arrivals land wherever
    /// they belong, not at the end. There is no way to recover them afterwards
    /// by looking at the set.
    pub fn absorb(&mut self, items: Vec<(String, Serial)>) -> Vec<(String, Serial)> {
        items
            .into_iter()
            .filter(|(kind, serial)| self.items.insert((kind.clone(), *serial)))
            .collect()
    }

    /// Reads a ledger back from disk. A missing file is an empty ledger.
    ///
    /// This is the difference between an inventory and a session's tally. The
    /// set is grow-only precisely so that it can outlive the process that
    /// built it; keeping it only in memory threw that away every restart.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut ledger = Self::new();
        match std::fs::File::open(path) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                ledger.absorb(decode_items(&bytes));
                Ok(ledger)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ledger),
            Err(error) => Err(error),
        }
    }
}

/// Adds items to the file without rewriting it.
///
/// Append rather than replace, because the set only ever grows: what is already
/// written is never wrong, so there is nothing to rewrite. A full rewrite of a
/// third of a million items several times a second would also be the single
/// most expensive thing this program does.
pub fn append(path: &Path, items: &[(String, Serial)]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&encode_items(
        items.iter().map(|(kind, serial)| (kind.as_str(), serial)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: [u8; 32] = [7; 32];

    fn ledger_of(ticks: &[u64]) -> Ledger {
        let mut ledger = Ledger::new();
        for tick in ticks {
            ledger.record("widget", Serial::derive(&JOB, *tick, "widget", 0));
        }
        ledger
    }

    #[test]
    fn two_nodes_holding_the_same_things_agree_on_the_digest() {
        // Insertion order differs, because two nodes hear about things in
        // whatever order the network delivers them. The digest is what lets
        // them notice they nonetheless match, without shipping the contents.
        let forwards = ledger_of(&[1, 2, 3, 4, 5]);
        let backwards = ledger_of(&[5, 4, 3, 2, 1]);
        assert_eq!(forwards.digest(), backwards.digest());
    }

    #[test]
    fn a_missing_item_changes_the_digest() {
        assert_ne!(ledger_of(&[1, 2, 3]).digest(), ledger_of(&[1, 2]).digest());
    }

    #[test]
    fn a_ledger_survives_being_written_down_and_read_back() {
        let original = ledger_of(&[1, 2, 3]);
        let bytes = encode_items(original.iter());
        let mut recovered = Ledger::new();
        recovered.absorb(decode_items(&bytes));
        assert_eq!(original.digest(), recovered.digest());
        assert_eq!(recovered.len(), 3);
    }

    #[test]
    fn a_file_cut_off_mid_record_costs_only_that_record() {
        // What a power cut during an append actually leaves behind. Losing the
        // last item is correct; refusing to start is not.
        let bytes = encode_items(ledger_of(&[1, 2, 3]).iter());
        for cut in 1..bytes.len() {
            let recovered = decode_items(&bytes[..cut]);
            assert!(recovered.len() <= 3, "invented items from truncated bytes");
        }
        // Two whole records plus a fragment of the third.
        let whole = 4 + "widget".len() + 32;
        assert_eq!(decode_items(&bytes[..whole * 2 + 3]).len(), 2);
    }

    #[test]
    fn absorbing_reports_only_what_was_new() {
        let mut ledger = ledger_of(&[1, 2]);
        let arriving = encode_items(ledger_of(&[2, 3]).iter());
        assert_eq!(ledger.absorb(decode_items(&arriving)).len(), 1, "only tick 3 is new");
        assert_eq!(ledger.len(), 3);
        // Hearing it again changes nothing, which is why nobody has to track
        // what they have already sent.
        assert!(ledger.absorb(decode_items(&arriving)).is_empty());
    }

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
