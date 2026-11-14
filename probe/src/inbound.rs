//! The idle-timeout test that works on routers which preserve port numbers.
//!
//! `--mapping-lifetime` asks a STUN server twice, either side of a silence, and
//! compares the external ports. Its second question is itself an outbound
//! packet: if the mapping had expired, that packet makes a new one, and a
//! router that preserves port numbers gives the new mapping the old number. So
//! on those routers "unchanged" cannot tell survival from expiry. diamond's
//! friend's router is one (2026-11-14), and it is the router being measured.
//!
//! This test never speaks after the silence until it knows the answer. A
//! cooperating peer — one of our own machines, never a rented server — sends a
//! packet *in* once the silence is over. If it arrives, the mapping survived
//! without being refreshed; that is exactly what a peer-to-peer connection
//! needs, because it is the peer's packet that must get through.
//!
//! The sequence, on one client socket:
//!
//! 1. `HELLO`: the client introduces itself; the responder answers `SEEN` with
//!    the address it observed, which is the mapping under test. Peer-observed,
//!    so no STUN server is involved at all.
//! 2. `FLOW` × 3, a second apart, each echoed. A mesh connection is a two-way
//!    flow that has lasted a while, and routers often keep those longer than a
//!    single request (Linux promotes a UDP flow to its longer "stream" timeout
//!    once it has carried replies for two seconds). One request and one reply
//!    would measure the wrong thing.
//! 3. Silence, measured from the last packet the client heard.
//! 4. `PROBE` from the responder, three times a second apart. Arrival is
//!    survival. Nothing arriving is expiry — provided step 5 confirms that the
//!    path itself still works.
//! 5. `CHECK` from the client (now allowed to speak), answered `CHECKED`.
//!
//! **The responder's own router must not forget its half while it waits.**
//! riverboat's router randomises ports, so if its record of the flow expired,
//! the `PROBE` would leave from a new port, and a router filtering by address
//! and port at the client's end would drop it: a false "expired". So during the
//! silence the responder sends `KEEP` with a small TTL (default 2). It passes
//! through the responder's own router, refreshing its record, and dies a hop
//! later, long before it could reach the client's router and refresh the
//! mapping under test. If one ever does arrive, the client reports the run
//! inconclusive rather than counting it — the assumption checks itself.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime};

use crate::{format_utc, json_string, random_u64, Family, Outcome};

const PREFIX: &str = "ME-INBOUND/1";
const FLOW_ROUNDS: u32 = 3;
const FLOW_EVERY: Duration = Duration::from_secs(1);
const REPLY_WAIT: Duration = Duration::from_secs(2);
const PROBES: u8 = 3;
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// How long past the silence the client keeps listening: the three probes
/// span two seconds, plus a generous allowance for a slow path.
const PROBE_GRACE: Duration = Duration::from_secs(6);
const KEEP_EVERY: Duration = Duration::from_secs(10);
/// Longer than the longest interval the client can draw, with room to spare.
const MAX_IDLE: u64 = 900;
const MAX_SESSIONS: usize = 32;
/// How long a responder remembers a session after its probes, so a `CHECK`
/// that follows a lost probe can still be answered.
const LINGER: Duration = Duration::from_secs(120);
/// The token travels in the environment rather than on the command line, so it
/// does not appear in `ps`.
pub const TOKEN_ENV: &str = "MESH_PROBE_INBOUND_TOKEN";

