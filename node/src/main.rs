//! Phase 0 mesh node.
//!
//! Does one thing: forms an overlay with other nodes, keeps track of who is in
//! it, and notices when someone disappears — distinguishing a machine that left
//! cleanly from one that was unplugged.
//!
//! Jobs, checkpointing, and migration come later. Membership has to be
//! trustworthy first, because everything else is built on knowing who is here.

mod job;
mod ledger;
mod portmap;

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, mdns, noise, ping, tcp, yamux, Multiaddr, PeerId};

/// The topic nodes of one mesh publish to, namespaced by the mesh's name.
///
/// Nodes only ever see each other through this topic, so two meshes on the same
/// network with different names are genuinely separate: they discover each
/// other's addresses and then ignore each other entirely. That is what §5.1
/// means by city-states being separate shards, and it is also what keeps a test
/// run from being joined by whatever was left running from the last one.
fn heartbeat_topic(mesh: &str) -> String {
    format!("machine-elves/{mesh}/heartbeat/0.1")
}

/// "I am still here."
const KIND_HEARTBEAT: u8 = 1;

/// "I am leaving on purpose."
///
/// This is what §9.6 calls a graceful drain, and it has to be an announcement
/// rather than anything the transport layer reports. A killed process still has
/// its sockets closed tidily by the kernel, so a clean close says nothing about
/// whether the departure was orderly — only the node itself knows that.
const KIND_DEPARTING: u8 = 2;

/// "Here is the job's state as of this tick."
///
/// Sent by whichever node is currently running the job, to everyone. A peer
/// that holds a recent checkpoint can continue the work without asking anyone
/// for anything, which is what makes takeover fast enough to be worth doing.
const KIND_CHECKPOINT: u8 = 3;

/// "I can see this peer at this address."
///
/// A node behind address translation cannot know its own address — only the
/// peer receiving its packets can see where they came from. So nobody
/// advertises themselves; everyone reports what they observe of others, and a
/// node learns where it lives by hearing itself described.
///
/// This is also what makes two translated peers able to meet. Announcements
/// reach the whole mesh at once, so everyone dials the newcomer at the same
/// moment while the newcomer dials back — and simultaneous dialling is the
/// only thing that opens a path between two machines neither of which can be
/// called. **The mesh is its own rendezvous**, which is why §11.6 can refuse
/// rented infrastructure without refusing the thing it would have provided.
const KIND_OBSERVED: u8 = 4;

/// How often a node repeats what it can see, for anyone who has since arrived.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);

/// How often the node running a job advances it.
const DEFAULT_JOB_TICK_MS: u64 = 200;

/// The port a node listens on unless told otherwise.
///
/// Fixed rather than chosen by the system, and that is what makes a node
/// findable. A router that preserves port numbers — as the home connection
/// measured here does — then maps this to the same external port every time,
/// so a peer's address survives restarts and can be remembered. Bound to zero
/// the system picks a new port each run, the external address changes with it,
/// and nothing about a peer is worth writing down.
const DEFAULT_PORT: u16 = 4001;

/// How often to try a peer that has not answered yet.
///
/// Retrying is not politeness, it is the mechanism. Two nodes behind address
/// translation cannot be dialled; the only thing that opens a path is both of
/// them sending outward at once, and each attempt is a packet outward. Whoever
/// starts second would fail permanently if the first had given up.
const DIAL_RETRY: Duration = Duration::from_secs(2);

/// How often a quiet connection is poked to stop a router forgetting it.
///
/// Measured rather than guessed: the mobile connection tested forgot an idle
/// mapping somewhere between 120 and 300 seconds, and the home connection did
/// not forget in over ten minutes across 45 attempts. The keepalive is governed
/// by the worst connection among the players, so 55 s leaves better than a
/// twofold margin on the shorter of the two.
///
/// **This is a different job from the heartbeat, and the separation is the
/// point.** Heartbeats run every second because that is what makes an
/// unannounced disappearance visible within three; keepalives run rarely
/// because their only purpose is stopping a router forgetting a path. Today the
/// heartbeats happen to keep mappings alive as a side effect, which is exactly
/// why this needs its own name — the moment detection is slowed, or a player
/// runs this on a metered connection where every packet is a cost, the two
/// requirements pull in opposite directions and one of them silently loses.
const DEFAULT_KEEPALIVE_MS: u64 = 55_000;

