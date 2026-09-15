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

/// How many buckets a summary has, and how many cells each bucket has.
///
/// 256 × 256 = 65,536 cells, addressed by a serial's first two bytes. Serials
/// are hashes, so items spread evenly: 1.7 million items is about 26 a cell.
pub const FANOUT: usize = 256;

/// What a peer needs to know about one part of the set to tell whether it
/// holds the same part: how many items, and the XOR of a slice of each serial.
///
/// XOR because it can be kept up to date one item at a time and needs no
/// ordering. A digest of the whole set had to be recomputed over every item —
/// 1.7 million SHA-256 updates a minute on diamond — and could only say
/// "something differs", so the answer to any difference was to send everything.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Cell {
    pub count: u32,
    pub xor: u64,
}

impl Cell {
    fn add(&mut self, serial: &Serial) {
        self.count += 1;
        // Bytes 8..16, not 0..2: those two address the cell, and are the same
        // for everything in it.
        self.xor ^= u64::from_le_bytes(serial.0[8..16].try_into().expect("8 bytes"));
    }

    fn fold(&mut self, other: &Cell) {
        self.count += other.count;
        self.xor ^= other.xor;
    }
}

/// Which positions differ between two summaries of the same shape.
pub fn differing(ours: &[Cell], theirs: &[Cell]) -> Vec<u8> {
    ours.iter()
        .zip(theirs)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(index, _)| index as u8)
        .collect()
}