#[derive(Debug, PartialEq, Clone)]
pub enum Msg {
    Hello { nonce: u64, idle: u64, token: String },
    Seen { nonce: u64, addr: SocketAddr },
    Flow { nonce: u64, round: u32 },
    Keep { nonce: u64 },
    Probe { nonce: u64, copy: u8 },
    Check { nonce: u64 },
    Checked { nonce: u64, addr: SocketAddr },
    Done { nonce: u64 },
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let body = match self {
            Msg::Hello { nonce, idle, token } => format!("HELLO {nonce} {idle} {token}"),
            Msg::Seen { nonce, addr } => format!("SEEN {nonce} {addr}"),
            Msg::Flow { nonce, round } => format!("FLOW {nonce} {round}"),
            Msg::Keep { nonce } => format!("KEEP {nonce}"),
            Msg::Probe { nonce, copy } => format!("PROBE {nonce} {copy}"),
            Msg::Check { nonce } => format!("CHECK {nonce}"),
            Msg::Checked { nonce, addr } => format!("CHECKED {nonce} {addr}"),
            Msg::Done { nonce } => format!("DONE {nonce}"),
        };
        format!("{PREFIX} {body}").into_bytes()
    }

    pub fn parse(bytes: &[u8]) -> Option<Msg> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut words = text.split(' ');
        if words.next()? != PREFIX {
            return None;
        }
        let kind = words.next()?;
        let nonce: u64 = words.next()?.parse().ok()?;
        let msg = match kind {
            "HELLO" => Msg::Hello {
                nonce,
                idle: words.next()?.parse().ok()?,
                token: words.next()?.to_string(),
            },
            "SEEN" => Msg::Seen { nonce, addr: words.next()?.parse().ok()? },
            "FLOW" => Msg::Flow { nonce, round: words.next()?.parse().ok()? },
            "KEEP" => Msg::Keep { nonce },
            "PROBE" => Msg::Probe { nonce, copy: words.next()?.parse().ok()? },
            "CHECK" => Msg::Check { nonce },
            "CHECKED" => Msg::Checked { nonce, addr: words.next()?.parse().ok()? },
            "DONE" => Msg::Done { nonce },
            _ => return None,
        };
        // Trailing words mean a different protocol, not a sloppy sender.
        if words.next().is_some() {
            return None;
        }
        Some(msg)
    }
}

// ------------------------------------------------------------------ client

pub struct InboundTest {
    pub idle_seconds: u64,
    pub responder: SocketAddr,
    pub local_port: u16,
    /// The external address the responder saw at the start: the mapping
    /// under test.
    pub observed: Option<SocketAddr>,
    /// What the responder saw after the silence, once the client spoke again.
    pub recheck: Option<SocketAddr>,
    pub flow_echoes: u32,
    /// From the last packet the client heard to the probe's arrival.
    pub arrived_after: Option<Duration>,
    pub outcome: Outcome,
}

/// What the evidence proves. Kept apart from the sockets so every branch can
/// be tested without a network.
pub fn judge(
    observed: Option<SocketAddr>,
    flow_echoes: u32,
    keep_leaked: bool,
    arrived: bool,
    recheck: Option<SocketAddr>,
) -> Outcome {
    let Some(observed) = observed else {
        return Outcome::Inconclusive("the responder did not answer");
    };
    if flow_echoes == 0 {
        return Outcome::Inconclusive("no flow: the responder's replies stopped arriving");
    }
    if keep_leaked {
        // An inbound packet during the silence refreshes the very mapping
        // being timed, so neither answer could be trusted.
        return Outcome::Inconclusive(
            "a keepalive meant to stop at the responder's own router reached this machine",
        );
    }
    if arrived {
        return Outcome::Survived;
    }
    match recheck {
        None => Outcome::Inconclusive("the responder could not be reached after the silence"),
        Some(again) if again.ip() != observed.ip() => {
            Outcome::Inconclusive("public address changed mid-test")
        }
        Some(_) => Outcome::Expired,
    }
}

