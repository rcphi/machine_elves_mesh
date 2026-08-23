//! Machine Elves — Phase 0 connection probe.
//!
//! Reports what one home internet connection can and cannot do, so that
//! `docs/phase-0-plan.md`'s go/no-go can be answered before any mesh code
//! is written. Sends nothing anywhere except STUN binding requests to
//! public servers, and prints the result locally for the operator to share.
//!
//! Deliberately depends on nothing outside the standard library: volunteers
//! run this on whatever machine they own, and a single static binary with no
//! runtime is the whole point.

use std::collections::hash_map::RandomState;
use std::env;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::thread;
use std::time::{Duration, SystemTime};

const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const TIMEOUT: Duration = Duration::from_secs(3);
const ATTEMPTS: usize = 3;

/// The idle intervals, in seconds, that the mapping-lifetime test chooses from.
///
/// Each run tests exactly one of these — this run stays silent for 90 seconds,
/// the next perhaps for 240 — and the answer accumulates over days as every
/// interval gets tried many times.
///
/// The alternative would be to narrow in on the answer within a single run, by
/// trying a long interval, then a shorter one, and so on. That is faster in
/// principle and worse here: it takes many minutes, and a failure partway
/// through wastes the whole attempt. One interval per run means a failed run
/// costs one data point.
const IDLE_INTERVALS: &[u64] = &[15, 30, 45, 60, 90, 120, 180, 240, 300, 420, 600];

/// STUN servers on deliberately distinct operators. Mapping behaviour can only
/// be classified by comparing what two *different* server IPs observe, so this
/// list must never collapse to one provider.
const STUN_SERVERS: &[&str] = &[
    "stun.l.google.com:19302",
    "stun.cloudflare.com:3478",
    "stun.nextcloud.com:443",
    "stun.sipgate.net:3478",
];