#[derive(Debug, PartialEq)]
enum Message {
    Heartbeat { label: String },
    Departing { label: String },
    Checkpoint { label: String, tick: u64, state: Vec<u8> },
    Observed { label: String, peer: PeerId, addr: Multiaddr },
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn take_u32(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let value = u32::from_le_bytes(bytes.get(*at..end)?.try_into().ok()?);
    *at = end;
    Some(value)
}

fn take_u64(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let end = at.checked_add(8)?;
    let value = u64::from_le_bytes(bytes.get(*at..end)?.try_into().ok()?);
    *at = end;
    Some(value)
}

fn take_bytes(bytes: &[u8], at: &mut usize) -> Option<Vec<u8>> {
    let len = take_u32(bytes, at)? as usize;
    let end = at.checked_add(len)?;
    let value = bytes.get(*at..end)?.to_vec();
    *at = end;
    Some(value)
}

fn encode_heartbeat(label: &str, counter: u64) -> Vec<u8> {
    let mut out = vec![KIND_HEARTBEAT];
    put_str(&mut out, label);
    out.extend_from_slice(&counter.to_le_bytes());
    out
}

fn encode_departing(label: &str) -> Vec<u8> {
    let mut out = vec![KIND_DEPARTING];
    put_str(&mut out, label);
    out
}

fn encode_observed(label: &str, peer: &PeerId, addr: &Multiaddr) -> Vec<u8> {
    let mut out = vec![KIND_OBSERVED];
    put_str(&mut out, label);
    let id = peer.to_bytes();
    out.extend_from_slice(&(id.len() as u32).to_le_bytes());
    out.extend_from_slice(&id);
    put_str(&mut out, &addr.to_string());
    out
}

fn encode_checkpoint(label: &str, tick: u64, state: &[u8]) -> Vec<u8> {
    let mut out = vec![KIND_CHECKPOINT];
    put_str(&mut out, label);
    out.extend_from_slice(&tick.to_le_bytes());
    out.extend_from_slice(&(state.len() as u32).to_le_bytes());
    out.extend_from_slice(state);
    out
}

/// Returns `None` for anything unrecognised or malformed.
///
/// A future version publishing kinds this one has never heard of must be
/// ignored rather than misread — mistaking any other message for a heartbeat
/// would make a departed node look present.
fn parse_message(bytes: &[u8]) -> Option<Message> {
    let kind = *bytes.first()?;
    let mut at = 1usize;
    let label = String::from_utf8(take_bytes(bytes, &mut at)?).ok()?;
    if label.is_empty() {
        return None;
    }
    match kind {
        KIND_HEARTBEAT => Some(Message::Heartbeat { label }),
        KIND_DEPARTING => Some(Message::Departing { label }),
        KIND_CHECKPOINT => {
            let tick = take_u64(bytes, &mut at)?;
            let state = take_bytes(bytes, &mut at)?;
            Some(Message::Checkpoint { label, tick, state })
        }
        KIND_OBSERVED => {
            let peer = PeerId::from_bytes(&take_bytes(bytes, &mut at)?).ok()?;
            let addr: Multiaddr = String::from_utf8(take_bytes(bytes, &mut at)?)
                .ok()?
                .parse()
                .ok()?;
            Some(Message::Observed { label, peer, addr })
        }
        _ => None,
    }
}

/// How often this node announces that it is still here.
///
/// Must be comfortably shorter than the detection threshold, so that losing a
/// single heartbeat to ordinary packet loss does not look like a dead machine.
const DEFAULT_HEARTBEAT_MS: u64 = 1_000;

/// How long a node may go silent before it is presumed gone.
///
/// Fixed at three seconds in `docs/phase-0-plan.md` before any measurement, and
/// deliberately generous: the cost of deciding too early is that the mesh
/// migrates work every time a wifi link stutters.
const DEFAULT_DETECT_MS: u64 = 3_000;

#[derive(libp2p::swarm::NetworkBehaviour)]
struct MeshBehaviour {
    /// Carries heartbeats. Gossip rather than direct messaging, so that
    /// membership does not require every node to hold a connection to every
    /// other — §11.6 rules out a full mesh.
    gossipsub: gossipsub::Behaviour,
    /// Finds other nodes on the same local network. Genuinely useful for the
    /// container rig and for a player's own machines, and incapable of finding
    /// anything across the internet (§11.6).
    mdns: mdns::tokio::Behaviour,
    /// Exchanges addresses and protocol versions on connect.
    identify: identify::Behaviour,
    /// Round-trip timing, which is the latency floor for any takeover.
    ping: ping::Behaviour,
}

struct Config {
    label: String,
    listen: Vec<Multiaddr>,
    dial: Vec<Multiaddr>,
    heartbeat: Duration,
    detect: Duration,
    json: bool,
    /// A job every node in the mesh holds. One of them runs it; the rest stand
    /// ready to continue it.
    job: Option<String>,
    job_tick: Duration,
    keepalive: Duration,
    /// Which mesh this node belongs to. Nodes in different meshes ignore one
    /// another completely.
    mesh: String,
    port: u16,
    /// Whether to ask the router to forward this node's port.
    ///
    /// On by default. A mesh where nobody is reachable cannot admit anyone it
    /// is not already connected to, so a node that never asks makes the whole
    /// arrangement depend on somebody else having asked. A router that says no
    /// simply says no, and the mapping lapses on its own if the node dies.
    map_port: bool,
    /// Local-network discovery. Off is a supported way to run: many machines
    /// disable it, and a node that depends on it has one route to a peer that
    /// an operator may have deliberately closed.
    mdns: bool,
    /// Whether this node starts as the one running the job.
    own: bool,
}

/// What is known about one other node.
/// A job the whole mesh holds, and the bookkeeping for who is running it.
struct Work {
    job: job::Job,
    /// Everything this node knows to exist. Not a count — see `ledger`.
    ledger: ledger::Ledger,
    /// The node currently advancing it. Every node agrees on this by watching
    /// checkpoints arrive, so nobody has to be told.
    owner: Option<PeerId>,
    state: Vec<u8>,
    tick: u64,
    /// When this node noticed the owner disappear. Only set between the
    /// disappearance and the takeover, and used to measure the gap.
    owner_lost_at: Option<Instant>,
    /// When this node may claim work nobody has answered for.
    ///
    /// A node told to run a job listens before doing so. Claiming immediately
    /// means a node returning from a reboot takes work that somebody else has
    /// been doing perfectly well, and both then run it — wasteful, and only
    /// harmless because identical work produces identical results.
    claim_after: Option<Instant>,
}

impl Work {
    /// Which of two nodes running the same job should keep it.
    ///
    /// Both sides evaluate this and must agree, or the job either stalls
    /// because each yields to the other, or is run twice because neither does.
    ///
    /// Whoever is further along keeps it: their work is the work that would be
    /// lost. Ties go to the lower identifier, which is arbitrary and, more
    /// importantly, the same arbitrary answer on both machines.
    fn theirs(mine: (u64, &PeerId), theirs: (u64, &PeerId)) -> bool {
        theirs.0 > mine.0 || (theirs.0 == mine.0 && theirs.1 < mine.1)
    }

    /// Decides, with no coordination at all, whether this node should continue
    /// the job now that its owner is gone.
    ///
    /// The rule is simply the lowest peer identifier among everyone still
    /// present. Every survivor holds the same membership list and applies the
    /// same comparison, so they all reach the same answer without exchanging a
    /// single message — and a vote here would cost more time than the takeover
    /// it was arranging.
    ///
    /// Two nodes briefly disagreeing is survivable rather than catastrophic:
    /// they would run identical ticks from identical state and produce
    /// identical results (§11.4), so the duplicate is wasted work, not damage.
    fn should_take_over(me: &PeerId, members: &HashMap<PeerId, Member>) -> bool {
        // `peer != me` matters as much as the comparison. If this node ever
        // appears in its own membership — which self-discovery over the local
        // network will cause — then without it the test is `me < me`, which is
        // false, and this node silently becomes ineligible forever.
        members.keys().filter(|peer| *peer != me).all(|peer| me < peer)
    }
}

struct Member {
    label: String,
    joined: Instant,
    last_heard: Instant,
    /// True once every transport connection to this peer has closed. A useful
    /// hint that something changed, and deliberately *not* used to decide
    /// whether a departure was orderly — see [`MSG_DEPARTING`].
    disconnected: bool,
}

fn main() -> Result<()> {
    // Running a job takes no network and no peers, so it is handled before the
    // swarm exists rather than as a mode of it.
    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args.iter().position(|a| a == "--run-job") {
        let path = args.get(at + 1).context("--run-job needs a path to a .wasm")?;
        let ticks: u64 = args
            .iter()
            .position(|a| a == "--ticks")
            .and_then(|i| args.get(i + 1))
            .map(|t| t.parse())
            .transpose()?
            .unwrap_or(20);
        return run_job(path, ticks);
    }

    let config = parse_args()?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run(config))
}