/// Sends `msg` and waits for the first reply that `accept` recognises,
/// retrying up to three times. Returns the reply and when it arrived.
fn ask<T>(
    socket: &UdpSocket,
    to: SocketAddr,
    msg: &Msg,
    accept: impl Fn(&Msg) -> Option<T>,
) -> Option<(T, Instant)> {
    for _ in 0..3 {
        socket.send_to(&msg.encode(), to).ok()?;
        let deadline = Instant::now() + REPLY_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let _ = socket.set_read_timeout(Some(left.max(Duration::from_millis(1))));
            let mut buf = [0u8; 512];
            match socket.recv_from(&mut buf) {
                Ok((len, from)) if from == to => {
                    if let Some(found) = Msg::parse(&buf[..len]).as_ref().and_then(&accept) {
                        return Some((found, Instant::now()));
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    None
}

pub fn run_client(responder: SocketAddr, token: &str, idle_seconds: u64, verbose: bool) -> InboundTest {
    let mut test = InboundTest {
        idle_seconds,
        responder,
        local_port: 0,
        observed: None,
        recheck: None,
        flow_echoes: 0,
        arrived_after: None,
        outcome: Outcome::Inconclusive("cannot open a socket"),
    };
    let Ok(socket) = UdpSocket::bind(Family::V4.wildcard()) else { return test };
    test.local_port = socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let nonce = random_u64();

    let hello = Msg::Hello { nonce, idle: idle_seconds, token: token.to_string() };
    let Some((observed, mut last_heard)) = ask(&socket, responder, &hello, |m| match m {
        Msg::Seen { nonce: n, addr } if *n == nonce => Some(*addr),
        _ => None,
    }) else {
        test.outcome = judge(None, 0, false, false, None);
        return test;
    };
    test.observed = Some(observed);
    if verbose {
        println!("{responder} sees this machine as {observed}.");
    }

    for round in 1..=FLOW_ROUNDS {
        std::thread::sleep(FLOW_EVERY);
        let flow = Msg::Flow { nonce, round };
        // One try each: a lost round is not worth a retry that would bunch
        // the flow up, and three rounds tolerate a loss.
        let _ = socket.send_to(&flow.encode(), responder);
        let deadline = Instant::now() + REPLY_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let _ = socket.set_read_timeout(Some(left.max(Duration::from_millis(1))));
            let mut buf = [0u8; 512];
            match socket.recv_from(&mut buf) {
                Ok((len, from)) if from == responder => {
                    if Msg::parse(&buf[..len]) == Some(Msg::Flow { nonce, round }) {
                        test.flow_echoes += 1;
                        last_heard = Instant::now();
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    if test.flow_echoes == 0 {
        test.outcome = judge(Some(observed), 0, false, false, None);
        return test;
    }

    if verbose {
        println!("Flow established ({}/{FLOW_ROUNDS} echoes). Silent for {idle_seconds} s…", test.flow_echoes);
    }

    // The silence. Nothing is sent from here until the answer is known.
    let mut keep_leaked = false;
    let deadline = last_heard + Duration::from_secs(idle_seconds) + PROBE_GRACE;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let _ = socket.set_read_timeout(Some(left.max(Duration::from_millis(1))));
        let mut buf = [0u8; 512];
        match socket.recv_from(&mut buf) {
            Ok((len, from)) if from.ip() == responder.ip() => match Msg::parse(&buf[..len]) {
                Some(Msg::Probe { nonce: n, .. }) if n == nonce => {
                    test.arrived_after = Some(last_heard.elapsed());
                    break;
                }
                Some(Msg::Keep { nonce: n }) if n == nonce => keep_leaked = true,
                _ => {}
            },
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }

    if test.arrived_after.is_some() {
        // Courtesy only: lets the responder forget the session early.
        let _ = socket.send_to(&Msg::Done { nonce }.encode(), responder);
    } else {
        test.recheck = ask(&socket, responder, &Msg::Check { nonce }, |m| match m {
            Msg::Checked { nonce: n, addr } if *n == nonce => Some(*addr),
            _ => None,
        })
        .map(|(addr, _)| addr);
    }

    test.outcome = judge(
        test.observed,
        test.flow_echoes,
        keep_leaked,
        test.arrived_after.is_some(),
        test.recheck,
    );
    test
}

pub fn report(test: &InboundTest) {
    println!();
    match &test.outcome {
        Outcome::Survived => println!(
            "After {} s of silence a packet from {} still got in: the router kept the mapping.",
            test.idle_seconds, test.responder
        ),
        Outcome::Expired => println!(
            "After {} s of silence nothing from {} got in, though it was reachable again at once:\n\
             the router had forgotten the mapping. A peer-to-peer connection idle that long dies.",
            test.idle_seconds, test.responder
        ),
        Outcome::Inconclusive(why) => println!("Inconclusive after {} s: {}.", test.idle_seconds, why),
    }
    println!();
    println!(
        "PROBE-INBOUND v0.1 | idle={}s | outcome={}",
        test.idle_seconds,
        test.outcome.tag()
    );
}

pub fn json(label: &str, test: &InboundTest) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let addr = |a: Option<SocketAddr>| match a {
        Some(a) => json_string(&a.to_string()),
        None => "null".to_string(),
    };
    let note = match &test.outcome {
        Outcome::Inconclusive(why) => json_string(why),
        _ => "null".to_string(),
    };
    format!(
        "{{\"schema\":1,\"ts_unix\":{},\"ts_utc\":{},\"label\":{},\"test\":\"mapping-inbound\",\
         \"idle_seconds\":{},\"responder\":{},\"local_port\":{},\"observed\":{},\"recheck\":{},\
         \"flow_echoes\":{},\"arrived_after_ms\":{},\"outcome\":{},\"note\":{}}}",
        now,
        json_string(&format_utc(now)),
        json_string(label),
        test.idle_seconds,
        json_string(&test.responder.to_string()),
        test.local_port,
        addr(test.observed),
        addr(test.recheck),
        test.flow_echoes,
        test.arrived_after
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_string(test.outcome.tag()),
        note
    )
}

// --------------------------------------------------------------- responder

struct Session {
    peer: SocketAddr,
    idle: Duration,
    /// When this responder last sent the client anything: the silence is
    /// timed from here, because it is the last packet the client heard.
    last_sent: Instant,
    last_keep: Instant,
    probes: u8,
    last_probe: Option<Instant>,
}

/// What a responder should send now for one session, if anything.
#[derive(Debug, PartialEq)]
pub enum Due {
    Nothing,
    Keep,
    Probe(u8),
    Forget,
}

/// The responder's schedule, apart from its socket so it can be tested.
pub fn due(
    now: Instant,
    last_sent: Instant,
    idle: Duration,
    probes: u8,
    last_probe: Option<Instant>,
    last_keep: Instant,
    keep_ttl: u32,
) -> Due {
    let probe_at = last_sent + idle;
    if probes >= PROBES {
        return if now >= probe_at + LINGER { Due::Forget } else { Due::Nothing };
    }
    if now >= probe_at {
        return match last_probe {
            Some(at) if now < at + PROBE_EVERY => Due::Nothing,
            _ => Due::Probe(probes + 1),
        };
    }
    // Keepalives stop a second short of the probe, so the last thing to pass
    // the responder's router before the probe is not crowding it.
    if keep_ttl > 0 && now >= last_keep + KEEP_EVERY && now + Duration::from_secs(1) < probe_at {
        return Due::Keep;
    }
    Due::Nothing
}

pub fn run_responder(port: u16, token: &str, keep_ttl: u32) -> io::Result<()> {
    let socket = UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], port)))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let normal_ttl = socket.ttl()?;
    let mut sessions: HashMap<u64, Session> = HashMap::new();
    println!("inbound responder on udp/{port}, keepalive ttl {keep_ttl}");

    loop {
        let mut buf = [0u8; 512];
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                let now = Instant::now();
                match Msg::parse(&buf[..len]) {
                    Some(Msg::Hello { nonce, idle, token: offered })
                        if offered == token && (1..=MAX_IDLE).contains(&idle) =>
                    {
                        // One session per source address: a client that
                        // retries starts afresh, and nobody can queue up
                        // probes at a victim by asking many times.
                        sessions.retain(|n, s| *n == nonce || s.peer.ip() != from.ip());
                        if !sessions.contains_key(&nonce) && sessions.len() >= MAX_SESSIONS {
                            continue;
                        }
                        let fresh = !sessions.contains_key(&nonce);
                        let session = sessions.entry(nonce).or_insert(Session {
                            peer: from,
                            idle: Duration::from_secs(idle),
                            last_sent: now,
                            last_keep: now,
                            probes: 0,
                            last_probe: None,
                        });
                        session.peer = from;
                        session.last_sent = now;
                        session.last_keep = now;
                        let _ = socket.send_to(&Msg::Seen { nonce, addr: from }.encode(), from);
                        if fresh {
                            println!("{from}: session {nonce}, silence {idle} s");
                        }
                    }
                    Some(Msg::Flow { nonce, round }) => {
                        if let Some(s) = sessions.get_mut(&nonce).filter(|s| s.peer == from && s.probes == 0) {
                            s.last_sent = now;
                            s.last_keep = now;
                            let _ = socket.send_to(&Msg::Flow { nonce, round }.encode(), from);
                        }
                    }
                    Some(Msg::Check { nonce }) => {
                        // Answered to whoever asks, from wherever: after an
                        // expiry the client's address may have changed, and
                        // that change is part of the evidence.
                        if sessions.contains_key(&nonce) {
                            let _ = socket.send_to(&Msg::Checked { nonce, addr: from }.encode(), from);
                            println!("{from}: session {nonce} checked (no probe got through)");
                            sessions.remove(&nonce);
                        }
                    }
                    Some(Msg::Done { nonce }) => {
                        if sessions.get(&nonce).is_some_and(|s| s.peer.ip() == from.ip()) {
                            println!("{from}: session {nonce} done (a probe got through)");
                            sessions.remove(&nonce);
                        }
                    }
                    _ => {}
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }

        let now = Instant::now();
        let mut forget = Vec::new();
        for (nonce, s) in sessions.iter_mut() {
            match due(now, s.last_sent, s.idle, s.probes, s.last_probe, s.last_keep, keep_ttl) {
                Due::Nothing => {}
                Due::Keep => {
                    socket.set_ttl(keep_ttl)?;
                    let _ = socket.send_to(&Msg::Keep { nonce: *nonce }.encode(), s.peer);
                    socket.set_ttl(normal_ttl)?;
                    s.last_keep = now;
                }
                Due::Probe(copy) => {
                    let _ = socket.send_to(&Msg::Probe { nonce: *nonce, copy }.encode(), s.peer);
                    s.probes = copy;
                    s.last_probe = Some(now);
                }
                Due::Forget => forget.push(*nonce),
            }
        }
        for nonce in forget {
            sessions.remove(&nonce);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn every_message_survives_a_round_trip() {
        let all = [
            Msg::Hello { nonce: 7, idle: 120, token: "s3cret".into() },
            Msg::Seen { nonce: 7, addr: addr("198.51.100.4:41999") },
            Msg::Flow { nonce: 7, round: 2 },
            Msg::Keep { nonce: 7 },
            Msg::Probe { nonce: 7, copy: 3 },
            Msg::Check { nonce: 7 },
            Msg::Checked { nonce: 7, addr: addr("198.51.100.4:50000") },
            Msg::Done { nonce: 7 },
        ];
        for msg in all {
            assert_eq!(Msg::parse(&msg.encode()), Some(msg));
        }
    }

    #[test]
    fn foreign_and_malformed_packets_are_not_messages() {
        assert_eq!(Msg::parse(b"MACHINE-ELVES-PUNCH"), None);
        assert_eq!(Msg::parse(b"ME-INBOUND/1 PROBE notanumber 1"), None);
        assert_eq!(Msg::parse(b"ME-INBOUND/1 KEEP 7 extra"), None);
        assert_eq!(Msg::parse(b"ME-INBOUND/2 KEEP 7"), None);
        assert_eq!(Msg::parse(&[0xff, 0xfe]), None);
    }

    #[test]
    fn a_probe_that_got_in_is_survival() {
        let seen = Some(addr("198.51.100.4:41999"));
        assert_eq!(judge(seen, 3, false, true, None), Outcome::Survived);
    }

    #[test]
    fn nothing_in_but_reachable_again_is_expiry() {
        let seen = Some(addr("198.51.100.4:41999"));
        // Same port again on a preserving router: still expiry, because
        // nothing was sent from this end until after the probe failed.
        assert_eq!(judge(seen, 3, false, false, seen), Outcome::Expired);
    }

    #[test]
    fn nothing_in_and_unreachable_proves_nothing() {
        let seen = Some(addr("198.51.100.4:41999"));
        assert!(matches!(judge(seen, 3, false, false, None), Outcome::Inconclusive(_)));
    }

    #[test]
    fn a_leaked_keepalive_voids_even_a_survival() {
        let seen = Some(addr("198.51.100.4:41999"));
        assert!(matches!(judge(seen, 3, true, true, None), Outcome::Inconclusive(_)));
    }

    #[test]
    fn no_flow_or_no_answer_proves_nothing() {
        assert!(matches!(judge(None, 0, false, false, None), Outcome::Inconclusive(_)));
        let seen = Some(addr("198.51.100.4:41999"));
        assert!(matches!(judge(seen, 0, false, true, None), Outcome::Inconclusive(_)));
    }

    #[test]
    fn a_moved_public_address_proves_nothing() {
        let seen = Some(addr("198.51.100.4:41999"));
        let moved = Some(addr("198.51.100.9:41999"));
        assert!(matches!(judge(seen, 3, false, false, moved), Outcome::Inconclusive(_)));
    }

    #[test]
    fn the_responder_keeps_its_own_router_awake_then_probes_three_times() {
        let t0 = Instant::now();
        let idle = Duration::from_secs(60);
        let s = |n| Duration::from_secs(n);
        // Quiet until the first keepalive is due.
        assert_eq!(due(t0 + s(5), t0, idle, 0, None, t0, 2), Due::Nothing);
        assert_eq!(due(t0 + s(10), t0, idle, 0, None, t0, 2), Due::Keep);
        // No keepalive in the last second before the probe.
        assert_eq!(due(t0 + s(59), t0, idle, 0, None, t0 + s(50), 2), Due::Nothing);
        // Probes at the silence's end, a second apart, then nothing.
        assert_eq!(due(t0 + s(60), t0, idle, 0, None, t0 + s(50), 2), Due::Probe(1));
        assert_eq!(due(t0 + s(60), t0, idle, 1, Some(t0 + s(60)), t0, 2), Due::Nothing);
        assert_eq!(due(t0 + s(61), t0, idle, 1, Some(t0 + s(60)), t0, 2), Due::Probe(2));
        assert_eq!(due(t0 + s(62), t0, idle, 2, Some(t0 + s(61)), t0, 2), Due::Probe(3));
        assert_eq!(due(t0 + s(70), t0, idle, 3, Some(t0 + s(62)), t0, 2), Due::Nothing);
        assert_eq!(due(t0 + s(60) + LINGER, t0, idle, 3, Some(t0 + s(62)), t0, 2), Due::Forget);
    }

    #[test]
    fn keepalives_can_be_switched_off() {
        let t0 = Instant::now();
        assert_eq!(
            due(t0 + Duration::from_secs(30), t0, Duration::from_secs(60), 0, None, t0, 0),
            Due::Nothing
        );
    }

    /// The whole exchange over loopback, where nothing expires: the probe
    /// must arrive, and arrive no earlier than the silence it was asked for.
    #[test]
    fn a_loopback_run_survives_after_the_full_silence() {
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        std::thread::spawn(move || run_responder(port, "t", 0));
        std::thread::sleep(Duration::from_millis(200));

        let started = Instant::now();
        let test = run_client(SocketAddr::from(([127, 0, 0, 1], port)), "t", 2, false);
        assert_eq!(test.outcome, Outcome::Survived, "{:?}", test.outcome.tag());
        assert_eq!(test.flow_echoes, FLOW_ROUNDS);
        let after = test.arrived_after.expect("arrived");
        // Timed from the last echo the client heard; the responder times from
        // when it sent it, so a few milliseconds either way is the same moment.
        assert!(after >= Duration::from_millis(1900), "arrived after {after:?}, before the silence ended");
        assert!(started.elapsed() < Duration::from_secs(12));
    }

    #[test]
    fn a_wrong_token_gets_no_answer() {
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        std::thread::spawn(move || run_responder(port, "right", 0));
        std::thread::sleep(Duration::from_millis(200));
        let test = run_client(SocketAddr::from(([127, 0, 0, 1], port)), "wrong", 2, false);
        assert!(test.observed.is_none());
        assert!(matches!(test.outcome, Outcome::Inconclusive(_)));
    }
}
