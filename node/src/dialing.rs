//! When to try each address next.
//!
//! Run F found the node dialling every address it could not reach every 2 s,
//! for as long as it ran: about 13 MB/h per address, two-thirds of a hotspot's
//! data spent on a peer that never answered. Each address now keeps its own
//! schedule. The first retry is still 2 s, so a peer that restarted is back
//! within seconds; after that the wait doubles, to a cap set by what kind of
//! address it is.
//!
//! **Only two things bring an address forward:** being learned for the first
//! time, and losing a direct connection to its peer ([`Schedule::reset`]).
//! Hearing about it again is not one of them. A peer behind translation is
//! heard constantly through whoever forwards its gossip, and an address
//! re-announced every 30 s that reset its own backoff would be dialled as
//! often as it was before any of this existed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::{Multiaddr, PeerId};

/// What kind of address this is, which sets how patiently it is retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Given on the command line: the way into the mesh.
    Configured,
    /// From the peer book: a peer that was reachable before.
    Remembered,
    /// Announced by a member, which saw the peer there.
    Observed,
    /// Through an introducer. Each attempt is a punch, and costs the
    /// introducer some of its relay budget.
    Circuit,
}

/// The first retry for every kind except circuits.
pub const FIRST_RETRY: Duration = Duration::from_secs(2);
/// The first retry for a circuit: a punch that failed will fail again at once.
pub const CIRCUIT_FIRST_RETRY: Duration = Duration::from_secs(60);

/// How long after losing a direct connection its addresses are retried at
/// `FIRST_RETRY`, before the backoff starts. Found in the chaos rig: a peer
/// just lost is usually back soon — a partition ends, a restart completes, a
/// hotspot reconnects — and a backoff begun at once rejoined up to a minute
/// late. A peer that never returns costs about 60 quick attempts, once.
pub const FAST_AFTER_LOSS: Duration = Duration::from_secs(120);

/// The longest wait between two attempts at one address.
pub fn cap(kind: Kind) -> Duration {
    match kind {
        Kind::Configured => Duration::from_secs(60),
        Kind::Remembered | Kind::Observed => Duration::from_secs(300),
        Kind::Circuit => Duration::from_secs(1800),
    }
}

fn first(kind: Kind) -> Duration {
    if kind == Kind::Circuit { CIRCUIT_FIRST_RETRY } else { FIRST_RETRY }
}

struct Entry {
    kind: Kind,
    peer: Option<PeerId>,
    next: Instant,
    interval: Duration,
    /// Until when retries stay at the first interval, after a loss.
    fast_until: Option<Instant>,
}

/// Every address this node is trying to reach, and when it tries each next.
#[derive(Default)]
pub struct Schedule {
    entries: HashMap<Multiaddr, Entry>,
}