fn parse_args() -> Result<Config> {
    let mut config = Config {
        label: hostname(),
        listen: Vec::new(),
        dial: Vec::new(),
        heartbeat: Duration::from_millis(DEFAULT_HEARTBEAT_MS),
        detect: Duration::from_millis(DEFAULT_DETECT_MS),
        json: false,
        job: None,
        job_tick: Duration::from_millis(DEFAULT_JOB_TICK_MS),
        keepalive: Duration::from_millis(DEFAULT_KEEPALIVE_MS),
        mesh: "default".to_string(),
        port: DEFAULT_PORT,
        map_port: true,
        mdns: true,
        own: false,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--label" => config.label = args.next().context("--label needs a name")?,
            "--listen" => config
                .listen
                .push(args.next().context("--listen needs an address")?.parse()?),
            "--peer" => config
                .dial
                .push(args.next().context("--peer needs an address")?.parse()?),
            "--heartbeat-ms" => {
                config.heartbeat = Duration::from_millis(
                    args.next().context("--heartbeat-ms needs a number")?.parse()?,
                )
            }
            "--detect-ms" => {
                config.detect =
                    Duration::from_millis(args.next().context("--detect-ms needs a number")?.parse()?)
            }
            "--json" => config.json = true,
            "--job" => config.job = Some(args.next().context("--job needs a path")?),
            "--mesh" => config.mesh = args.next().context("--mesh needs a name")?,
            "--port" => config.port = args.next().context("--port needs a number")?.parse()?,
            "--keepalive-ms" => {
                config.keepalive = Duration::from_millis(
                    args.next().context("--keepalive-ms needs a number")?.parse()?,
                )
            }
            "--own" => config.own = true,
            "--no-mdns" => config.mdns = false,
            "--no-map-port" => config.map_port = false,
            "--job-tick-ms" => {
                config.job_tick =
                    Duration::from_millis(args.next().context("--job-tick-ms needs a number")?.parse()?)
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    if config.listen.is_empty() {
        // QUIC first: it establishes in fewer round trips and is friendlier to
        // the address translation most players sit behind. TCP is kept as the
        // fallback for networks that block or mangle UDP.
        let port = config.port;
        config.listen.push(format!("/ip4/0.0.0.0/udp/{port}/quic-v1").parse()?);
        config.listen.push(format!("/ip4/0.0.0.0/tcp/{port}").parse()?);

        // And IPv6, where a machine has it. This is not symmetry for its own
        // sake: a peer with a global IPv6 address has no translation in front
        // of it, nothing to punch through, no mapping to expire, and no
        // keepalive to maintain. It is the easiest peer in the world to reach
        // and listening only on IPv4 makes it unreachable.
        //
        // It matters twice over, because the two families cannot address each
        // other at all. A peer holding both is the only route between members
        // that have one and members that have the other, and a city whose last
        // dual-stack member leaves does not degrade — it splits into two meshes
        // that cannot see one another.
        //
        // Failing to bind is normal and not an error: most machines have no
        // IPv6 route, which is exactly what the connectivity probe reports.
        config.listen.push(format!("/ip6/::/udp/{port}/quic-v1").parse()?);
        config.listen.push(format!("/ip6/::/tcp/{port}").parse()?);
    }

    anyhow::ensure!(
        config.heartbeat * 2 <= config.detect,
        "heartbeat ({:?}) must be at most half the detection threshold ({:?}), or a single \
         lost packet looks like a dead machine",
        config.heartbeat,
        config.detect
    );

    Ok(config)
}

/// Runs a job for some ticks and shows what it did.
///
/// Every tick feeds the previous state back in, which is the whole contract:
/// the job keeps nothing of its own between calls.
fn run_job(path: &str, ticks: u64) -> Result<()> {
    let job = job::Job::load(path, job::DEFAULT_FUEL)?;
    println!("running {path} for {ticks} ticks\n");

    let mut state: Vec<u8> = Vec::new();
    let mut total_fuel = 0u64;

    for tick in 0..ticks {
        // Steel arriving and people turning up are the world's business, not
        // the job's, so they are handed in rather than fetched.
        let mut inputs = Vec::new();
        inputs.extend_from_slice(&tick.to_le_bytes());
        inputs.extend_from_slice(&4u32.to_le_bytes()); // steel delivered
        inputs.extend_from_slice(&3u32.to_le_bytes()); // workers present

        let outcome = job.tick(&state, &inputs)?;
        total_fuel += outcome.fuel_used;

        for line in String::from_utf8_lossy(&outcome.effects).lines() {
            println!("  tick {tick:>3}  {line}");
        }
        state = outcome.state;
    }

    println!(
        "\n{ticks} ticks, {} bytes of state, {total_fuel} fuel ({} per tick)",
        state.len(),
        total_fuel / ticks.max(1)
    );
    Ok(())
}

fn print_usage() {
    println!("mesh-node — forms an overlay and tracks who is in it");
    println!();
    println!("  --label <name>       identify this node in the output");
    println!("  --listen <multiaddr> address to listen on (repeatable)");
    println!("  --peer <multiaddr>   address of a node to dial (repeatable)");
    println!("  --heartbeat-ms <n>   how often to announce presence (default {DEFAULT_HEARTBEAT_MS})");
    println!("  --detect-ms <n>      silence before a peer is presumed gone (default {DEFAULT_DETECT_MS})");
    println!("  --json               emit machine-readable events");
    println!("  --run-job <file.wasm> [--ticks N]");
    println!("                       run a job locally and show its effects, then exit");
    println!("  --keepalive-ms <n>   how often to poke a quiet connection so a router does");
    println!("                       not forget it (default {DEFAULT_KEEPALIVE_MS})");
    println!("  --port <n>           port to listen on (default {DEFAULT_PORT}). Fixed, so that a");
    println!("                       router preserving port numbers gives this node the same");
    println!("                       address every restart and peers can remember it");
    println!("  --no-map-port        do not ask the router to forward this node's port.");
    println!("                       Asking is the default: a mesh needs somebody reachable,");
    println!("                       and a router that declines simply declines");
    println!("  --no-mdns            do not use local-network discovery. Peers are then");
    println!("                       found only by address, which is how they are found");
    println!("                       across the internet in any case");
    println!("  --mesh <name>        which mesh to join (default \"default\"). Nodes in");
    println!("                       different meshes ignore each other entirely");
    println!("  --job <file.wasm>    hold this job, ready to run or to continue it");
    println!("  --own                start as the node running the job");
    println!("  --job-tick-ms <n>    how often the running node advances it (default {DEFAULT_JOB_TICK_MS})");
    println!("  --help               show this");
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unlabelled".to_string())
}

async fn run(config: Config) -> Result<()> {
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            let keepalive = config.keepalive;
            let use_mdns = config.mdns;
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub::ConfigBuilder::default()
                    // Heartbeats are worthless once late, so the network should
                    // not spend effort re-delivering old ones.
                    .heartbeat_interval(Duration::from_millis(500))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .build()
                    .map_err(std::io::Error::other)?,
            )?;
            Ok(MeshBehaviour {
                gossipsub,
                mdns: mdns::tokio::Behaviour::new(
                    if use_mdns {
                        mdns::Config::default()
                    } else {
                        // Left in place but never speaking: a query interval
                        // beyond any run's lifetime is simpler than making the
                        // behaviour optional, and cannot half-work.
                        mdns::Config { query_interval: Duration::from_secs(86_400 * 365), ..Default::default() }
                    },
                    key.public().to_peer_id(),
                )?,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/machine-elves/0.1".into(),
                    key.public(),
                )),
                // Ping is the keepalive. Giving it an explicit interval means
                // paths stay open because something is deliberately holding
                // them open, rather than because gossip happens to be chatty
                // enough this week.
                ping: ping::Behaviour::new(ping::Config::new().with_interval(keepalive)),
            })
        })?
        // Comfortably longer than the keepalive, or a connection would be
        // dropped for idleness between the very pokes meant to preserve it.
        .with_swarm_config(|c| c.with_idle_connection_timeout(config.keepalive * 3))
        .build();

    let topic = gossipsub::IdentTopic::new(heartbeat_topic(&config.mesh));
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    for addr in &config.listen {
        swarm.listen_on(addr.clone())?;
    }
    // Peers we are trying to reach and have not reached yet. Kept rather than
    // dialled once and forgotten, because a single attempt only succeeds if the
    // other side happened to already be listening — and neither side can be
    // listening in any useful sense while behind address translation.
    let mut pending: Vec<Multiaddr> = config.dial.clone();
    // Which address reached which peer, so that losing the peer puts the
    // address back on the list. Without this a node dials until it succeeds
    // once and then never again: the first disconnection is permanent, and no
    // amount of waiting fixes it because nothing is trying.
    let mut reached_by: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    let mut dial_attempts: u32 = 0;
    let mut dial_retry = tokio::time::interval(DIAL_RETRY);
    // Where this node can currently see each peer. Observed, never claimed:
    // a peer's own idea of its address is worthless behind translation.
    let mut seen_at: HashMap<PeerId, Multiaddr> = HashMap::new();
    let mut announce = tokio::time::interval(ANNOUNCE_EVERY);
    let mut my_address: Option<Multiaddr> = None;
    let mut renew_mapping = tokio::time::interval(portmap::RENEW_EVERY);
    let mut mapping: Option<portmap::Mapping> = None;

    let me = *swarm.local_peer_id();
    emit(
        &config,
        "started",
        &format!("node {} is {} on mesh \"{}\"", config.label, me, config.mesh),
        &[("peer_id", &me.to_string()), ("mesh", &config.mesh)],
    );

    let mut work = match &config.job {
        Some(path) => {
            let job = job::Job::load(path, job::DEFAULT_FUEL)?;
            emit(&config, "job-loaded",
                 &format!("holding {path}{}", if config.own { ", running it" } else { ", standing by" }),
                 &[("path", path), ("owner", if config.own { "true" } else { "false" })]);
            Some(Work {
                job,
                ledger: ledger::Ledger::new(),
                // Not claimed yet, even when told to run it. Listening first
                // costs a few seconds once; claiming first costs duplicated
                // work every time a node restarts.
                owner: None,
                state: Vec::new(),
                tick: 0,
                owner_lost_at: None,
                claim_after: config
                    .own
                    .then(|| Instant::now() + config.detect * 3),
            })
        }
        None => None,
    };

    let mut members: HashMap<PeerId, Member> = HashMap::new();
    let mut heartbeat = tokio::time::interval(config.heartbeat);
    let mut job_tick = tokio::time::interval(config.job_tick);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    // Checked several times per detection window so that a departure is
    // reported close to when it actually crossed the threshold, rather than up
    // to a whole window late.
    let mut sweep = tokio::time::interval(config.detect / 4);
    let mut counter: u64 = 0;

    loop {
        tokio::select! {
            _ = dial_retry.tick(), if !pending.is_empty() => {
                dial_attempts += 1;
                for addr in &pending {
                    // Failures are expected and frequent: until the other side
                    // is also sending, there is nothing at the far end willing
                    // to answer. The attempt is worth making anyway, because
                    // the outgoing packet is what opens this side's path.
                    let _ = swarm.dial(addr.clone());
                }
                if dial_attempts == 1 || dial_attempts % 15 == 0 {
                    emit(&config, "dialling",
                         &format!("still trying {} peer(s), attempt {dial_attempts}", pending.len()),
                         &[("peers", &pending.len().to_string()),
                           ("attempts", &dial_attempts.to_string())]);
                }
            }

            // Repeat what we can see, for anyone who has arrived since. New
            // members hear the whole roster this way and dial into it, rather
            // than waiting to be found.
            // Asked for repeatedly, not once. A mapping expires, and a node
            // that asks at startup and forgets stops being reachable an hour
            // later while appearing perfectly healthy — which is the kind of
            // failure that gets diagnosed as something else entirely.
            _ = renew_mapping.tick(), if config.map_port => {
                match portmap::request(config.port, portmap::LIFETIME_SECS).await {
                    Ok(now) => {
                        if mapping != Some(now) {
                            emit(&config, "port-mapped",
                                 &format!("the router forwards {}:{} to this node for {}s",
                                          now.external_ip, now.external_port, now.lifetime),
                                 &[("external", &format!("{}:{}", now.external_ip, now.external_port)),
                                   ("lifetime", &now.lifetime.to_string()),
                                   ("as_asked", if now.external_port == config.port { "true" } else { "false" })]);
                            mapping = Some(now);
                        }
                    }
                    Err(error) => {
                        // Reported every time rather than once. If a router
                        // stops renewing, reachability is being lost right now,
                        // and a single line an hour ago would not say so.
                        emit(&config, "port-map-failed", &format!("{error}"),
                             &[("error", &error.to_string()),
                               ("was_mapped", if mapping.is_some() { "true" } else { "false" })]);
                        mapping = None;
                    }
                }
            }

            _ = announce.tick() => {
                for (peer, addr) in &seen_at {
                    let _ = swarm.behaviour_mut().gossipsub
                        .publish(topic.clone(), encode_observed(&config.label, peer, addr));
                }
            }

            _ = heartbeat.tick() => {
                counter += 1;
                // Failing to publish is normal and uninteresting while this node
                // is the only one subscribed to the topic.
                let _ = swarm.behaviour_mut().gossipsub
                    .publish(topic.clone(), encode_heartbeat(&config.label, counter));
            }

            _ = job_tick.tick() => {
                let Some(work) = work.as_mut() else { continue };

                // Claim only after listening long enough to have heard an
                // existing owner. Any checkpoint received clears this, so the
                // claim happens exactly when nobody answered for the work.
                if let Some(at) = work.claim_after {
                    if work.owner.is_none() && Instant::now() >= at {
                        work.claim_after = None;
                        work.owner = Some(me);
                        emit(&config, "claimed",
                             "nobody was running this job, so this node is",
                             &[("tick", &work.tick.to_string())]);
                    }
                }

                if work.owner != Some(me) { continue }

                let mut inputs = Vec::new();
                inputs.extend_from_slice(&work.tick.to_le_bytes());
                inputs.extend_from_slice(&4u32.to_le_bytes());  // steel delivered
                inputs.extend_from_slice(&3u32.to_le_bytes());  // workers present

                match work.job.tick(&work.state, &inputs) {
                    Ok(outcome) => {
                        work.state = outcome.state;
                        work.tick += 1;
                        // Effects are reported with the tick that produced
                        // them, and that pairing is load-bearing rather than
                        // decorative. Two nodes may briefly continue the same
                        // job — a node that has not yet heard from a peer
                        // believes it is alone — and being deterministic, they
                        // produce identical effects. Wasted work is acceptable;
                        // widgets counted twice is not. Whatever applies these
                        // must therefore treat (job, tick) as the identity of
                        // an effect and ignore a repeat.
                        let effects = String::from_utf8_lossy(&outcome.effects).into_owned();
                        for line in effects.lines() {
                            emit(&config, "effect", &format!("tick {} — {line}", work.tick),
                                 &[("tick", &work.tick.to_string()), ("effect", line)]);
                        }
                        record_production(&config, work, &effects);
                        // Checkpointed every tick because this state is tiny.
                        // Real work would checkpoint less often and trade a
                        // little replayed work for a lot less traffic.
                        let _ = swarm.behaviour_mut().gossipsub.publish(
                            topic.clone(),
                            encode_checkpoint(&config.label, work.tick, &work.state),
                        );
                    }
                    Err(error) => {
                        emit(&config, "job-failed", &format!("{error:#}"),
                             &[("tick", &work.tick.to_string())]);
                        // Stop rather than spin: a job failing every tick would
                        // otherwise flood the mesh with identical complaints.
                        work.owner = None;
                    }
                }
            }

            // Announce the departure rather than simply exiting, so peers learn
            // immediately instead of waiting out the detection window. This is
            // the whole difference between a planned handoff and a hole.
            _ = terminate.recv() => { return depart(&mut swarm, &topic, &config).await; }
            _ = interrupt.recv() => { return depart(&mut swarm, &topic, &config).await; }

            _ = sweep.tick() => {
                let now = Instant::now();
                let gone: Vec<PeerId> = members
                    .iter()
                    .filter(|(_, m)| now.duration_since(m.last_heard) > config.detect)
                    .map(|(peer, _)| *peer)
                    .collect();

                for peer in gone {
                    if let Some(member) = members.remove(&peer) {
                        let silent_for = now.duration_since(member.last_heard);
                        // Reaching this point means no goodbye ever arrived: the
                        // machine was unplugged, lost power, lost its network, or
                        // hung. This is the case recovery has to be fast enough
                        // to survive, and the only one that costs a visible gap.
                        if let Some(w) = work.as_mut() {
                            if w.owner == Some(peer) {
                                w.owner = None;
                                w.owner_lost_at = Some(now);
                            }
                        }
                        emit(
                            &config,
                            "vanished",
                            &format!(
                                "{} vanished — {:.1}s of silence, no goodbye{}",
                                member.label,
                                silent_for.as_secs_f64(),
                                if member.disconnected { " (transport had dropped)" } else { "" }
                            ),
                            &[
                                ("peer_id", &peer.to_string()),
                                ("label", &member.label),
                                ("silent_ms", &silent_for.as_millis().to_string()),
                                ("transport_dropped", if member.disconnected { "true" } else { "false" }),
                                ("was_present_ms",
                                 &now.duration_since(member.joined).as_millis().to_string()),
                            ],
                        );
                    }
                }

                // Run the election after the whole sweep, so the decision is
                // made against the final membership rather than a list still
                // being emptied.
                if let Some(w) = work.as_mut() {
                    if w.owner.is_none() && w.owner_lost_at.is_some() {
                        take_over_if_ours(&config, me, w, &members);
                    }
                }
            }

            event = swarm.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    // Stop dialling this one. The address matched must be the
                    // address *we asked for*, not the one the connection ended
                    // up on: a peer dialled at one address commonly answers
                    // from another — a different interface, or the public
                    // address its translation assigned — and comparing against
                    // that leaves the peer on the list forever, quietly dialled
                    // for the rest of the run.
                    let reached = endpoint.get_remote_address().clone();
                    let before = pending.len();
                    if let libp2p::core::ConnectedPoint::Dialer { address, .. } = &endpoint {
                        pending.retain(|addr| addr != address);
                        // Remembered so it can be dialled again later. A peer
                        // reached once is a peer worth trying to reach again,
                        // and a configured address is the most reliable thing a
                        // node has — it does not go stale the way an observed
                        // one does.
                        let known = reached_by.entry(peer_id).or_default();
                        if !known.contains(address) {
                            known.push(address.clone());
                        }
                    }
                    emit(&config, "connected",
                         &format!("reached {peer_id} at {reached}"),
                         &[("peer_id", &peer_id.to_string()),
                           ("addr", &reached.to_string()),
                           ("after_attempts", &dial_attempts.to_string()),
                           ("still_pending", &pending.len().to_string())]);
                    if before > 0 && pending.is_empty() {
                        emit(&config, "all-reached", "every peer given on the command line answered", &[]);
                    }

                    // Tell everyone where this peer can be reached, at once.
                    // A newcomer is dialled by the whole mesh in the same
                    // moment, while it dials back — which is the only way two
                    // machines behind translation ever meet.
                    seen_at.insert(peer_id, reached.clone());
                    let _ = swarm.behaviour_mut().gossipsub
                        .publish(topic.clone(), encode_observed(&config.label, &peer_id, &reached));
                }

                // Reported because their absence is a diagnosis. A peer that
                // cannot reach us at all looks exactly like a peer that reaches
                // us and fails to shake hands, unless the arrival itself is
                // visible — and the two have nothing to do with each other.
                SwarmEvent::IncomingConnection { send_back_addr, .. } => {
                    emit(&config, "incoming", &format!("something is calling from {send_back_addr}"),
                         &[("from", &send_back_addr.to_string())]);
                }

                SwarmEvent::IncomingConnectionError { send_back_addr, error, .. } => {
                    emit(&config, "incoming-failed",
                         &format!("{send_back_addr} reached us but the connection failed: {error}"),
                         &[("from", &send_back_addr.to_string()), ("error", &error.to_string())]);
                }

                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    // Expected constantly while punching, so only the reason is
                    // kept, and only when it changes what we would conclude.
                    let reason = error.to_string();
                    if !reason.contains("Timeout") && !reason.contains("timed out") {
                        emit(&config, "dial-failed", &reason,
                             &[("peer", &peer_id.map(|p| p.to_string()).unwrap_or_default()),
                               ("error", &reason)]);
                    }
                }

                SwarmEvent::NewListenAddr { address, .. } => {
                    emit(&config, "listening", &format!("listening on {address}"),
                         &[("addr", &address.to_string())]);
                }

                // A listener that fails to start is usually this machine having
                // no route for that address family, which is ordinary. Reported
                // rather than swallowed, because "nobody can reach me" and
                // "I never opened the door" look identical from outside.
                SwarmEvent::ListenerError { error, .. } => {
                    emit(&config, "listen-failed", &format!("{error}"),
                         &[("error", &error.to_string())]);
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                    if !config.mdns { continue }
                    for (peer, addr) in peers {
                        // A node announces itself on every interface it holds,
                        // so it discovers its own advertisements and would
                        // otherwise dial itself and enter its own membership
                        // list. That is quietly fatal: the takeover rule asks
                        // whether this node's identifier is lower than every
                        // member's, and it is never lower than its own, so a
                        // node that sees itself can never continue a job.
                        if peer == me { continue }
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        let _ = swarm.dial(addr);
                    }
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. }
                )) => {
                    let Some(source) = message.source else { continue };
                    // Belt as well as braces: whatever route a message took, a
                    // node must never treat itself as a peer.
                    if source == me { continue }
                    let Some(parsed) = parse_message(&message.data) else { continue };

                    // Anything heard from a peer proves it is alive, whatever it
                    // had to say. Counting only heartbeats would let a node
                    // sending a steady stream of checkpoints be declared dead.
                    let label = match &parsed {
                        Message::Heartbeat { label }
                        | Message::Departing { label }
                        | Message::Checkpoint { label, .. }
                        | Message::Observed { label, .. } => label.clone(),
                    };
                    let known = members.contains_key(&source);
                    if !known && !matches!(parsed, Message::Departing { .. }) {
                        members.insert(source, Member {
                            label: label.clone(),
                            joined: Instant::now(),
                            last_heard: Instant::now(),
                            disconnected: false,
                        });
                        emit(&config, "joined", &format!("{label} joined"),
                             &[("peer_id", &source.to_string()), ("label", &label)]);
                        // Answer immediately rather than waiting for the next
                        // interval. Until two nodes have heard from each other
                        // they each believe they are alone, and a node that
                        // believes it is alone will continue a job that someone
                        // else is also continuing.
                        counter += 1;
                        let _ = swarm.behaviour_mut().gossipsub
                            .publish(topic.clone(), encode_heartbeat(&config.label, counter));
                    } else if let Some(member) = members.get_mut(&source) {
                        member.last_heard = Instant::now();
                        member.disconnected = false;
                    }

                    match parsed {
                        Message::Heartbeat { .. } => {}

                        Message::Departing { .. } => {
                            if let Some(member) = members.remove(&source) {
                                // An announced departure needs no detection
                                // window: the mesh knows at once and can hand
                                // work over before anything stalls.
                                emit(&config, "left",
                                     &format!("{} left, announced", member.label),
                                     &[("peer_id", &source.to_string()),
                                       ("label", &member.label),
                                       ("was_present_ms",
                                        &Instant::now().duration_since(member.joined)
                                            .as_millis().to_string())]);
                            }
                            if let Some(w) = work.as_mut() {
                                if w.owner == Some(source) {
                                    w.owner = None;
                                    w.owner_lost_at = Some(Instant::now());
                                    take_over_if_ours(&config, me, w, &members);
                                }
                            }
                        }

                        Message::Observed { peer, addr, .. } => {
                            if peer == me {
                                // Hearing ourselves described is how a node
                                // behind translation learns its own address —
                                // there is no other way to find out, and no
                                // server was asked.
                                if my_address.as_ref() != Some(&addr) {
                                    emit(&config, "my-address",
                                         &format!("{label} sees this node at {addr}"),
                                         &[("addr", &addr.to_string()),
                                           ("observed_by", &label)]);
                                    my_address = Some(addr);
                                }
                                continue;
                            }
                            if swarm.is_connected(&peer) || pending.contains(&addr) {
                                continue;
                            }
                            emit(&config, "learned-peer",
                                 &format!("{label} says {peer} is at {addr}"),
                                 &[("peer_id", &peer.to_string()), ("addr", &addr.to_string()),
                                   ("from", &label)]);
                            // Dial immediately as well as adding to the retry
                            // list. Everyone who heard this is dialling now,
                            // and arriving together is the entire point.
                            let _ = swarm.dial(addr.clone());
                            pending.push(addr);
                        }

                        Message::Checkpoint { tick, state, .. } => {
                            let Some(w) = work.as_mut() else { continue };

                            // Somebody is running it, so this node has no cause
                            // to claim it.
                            w.claim_after = None;

                            if w.owner == Some(me) {
                                // Two nodes running the same job, which a
                                // partition produces whenever both sides
                                // continue. Resolved by a rule both apply
                                // identically — deferring to whoever sent last
                                // would make each yield to the other in turn,
                                // and the job would stop entirely.
                                if !Work::theirs((w.tick, &me), (tick, &source)) {
                                    continue;
                                }
                                emit(&config, "yielded",
                                     &format!("{label} is further along, so it keeps the job"),
                                     &[("peer_id", &source.to_string()), ("label", &label),
                                       ("their_tick", &tick.to_string()),
                                       ("our_tick", &w.tick.to_string())]);
                            } else if w.owner != Some(source) {
                                emit(&config, "job-owner",
                                     &format!("{label} is running the job"),
                                     &[("peer_id", &source.to_string()), ("label", &label)]);
                            }

                            w.owner = Some(source);
                            w.owner_lost_at = None;
                            // Older checkpoints can arrive late; taking one
                            // would silently undo completed work.
                            if tick >= w.tick {
                                w.tick = tick;
                                w.state = state;
                            }
                        }
                    }
                }

                SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                    // Only the last connection closing means the transport is
                    // fully gone; a node may hold several at once, commonly one
                    // over QUIC and one over TCP.
                    if num_established == 0 {
                        if let Some(member) = members.get_mut(&peer_id) {
                            member.disconnected = true;
                        }
                        // Stop telling others where to find a peer we can no
                        // longer find ourselves.
                        seen_at.remove(&peer_id);

                        // And start trying to reach it again. A connection
                        // ending is the ordinary case — a laptop sleeps, a
                        // hotspot cycles, a router reboots — and a mesh that
                        // treats it as final is a mesh that quietly shrinks by
                        // one every time anything happens.
                        if let Some(addresses) = reached_by.get(&peer_id) {
                            let mut resumed = 0;
                            for addr in addresses {
                                if !pending.contains(addr) {
                                    pending.push(addr.clone());
                                    resumed += 1;
                                }
                            }
                            if resumed > 0 {
                                dial_attempts = 0;
                                emit(&config, "redialling",
                                     &format!("lost a peer; trying its {resumed} known address(es) again"),
                                     &[("peer_id", &peer_id.to_string()),
                                       ("addresses", &resumed.to_string())]);
                            }
                        }
                    }
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Ping(ping::Event {
                    peer, result: Ok(rtt), ..
                })) => {
                    emit(&config, "rtt", &format!("{peer} round trip {rtt:?}"),
                         &[("peer_id", &peer.to_string()),
                           ("rtt_ms", &rtt.as_millis().to_string())]);
                }

                _ => {}
            }
        }
    }
}

