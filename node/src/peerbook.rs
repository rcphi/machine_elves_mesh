//! Peers remembered across restarts.
//!
//! Run F's question was what a restarted node does when the only peer it was
//! told about has gone. It knew nobody else, because nothing survived the
//! restart but its identity and its inventory. The plan's rule (2026-08-22)
//! decides what to keep: **remember the peers you can dial; the ones you
//! cannot dial will remember you.** Every peer is recorded, but only those
//! known to be reachable are dialled on start.
//!
//! **Where each peer was last seen is kept too, reachable or not**, for one
//! use: a node back from a long absence dials those addresses briefly. Its
//! peers behind translation have been dialling it all along, which holds
//! their routers open toward it, so it gets through at once instead of
//! waiting up to a minute for their next attempt (the operator's option 2,
//! 2026-11-18).
//!
//! The file is one line per peer, tab-separated: peer id, reachable (0 or 1),
//! when last heard (unix ms), label, addresses separated by spaces, and the
//! address it was last seen at (empty if none). A line from before that last
//! field existed has five fields and still reads. It is
//! written whole to a temporary file and renamed over the old one, so a crash
//! mid-write leaves the previous book, never half of one. A line that cannot
//! be read is skipped and counted: a damaged book costs some memory, never the
//! node's start.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use libp2p::{Multiaddr, PeerId};

/// How stale a last-seen time may grow before seeing the peer again is worth
/// a write: it decides only whether a peer counts as recent.
pub const SEEN_REFRESH_MS: u64 = 600_000;

/// How many addresses are kept for one peer: the most recently dialled.
pub const MAX_ADDRS_PER_PEER: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remembered {
    pub label: String,
    pub addrs: Vec<Multiaddr>,
    pub reachable: bool,
    pub last_heard_unix_ms: u64,
    /// The address this node last had a direct connection with it at.
    pub last_seen_at: Option<Multiaddr>,
}

#[derive(Default)]
pub struct PeerBook {
    entries: BTreeMap<PeerId, Remembered>,
}