impl Schedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules `addr`, due at once, and says whether it was new. An address
    /// already scheduled keeps its backoff: see the module notes. Its kind
    /// can only become `Configured`, the most urgent.
    pub fn add(&mut self, addr: Multiaddr, kind: Kind, peer: Option<PeerId>, now: Instant) -> bool {
        if let Some(entry) = self.entries.get_mut(&addr) {
            if kind == Kind::Configured && entry.kind != Kind::Configured {
                entry.kind = Kind::Configured;
                entry.interval = entry.interval.min(cap(Kind::Configured));
            }
            if entry.peer.is_none() {
                entry.peer = peer;
            }
            return false;
        }
        self.entries.insert(addr, Entry { kind, peer, next: now, interval: first(kind), fast_until: None });
        true
    }

    /// Due now, with its backoff started again: a direct connection to its
    /// peer was just lost, so the peer may well be back in seconds.
    ///
    /// A no-op for circuits. A relayed connection ends at the introducer's
    /// limit by design, and that ending must not start another punch.
    pub fn reset(&mut self, addr: &Multiaddr, now: Instant) {
        if let Some(entry) = self.entries.get_mut(addr) {
            if entry.kind != Kind::Circuit {
                entry.next = now;
                entry.interval = first(entry.kind);
                entry.fast_until = Some(now + FAST_AFTER_LOSS);
            }
        }
    }

    /// Stops trying every address tagged with `peer`, which is now connected.
    pub fn forget_peer(&mut self, peer: &PeerId) {
        self.entries.retain(|_, entry| entry.peer.as_ref() != Some(peer));
    }

    pub fn remove(&mut self, addr: &Multiaddr) {
        self.entries.remove(addr);
    }

    pub fn contains(&self, addr: &Multiaddr) -> bool {
        self.entries.contains_key(addr)
    }

    /// The addresses due at `now`. Each is moved to `now` plus its interval,
    /// and the interval doubles, up to its kind's cap.
    pub fn due(&mut self, now: Instant) -> Vec<Multiaddr> {
        let mut due = Vec::new();
        for (addr, entry) in &mut self.entries {
            if entry.next <= now {
                due.push(addr.clone());
                entry.next = now + entry.interval;
                if entry.fast_until.is_none_or(|until| now >= until) {
                    entry.interval = (entry.interval * 2).min(cap(entry.kind));
                }
            }
        }
        due
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::{Multiaddr, PeerId};
    use std::time::{Duration, Instant};

    fn addr(port: u16) -> Multiaddr {
        format!("/ip4/198.51.100.7/udp/{port}/quic-v1").parse().unwrap()
    }

    /// Seconds between successive attempts at one address, stepping a clock
    /// one second at a time.
    fn gaps(kind: Kind, n: usize) -> Vec<u64> {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), kind, None, t0);
        let mut hits = Vec::new();
        let mut second = 0u64;
        while hits.len() < n + 1 {
            if !s.due(t0 + Duration::from_secs(second)).is_empty() {
                hits.push(second);
            }
            second += 1;
        }
        hits.windows(2).map(|w| w[1] - w[0]).collect()
    }

    fn attempts_in_an_hour(kind: Kind) -> usize {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), kind, None, t0);
        (0..3600).filter(|&sec| !s.due(t0 + Duration::from_secs(sec)).is_empty()).count()
    }

    #[test]
    fn a_new_address_is_due_at_once() {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        assert!(s.add(addr(1), Kind::Observed, None, t0));
        assert_eq!(s.due(t0), vec![addr(1)]);
        assert!(s.due(t0).is_empty(), "once tried, it waits");
    }

    #[test]
    fn observed_retries_double_from_two_seconds_to_five_minutes() {
        assert_eq!(gaps(Kind::Observed, 10), vec![2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
    }

    #[test]
    fn configured_retries_cap_at_a_minute() {
        assert_eq!(gaps(Kind::Configured, 7), vec![2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn circuits_start_at_a_minute_and_cap_at_half_an_hour() {
        assert_eq!(gaps(Kind::Circuit, 7), vec![60, 120, 240, 480, 960, 1800, 1800]);
    }

    /// Run F: riverboat re-announced yoinkerz's address every 30 s, and each
    /// announcement must not put diamond's dialling back at 2 s.
    #[test]
    fn a_known_address_announced_again_keeps_its_backoff() {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), Kind::Observed, None, t0);
        let mut last = 0;
        let mut hits = 0;
        for sec in 0..100 {
            if !s.due(t0 + Duration::from_secs(sec)).is_empty() {
                hits += 1;
                last = sec;
                if hits == 3 {
                    break;
                }
            }
        }
        assert_eq!(last, 6, "tried at 0, 2 and 6");
        assert!(!s.add(addr(1), Kind::Observed, None, t0 + Duration::from_secs(7)));
        assert!(s.due(t0 + Duration::from_secs(13)).is_empty(), "the next try is still at 14");
        assert_eq!(s.due(t0 + Duration::from_secs(14)), vec![addr(1)]);
    }

    #[test]
    fn reset_makes_an_address_due_now_and_starts_again() {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), Kind::Observed, None, t0);
        for sec in 0..200 {
            s.due(t0 + Duration::from_secs(sec));
        }
        let t = t0 + Duration::from_secs(200);
        s.reset(&addr(1), t);
        assert_eq!(s.due(t), vec![addr(1)]);
        assert!(s.due(t + Duration::from_secs(1)).is_empty());
        assert_eq!(s.due(t + Duration::from_secs(2)), vec![addr(1)]);
    }

    /// A relayed connection ends at the relay's 120 s limit. If that loss put
    /// the circuit back at the front, every introduction would cost 128 KiB
    /// of the introducer's budget every two minutes.
    #[test]
    fn resetting_a_circuit_address_does_not_hurry_it() {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), Kind::Circuit, None, t0);
        assert_eq!(s.due(t0), vec![addr(1)]);
        s.reset(&addr(1), t0 + Duration::from_secs(5));
        assert!(s.due(t0 + Duration::from_secs(5)).is_empty());
        assert!(s.due(t0 + Duration::from_secs(59)).is_empty());
        assert_eq!(s.due(t0 + Duration::from_secs(60)), vec![addr(1)]);
    }

    #[test]
    fn forgetting_a_peer_removes_all_its_addresses() {
        let t0 = Instant::now();
        let peer = PeerId::random();
        let mut s = Schedule::new();
        s.add(addr(1), Kind::Observed, Some(peer), t0);
        s.add(addr(2), Kind::Circuit, Some(peer), t0);
        s.add(addr(3), Kind::Configured, None, t0);
        s.forget_peer(&peer);
        assert_eq!(s.len(), 1);
        assert!(s.contains(&addr(3)));
        s.remove(&addr(3));
        assert_eq!(s.len(), 0);
    }

    /// Found in the chaos rig: a 51 s partition ended between the backoff's
    /// 30 s and 62 s attempts, so the two nodes rejoined 23 s after the
    /// network had healed, where a 2 s retry had always rejoined within 2 s.
    /// A peer just lost is usually back soon: retries stay at 2 s for a while.
    #[test]
    fn after_a_lost_connection_retries_stay_at_two_seconds_for_two_minutes() {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), Kind::Configured, None, t0);
        s.due(t0);
        s.reset(&addr(1), t0);
        let mut hits = Vec::new();
        for sec in 0..300 {
            if !s.due(t0 + Duration::from_secs(sec)).is_empty() {
                hits.push(sec);
            }
        }
        let gaps: Vec<u64> = hits.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().take_while(|&&g| g == 2).count() >= 59, "{gaps:?}");
        assert_eq!(FAST_AFTER_LOSS, Duration::from_secs(120));
        assert!(gaps.iter().any(|&g| g > 2), "and then it backs off: {gaps:?}");
    }

    #[test]
    fn a_lost_peer_that_never_returns_costs_under_130_attempts_in_its_first_hour() {
        let t0 = Instant::now();
        let mut s = Schedule::new();
        s.add(addr(1), Kind::Configured, None, t0);
        s.due(t0);
        s.reset(&addr(1), t0);
        let n = (0..3600).filter(|&sec| !s.due(t0 + Duration::from_secs(sec)).is_empty()).count();
        assert!(n <= 130, "{n}");
    }

    #[test]
    fn a_dead_observed_address_is_tried_at_most_20_times_an_hour() {
        assert!(attempts_in_an_hour(Kind::Observed) <= 20, "{}", attempts_in_an_hour(Kind::Observed));
    }

    #[test]
    fn a_dead_configured_address_is_tried_at_most_65_times_an_hour() {
        assert!(attempts_in_an_hour(Kind::Configured) <= 65, "{}", attempts_in_an_hour(Kind::Configured));
    }
}
