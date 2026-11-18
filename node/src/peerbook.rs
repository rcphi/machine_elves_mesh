//! Peers remembered across restarts.
//!
//! Run F's question was what a restarted node does when the only peer it was
//! told about has gone. It knew nobody else, because nothing survived the
//! restart but its identity and its inventory. The plan's rule (2026-08-22)
//! decides what to keep: **remember the peers you can dial; the ones you
//! cannot dial will remember you.** Every peer is recorded, but only those
//! known to be reachable are dialled on start.
//!
//! The file is one line per peer, tab-separated: peer id, reachable (0 or 1),
//! when last heard (unix ms), label, then addresses separated by spaces. It is
//! written whole to a temporary file and renamed over the old one, so a crash
//! mid-write leaves the previous book, never half of one. A line that cannot
//! be read is skipped and counted: a damaged book costs some memory, never the
//! node's start.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use libp2p::{Multiaddr, PeerId};

/// How many addresses are kept for one peer: the most recently dialled.
pub const MAX_ADDRS_PER_PEER: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remembered {
    pub label: String,
    pub addrs: Vec<Multiaddr>,
    pub reachable: bool,
    pub last_heard_unix_ms: u64,
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
            text.push_str(&format!("{peer}\t{}\t{}\t{label}\t{}\n",
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

    /// Every address of every peer remembered as reachable.
    pub fn addresses_to_dial(&self) -> Vec<Multiaddr> {
        self.entries.values().filter(|e| e.reachable).flat_map(|e| e.addrs.iter().cloned()).collect()
    }

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
    if fields.next().is_some() {
        return None;
    }
    Some((peer, Remembered { label, addrs, reachable, last_heard_unix_ms }))
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