/// Everything known to exist.
///
/// A set rather than a count. Counting would make "add this widget" an
/// operation whose result depends on how many times it happened; membership
/// does not.
#[derive(Debug, Clone)]
pub struct Ledger {
    items: BTreeSet<(String, Serial)>,
    /// Every kind held, so a cell's items can be found by range per kind —
    /// the set is ordered by kind first, then serial.
    kinds: BTreeSet<String>,
    /// FANOUT × FANOUT cells, maintained as items arrive.
    cells: Vec<Cell>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            items: BTreeSet::new(),
            kinds: BTreeSet::new(),
            cells: vec![Cell::default(); FANOUT * FANOUT],
        }
    }
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// The only way anything enters the set, so the cells cannot drift from it.
    fn insert(&mut self, kind: &str, serial: Serial) -> bool {
        if !self.items.insert((kind.to_string(), serial)) {
            return false;
        }
        if !self.kinds.contains(kind) {
            self.kinds.insert(kind.to_string());
        }
        let index = serial.0[0] as usize * FANOUT + serial.0[1] as usize;
        self.cells[index].add(&serial);
        true
    }

    /// Records a production. Returns whether this was genuinely new.
    ///
    /// A repeat is not an error and not a special case; it simply does not
    /// change anything.
    pub fn record(&mut self, kind: &str, serial: Serial) -> bool {
        self.insert(kind, serial)
    }

    /// One cell per bucket: the whole set in 256 parts, about 3 KB on the wire.
    pub fn summary(&self) -> Vec<Cell> {
        self.cells
            .chunks(FANOUT)
            .map(|bucket| {
                let mut total = Cell::default();
                bucket.iter().for_each(|cell| total.fold(cell));
                total
            })
            .collect()
    }

    /// The cells inside one bucket.
    pub fn bucket(&self, bucket: u8) -> Vec<Cell> {
        let start = bucket as usize * FANOUT;
        self.cells[start..start + FANOUT].to_vec()
    }

    /// The items in one cell.
    pub fn items_in(&self, bucket: u8, cell: u8) -> Vec<(String, Serial)> {
        let mut low = [0u8; 32];
        low[0] = bucket;
        low[1] = cell;
        let mut high = [0xffu8; 32];
        high[0] = bucket;
        high[1] = cell;
        let mut found = Vec::new();
        for kind in &self.kinds {
            let from = (kind.clone(), Serial(low));
            let to = (kind.clone(), Serial(high));
            found.extend(self.items.range(from..=to).cloned());
        }
        found
    }

    /// Absorbs another node's view of the world.
    ///
    /// Union is commutative, associative, and idempotent, so nodes converge
    /// whatever order they hear things in and however often — which is why no
    /// node ever has to ask another what it has already seen.
    #[cfg(test)]
    pub fn merge(&mut self, other: &Ledger) {
        self.absorb(other.items.iter().cloned().collect());
    }

    #[cfg(test)]
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
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Serial)> {
        self.items.iter().map(|(kind, serial)| (kind.as_str(), serial))
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
            .filter(|(kind, serial)| self.insert(kind, *serial))
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
        assert_eq!(forwards.summary(), backwards.summary());
    }

    /// Runs the repair exchange between two ledgers to completion, the way two
    /// nodes would, and returns how many items crossed. Mirrors main.rs: a
    /// summary picks buckets, a bucket's cells pick cells, and both sides send
    /// what they hold in the cells that differ.
    fn reconcile(a: &mut Ledger, b: &mut Ledger, buckets_per_round: usize) -> (usize, usize) {
        let mut sent = 0;
        let mut rounds = 0;
        while differing(&a.summary(), &b.summary()).len() > 0 {
            rounds += 1;
            assert!(rounds < 1000, "repair did not converge");
            let buckets = differing(&a.summary(), &b.summary());
            for bucket in buckets.into_iter().take(buckets_per_round) {
                for cell in differing(&a.bucket(bucket), &b.bucket(bucket)) {
                    let from_a = a.items_in(bucket, cell);
                    let from_b = b.items_in(bucket, cell);
                    sent += from_a.len() + from_b.len();
                    b.absorb(from_a);
                    a.absorb(from_b);
                }
            }
        }
        (sent, rounds)
    }

    #[test]
    fn repair_sends_what_differs_not_everything() {
        // The night yoinkerz spent on a hotspot: 1.3 million items in common,
        // 88 missing, and every minute all 1.3 million were offered. 537 times,
        // 7.54 GB. Here the common part is smaller, the principle the same.
        let mut a = ledger_of(&(0..50_000).collect::<Vec<_>>());
        let mut b = a.clone();
        for tick in 50_000..50_088 {
            a.record("widget", Serial::derive(&JOB, tick, "widget", 0));
        }
        let (sent, _) = reconcile(&mut a, &mut b, 8);
        assert_eq!(a.len(), b.len());
        assert_eq!(a.summary(), b.summary());
        // Each missing item brings its cell-mates (under one each at this size),
        // and each side sends its own copy of the cell: a small multiple of 88,
        // never a fraction of 50,000.
        assert!(sent < 88 * 8, "sent {sent} items to repair 88");
    }

    #[test]
    fn repair_fills_an_empty_node_in_bounded_rounds() {
        // A node arriving with nothing still gets everything — just not all at
        // once, which is what overflowed the send queue before.
        let mut full = ledger_of(&(0..20_000).collect::<Vec<_>>());
        let mut empty = Ledger::new();
        let (_, rounds) = reconcile(&mut full, &mut empty, 8);
        assert_eq!(empty.len(), 20_000);
        assert_eq!(rounds, FANOUT.div_ceil(8), "eight buckets a round");
    }

    #[test]
    fn items_differing_on_both_sides_reach_both_sides() {
        let mut a = ledger_of(&(0..1_000).collect::<Vec<_>>());
        let mut b = a.clone();
        a.record("widget", Serial::derive(&JOB, 5_000, "widget", 0));
        b.record("gear", Serial::derive(&JOB, 6_000, "gear", 0));
        reconcile(&mut a, &mut b, 8);
        assert_eq!(a.len(), 1_002);
        assert_eq!(b.len(), 1_002);
    }

    #[test]
    fn a_cell_holds_exactly_the_items_addressed_to_it() {
        let ledger = ledger_of(&(0..5_000).collect::<Vec<_>>());
        let mut total = 0;
        for bucket in 0..=255u8 {
            let cells = ledger.bucket(bucket);
            for cell in 0..=255u8 {
                let items = ledger.items_in(bucket, cell);
                assert_eq!(items.len() as u32, cells[cell as usize].count);
                total += items.len();
            }
        }
        assert_eq!(total, 5_000, "every item in exactly one cell");
    }

    #[test]
    fn a_missing_item_changes_the_digest() {
        assert_ne!(ledger_of(&[1, 2, 3]).summary(), ledger_of(&[1, 2]).summary());
    }

    #[test]
    fn a_ledger_survives_being_written_down_and_read_back() {
        let original = ledger_of(&[1, 2, 3]);
        let bytes = encode_items(original.iter());
        let mut recovered = Ledger::new();
        recovered.absorb(decode_items(&bytes));
        assert_eq!(original.summary(), recovered.summary());
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