impl PeerBook {
    /// The book at `path`, and how many of its lines could not be read. A
    /// missing file is an empty book; only a failure to read the file at all
    /// is an error.
    pub fn load(path: &Path) -> io::Result<(PeerBook, usize)> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((PeerBook::default(), 0)),
            Err(e) => return Err(e),
        };
        let mut book = PeerBook::default();
        let mut skipped = 0;
        for line in bytes.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            match std::str::from_utf8(line).ok().and_then(parse_line) {
                Some((peer, entry)) => {
                    book.entries.insert(peer, entry);
                }
                None => skipped += 1,
            }
        }
        Ok((book, skipped))
    }

    /// Writes the whole book to `<path>.tmp`, syncs it, and renames it over
    /// `path`.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        let mut text = String::new();
        for (peer, e) in &self.entries {
            let label: String = e.label.chars().map(|c| if c == '\t' || c == '\n' { ' ' } else { c }).collect();
            let addrs: Vec<String> = e.addrs.iter().map(|a| a.to_string()).collect();
            let seen = e.last_seen_at.as_ref().map(|a| a.to_string()).unwrap_or_default();
            text.push_str(&format!("{peer}\t{}\t{}\t{label}\t{}\t{seen}\n",
                                   u8::from(e.reachable), e.last_heard_unix_ms, addrs.join(" ")));
        }
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// Records that `addr` reached `peer` by being dialled. True if the book
    /// changed.
    pub fn dialled(&mut self, peer: PeerId, addr: &Multiaddr, now_unix_ms: u64) -> bool {
        let entry = self.entries.entry(peer).or_default();
        if entry.addrs.contains(addr) {
            return false;
        }
        entry.addrs.push(addr.clone());
        if entry.addrs.len() > MAX_ADDRS_PER_PEER {
            entry.addrs.remove(0);
        }
        entry.last_heard_unix_ms = now_unix_ms;
        true
    }

    pub fn named(&mut self, peer: PeerId, label: &str) -> bool {
        let entry = self.entries.entry(peer).or_default();
        if entry.label == label {
            return false;
        }
        entry.label = label.to_string();
        true
    }

    pub fn set_reachable(&mut self, peer: PeerId, reachable: bool) -> bool {
        let entry = self.entries.entry(peer).or_default();
        if entry.reachable == reachable {
            return false;
        }
        entry.reachable = reachable;
        true
    }

    /// Records a direct connection with `peer` at `addr`. True when that is
    /// worth writing: a new place, or a last-seen time gone stale.
    pub fn seen(&mut self, peer: PeerId, addr: &Multiaddr, now_unix_ms: u64) -> bool {
        let entry = self.entries.entry(peer).or_default();
        let moved = entry.last_seen_at.as_ref() != Some(addr);
        let stale = now_unix_ms.saturating_sub(entry.last_heard_unix_ms) > SEEN_REFRESH_MS;
        if moved || stale {
            entry.last_seen_at = Some(addr.clone());
            entry.last_heard_unix_ms = now_unix_ms;
            return true;
        }
        false
    }

    /// Peers not remembered as reachable, seen within `within_ms` of
    /// `now_unix_ms`, with where they were last seen.
    pub fn seen_recently(&self, now_unix_ms: u64, within_ms: u64) -> Vec<(PeerId, Multiaddr)> {
        self.entries
            .iter()
            .filter(|(_, e)| !e.reachable && now_unix_ms.saturating_sub(e.last_heard_unix_ms) <= within_ms)
            .filter_map(|(peer, e)| e.last_seen_at.clone().map(|a| (*peer, a)))
            .collect()
    }

    /// Every address of every peer remembered as reachable.
    pub fn addresses_to_dial(&self) -> Vec<Multiaddr> {
        self.entries.values().filter(|e| e.reachable).flat_map(|e| e.addrs.iter().cloned()).collect()
    }

    #[cfg(test)]
    pub fn get(&self, peer: &PeerId) -> Option<&Remembered> {
        self.entries.get(peer)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

fn parse_line(line: &str) -> Option<(PeerId, Remembered)> {
    let mut fields = line.split('\t');
    let peer: PeerId = fields.next()?.parse().ok()?;
    let reachable = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    let last_heard_unix_ms = fields.next()?.parse().ok()?;
    let label = fields.next()?.to_string();
    let addrs = fields
        .next()?
        .split(' ')
        .filter(|a| !a.is_empty())
        .map(|a| a.parse().ok())
        .collect::<Option<Vec<Multiaddr>>>()?;
    let last_seen_at = match fields.next() {
        None | Some("") => None,
        Some(text) => Some(text.parse().ok()?),
    };
    if fields.next().is_some() {
        return None;
    }
    Some((peer, Remembered { label, addrs, reachable, last_heard_unix_ms, last_seen_at }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::{Multiaddr, PeerId};
    use std::path::PathBuf;

    fn addr(port: u16) -> Multiaddr {
        format!("/ip4/198.51.100.7/udp/{port}/quic-v1").parse().unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerbook-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("peers")
    }

    #[test]
    fn a_book_survives_a_round_trip() {
        let path = scratch("round-trip");
        let (a, b) = (PeerId::random(), PeerId::random());
        let mut book = PeerBook::default();
        book.dialled(a, &addr(4001), 1_000);
        book.named(a, "riverboat");
        book.set_reachable(a, true);
        book.dialled(b, &addr(4002), 2_000);
        book.save(&path).unwrap();
        let (loaded, skipped) = PeerBook::load(&path).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get(&a).unwrap(), book.get(&a).unwrap());
        assert_eq!(loaded.get(&b).unwrap(), book.get(&b).unwrap());
    }

    #[test]
    fn a_missing_file_is_an_empty_book() {
        let (book, skipped) = PeerBook::load(&scratch("missing")).unwrap();
        assert_eq!((book.len(), skipped), (0, 0));
    }

    #[test]
    fn a_damaged_line_is_skipped_and_counted() {
        let path = scratch("damaged");
        let mut book = PeerBook::default();
        book.dialled(PeerId::random(), &addr(1), 1);
        book.dialled(PeerId::random(), &addr(2), 2);
        book.save(&path).unwrap();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not a peer\tat all\n");
        std::fs::write(&path, text).unwrap();
        let (loaded, skipped) = PeerBook::load(&path).unwrap();
        assert_eq!((loaded.len(), skipped), (2, 1));
    }

    #[test]
    fn a_book_of_garbage_starts_empty_and_says_so() {
        let path = scratch("garbage");
        std::fs::write(&path, [0xff, 0xfe, b'\n', 0x00, b'\t', b'\n', 0x80]).unwrap();
        let (loaded, skipped) = PeerBook::load(&path).expect("never an error for bad contents");
        assert_eq!(loaded.len(), 0);
        assert!(skipped > 0);
    }

    #[test]
    fn a_leftover_temporary_file_does_not_matter() {
        let path = scratch("leftover");
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, "half a write from a crash").unwrap();
        let mut book = PeerBook::default();
        book.dialled(PeerId::random(), &addr(1), 1);
        book.save(&path).unwrap();
        assert!(!tmp.exists());
        assert_eq!(PeerBook::load(&path).unwrap().0.len(), 1);
    }

    #[test]
    fn only_peers_remembered_as_reachable_are_dialled_on_start() {
        let (a, b) = (PeerId::random(), PeerId::random());
        let mut book = PeerBook::default();
        book.dialled(a, &addr(1), 1);
        book.set_reachable(a, true);
        book.dialled(b, &addr(2), 1);
        assert_eq!(book.addresses_to_dial(), vec![addr(1)]);
    }

    #[test]
    fn a_peer_keeps_its_four_most_recent_addresses() {
        let a = PeerId::random();
        let mut book = PeerBook::default();
        for port in 1..=6 {
            book.dialled(a, &addr(port), port as u64);
        }
        assert_eq!(book.get(&a).unwrap().addrs, vec![addr(3), addr(4), addr(5), addr(6)]);
        assert_eq!(MAX_ADDRS_PER_PEER, 4);
    }

    #[test]
    fn where_a_peer_was_last_seen_survives_a_round_trip() {
        let path = scratch("seen");
        let a = PeerId::random();
        let mut book = PeerBook::default();
        assert!(book.seen(a, &addr(5465), 1_000));
        book.save(&path).unwrap();
        let (loaded, skipped) = PeerBook::load(&path).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(loaded.get(&a).unwrap().last_seen_at, Some(addr(5465)));
    }

    /// A book written before last-seen addresses existed has five fields.
    #[test]
    fn a_book_from_before_last_seen_addresses_still_reads() {
        let path = scratch("five-fields");
        let a = PeerId::random();
        std::fs::write(&path, format!("{a}\t1\t7\triverboat\t{}\n", addr(4001))).unwrap();
        let (loaded, skipped) = PeerBook::load(&path).unwrap();
        assert_eq!((loaded.len(), skipped), (1, 0));
        assert_eq!(loaded.get(&a).unwrap().last_seen_at, None);
    }

    /// Where to dial on coming back: peers seen within the window, at the
    /// address they were last seen at, except those remembered as reachable,
    /// which are dialled anyway.
    #[test]
    fn peers_seen_recently_are_dialled_briefly_on_return() {
        let (recent, old, reachable) = (PeerId::random(), PeerId::random(), PeerId::random());
        let mut book = PeerBook::default();
        book.seen(recent, &addr(1), 10_000_000);
        book.seen(old, &addr(2), 1_000);
        book.seen(reachable, &addr(3), 10_000_000);
        book.set_reachable(reachable, true);
        assert_eq!(book.seen_recently(10_000_000 + 3_600_000, 86_400_000 / 24 * 2), vec![(recent, addr(1))]);
    }

    #[test]
    fn seeing_a_peer_where_it_was_already_seen_changes_nothing_worth_writing() {
        let a = PeerId::random();
        let mut book = PeerBook::default();
        assert!(book.seen(a, &addr(1), 1_000));
        assert!(!book.seen(a, &addr(1), 2_000), "a minute later, same place: not worth a write");
        assert!(book.seen(a, &addr(1), 1_000 + SEEN_REFRESH_MS + 1), "but it is kept roughly current");
        assert!(book.seen(a, &addr(2), 1_000 + SEEN_REFRESH_MS + 2), "and a new place is news");
    }

    #[test]
    fn recording_what_is_already_known_changes_nothing() {
        let a = PeerId::random();
        let mut book = PeerBook::default();
        assert!(book.dialled(a, &addr(1), 1));
        assert!(!book.dialled(a, &addr(1), 1));
        assert!(book.named(a, "x"));
        assert!(!book.named(a, "x"));
        assert!(book.set_reachable(a, true));
        assert!(!book.set_reachable(a, true));
    }
}