/// Turns "produce widget 2" into two widgets with identities of their own.
///
/// The serial is derived from the job's code, the tick, and which item of that
/// tick it is — never assigned by anyone. So when two nodes briefly continue
/// the same job, they do not make two widgets each that someone must later
/// reconcile: they make *the same* widgets, and recording one twice records it
/// once. There is nothing to deduplicate because there is no duplicate.
fn record_production(config: &Config, work: &mut Work, effects: &str) {
    for line in effects.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("produce") {
            continue;
        }
        let Some(kind) = parts.next() else { continue };
        let count: u32 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(1);

        let mut fresh = 0;
        for ordinal in 0..count {
            let serial = ledger::Serial::derive(work.job.id(), work.tick, kind, ordinal);
            if work.ledger.record(kind, serial) {
                fresh += 1;
            }
        }

        emit(
            config,
            "produced",
            &format!(
                "{count} {kind}, {fresh} new — {} in the ledger",
                work.ledger.count(kind)
            ),
            &[
                ("kind", kind),
                ("claimed", &count.to_string()),
                ("new", &fresh.to_string()),
                ("total", &work.ledger.count(kind).to_string()),
                ("tick", &work.tick.to_string()),
            ],
        );
    }
}

/// Continues a job whose owner has gone, if this node is the one that should.
///
/// No handshake and no agreement: every survivor holds the same membership and
/// applies the same rule, so they arrive at the same answer independently. The
/// job resumes from the last checkpoint received, which is why checkpoints are
/// broadcast to everyone rather than to a chosen successor — the successor is
/// not known until the moment it is needed.
fn take_over_if_ours(
    config: &Config,
    me: PeerId,
    work: &mut Work,
    members: &HashMap<PeerId, Member>,
) {
    if !Work::should_take_over(&me, members) {
        return;
    }
    let gap = work
        .owner_lost_at
        .map(|at| Instant::now().duration_since(at))
        .unwrap_or_default();

    work.owner = Some(me);
    work.owner_lost_at = None;

    emit(
        config,
        "took-over",
        &format!(
            "continuing the job from tick {} — {:.0} ms after noticing",
            work.tick,
            gap.as_secs_f64() * 1000.0
        ),
        &[
            ("tick", &work.tick.to_string()),
            ("decision_ms", &gap.as_millis().to_string()),
            ("state_bytes", &work.state.len().to_string()),
            ("survivors", &(members.len() + 1).to_string()),
        ],
    );
}