fn main() {
    let mut json = false;
    let mut label = String::from("unlabelled");
    let mut mapping_lifetime = false;
    let mut idle: Option<u64> = None;
    let mut punch_mode = false;
    let mut punch_peer: Option<SocketAddr> = None;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--label" => label = args.next().unwrap_or_else(|| "unlabelled".to_string()),
            "--mapping-lifetime" => mapping_lifetime = true,
            "--punch" => punch_mode = true,
            "--peer" => {
                punch_peer = args.next().and_then(|v| v.parse().ok());
                if punch_peer.is_none() {
                    eprintln!("--peer needs an address:port");
                    std::process::exit(2);
                }
            }
            "--idle" => {
                idle = args.next().and_then(|v| v.parse().ok());
                if idle.is_none() {
                    eprintln!("--idle needs a number of seconds");
                    std::process::exit(2);
                }
            }
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                eprintln!("unknown argument: {}", other);
                print_usage();
                std::process::exit(2);
            }
        }
    }

    if punch_mode {
        if let Err(error) = punch(punch_peer, idle.unwrap_or(60)) {
            eprintln!("punch failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    if mapping_lifetime {
        let seconds = idle.unwrap_or_else(pick_idle_interval);
        let test = probe_mapping_lifetime(seconds, !json);
        if json {
            println!("{}", mapping_json(&label, &test));
        } else {
            report_mapping(&test);
        }
        return;
    }

    if !json {
        println!("Machine Elves — connection probe v0.1");
        println!("=====================================");
        println!();
        println!("Measuring what this connection can do. Takes under a minute.");
        println!("Nothing is uploaded; the report is printed here for you to send back.");
        println!();
    }

    let v4 = probe_family(Family::V4, !json);
    let v6 = probe_family(Family::V6, !json);

    if json {
        println!("{}", report_json(&label, &v4, &v6));
    } else {
        report(&v4, &v6);
    }
}

fn print_usage() {
    println!("mesh-probe — reports what this internet connection can and cannot do");
    println!();
    println!("  --json           emit one machine-readable record instead of prose");
    println!("  --label <name>   identify this machine in the record");
    println!("  --mapping-lifetime");
    println!("                   measure whether this router still remembers an idle");
    println!("                   connection after a while. Takes as long as it waits.");
    println!("  --idle <seconds> force one idle interval instead of choosing at random;");
    println!("                   with --punch, how long to keep trying (default 60)");
    println!("  --punch          try to reach another machine directly, with nothing in");
    println!("                   the middle. Run it on both machines at once.");
    println!("  --peer <addr>    the other machine's address, if you already have it");
    println!("  --help           show this");
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Family {
    V4,
    V6,
}

impl Family {
    fn label(self) -> &'static str {
        match self {
            Family::V4 => "IPv4",
            Family::V6 => "IPv6",
        }
    }

    fn wildcard(self) -> SocketAddr {
        match self {
            Family::V4 => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            Family::V6 => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        }
    }

    fn matches(self, addr: &SocketAddr) -> bool {
        matches!(
            (self, addr),
            (Family::V4, SocketAddr::V4(_)) | (Family::V6, SocketAddr::V6(_))
        )
    }
}

struct Observation {
    server: String,
    server_ip: IpAddr,
    mapped: SocketAddr,
}

struct FamilyResult {
    family: Family,
    local: Option<IpAddr>,
    observations: Vec<Observation>,
    errors: Vec<String>,
}

/// Runs every STUN query for one address family through a *single* socket.
///
/// This is not incidental. Mapping behaviour is defined as whether the same
/// local port is translated to the same external port when talking to
/// different destinations, so reusing one socket is what makes the comparison
/// meaningful. A fresh socket per server would measure nothing.
fn probe_family(family: Family, verbose: bool) -> FamilyResult {
    let mut result = FamilyResult {
        family,
        local: None,
        observations: Vec::new(),
        errors: Vec::new(),
    };

    let socket = match UdpSocket::bind(family.wildcard()) {
        Ok(s) => s,
        Err(e) => {
            result
                .errors
                .push(format!("cannot open a {} socket: {}", family.label(), e));
            return result;
        }
    };
    let _ = socket.set_read_timeout(Some(TIMEOUT));

    if verbose {
        println!("Checking {}…", family.label());
    }

    for server in STUN_SERVERS {
        let addr = match resolve(server, family) {
            Some(a) => a,
            None => continue,
        };

        if result.local.is_none() {
            result.local = outbound_address(family, addr);
        }

        match stun_binding(&socket, addr) {
            Ok(mapped) => {
                if verbose {
                    println!("  {:<28} sees us as {}", server, mapped);
                }
                result.observations.push(Observation {
                    server: (*server).to_string(),
                    server_ip: addr.ip(),
                    mapped,
                });
            }
            Err(e) => {
                let why = match e.kind() {
                    ErrorKind::WouldBlock | ErrorKind::TimedOut => "no reply".to_string(),
                    _ => e.to_string(),
                };
                if verbose {
                    println!("  {:<28} {}", server, why);
                }
                result.errors.push(format!("{}: {}", server, why));
            }
        }
    }
    if verbose {
        println!();
    }
    result
}

fn resolve(server: &str, family: Family) -> Option<SocketAddr> {
    server
        .to_socket_addrs()
        .ok()?
        .find(|addr| family.matches(addr))
}

/// The address this machine would use to reach the outside world.
///
/// Connecting a UDP socket sends no packets — it only fixes the route — so
/// this reads the kernel's own choice of source address without touching the
/// network. Enumerating interfaces directly would need a platform-specific
/// dependency, which the no-dependencies rule rules out.
fn outbound_address(family: Family, via: SocketAddr) -> Option<IpAddr> {
    let socket = UdpSocket::bind(family.wildcard()).ok()?;
    socket.connect(via).ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

fn stun_binding(socket: &UdpSocket, server: SocketAddr) -> io::Result<SocketAddr> {
    let mut last = io::Error::new(ErrorKind::TimedOut, "no reply");

    for _ in 0..ATTEMPTS {
        let transaction = random_transaction_id();
        let request = build_binding_request(&transaction);
        socket.send_to(&request, server)?;

        let mut buf = [0u8; 1500];
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                // A late reply to an earlier query can arrive on this shared
                // socket; the transaction ID is what makes them distinguishable.
                if from.ip() != server.ip() {
                    continue;
                }
                match parse_binding_response(&buf[..len], &transaction) {
                    Some(mapped) => return Ok(mapped),
                    None => {
                        last = io::Error::new(ErrorKind::InvalidData, "unrecognised reply");
                    }
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn build_binding_request(transaction: &[u8; 12]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(20);
    msg.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes()); // no attributes
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(transaction);
    msg
}

fn parse_binding_response(buf: &[u8], transaction: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 {
        return None;
    }
    if u16::from_be_bytes([buf[0], buf[1]]) != BINDING_SUCCESS {
        return None;
    }
    if u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != MAGIC_COOKIE {
        return None;
    }
    if &buf[8..20] != transaction {
        return None;
    }

    let declared = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let end = (20 + declared).min(buf.len());
    let mut cursor = 20;
    let mut fallback = None;

    while cursor + 4 <= end {
        let attr_type = u16::from_be_bytes([buf[cursor], buf[cursor + 1]]);
        let attr_len = u16::from_be_bytes([buf[cursor + 2], buf[cursor + 3]]) as usize;
        let value_start = cursor + 4;
        let value_end = value_start + attr_len;
        if value_end > end {
            break;
        }
        let value = &buf[value_start..value_end];

        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS => {
                if let Some(addr) = decode_address(value, true, transaction) {
                    return Some(addr);
                }
            }
            ATTR_MAPPED_ADDRESS => {
                // Obsolete but still emitted by some servers. Kept only as a
                // fallback so an older server does not read as a failure.
                if fallback.is_none() {
                    fallback = decode_address(value, false, transaction);
                }
            }
            _ => {}
        }

        // Attribute values are padded to a four-byte boundary.
        cursor = value_end + ((4 - (attr_len % 4)) % 4);
    }
    fallback
}

fn decode_address(value: &[u8], xored: bool, transaction: &[u8; 12]) -> Option<SocketAddr> {
    if value.len() < 4 {
        return None;
    }
    let family = value[1];
    let mut port = u16::from_be_bytes([value[2], value[3]]);
    if xored {
        port ^= (MAGIC_COOKIE >> 16) as u16;
    }

    match family {
        0x01 => {
            let raw = value.get(4..8)?;
            let mut octets = [0u8; 4];
            octets.copy_from_slice(raw);
            if xored {
                for (i, byte) in MAGIC_COOKIE.to_be_bytes().iter().enumerate() {
                    octets[i] ^= byte;
                }
            }
            Some(SocketAddr::from((Ipv4Addr::from(octets), port)))
        }
        0x02 => {
            let raw = value.get(4..20)?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(raw);
            if xored {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
                key[4..].copy_from_slice(transaction);
                for i in 0..16 {
                    octets[i] ^= key[i];
                }
            }
            Some(SocketAddr::from((Ipv6Addr::from(octets), port)))
        }
        _ => None,
    }
}

/// Transaction IDs only need to be unpredictable enough to match a reply to
/// its request. `RandomState` is seeded randomly per process, which is
/// sufficient here and avoids a dependency for a diagnostic tool.
fn random_transaction_id() -> [u8; 12] {
    let mut id = [0u8; 12];
    let state = RandomState::new();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    for (chunk, salt) in id.chunks_mut(4).zip(0u64..) {
        let mut hasher = state.build_hasher();
        hasher.write_u64(nanos);
        hasher.write_u64(salt);
        let bytes = hasher.finish().to_be_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    id
}

// ---------------------------------------------------------------- reporting

fn report(v4: &FamilyResult, v6: &FamilyResult) {
    let v4_verdict = describe_v4(v4);
    let v6_verdict = describe_v6(v6);

    println!("What this means");
    println!("---------------");
    for line in &v4_verdict.notes {
        println!("  {}", line);
    }
    for line in &v6_verdict.notes {
        println!("  {}", line);
    }

    println!();
    println!("Please send back the line below.");
    println!();
    println!(
        "PROBE v0.1 | ipv4={} | ipv6={} | relay-candidate={}",
        v4_verdict.tag,
        v6_verdict.tag,
        if v4_verdict.reachable || v6_verdict.reachable {
            "yes"
        } else {
            "no"
        }
    );
}

struct Verdict {
    tag: String,
    notes: Vec<String>,
    reachable: bool,
}

fn describe_v4(result: &FamilyResult) -> Verdict {
    debug_assert!(result.family == Family::V4);
    let mut notes = Vec::new();

    if result.observations.is_empty() {
        notes.push(
            "IPv4: no STUN server could be reached. Either this connection blocks outbound UDP, \
             or it has no IPv4 route at all."
                .to_string(),
        );
        return Verdict {
            tag: "unreachable".to_string(),
            notes,
            reachable: false,
        };
    }

    let mapped = result.observations[0].mapped;
    let local = result.local;

    // Distinct observers are what make the comparison meaningful.
    let mut distinct_servers: Vec<IpAddr> = result.observations.iter().map(|o| o.server_ip).collect();
    distinct_servers.sort();
    distinct_servers.dedup();

    let ports: Vec<u16> = result.observations.iter().map(|o| o.mapped.port()).collect();
    let consistent = ports.windows(2).all(|w| w[0] == w[1]);

    let no_nat = local == Some(mapped.ip());
    let cgnat = matches!(mapped.ip(), IpAddr::V4(ip) if is_shared_address_space(ip));
    let double_nat = matches!(mapped.ip(), IpAddr::V4(ip) if ip.is_private());

    if let Some(local_ip) = local {
        notes.push(format!("IPv4: this machine's own address is {}", local_ip));
    }
    notes.push(format!("IPv4: the internet sees this machine as {}", mapped));

    let tag;
    let reachable;

    if no_nat {
        tag = "public".to_string();
        reachable = true;
        notes.push(
            "IPv4: this connection has a public address with no translation in the way, so it \
             is a candidate for carrying relay duty. Whether a firewall permits incoming \
             connections is a separate question this probe cannot answer alone."
                .to_string(),
        );
    } else if cgnat {
        tag = "cgnat".to_string();
        reachable = false;
        notes.push(
            "IPv4: CARRIER-GRADE NAT confirmed — the address the internet sees is itself inside \
             the ISP's shared range. Incoming connections are impossible on IPv4, and direct \
             connections to other such machines will usually fail."
                .to_string(),
        );
    } else if double_nat {
        tag = "double-nat".to_string();
        reachable = false;
        notes.push(
            "IPv4: the address the internet sees is itself private, meaning at least two layers \
             of translation. Treat this the same as carrier-grade NAT."
                .to_string(),
        );
    } else if distinct_servers.len() < 2 {
        tag = "nat-unclassified".to_string();
        reachable = false;
        notes.push(
            "IPv4: behind translation, but only one STUN operator answered, so the mapping \
             behaviour could not be classified. Re-run when the network is quieter."
                .to_string(),
        );
    } else if consistent {
        tag = "nat-endpoint-independent".to_string();
        reachable = false;
        notes.push(
            "IPv4: behind translation, but the same external port is used no matter who is being \
             contacted. This is the good case — hole punching should work against other peers."
                .to_string(),
        );
    } else {
        tag = "nat-address-dependent".to_string();
        reachable = false;
        let seen: Vec<String> = result
            .observations
            .iter()
            .map(|o| format!("{} -> :{}", o.server, o.mapped.port()))
            .collect();
        notes.push(format!(
            "IPv4: behind translation that assigns a different external port per destination \
             ({}). This is the hard case — hole punching is unreliable and a relay will usually \
             be needed.",
            seen.join(", ")
        ));
    }

    Verdict {
        tag,
        notes,
        reachable,
    }
}

fn describe_v6(result: &FamilyResult) -> Verdict {
    debug_assert!(result.family == Family::V6);
    let mut notes = Vec::new();

    let global = result
        .observations
        .iter()
        .find(|o| matches!(o.mapped.ip(), IpAddr::V6(ip) if is_global_unicast_v6(ip)));

    match global {
        Some(observation) => {
            notes.push(format!(
                "IPv6: working, with the global address {}",
                observation.mapped.ip()
            ));
            notes.push(
                "IPv6: this is the outcome that makes the whole NAT question moot. Two peers \
                 that both have working IPv6 can address each other directly, with no traversal \
                 and no relay."
                    .to_string(),
            );
            notes.push(
                "IPv6: CANDIDATE, NOT CONFIRMED. Holding a global address means packets could \
                 reach this machine; whether they are allowed to is a separate question this \
                 probe cannot answer alone. Carrier and device firewalls routinely block \
                 incoming IPv6. Confirming it needs a cooperating peer that also has IPv6."
                    .to_string(),
            );
            Verdict {
                tag: "global".to_string(),
                notes,
                reachable: true,
            }
        }
        None => {
            notes.push(
                "IPv6: not available on this connection. Every peer connection must therefore \
                 survive IPv4 translation."
                    .to_string(),
            );
            Verdict {
                tag: "none".to_string(),
                notes,
                reachable: false,
            }
        }
    }
}

/// RFC 6598 shared address space — the range ISPs use for carrier-grade NAT.
/// Seeing it as one's *public* address is unambiguous proof of it.
fn is_shared_address_space(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

fn is_global_unicast_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    // 2000::/3, excluding link-local fe80::/10 and unique-local fc00::/7.
    (first & 0xE000) == 0x2000
}

// -------------------------------------------------------- mapping lifetime

/// What became of a mapping after a period of silence.
#[derive(PartialEq, Debug)]
enum Outcome {
    /// The router still remembered. The mesh can idle this long safely.
    Survived,
    /// The router forgot. A connection idle this long would have died silently.
    Expired,
    /// Something else changed underneath the test, so it proves nothing.
    Inconclusive(&'static str),
}

impl Outcome {
    fn tag(&self) -> &'static str {
        match self {
            Outcome::Survived => "survived",
            Outcome::Expired => "expired",
            Outcome::Inconclusive(_) => "inconclusive",
        }
    }
}

struct MappingTest {
    idle_seconds: u64,
    server: String,
    first: Option<SocketAddr>,
    second: Option<SocketAddr>,
    outcome: Outcome,
}

/// Measures whether a home router still remembers an idle connection.
///
/// A router rewrites outgoing packets to its own address and remembers the
/// pairing so replies can be delivered. That memory expires, and when it does a
/// peer-to-peer connection dies silently — packets simply stop arriving. The
/// mesh must therefore send periodic keepalives, and this measures how often.
///
/// The method: learn the external port, say nothing at all for `idle_seconds`,
/// then ask the same server again through the same socket. An unchanged port
/// means the mapping survived.
fn probe_mapping_lifetime(idle_seconds: u64, verbose: bool) -> MappingTest {
    let unusable = |why: &'static str, server: String| MappingTest {
        idle_seconds,
        server,
        first: None,
        second: None,
        outcome: Outcome::Inconclusive(why),
    };

    let socket = match UdpSocket::bind(Family::V4.wildcard()) {
        Ok(s) => s,
        Err(_) => return unusable("cannot open a socket", "-".to_string()),
    };
    let _ = socket.set_read_timeout(Some(TIMEOUT));

    // Both observations must come from the same server. Under
    // address-dependent mapping a different server sees a different port
    // anyway, so comparing across servers would report expiry every time.
    let mut chosen = None;
    for candidate in STUN_SERVERS {
        if let Some(addr) = resolve(candidate, Family::V4) {
            if let Ok(mapped) = stun_binding(&socket, addr) {
                chosen = Some((candidate.to_string(), addr, mapped));
                break;
            }
        }
    }

    let (server, addr, first) = match chosen {
        Some(found) => found,
        None => return unusable("no STUN server answered", "-".to_string()),
    };

    if verbose {
        println!("Seen as {} via {}.", first, server);
        println!("Staying silent for {} seconds…", idle_seconds);
    }

    thread::sleep(Duration::from_secs(idle_seconds));

    let second = match stun_binding(&socket, addr) {
        Ok(mapped) => mapped,
        Err(_) => {
            let mut test = unusable("no reply to the second query", server);
            test.first = Some(first);
            return test;
        }
    };

    // A changed public address means the ISP moved us, which would look
    // identical to an expired mapping while proving nothing about the router.
    let outcome = if first.ip() != second.ip() {
        Outcome::Inconclusive("public address changed mid-test")
    } else if first.port() == second.port() {
        Outcome::Survived
    } else {
        Outcome::Expired
    };

    MappingTest {
        idle_seconds,
        server,
        first: Some(first),
        second: Some(second),
        outcome,
    }
}

/// Chooses one interval at random.
///
/// Stateless deliberately: the service runs under a read-only filesystem, and
/// coverage comes from repetition across days rather than from bookkeeping.
fn pick_idle_interval() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );
    IDLE_INTERVALS[(hasher.finish() % IDLE_INTERVALS.len() as u64) as usize]
}

fn report_mapping(test: &MappingTest) {
    println!();
    match &test.outcome {
        Outcome::Survived => println!(
            "After {}s of silence the router still remembered this connection.",
            test.idle_seconds
        ),
        Outcome::Expired => println!(
            "After {}s of silence the router had forgotten this connection.\n\
             A peer-to-peer connection left idle that long would have died without warning.",
            test.idle_seconds
        ),
        Outcome::Inconclusive(why) => {
            println!("Inconclusive after {}s: {}.", test.idle_seconds, why)
        }
    }
    if let (Some(first), Some(second)) = (test.first, test.second) {
        println!("  before: {}\n  after:  {}", first, second);
    }
    println!();
    println!(
        "PROBE-MAPPING v0.1 | idle={}s | outcome={}",
        test.idle_seconds,
        test.outcome.tag()
    );
}

fn mapping_json(label: &str, test: &MappingTest) -> String {
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
        "{{\"schema\":1,\"ts_unix\":{},\"ts_utc\":{},\"label\":{},\"test\":\"mapping-lifetime\",\
         \"idle_seconds\":{},\"server\":{},\"first\":{},\"second\":{},\"outcome\":{},\"note\":{}}}",
        now,
        json_string(&format_utc(now)),
        json_string(label),
        test.idle_seconds,
        json_string(&test.server),
        addr(test.first),
        addr(test.second),
        json_string(test.outcome.tag()),
        note
    )
}