/// Announces departure and waits briefly for the message to propagate.
///
/// The wait is the entire point. Exiting immediately would publish into a
/// socket that closes before anything is sent, and peers would have to discover
/// the departure by timing out — which is exactly what announcing it avoids.
async fn depart(
    swarm: &mut libp2p::Swarm<MeshBehaviour>,
    topic: &gossipsub::IdentTopic,
    config: &Config,
) -> Result<()> {
    let published = swarm
        .behaviour_mut()
        .gossipsub
        .publish(topic.clone(), encode_departing(&config.label))
        .is_ok();

    emit(
        config,
        "departing",
        if published {
            "announced departure, draining"
        } else {
            "departing, but nobody was listening"
        },
        &[("announced", if published { "true" } else { "false" })],
    );

    // Give the port back. It would lapse on its own, but leaving a door open in
    // somebody's router after leaving is untidy in the same way as vanishing
    // without saying goodbye.
    if config.map_port {
        let _ = portmap::release(config.port).await;
    }

    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            _ = swarm.select_next_some() => {}
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    Ok(())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Prints an event as prose or as one JSON record, matching the probe's habit
/// of being readable by a person and parseable by a script from the same run.
fn emit(config: &Config, kind: &str, prose: &str, fields: &[(&str, &str)]) {
    if !config.json {
        println!("[{}] {}", config.label, prose);
        return;
    }
    let mut out = format!(
        "{{\"ts_unix_ms\":{},\"node\":{},\"event\":{}",
        now_millis(),
        json_string(&config.label),
        json_string(kind)
    );
    for (key, value) in fields {
        out.push_str(&format!(",{}:{}", json_string(key), json_string(value)));
    }
    out.push('}');
    println!("{out}");
}

fn json_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keepalive_leaves_margin_on_the_worst_measured_connection() {
        // The mobile connection tested forgot an idle mapping between 120s and
        // 300s. A keepalive without real margin is one dropped packet away from
        // a connection that dies with no error anywhere.
        let keepalive = Duration::from_millis(DEFAULT_KEEPALIVE_MS);
        let shortest_confirmed_safe = Duration::from_secs(120);
        assert!(keepalive * 2 <= shortest_confirmed_safe);
    }

    #[test]
    fn keepalive_and_heartbeat_are_separate_settings() {
        // They serve opposite needs — detection wants frequent, mapping upkeep
        // wants rare — and collapsing them means one silently loses.
        assert!(Duration::from_millis(DEFAULT_KEEPALIVE_MS)
            > Duration::from_millis(DEFAULT_HEARTBEAT_MS) * 10);
    }

    #[test]
    fn different_meshes_never_share_a_topic() {
        // Two meshes on one network must not find each other. Without this a
        // leftover process from an earlier run joins the next one and quietly
        // changes its results — which is exactly how this was discovered.
        assert_ne!(heartbeat_topic("alpha"), heartbeat_topic("beta"));
        assert!(heartbeat_topic("shangri-la").contains("shangri-la"));
    }

    #[test]
    fn messages_survive_a_round_trip() {
        assert_eq!(
            parse_message(&encode_heartbeat("diamond", 42)),
            Some(Message::Heartbeat { label: "diamond".into() })
        );
        assert_eq!(
            parse_message(&encode_departing("diamond")),
            Some(Message::Departing { label: "diamond".into() })
        );
        assert_eq!(
            parse_message(&encode_checkpoint("diamond", 7, b"opaque")),
            Some(Message::Checkpoint {
                label: "diamond".into(),
                tick: 7,
                state: b"opaque".to_vec()
            })
        );
    }

    #[test]
    fn checkpoints_carry_bytes_that_are_not_text() {
        // State is opaque, so the wire format has to survive anything a job
        // chooses to put in it — the reason this is framed rather than
        // delimited.
        let state: Vec<u8> = (0u8..=255).collect();
        let parsed = parse_message(&encode_checkpoint("n", 1, &state)).expect("parses");
        assert_eq!(parsed, Message::Checkpoint { label: "n".into(), tick: 1, state });
    }

    #[test]
    fn ignores_messages_it_does_not_understand() {
        // A later version publishing a new kind must not be mistaken for a
        // heartbeat, which would make a departed node look present.
        assert_eq!(parse_message(&[]), None);
        assert_eq!(parse_message(&[KIND_HEARTBEAT]), None);
        assert_eq!(parse_message(&[99, 1, 0, 0, 0, b'x']), None);
        // A length claiming more than the message holds.
        assert_eq!(parse_message(&[KIND_HEARTBEAT, 255, 0, 0, 0, b'x']), None);
    }

    #[test]
    fn a_goodbye_is_never_mistaken_for_a_heartbeat() {
        // The whole distinction between a few milliseconds and a full detection
        // window rests on these two never being confused.
        assert!(matches!(
            parse_message(&encode_departing("beta")),
            Some(Message::Departing { .. })
        ));
    }

    #[test]
    fn the_lowest_peer_identifier_takes_over_and_only_that_one() {
        // Every survivor runs this against the same membership, so exactly one
        // of them may answer yes — otherwise the job is either dropped by all
        // of them or picked up by all of them.
        let peers: Vec<PeerId> = (0..5).map(|_| PeerId::random()).collect();
        let mut sorted = peers.clone();
        sorted.sort();

        let member = |label: &str| Member {
            label: label.into(),
            joined: Instant::now(),
            last_heard: Instant::now(),
            disconnected: false,
        };

        let mut winners = 0;
        for me in &peers {
            let others: HashMap<PeerId, Member> = peers
                .iter()
                .filter(|p| *p != me)
                .map(|p| (*p, member("peer")))
                .collect();
            if Work::should_take_over(me, &others) {
                winners += 1;
                assert_eq!(me, &sorted[0], "the wrong node volunteered");
            }
        }
        assert_eq!(winners, 1, "exactly one node must take over");
    }

    #[test]
    fn the_node_further_along_keeps_the_job() {
        let a = PeerId::random();
        let b = PeerId::random();
        // Whoever has done more work keeps it: their ticks are the ones that
        // would be thrown away.
        assert!(Work::theirs((10, &a), (12, &b)), "behind, should yield");
        assert!(!Work::theirs((12, &a), (10, &b)), "ahead, should keep");
    }

    #[test]
    fn a_tie_is_broken_the_same_way_on_both_machines() {
        // The rule matters more than the winner. If the two sides disagree,
        // either both yield and the job stops, or neither does and it runs
        // twice for as long as the disagreement lasts.
        let mut peers = vec![PeerId::random(), PeerId::random()];
        peers.sort();
        let (low, high) = (&peers[0], &peers[1]);
        assert!(Work::theirs((5, high), (5, low)), "high yields to low");
        assert!(!Work::theirs((5, low), (5, high)), "low keeps it");
    }

    #[test]
    fn a_node_that_somehow_sees_itself_is_still_eligible() {
        // Self-discovery over the local network put a node in its own
        // membership list, and `me < me` being false made it permanently
        // unable to continue a job — with no error anywhere to show for it.
        let me = PeerId::random();
        let mut members = HashMap::new();
        members.insert(me, Member {
            label: "me".into(),
            joined: Instant::now(),
            last_heard: Instant::now(),
            disconnected: false,
        });
        assert!(Work::should_take_over(&me, &members));
    }

    #[test]
    fn a_lone_survivor_takes_over() {
        // The last node standing has nobody to lose the comparison to, and the
        // job should continue rather than stop because no election was possible.
        assert!(Work::should_take_over(&PeerId::random(), &HashMap::new()));
    }

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("tab\there"), "\"tab\\u0009here\"");
    }

    #[test]
    fn heartbeat_must_leave_room_for_a_lost_packet() {
        // Guards the invariant parse_args enforces: at least two heartbeats
        // must fit inside the detection window, so one dropped packet cannot
        // make a healthy machine look dead.
        let heartbeat = Duration::from_millis(DEFAULT_HEARTBEAT_MS);
        let detect = Duration::from_millis(DEFAULT_DETECT_MS);
        assert!(heartbeat * 2 <= detect);
    }
}