// ----------------------------------------------------------- hole punching

/// How often each side sends while trying to meet.
const PUNCH_EVERY: Duration = Duration::from_millis(250);

/// How often the mapping is refreshed while waiting for the other address.
///
/// Comfortably under the shortest measured idle timeout, so that a mapping
/// learned now is still the mapping in use when the peer's address finally
/// arrives — which may be minutes later, since a person is copying it by hand.
const REFRESH_EVERY: Duration = Duration::from_secs(20);

const PUNCH_HELLO: &[u8] = b"MACHINE-ELVES-PUNCH";
const PUNCH_REPLY: &[u8] = b"MACHINE-ELVES-PONG";

/// Tries to reach another machine directly, with no server in the middle.
///
/// Both sides sit behind address translation, so neither can be dialled. The
/// only thing that opens a path is both of them sending outward at once: each
/// outbound packet creates a mapping, and with luck the other side's packet
/// arrives while that mapping is open.
///
/// **What this actually measures is filtering, not mapping.** The connectivity
/// probe reports which external port a router assigns; this reports whether the
/// router will admit a packet from someone it has not been introduced to. Those
/// are separate behaviours (RFC 4787 treats them separately) and only the
/// second decides whether two ordinary home connections can meet.
///
/// The socket is created once and never replaced. Mapping is per-socket, so
/// learning an address on one socket and sending from another would report an
/// address nothing is listening on.
fn punch(peer: Option<SocketAddr>, seconds: u64) -> io::Result<()> {
    let socket = UdpSocket::bind(Family::V4.wildcard())?;
    socket.set_read_timeout(Some(Duration::from_millis(120)))?;

    let server = STUN_SERVERS
        .iter()
        .find_map(|s| resolve(s, Family::V4))
        .ok_or_else(|| io::Error::other("no STUN server resolved"))?;

    let mut mapped = stun_binding(&socket, server)?;
    println!("Machine Elves — hole punch");
    println!("==========================");
    println!();
    println!("  This machine is reachable at:  {mapped}");
    println!();

    let peer = match peer {
        Some(peer) => peer,
        None => {
            println!("  Send that to the other machine, and paste theirs here.");
            println!("  The mapping is refreshed while you wait, so take your time.");
            println!();
            print!("  their address: ");
            let _ = io::Write::flush(&mut io::stdout());

            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                if std::io::BufRead::read_line(&mut io::stdin().lock(), &mut line).is_ok() {
                    let _ = tx.send(line);
                }
            });

            let mut refreshed = SystemTime::now();
            loop {
                if let Ok(line) = rx.try_recv() {
                    match line.trim().parse::<SocketAddr>() {
                        Ok(peer) => break peer,
                        Err(_) => return Err(io::Error::other("that is not an address:port")),
                    }
                }
                if refreshed.elapsed().unwrap_or_default() >= REFRESH_EVERY {
                    refreshed = SystemTime::now();
                    if let Ok(now) = stun_binding(&socket, server) {
                        // A changed port means the wait outlasted the mapping and
                        // whatever was sent to the other side is now wrong.
                        if now != mapped {
                            println!();
                            println!("  !! the address changed to {now} — send them this one instead");
                            print!("  their address: ");
                            let _ = io::Write::flush(&mut io::stdout());
                            mapped = now;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    };

    println!();
    println!("  Trying to reach {peer} for {seconds}s.");
    println!("  Start the other side too — neither can get in alone.");
    println!();

    let started = SystemTime::now();
    let deadline = Duration::from_secs(seconds);
    let mut last_sent = SystemTime::UNIX_EPOCH;
    let mut heard_from_them: Option<Duration> = None;
    let mut they_heard_us: Option<Duration> = None;
    let mut buf = [0u8; 256];

    while started.elapsed().unwrap_or_default() < deadline {
        if last_sent.elapsed().unwrap_or(Duration::MAX) >= PUNCH_EVERY {
            last_sent = SystemTime::now();
            let _ = socket.send_to(PUNCH_HELLO, peer);
        }

        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                // The port may differ from the one we were given: the other
                // side's router can assign a different one than it reported.
                // The address is what identifies them.
                if from.ip() != peer.ip() {
                    continue;
                }
                let elapsed = started.elapsed().unwrap_or_default();
                if buf[..len].starts_with(PUNCH_HELLO) {
                    if heard_from_them.is_none() {
                        println!("  <- their packet arrived from {from} after {:.1}s", elapsed.as_secs_f64());
                        heard_from_them = Some(elapsed);
                    }
                    let _ = socket.send_to(PUNCH_REPLY, from);
                } else if buf[..len].starts_with(PUNCH_REPLY) && they_heard_us.is_none() {
                    println!("  -> they answered ours after {:.1}s", elapsed.as_secs_f64());
                    they_heard_us = Some(elapsed);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }

        if heard_from_them.is_some() && they_heard_us.is_some() {
            break;
        }
    }

    println!();
    let verdict = match (heard_from_them, they_heard_us) {
        (Some(_), Some(_)) => {
            println!("  BOTH DIRECTIONS WORK. Two machines behind address translation reached");
            println!("  each other with nothing in the middle. This is the result Phase 0");
            println!("  was built to find.");
            "two-way"
        }
        (Some(_), None) => {
            println!("  Their packets reach us; ours do not reach them. Their router is the");
            println!("  stricter of the two — a path exists, but only one way, so a peer");
            println!("  connection would need help from somewhere.");
            "inbound-only"
        }
        (None, Some(_)) => {
            println!("  Ours reach them; theirs do not reach us. This router is the stricter");
            println!("  one. A path exists, but only one way.");
            "outbound-only"
        }
        (None, None) => {
            println!("  Nothing got through in either direction.");
            println!();
            println!("  Either the two sides did not overlap in time, or at least one router");
            println!("  refuses packets from anyone it was not introduced to. Try again with");
            println!("  both sides started together before concluding the second.");
            "no-contact"
        }
    };

    println!();
    println!("PUNCH v0.1 | me={mapped} | peer={peer} | result={verdict}");
    Ok(())
}

// ------------------------------------------------------------ machine output

/// One record per run, appended to a log by the systemd timer. Hand-rolled
/// because the no-dependencies rule applies here too, and the shape is small
/// and fixed.
fn report_json(label: &str, v4: &FamilyResult, v6: &FamilyResult) -> String {
    let v4_verdict = describe_v4(v4);
    let v6_verdict = describe_v6(v6);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut out = String::from("{");
    out.push_str(&format!("\"schema\":1,\"ts_unix\":{},", now));
    out.push_str(&format!("\"ts_utc\":\"{}\",", format_utc(now)));
    out.push_str(&format!("\"label\":{},", json_string(label)));
    out.push_str("\"test\":\"connectivity\",");
    out.push_str(&format!("\"ipv4\":{},", family_json(v4, &v4_verdict)));
    out.push_str(&format!("\"ipv6\":{},", family_json(v6, &v6_verdict)));
    out.push_str(&format!(
        "\"relay_candidate\":{}",
        v4_verdict.reachable || v6_verdict.reachable
    ));
    out.push('}');
    out
}

fn family_json(result: &FamilyResult, verdict: &Verdict) -> String {
    let observers: Vec<String> = result
        .observations
        .iter()
        .map(|o| {
            format!(
                "{{\"server\":{},\"mapped\":{}}}",
                json_string(&o.server),
                json_string(&o.mapped.to_string())
            )
        })
        .collect();
    let errors: Vec<String> = result.errors.iter().map(|e| json_string(e)).collect();

    format!(
        "{{\"tag\":{},\"local\":{},\"observers\":[{}],\"errors\":[{}]}}",
        json_string(&verdict.tag),
        match result.local {
            Some(ip) => json_string(&ip.to_string()),
            None => "null".to_string(),
        },
        observers.join(","),
        errors.join(",")
    )
}

fn json_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Epoch seconds to an ISO-8601 UTC timestamp.
///
/// Logs get read by a person deciding whether a box is healthy, and epoch
/// seconds are not readable. This is Howard Hinnant's `civil_from_days`, which
/// is exact and needs no calendar table.
fn format_utc(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let secs_of_day = epoch % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_ascend_and_span_the_interesting_range() {
        assert!(IDLE_INTERVALS.windows(2).all(|w| w[0] < w[1]));
        // RFC 4787 asks routers to hold UDP mappings for at least 120s, so the
        // list must have values on both sides of that rather than stopping short.
        assert!(IDLE_INTERVALS.first().copied().unwrap() < 120);
        assert!(IDLE_INTERVALS.last().copied().unwrap() > 120);
    }

    #[test]
    fn only_ever_picks_a_listed_interval() {
        for _ in 0..200 {
            assert!(IDLE_INTERVALS.contains(&pick_idle_interval()));
        }
    }

    #[test]
    fn mapping_record_is_parseable_and_tagged() {
        let test = MappingTest {
            idle_seconds: 120,
            server: "stun.example.net:3478".to_string(),
            first: Some("203.0.113.7:45485".parse().unwrap()),
            second: Some("203.0.113.7:45485".parse().unwrap()),
            outcome: Outcome::Survived,
        };
        let record = mapping_json("vol-2", &test);
        assert!(record.starts_with('{') && record.ends_with('}'));
        assert!(record.contains(r#""test":"mapping-lifetime""#));
        assert!(record.contains(r#""outcome":"survived""#));
        assert!(record.contains(r#""idle_seconds":120"#));
        assert!(record.contains(r#""note":null"#));
    }

    #[test]
    fn inconclusive_records_carry_their_reason() {
        let test = MappingTest {
            idle_seconds: 60,
            server: "-".to_string(),
            first: None,
            second: None,
            outcome: Outcome::Inconclusive("public address changed mid-test"),
        };
        let record = mapping_json("vol-2", &test);
        assert!(record.contains(r#""outcome":"inconclusive""#));
        assert!(record.contains(r#""note":"public address changed mid-test""#));
        assert!(record.contains(r#""first":null"#));
    }

    #[test]
    fn connectivity_records_are_tagged_too() {
        let empty = |family| FamilyResult {
            family,
            local: None,
            observations: Vec::new(),
            errors: Vec::new(),
        };
        let record = report_json("vol-1", &empty(Family::V4), &empty(Family::V6));
        assert!(record.contains(r#""test":"connectivity""#));
    }

    #[test]
    fn formats_known_epochs_as_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, which the naive arithmetic gets wrong.
        assert_eq!(format_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(json_string("line\nbreak"), r#""line\nbreak""#);
    }

    #[test]
    fn emits_parseable_record_when_everything_failed() {
        let empty = |family| FamilyResult {
            family,
            local: None,
            observations: Vec::new(),
            errors: vec!["no reply".to_string()],
        };
        let record = report_json("vol-1", &empty(Family::V4), &empty(Family::V6));
        assert!(record.starts_with('{') && record.ends_with('}'));
        assert!(record.contains(r#""label":"vol-1""#));
        assert!(record.contains(r#""relay_candidate":false"#));
        assert!(record.contains(r#""tag":"unreachable""#));
    }

    #[test]
    fn decodes_xor_mapped_ipv4() {
        let transaction = [0u8; 12];
        // 192.0.2.1:32853 encoded per RFC 5389 §15.2.
        let port = 32853u16 ^ (MAGIC_COOKIE >> 16) as u16;
        let mut octets = Ipv4Addr::new(192, 0, 2, 1).octets();
        for (i, byte) in MAGIC_COOKIE.to_be_bytes().iter().enumerate() {
            octets[i] ^= byte;
        }
        let mut value = vec![0x00, 0x01];
        value.extend_from_slice(&port.to_be_bytes());
        value.extend_from_slice(&octets);

        let decoded = decode_address(&value, true, &transaction).expect("decodes");
        assert_eq!(decoded, SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 32853)));
    }

    #[test]
    fn rejects_response_with_wrong_transaction_id() {
        let ours = [1u8; 12];
        let theirs = [2u8; 12];
        let mut msg = Vec::new();
        msg.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes());
        msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        msg.extend_from_slice(&theirs);
        assert!(parse_binding_response(&msg, &ours).is_none());
    }

    #[test]
    fn identifies_carrier_grade_nat_range() {
        assert!(is_shared_address_space(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_shared_address_space(Ipv4Addr::new(100, 127, 255, 255)));
        assert!(!is_shared_address_space(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_shared_address_space(Ipv4Addr::new(100, 128, 0, 0)));
    }

    #[test]
    fn identifies_global_ipv6_only() {
        assert!(is_global_unicast_v6("2001:db8::1".parse().unwrap()));
        assert!(!is_global_unicast_v6("fe80::1".parse().unwrap()));
        assert!(!is_global_unicast_v6("fd00::1".parse().unwrap()));
    }

    #[test]
    fn skips_unknown_attributes_to_find_the_mapped_address() {
        let transaction = [7u8; 12];
        let mut attrs = Vec::new();
        // An unknown attribute with a 3-byte value, forcing one byte of padding.
        attrs.extend_from_slice(&0x9999u16.to_be_bytes());
        attrs.extend_from_slice(&3u16.to_be_bytes());
        attrs.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0x00]);

        let port = 1234u16 ^ (MAGIC_COOKIE >> 16) as u16;
        let mut octets = Ipv4Addr::new(203, 0, 113, 5).octets();
        for (i, byte) in MAGIC_COOKIE.to_be_bytes().iter().enumerate() {
            octets[i] ^= byte;
        }
        attrs.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        attrs.extend_from_slice(&8u16.to_be_bytes());
        attrs.extend_from_slice(&[0x00, 0x01]);
        attrs.extend_from_slice(&port.to_be_bytes());
        attrs.extend_from_slice(&octets);

        let mut msg = Vec::new();
        msg.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
        msg.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
        msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        msg.extend_from_slice(&transaction);
        msg.extend_from_slice(&attrs);

        let decoded = parse_binding_response(&msg, &transaction).expect("finds address");
        assert_eq!(decoded.port(), 1234);
    }
}
