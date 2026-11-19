//! Phase 0 mesh node.
//!
//! Forms an overlay with other nodes, keeps track of who is in it, and notices
//! when someone disappears — distinguishing a machine that left cleanly from
//! one that was unplugged. On that membership it runs one sandboxed job (see
//! `job`), checkpoints it to every peer each tick, continues it on another node
//! when its host vanishes — on suspicion, before the loss is confirmed — and
//! keeps a grow-only inventory of what the job made (see `ledger`).
//!
//! Membership came first, because everything else is built on knowing who is
//! here; the rest is measured in `docs/phase-0-plan.md`.

mod dialing;
mod job;
mod ledger;
mod peerbook;
mod portmap;
mod upnp;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::SwarmEvent;
use libp2p::{autonat, dcutr, gossipsub, identify, mdns, noise, ping, relay, tcp, yamux, Multiaddr, PeerId};

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
///
/// It names the job it is a checkpoint of. Kind 3 did not, so a node holding
/// a different job file accepted another job's state as its own (review
/// finding 5). Kind 3 is retired and ignored; a new kind rather than a longer
/// kind 3, so that a node built before this ignores the new message instead
/// of misreading a job id as a tick.
const KIND_CHECKPOINT: u8 = 9;

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

/// "These things now exist."
///
/// The inventory does not travel in the checkpoint, and the reason is size: the
/// checkpoint goes out five times a second so that a takeover costs almost no
/// replayed work, and a third of a million items would not fit in it or in any
/// sane message. So the ledger is replicated the way a grow-only set wants to
/// be — each item announced once, as it is made, and merged by union wherever
/// it lands.
///
/// Union is why this needs no ordering, no acknowledgement, and no memory of
/// who has been told what. A delta heard twice is a delta heard once, and one
/// heard late is not late. The same message also carries pieces of an existing
/// ledger during a repair, because a chunk of history and a fresh production
/// are the same thing to a set.
const KIND_PRODUCED: u8 = 5;

/// "This is everything I hold, in 32 bytes."
///
/// Deltas alone converge only for nodes that were listening. Anyone offline
/// when an item was announced never hears of it again, because gossip does not
/// retain. So nodes periodically publish a summary of their set, and a
/// disagreement is what starts a repair.
///
/// Kind 6 was a single hash of the whole set. It could say only *that* two
/// nodes differed, so every difference was answered by sending everything —
/// and on a set that grows every tick, two healthy nodes always differ by
/// whatever is in flight. On 2026-09-13 diamond offered yoinkerz its entire
/// inventory 537 times in nine hours, to fix a gap of 88 items that the repair
/// never delivered, and the phone hotspot carried 7.54 GB. Kind 6 is retired
/// and ignored; the number is not reused.
///
/// A summary is 256 cells — count and XOR — one per bucket of the set by
/// serial. About 3 KB.
const KIND_LEDGER_SUMMARY: u8 = 7;

/// The 256 cells inside one bucket, addressed to one peer.
///
/// Sent by a node that saw that bucket differ in a peer's summary. The peer
/// answers with the items in the cells that differ, and — if this was not
/// itself an answer — its own cells for the bucket, so the first node can send
/// what the peer lacks. Two messages each way at most; no conversation.
const KIND_LEDGER_BUCKET: u8 = 8;

/// Buckets examined per repair round.
///
/// The bound on how much one round can cost. A bucket differing by a few items
/// costs a few kilobytes, so this is really the bound on catching up from
/// nothing: 32 buckets of up to 4,096 items is about 5 MB a round, and an empty
/// node is full after eight. Replayed on diamond's 1.7-million-item ledger with
/// 88 items missing — spread, being hashes, across 74 buckets — eight a round
/// took ten minutes to repair what 32 repairs in three.
const REPAIR_BUCKETS_PER_ROUND: usize = 32;

/// Items sent for one bucket in one answer.
///
/// Whole cells are sent or not at all, so a cell that did not fit is still
/// different next round and is sent then. The old repair published one burst
/// of 2,600 messages; the queue dropped the same tail every time, which is how
/// 88 items stayed missing through 537 attempts.
const REPAIR_ITEMS_PER_BUCKET: usize = 4_096;

/// How many items to put in one message, at most.
///
/// Gossipsub refuses anything oversized, and a refused repair is invisible —
/// it does not fail loudly, it simply never arrives. A count alone did not
/// bound the size: 512 items of a 256-byte kind is 150 KB against a 64 KiB cap
/// (review finding 9). So chunks are cut by encoded size as well; see
/// [`chunks_that_fit`].
const LEDGER_CHUNK: usize = 512;

/// The largest message gossipsub will carry, set explicitly rather than
/// inherited, because every size limit below is derived from it.
const GOSSIP_MAX_BYTES: usize = 65_536;

/// Room left for everything a message carries besides its payload: the
/// label, the kind, the signature and key, the topic and sequence number.
const GOSSIP_ENVELOPE: usize = 2_048;

/// The largest job state a checkpoint can carry. A job whose state outgrows
/// it still runs, but no standby could ever receive it, so a takeover would
/// start from nothing; that is treated as the job failing.
///
/// What a checkpoint carries besides the state is the kind, the label and its
/// length, the job's hash, the tick and the state's length: 49 bytes plus the
/// label, rounded up to 64.
const MAX_CHECKPOINT_STATE: usize = GOSSIP_MAX_BYTES - GOSSIP_ENVELOPE - MAX_LABEL - 64;

/// The longest label a node may have. It rides in every message.
const MAX_LABEL: usize = 64;

/// The most items one tick may produce. Fuel bounds the job's instructions,
/// not the host's: `produce widget 4294967295` is a few bytes of effect and
/// four billion serials, hashes and set insertions afterwards (review
/// finding 2). The factory makes at most a handful per tick.
const MAX_PRODUCED_PER_TICK: u64 = 4_096;

/// How often nodes summarise what they hold.
///
/// Rarely, because agreement is the normal case and this is the message that
/// confirms it. The deltas do the actual replicating; this only catches what
/// they missed.
const DIGEST_EVERY: Duration = Duration::from_secs(60);

/// The least time between starting two repairs.
///
/// Every node that hears a summary it disagrees with wants to start one.
/// Without a floor, a node joining an established mesh would draw a round from
/// everybody at once — the connection storm again, in different clothes. One
/// round, then quiet long enough to find out whether it worked. Only starting
/// is limited: answering a bucket someone asked about is the second half of a
/// round already paid for.
const REPAIR_NO_SOONER_THAN: Duration = Duration::from_secs(30);

/// Whether enough time has passed to start another repair round.
fn repair_allowed(last: &mut Option<Instant>) -> bool {
    let now = Instant::now();
    if last.is_none_or(|when| now.duration_since(when) >= REPAIR_NO_SOONER_THAN) {
        *last = Some(now);
        return true;
    }
    false
}

/// How often a node repeats what it can see, for anyone who has since arrived.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);

/// How often to write a line about work being done.
///
/// A job ticking five times a second produces some 432,000 events a day, and
/// left unchecked they were 100% of a log that reached 192 MB in ten days. They
/// are also the least informative thing in it: a factory producing widgets is
/// the steady hum, not an event. One line a minute says the same thing, and
/// leaves the log made of things that actually happened.
const WORK_SUMMARY_EVERY: Duration = Duration::from_secs(60);

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

/// How often, at most, the peer book is written. It changes when a peer is
/// first dialled or named, which is rare once a mesh has settled.
const BOOK_SAVE_EVERY: Duration = Duration::from_secs(10);

/// How recently a peer must have been seen for a restarted node to dial
/// where it last saw it. Generous, because what matters is whether the peer
/// is still trying to reach this node, not how long this node was away:
/// Run F's part 2 lasted 57 minutes.
const RETURN_DIAL_WITHIN: Duration = Duration::from_secs(24 * 3600);

/// The retry timer, built so that a quiet mesh does not store up work for it.
///
/// Its arm in the event loop is guarded by `dials.len() > 0`, so while
/// every peer is reachable the interval is never polled at all.
fn dial_retry_interval() -> tokio::time::Interval {
    let mut retry = tokio::time::interval(DIAL_RETRY);
    // Not the default `Burst`, which hands over every tick that elapsed while
    // nobody was polling. A lost peer is redialled once immediately and then
    // every DIAL_RETRY, however long the mesh was healthy first.
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    retry
}

/// How often a quiet connection is poked to stop a router forgetting it.
///
/// Measured rather than guessed, and governed by the worst connection among
/// the players: the longest silence it is confirmed to survive, halved, so one
/// lost keepalive still lands inside it. At the friend's house (2026-11-14/16,
/// 138 inbound runs) the path into diamond kept an idle flow through 90 s and
/// lost it at 120 s, the shortest yet: not the router, which kept it through
/// 120 s, but diamond's own firewall, whose connection tracking forgets an
/// idle UDP flow at Linux's default 120 s and then refuses what only a reply
/// rule would admit. A player behind a default firewall meets that first.
/// Halving 90 s gives 45 s, and 40 s sits a little under it, as 55 s once sat
/// under the hotspot's 60.
/// (The home connection's "over ten minutes" was never evidence: the test
/// behind it could not see expiry on a router that keeps port numbers.)
///
/// **This is a different job from the heartbeat, and the separation is the
/// point.** Heartbeats run every second because that is what makes an
/// unannounced disappearance visible within three; keepalives run rarely
/// because their only purpose is stopping a router forgetting a path. Today the
/// heartbeats happen to keep mappings alive as a side effect, which is exactly
/// why this needs its own name — the moment detection is slowed, or a player
/// runs this on a metered connection where every packet is a cost, the two
/// requirements pull in opposite directions and one of them silently loses.
const DEFAULT_KEEPALIVE_MS: u64 = 40_000;

#[derive(Debug, PartialEq)]
enum Message {
    /// `run` identifies one start of a node's process. The identity survives a
    /// restart by design (§8.1), which is exactly why it cannot say whether a
    /// peer that went silent and came back had restarted or had merely been
    /// unreachable — and those are different failures that must not be counted
    /// together.
    Heartbeat { label: String, run: Option<u64>, holding: Holding, reachable: Option<bool> },
    Departing { label: String },
    Checkpoint { label: String, job: [u8; 32], tick: u64, state: Vec<u8> },
    Observed { label: String, peer: PeerId, addr: Multiaddr },
    Produced { label: String, items: Vec<(String, ledger::Serial)> },
    LedgerSummary { label: String, held: u64, cells: Vec<ledger::Cell> },
    LedgerBucket { label: String, to: PeerId, bucket: u8, answer: bool, cells: Vec<ledger::Cell> },
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

/// What a member holds, as its heartbeats say.
///
/// The election asks who is lowest among the members, and it used to ask that
/// of every member, including one launched without a job, or with a different
/// one. A lower-identifier member that could not run the job then won every
/// election and did nothing, and the job stopped (review finding 5). Only
/// members holding the same job, by its code hash, are candidates now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Holding {
    /// Not said: no heartbeat heard yet, or a node built before heartbeats
    /// said. Not a candidate until it says.
    Unknown,
    /// In the mesh, holding no job.
    Nothing,
    Job([u8; 32]),
}

fn encode_heartbeat(label: &str, counter: u64, run: u64, job: Option<&[u8; 32]>, reachable: bool) -> Vec<u8> {
    let mut out = vec![KIND_HEARTBEAT];
    put_str(&mut out, label);
    out.extend_from_slice(&counter.to_le_bytes());
    out.extend_from_slice(&run.to_le_bytes());
    // Appended, so a node built before this reads the run and ignores the rest.
    match job {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            out.extend_from_slice(id);
        }
    }
    // Appended again: whether a member has confirmed it can reach this node,
    // which is what makes it worth asking for introductions.
    out.push(u8::from(reachable));
    out
}

/// Whether a heartbeat says its sender can be reached. Unsaid is no.
fn member_reachable(message: &Message) -> bool {
    matches!(message, Message::Heartbeat { reachable: Some(true), .. })
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

fn encode_checkpoint(label: &str, job: &[u8; 32], tick: u64, state: &[u8]) -> Vec<u8> {
    let mut out = vec![KIND_CHECKPOINT];
    put_str(&mut out, label);
    out.extend_from_slice(job);
    out.extend_from_slice(&tick.to_le_bytes());
    out.extend_from_slice(&(state.len() as u32).to_le_bytes());
    out.extend_from_slice(state);
    out
}

fn encode_produced(label: &str, items: &[(String, ledger::Serial)]) -> Vec<u8> {
    let mut out = vec![KIND_PRODUCED];
    put_str(&mut out, label);
    let body = ledger::encode_items(items.iter().map(|(kind, serial)| (kind.as_str(), serial)));
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn put_cells(out: &mut Vec<u8>, cells: &[ledger::Cell]) {
    for cell in cells {
        out.extend_from_slice(&cell.count.to_le_bytes());
        out.extend_from_slice(&cell.xor.to_le_bytes());
    }
}

/// Exactly FANOUT cells, or nothing: a short summary compared against a full
/// one would report every missing position as a difference.
fn take_cells(bytes: &[u8], at: &mut usize) -> Option<Vec<ledger::Cell>> {
    (0..ledger::FANOUT)
        .map(|_| Some(ledger::Cell { count: take_u32(bytes, at)?, xor: take_u64(bytes, at)? }))
        .collect()
}

fn encode_ledger_summary(label: &str, held: u64, cells: &[ledger::Cell]) -> Vec<u8> {
    let mut out = vec![KIND_LEDGER_SUMMARY];
    put_str(&mut out, label);
    out.extend_from_slice(&held.to_le_bytes());
    put_cells(&mut out, cells);
    out
}

fn encode_ledger_bucket(label: &str, to: &PeerId, bucket: u8, answer: bool, cells: &[ledger::Cell]) -> Vec<u8> {
    let mut out = vec![KIND_LEDGER_BUCKET];
    put_str(&mut out, label);
    let id = to.to_bytes();
    out.extend_from_slice(&(id.len() as u32).to_le_bytes());
    out.extend_from_slice(&id);
    out.push(bucket);
    out.push(answer as u8);
    put_cells(&mut out, cells);
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
        KIND_HEARTBEAT => {
            // The counter is stepped over, not used. The run after it is
            // optional, so a node built before runs existed is still heard as
            // alive rather than dropped as malformed.
            let run = take_u64(bytes, &mut at).and_then(|_| take_u64(bytes, &mut at));
            let (holding, after) = match bytes.get(at) {
                Some(0) => (Holding::Nothing, Some(at + 1)),
                Some(1) => match bytes.get(at + 1..at + 33) {
                    Some(id) => (Holding::Job(id.try_into().expect("32 bytes")), Some(at + 33)),
                    None => (Holding::Unknown, None),
                },
                _ => (Holding::Unknown, None),
            };
            // Absent from a node built before it was said: not reachable, so
            // never asked to introduce anyone.
            let reachable = after.and_then(|at| match bytes.get(at) {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            });
            Some(Message::Heartbeat { label, run, holding, reachable })
        }
        KIND_DEPARTING => Some(Message::Departing { label }),
        KIND_CHECKPOINT => {
            let job: [u8; 32] = bytes.get(at..at + 32)?.try_into().ok()?;
            at += 32;
            let tick = take_u64(bytes, &mut at)?;
            let state = take_bytes(bytes, &mut at)?;
            Some(Message::Checkpoint { label, job, tick, state })
        }
        KIND_OBSERVED => {
            let peer = PeerId::from_bytes(&take_bytes(bytes, &mut at)?).ok()?;
            let addr: Multiaddr = String::from_utf8(take_bytes(bytes, &mut at)?)
                .ok()?
                .parse()
                .ok()?;
            Some(Message::Observed { label, peer, addr })
        }
        KIND_PRODUCED => {
            let items = ledger::decode_items(&take_bytes(bytes, &mut at)?);
            // An empty batch is not worth announcing, and accepting one would
            // mean a malformed message and a meaningless one look the same.
            if items.is_empty() {
                return None;
            }
            Some(Message::Produced { label, items })
        }
        KIND_LEDGER_SUMMARY => {
            let held = take_u64(bytes, &mut at)?;
            let cells = take_cells(bytes, &mut at)?;
            Some(Message::LedgerSummary { label, held, cells })
        }
        KIND_LEDGER_BUCKET => {
            let to = PeerId::from_bytes(&take_bytes(bytes, &mut at)?).ok()?;
            let bucket = *bytes.get(at)?;
            let answer = *bytes.get(at + 1)? != 0;
            at += 2;
            let cells = take_cells(bytes, &mut at)?;
            Some(Message::LedgerBucket { label, to, bucket, answer, cells })
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
/// `docs/phase-0-plan.md` fixed detection at 2–3 s before any measurement.
/// This was three seconds, which could not meet that: the sweep runs every
/// quarter window, so a loss is noticed between 1.0 and 1.25 windows after the
/// last word — 3.0 to 3.75 s, over the threshold by construction. Two seconds
/// lands at 2.0 to 2.5 s. Changed before the timed runs, not after them, so the
/// threshold stayed fixed and only the setting moved.
///
/// The cost of deciding early is migrating work every time a link stutters.
/// The timed runs count those false alarms rather than assuming there are none.
const DEFAULT_DETECT_MS: u64 = 2_000;

#[derive(libp2p::swarm::NetworkBehaviour)]
struct MeshBehaviour {
    /// Introduces two members that cannot reach each other, so that DCUtR can
    /// time their punch. On every node, because behaviours are fixed when the
    /// swarm is built and reachability changes while it runs; who is *asked*
    /// to introduce is decided by the heartbeat's reachable flag. Default
    /// limits: see [`relay_config`].
    relay: relay::Behaviour,
    relay_client: relay::client::Behaviour,
    /// Coordinated hole punching over an introduction.
    dcutr: dcutr::Behaviour,
    /// Whether members can reach this node: a member dials back, and only a
    /// connection that arrives counts. Members are the only servers asked.
    autonat_client: autonat::v2::client::Behaviour,
    autonat_server: autonat::v2::server::Behaviour,
    /// Carries heartbeats. Gossip rather than direct messaging, so that
    /// membership does not require every node to hold a connection to every
    /// other — §11.6 rules out a full mesh.
    gossipsub: gossipsub::Behaviour,
    /// Finds other nodes on the same local network. Genuinely useful for the
    /// container rig and for a player's own machines, and incapable of finding
    /// anything across the internet (§11.6).
    ///
    /// Absent, not idle, under `--no-mdns`: see [`mdns_behaviour`].
    mdns: Toggle<mdns::tokio::Behaviour>,
    /// Exchanges addresses and protocol versions on connect.
    identify: identify::Behaviour,
    /// Round-trip timing, which is the latency floor for any takeover.
    ping: ping::Behaviour,
}

/// Local discovery, or nothing at all.
///
/// This used to keep the behaviour under `--no-mdns` with a year-long query
/// interval, on the theory that it would then never speak. It still spoke.
/// libp2p-mdns opens a multicast socket and joins 224.0.0.251 on every
/// interface with an address, sends its first query as soon as an interface
/// appears, and answers queries regardless of the interval. Found on diamond,
/// 2026-11-14: the node held two sockets on 5353 and had joined the mDNS group
/// on the friend's LAN and on the LTE interface, which is the management link
/// the mesh must never touch. `Toggle` with `None` opens no sockets at all.
fn mdns_behaviour(enabled: bool, me: PeerId) -> std::io::Result<Toggle<mdns::tokio::Behaviour>> {
    Ok(Toggle::from(if enabled {
        Some(mdns::tokio::Behaviour::new(mdns::Config::default(), me)?)
    } else {
        None
    }))
}

struct Config {
    label: String,
    listen: Vec<Multiaddr>,
    dial: Vec<Multiaddr>,
    heartbeat: Duration,
    detect: Duration,
    /// How long an owner's silence starts a standby continuing its job, before
    /// anyone is presumed gone. `None` runs the node exactly as it ran before
    /// speculation existed.
    speculate_after: Option<Duration>,
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
    /// Where this node's identity is kept between runs.
    ///
    /// §8.1 makes identity a persistent keypair, and it was being generated
    /// fresh every start — so a node returning from a restart was a stranger
    /// who happened to be at a familiar address. Nothing could recognise it,
    /// no peer could remember it, and a cache of peers was a cache of
    /// identities that would never appear again.
    identity: String,
    /// Where the inventory lives between runs.
    ///
    /// Kept beside the identity because it is the same kind of thing: the part
    /// of a node that is not recreated by starting it again. Held only in
    /// memory, a ledger was a tally of the current session wearing the name of
    /// a permanent record.
    ledger: String,
    /// Where remembered peers live between runs. See [`peerbook`].
    peer_book: String,
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
    /// Who that owner was, so the takeover can name what it continued. A
    /// summary matching takeovers to disruptions otherwise has only timing to
    /// go on, and two disruptions close together can share one takeover
    /// (review finding 14).
    owner_lost_label: Option<String>,
    /// Items the ledger holds that could not yet be written down. See
    /// [`ledger::persist`].
    unsaved: Vec<(String, ledger::Serial)>,
    /// When the last checkpoint arrived from whoever was running the job.
    last_checkpoint_at: Option<Instant>,
    /// Set by a takeover, cleared by the first tick after it.
    resuming: Option<Resuming>,
    /// Set while continuing a job whose owner is suspected gone but has not
    /// been declared so. See [`Speculating`].
    speculating: Option<Speculating>,
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

    /// Whether `me` is the lowest identifier among the peers given.
    ///
    /// `peer != me` matters as much as the comparison. If this node ever
    /// appears in its own membership — which self-discovery over the local
    /// network will cause — then without it the test is `me < me`, which is
    /// false, and this node silently becomes ineligible forever.
    fn lowest_among<'a>(me: &PeerId, peers: impl Iterator<Item = &'a PeerId>) -> bool {
        peers.filter(|peer| *peer != me).all(|peer| me < peer)
    }

    /// Whether this node is the one to continue a job whose owner has gone.
    ///
    /// There is no coordination here and no vote. Every survivor runs this
    /// against the same membership and reaches the same answer on its own,
    /// which is why exactly one of them acts — and why nothing has to be
    /// exchanged first. Ties go to the lower identifier: arbitrary, and that
    /// is the entire virtue, because it is the same arbitrary answer on every
    /// machine. Arranging a vote would cost more messages, and more of the
    /// gap a player is sitting through, than the takeover it arranges.
    fn should_take_over(me: &PeerId, members: &HashMap<PeerId, Member>, job: &[u8; 32]) -> bool {
        Self::lowest_among(me, Self::able(members, job))
    }

    /// Members that could continue this job: those whose heartbeats say they
    /// hold the same code. See [`Holding`].
    fn able<'a>(
        members: &'a HashMap<PeerId, Member>,
        job: &'a [u8; 32],
    ) -> impl Iterator<Item = &'a PeerId> {
        members
            .iter()
            .filter(move |(_, m)| m.holding == Holding::Job(*job))
            .map(|(peer, _)| peer)
    }

    /// Whether to start continuing a job whose owner has gone quiet but is not
    /// yet presumed gone.
    ///
    /// The same election the takeover uses, run early and with the suspect
    /// excluded — it is precisely the node whose absence is being assumed, and
    /// counting it would, in a two-node mesh, make the survivor ineligible
    /// whenever its identifier is the higher of the two.
    fn should_speculate(
        me: &PeerId,
        owner: &PeerId,
        members: &HashMap<PeerId, Member>,
        now: Instant,
        after: Duration,
        job: &[u8; 32],
    ) -> bool {
        members
            .get(owner)
            .is_some_and(|suspect| now.duration_since(suspect.last_heard) >= after)
            && Self::lowest_among(me, Self::able(members, job).filter(|peer| *peer != owner))
    }
}

/// The outcome of a checkpoint arriving while this node is speculating.
///
/// A checkpoint is the answer to the suspicion that started the speculation,
/// so it is resolved by the same rule two owners always use rather than a
/// special case of its own — see [`resolve_speculation`].
#[derive(Debug, PartialEq)]
enum Speculation {
    /// The sender was further along (or tied with the lower identifier), so
    /// this node's speculative run is discarded. `ticks_wasted` is counted
    /// from where the speculation began, not from zero — a speculation that
    /// never got a tick in wasted none.
    Abandoned { ticks_wasted: u64 },
    /// This node was further along, so it keeps the job; the sender yields
    /// when it hears this node's next checkpoint.
    Outran,
}

/// Whether the suspect itself has disproved the suspicion.
///
/// This is what [`Speculating::about`] is for: a suspicion names whose silence
/// it rests on, so it can be checked against that peer rather than against
/// whoever happens to be quiet now.
///
/// Without this check speculation is left only by a checkpoint or a takeover,
/// and a peer can send neither while its heartbeats arrive and its checkpoints
/// are dropped — a lossy link rather than a dead machine, which is what a real
/// home connection produces and what a loopback rig will never show. Such a
/// suspect never vanishes, so no takeover ever confirms the suspicion, and
/// never checkpoints, so nothing ever answers it. The speculator would run as
/// a permanent shadow owner, always a little behind the owner it can never
/// displace — and the owner, hearing nobody ahead of it, would never yield.
fn suspicion_disproved(spec: &Speculating, members: &HashMap<PeerId, Member>) -> bool {
    members
        .get(&spec.about)
        .is_some_and(|suspect| suspect.last_heard > spec.since)
}

/// Ends a speculation that is not going to become this node's job, and says
/// how far it had got.
///
/// The resume goes with it. A speculation that had not yet ticked still owes
/// one, and nothing ticks after this, so leaving it armed would report a
/// resume for work that never resumed — the same reasoning as the checkpoint
/// case, which is why both exits use this rather than each clearing what it
/// remembers to clear.
fn stand_down(
    speculating: &mut Option<Speculating>,
    resuming: &mut Option<Resuming>,
) -> Option<Speculating> {
    let spec = speculating.take();
    if spec.is_some() {
        *resuming = None;
    }
    spec
}

/// Which of a speculating node and the checkpoint's sender keeps the job.
///
/// Deliberately just [`Work::theirs`] plus the arithmetic for how much was
/// wasted: a speculator is not a special case of the two-owners rule, it is
/// an instance of it. Kept separate from the `Message::Checkpoint` handler so
/// the decision can be tested without driving the event loop.
fn resolve_speculation(
    spec: &Speculating,
    mine: (u64, &PeerId),
    theirs: (u64, &PeerId),
) -> Speculation {
    if Work::theirs(mine, theirs) {
        Speculation::Abandoned { ticks_wasted: mine.0.saturating_sub(spec.from_tick) }
    } else {
        Speculation::Outran
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
    /// Which start of the peer's process has been heard from, once a heartbeat
    /// has said. See [`Message::Heartbeat`].
    run: Option<u64>,
    /// Which job it holds, once a heartbeat has said. See [`Holding`].
    holding: Holding,
    /// Whether its heartbeat says a member has confirmed it can be reached.
    /// False until said, and for a node built before it was said.
    reachable: bool,
}

/// Peers declared gone, remembered so their return can be classified.
///
/// A peer that comes back after being declared gone either restarted — the
/// takeover was needed — or never stopped, and was only unreachable for longer
/// than the detection window. The second is a false alarm: work migrated that
/// did not have to. Counting both as "vanished" would hide the cost of setting
/// detection short, which is the exact trade the timed runs exist to measure.
#[derive(Default)]
struct Absences {
    gone: HashMap<PeerId, (u64, Instant)>,
}

#[derive(Debug, PartialEq)]
struct Return {
    restarted: bool,
    away: Duration,
}

impl Absences {
    /// Only a peer whose run is known can be judged on return; one that
    /// vanished before its first heartbeat arrived is simply not remembered.
    fn vanished(&mut self, peer: PeerId, run: Option<u64>, at: Instant) {
        if let Some(run) = run {
            self.gone.insert(peer, (run, at));
        }
    }

    /// Reports a return once, on the first heartbeat after it.
    fn heard(&mut self, peer: &PeerId, run: u64, now: Instant) -> Option<Return> {
        let (before, at) = self.gone.remove(peer)?;
        Some(Return { restarted: before != run, away: now.duration_since(at) })
    }
}

/// A takeover between its decision and the first tick run after it.
///
/// The decision is not the recovery. What a player sees is the gap between the
/// last state that reached this node and the first state it produces, and that
/// spans detection, the decision, and waiting for the next tick. Every instant
/// here is on this node's clock, so no measurement subtracts one machine's time
/// from another's — a laptop on a phone hotspot and a box on home broadband do
/// not agree to the millisecond, and the thresholds are in milliseconds.
struct Resuming {
    cause: &'static str,
    decided: Instant,
    last_checkpoint: Option<Instant>,
}

impl Resuming {
    /// (decision to first tick, last checkpoint heard to first tick)
    fn timings(&self, now: Instant) -> (Duration, Option<Duration>) {
        (
            now.duration_since(self.decided),
            self.last_checkpoint.map(|at| now.duration_since(at)),
        )
    }
}

/// A job being continued on suspicion alone.
///
/// The owner has gone quiet but has not been declared gone, so `Work::owner`
/// still points at it: nothing has been decided, and this node is running the
/// job only because a stall costs a player more than duplicate work costs the
/// mesh.
struct Speculating {
    /// Whose silence started it.
    about: PeerId,
    since: Instant,
    /// The tick reached when it began, so work done on a suspicion that turns
    /// out to be wrong can be counted rather than guessed at.
    from_tick: u64,
}

fn main() -> Result<()> {
    // Off unless asked for: the event log on stdout is the record, and this is
    // a diagnostic beside it, for what libp2p explains only in its own logs.
    if let Ok(filter) = std::env::var("MESH_NODE_TRACE") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
    }
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

/// Every setting at its default, as a node started with no flags has them.
fn default_config() -> Config {
    Config {
        label: hostname(),
        listen: Vec::new(),
        dial: Vec::new(),
        heartbeat: Duration::from_millis(DEFAULT_HEARTBEAT_MS),
        detect: Duration::from_millis(DEFAULT_DETECT_MS),
        speculate_after: None,
        json: false,
        job: None,
        job_tick: Duration::from_millis(DEFAULT_JOB_TICK_MS),
        keepalive: Duration::from_millis(DEFAULT_KEEPALIVE_MS),
        mesh: "default".to_string(),
        port: DEFAULT_PORT,
        identity: "/var/lib/mesh-node/identity".to_string(),
        ledger: "/var/lib/mesh-node/ledger".to_string(),
        peer_book: "/var/lib/mesh-node/peers".to_string(),
        map_port: true,
        mdns: true,
        own: false,
    }
}

fn parse_args() -> Result<Config> {
    let mut config = default_config();

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
            "--speculate-after-ms" => {
                let ms: u64 = args.next().context("--speculate-after-ms needs a number")?.parse()?;
                // Zero is off rather than instant. A standby that suspected an
                // owner the moment a packet was late would run the job
                // permanently alongside it.
                config.speculate_after = (ms > 0).then(|| Duration::from_millis(ms));
            }
            "--json" => config.json = true,
            "--job" => config.job = Some(args.next().context("--job needs a path")?),
            "--mesh" => config.mesh = args.next().context("--mesh needs a name")?,
            "--port" => config.port = args.next().context("--port needs a number")?.parse()?,
            "--identity" => config.identity = args.next().context("--identity needs a path")?,
            "--ledger" => config.ledger = args.next().context("--ledger needs a path")?,
            "--peer-book" => config.peer_book = args.next().context("--peer-book needs a path")?,
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

    // A quarter of the detection window, which is also how often the sweep
    // runs, so suspicion is noticed on the first sweep that could notice it.
    // Derived rather than independent: retuning detection retunes this, and a
    // pair of numbers that must stay ordered would eventually be set out of
    // order.
    let speculating_by_default = !std::env::args().any(|a| a == "--speculate-after-ms");
    if speculating_by_default {
        config.speculate_after = Some(config.detect / 4);
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

    // A label rides in every message, so it is part of every size budget.
    if config.label.is_empty() || config.label.len() > MAX_LABEL {
        anyhow::bail!("a label must be 1 to {MAX_LABEL} bytes, not {}", config.label.len());
    }
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
    println!("  --speculate-after-ms <n>");
    println!("                       silence before a standby starts continuing the job, ahead");
    println!("                       of any decision that the owner is gone (default: a quarter");
    println!("                       of --detect-ms). 0 turns speculation off");
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
    println!("  --identity <file>    where to keep this node's identity, so it is the");
    println!("                       same node after a restart (default");
    println!("                       /var/lib/mesh-node/identity)");
    println!("  --ledger <file>      where to keep what this node knows exists, so an");
    println!("                       inventory outlives the process that built it");
    println!("                       (default /var/lib/mesh-node/ledger)");
    println!("  --peer-book <file>   where to remember peers, so a restarted node can find");
    println!("                       its way back in (default /var/lib/mesh-node/peers)");
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

/// The keypair that makes this node the same node it was yesterday.
///
/// Written on first use and read thereafter. A node whose identity file cannot
/// be written still runs — it simply cannot be recognised across a restart,
/// which is worth continuing for and worth saying out loud.
fn load_or_create_identity(config: &Config) -> Result<libp2p::identity::Keypair> {
    use libp2p::identity::Keypair;
    let path = std::path::Path::new(&config.identity);

    if let Ok(bytes) = std::fs::read(path) {
        match Keypair::from_protobuf_encoding(&bytes) {
            Ok(keypair) => {
                emit(config, "identity-loaded",
                     &format!("this node is {} again", keypair.public().to_peer_id()),
                     &[("peer_id", &keypair.public().to_peer_id().to_string()),
                       ("path", &config.identity)]);
                return Ok(keypair);
            }
            // A corrupt file is not a reason to refuse to start, but replacing
            // it silently would change who this node is without saying so.
            Err(error) => {
                emit(config, "identity-unreadable",
                     &format!("{} could not be read ({error}) — making a new identity, \
                               and every peer that remembered this node will not know it",
                              config.identity),
                     &[("path", &config.identity), ("error", &error.to_string())]);
            }
        }
    }

    let keypair = Keypair::generate_ed25519();
    let encoded = keypair
        .to_protobuf_encoding()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, &encoded) {
        Ok(()) => {
            // Readable only by this node: the private key is what makes it
            // this node rather than any other.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            emit(config, "identity-created",
                 &format!("a new identity, kept at {}", config.identity),
                 &[("peer_id", &keypair.public().to_peer_id().to_string()),
                   ("path", &config.identity)]);
        }
        Err(error) => {
            emit(config, "identity-not-saved",
                 &format!("could not write {} ({error}) — this node will be a stranger \
                           after every restart",
                          config.identity),
                 &[("path", &config.identity), ("error", &error.to_string())]);
        }
    }
    Ok(keypair)
}

/// The relay at libp2p's defaults: 2 minutes and 128 KiB per introduction.
fn relay_config() -> relay::Config {
    relay::Config::default()
}

/// How often AutoNAT asks a member to dial this node back. Its own default is
/// every 5 s.
const AUTONAT_PROBE_EVERY: Duration = Duration::from_secs(60);
/// How many of this node's candidate addresses it tests.
const AUTONAT_MAX_CANDIDATES: usize = 4;

/// How long after dialling an address a connection from it still proves
/// nothing about this node's reachability.
const UNSOLICITED_AFTER: Duration = Duration::from_secs(600);

/// The IP address an address names, if it names one.
fn ip_of(addr: &Multiaddr) -> Option<std::net::IpAddr> {
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::Ip4(ip) => Some(std::net::IpAddr::V4(ip)),
        libp2p::multiaddr::Protocol::Ip6(ip) => Some(std::net::IpAddr::V6(ip)),
        _ => None,
    })
}

/// Whether a connection arriving from `remote` proves this node reachable:
/// true unless this node dialled that address within `UNSOLICITED_AFTER`.
///
/// Found in the NAT rig, 2026-11-18: AutoNAT v2's server answers only on
/// connections it received, so a node is only ever tested by peers it dialled.
/// A reachable member is the one everybody dials and that dials nobody, so it
/// was never tested and never introduced anyone. Routers that admit only
/// replies — both of Run F's did — let in a connection from an address this
/// node has not sent to only if its door is open, which is the operator's
/// rule exactly: reachability confirmed by a successful connection.
fn unsolicited(remote: std::net::IpAddr, dialled: &HashMap<std::net::IpAddr, Instant>, now: Instant) -> bool {
    dialled.get(&remote).is_none_or(|at| now.duration_since(*at) > UNSOLICITED_AFTER)
}

/// Whether a connection proves this node reachable: one that arrived, not
/// through an introducer, from an address this node has had no contact with
/// for `UNSOLICITED_AFTER`. Contact is any direct connection either way, not
/// only a dial (review I1): a link that drops and is redialled from the far
/// side comes in through the flow that link left open.
fn proves_reachable(
    endpoint: &libp2p::core::ConnectedPoint,
    contacts: &HashMap<std::net::IpAddr, Instant>,
    now: Instant,
) -> bool {
    match endpoint {
        libp2p::core::ConnectedPoint::Listener { send_back_addr, .. } => {
            !connection_relayed(endpoint) && ip_of(send_back_addr).is_some_and(|ip| unsolicited(ip, contacts, now))
        }
        libp2p::core::ConnectedPoint::Dialer { .. } => false,
    }
}

/// Forgets a closed connection; true when it was the last direct one to
/// `peer`. Review I3: only that is a loss worth hurrying a redial for — not
/// an introduction ending at its limit, not silence through a forwarder.
fn lost_last_direct(
    direct: &mut HashMap<libp2p::swarm::ConnectionId, PeerId>,
    closed: libp2p::swarm::ConnectionId,
    peer: PeerId,
) -> bool {
    direct.remove(&closed).is_some() && !direct.values().any(|p| *p == peer)
}

/// Whether a member has confirmed it can reach this node, at any address.
#[derive(Default)]
struct Reachability {
    confirmed: std::collections::BTreeSet<Multiaddr>,
    /// Withdrawn when the router stopped forwarding, kept to restore when it
    /// forwards again. See [`Reachability::suspend`].
    suspended: Vec<Multiaddr>,
}

/// Whether the internet routes to this IP. Review I2: a sibling on the same
/// home network, or a hairpinned dial, reaches a private address, which says
/// nothing about anyone else reaching it. Documentation ranges count, so the
/// NAT rig's public side is public.
fn is_global(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
              || v4.is_broadcast() || v4.is_multicast()
              || (o[0] == 100 && (o[1] & 0xc0) == 64))
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
              || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80)
        }
    }
}

impl Reachability {
    /// Records a confirmed address; true when this makes the node reachable.
    fn confirm(&mut self, addr: Multiaddr) -> bool {
        // An address through an introducer is someone else's door, not this
        // node's: holding one says nothing about being reachable directly.
        if is_relayed(&addr) || !ip_of(&addr).is_some_and(is_global) {
            return false;
        }
        let was = self.is_reachable();
        self.confirmed.insert(addr);
        !was
    }

    /// Forgets an address; true when the last one goes.
    fn expire(&mut self, addr: &Multiaddr) -> bool {
        let was = self.is_reachable();
        self.confirmed.remove(addr);
        was && !self.is_reachable()
    }

    fn is_reachable(&self) -> bool {
        !self.confirmed.is_empty()
    }

    /// Withdraws every confirmed address, returning them: the router stopped
    /// forwarding, so the door may be shut.
    fn suspend(&mut self) -> Vec<Multiaddr> {
        let withdrawn: Vec<Multiaddr> = std::mem::take(&mut self.confirmed).into_iter().collect();
        self.suspended.extend(withdrawn.iter().cloned());
        withdrawn
    }

    /// Restores what `suspend` withdrew, on the next grant; true when that
    /// makes the node reachable again. Review M1: without this, one failed
    /// renewal ended introductions for good.
    fn restore(&mut self) -> bool {
        let was = self.is_reachable();
        for addr in std::mem::take(&mut self.suspended) {
            self.confirmed.insert(addr);
        }
        !was && self.is_reachable()
    }
}

/// How many reachable members an unreachable node keeps introductions on, so
/// that no single one of them is critical. The operator's call, 2026-11-18.
const MAX_INTRODUCERS: usize = 3;

/// Which members this node should hold introductions on.
///
/// None if this node is reachable itself: others can simply dial it. Otherwise
/// members whose heartbeat says they are reachable and which this node is
/// directly connected to — an introduction rides on that connection. Those
/// already held are kept while they still qualify, so the set does not churn;
/// the lowest identifiers among the rest fill any places left.
fn choose_introducers(
    me_reachable: bool,
    members: &HashMap<PeerId, Member>,
    directly_connected: &HashSet<PeerId>,
    held: &BTreeSet<PeerId>,
    max: usize,
) -> BTreeSet<PeerId> {
    if me_reachable {
        return BTreeSet::new();
    }
    let qualifies = |peer: &PeerId| {
        directly_connected.contains(peer) && members.get(peer).is_some_and(|m| m.reachable)
    };
    let mut chosen: BTreeSet<PeerId> = held.iter().filter(|p| qualifies(p)).copied().take(max).collect();
    let mut others: Vec<PeerId> = members.keys().filter(|p| qualifies(p) && !chosen.contains(p)).copied().collect();
    others.sort();
    for peer in others {
        if chosen.len() >= max {
            break;
        }
        chosen.insert(peer);
    }
    chosen
}

/// How long an introducer that refused, or whose introduction ended in an
/// error, waits before it is asked again.
const INTRODUCER_RETRY: Duration = Duration::from_secs(60);

/// The directly connected members that may be asked to introduce now: all of
/// them except those that refused within `INTRODUCER_RETRY`.
fn eligible_introducers(
    directly_connected: &HashSet<PeerId>,
    refused_at: &HashMap<PeerId, Instant>,
    now: Instant,
) -> HashSet<PeerId> {
    directly_connected
        .iter()
        .filter(|p| refused_at.get(p).is_none_or(|at| now.duration_since(*at) >= INTRODUCER_RETRY))
        .copied()
        .collect()
}

/// Whether an address goes through an introducer.
fn is_relayed(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::P2pCircuit))
}

/// Whether a connection goes through an introducer, from either end: the
/// dialling end sees the circuit in the remote address, the receiving end in
/// its local one (its remote is only `/p2p/<peer>`).
fn connection_relayed(endpoint: &libp2p::core::ConnectedPoint) -> bool {
    match endpoint {
        libp2p::core::ConnectedPoint::Dialer { address, .. } => is_relayed(address),
        libp2p::core::ConnectedPoint::Listener { local_addr, send_back_addr } => {
            is_relayed(local_addr) || is_relayed(send_back_addr)
        }
    }
}

/// The address to listen on for introductions through `relay`, reached at
/// `relay_addr`: `relay_addr/p2p/<relay>/p2p-circuit`, naming the relay once.
fn circuit_listen_addr(relay_addr: &Multiaddr, relay: PeerId) -> Multiaddr {
    use libp2p::multiaddr::Protocol;
    let mut addr = relay_addr.clone();
    if !matches!(addr.iter().last(), Some(Protocol::P2p(p)) if p == relay) {
        addr.push(Protocol::P2p(relay));
    }
    addr.with(Protocol::P2pCircuit)
}

/// The peer an address names last, if it names one.
fn last_peer(addr: &Multiaddr) -> Option<PeerId> {
    match addr.iter().last() {
        Some(libp2p::multiaddr::Protocol::P2p(peer)) => Some(peer),
        _ => None,
    }
}

/// `addr` ending in `/p2p/<peer>`, which dialling through a circuit needs.
fn dialable(addr: &Multiaddr, peer: PeerId) -> Multiaddr {
    use libp2p::multiaddr::Protocol;
    if matches!(addr.iter().last(), Some(Protocol::P2p(p)) if p == peer) {
        addr.clone()
    } else {
        addr.clone().with(Protocol::P2p(peer))
    }
}

fn build_swarm(config: &Config, keypair: libp2p::identity::Keypair) -> Result<libp2p::Swarm<MeshBehaviour>> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay_client| {
            let me = key.public().to_peer_id();
            let keepalive = config.keepalive;
            let use_mdns = config.mdns;
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub::ConfigBuilder::default()
                    // Heartbeats are worthless once late, so the network should
                    // not spend effort re-delivering old ones.
                    .heartbeat_interval(Duration::from_millis(500))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .max_transmit_size(GOSSIP_MAX_BYTES)
                    .build()
                    .map_err(std::io::Error::other)?,
            )?;
            Ok(MeshBehaviour {
                gossipsub,
                mdns: mdns_behaviour(use_mdns, key.public().to_peer_id())?,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/machine-elves/0.1".into(),
                    key.public(),
                )),
                // Ping is the keepalive. Giving it an explicit interval means
                // paths stay open because something is deliberately holding
                // them open, rather than because gossip happens to be chatty
                // enough this week.
                ping: ping::Behaviour::new(ping::Config::new().with_interval(keepalive)),
                relay: relay::Behaviour::new(me, relay_config()),
                relay_client,
                dcutr: dcutr::Behaviour::new(me),
                autonat_client: autonat::v2::client::Behaviour::new(
                    rand_core::OsRng,
                    autonat::v2::client::Config::default()
                        .with_probe_interval(AUTONAT_PROBE_EVERY)
                        .with_max_candidates(AUTONAT_MAX_CANDIDATES),
                ),
                autonat_server: autonat::v2::server::Behaviour::default(),
            })
        })?
        // Comfortably longer than the keepalive, or a connection would be
        // dropped for idleness between the very pokes meant to preserve it.
        .with_swarm_config(|c| c.with_idle_connection_timeout(config.keepalive * 3))
        .build();
    Ok(swarm)
}

async fn run(config: Config) -> Result<()> {
    let keypair = load_or_create_identity(&config)?;
    let mut swarm = build_swarm(&config, keypair)?;
    let mut reachability = Reachability::default();
    // When this node last dialled each IP, so that a connection back from one
    // it dialled is not taken as proof that it can be reached.
    let mut dialled_ips: HashMap<std::net::IpAddr, Instant> = HashMap::new();
    // Connections that arrived unasked, waiting for identify to say which
    // address the member reached this node at.
    let mut proven_conns: HashSet<libp2p::swarm::ConnectionId> = HashSet::new();
    // When each introducer last refused, so it is not asked twice a second.
    let mut introducer_refused: HashMap<PeerId, Instant> = HashMap::new();
    // Introducers this node holds, by the listener each one's circuit is on.
    let mut introducers: HashMap<PeerId, libp2p::core::transport::ListenerId> = HashMap::new();
    // This node's addresses through its introducers, announced like any other.
    let mut circuit_addrs: BTreeSet<Multiaddr> = BTreeSet::new();
    // Other members' addresses through their introducers, kept so that losing
    // a direct link to one of them can try a punch again.
    let mut circuits_of: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    // Connections that do not go through anyone, by connection, so that an
    // introduction is only ever made over a direct one.
    let mut direct: HashMap<libp2p::swarm::ConnectionId, PeerId> = HashMap::new();

    let topic = gossipsub::IdentTopic::new(heartbeat_topic(&config.mesh));
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    for addr in &config.listen {
        swarm.listen_on(addr.clone())?;
    }
    // Peers we are trying to reach and have not reached yet. Kept rather than
    // dialled once and forgotten, because a single attempt only succeeds if the
    // other side happened to already be listening — and neither side can be
    // listening in any useful sense while behind address translation.
    //
    // Each address keeps its own backoff (see `dialing`): Run F spent about
    // 13 MB/h on every address that never answered, dialled every 2 s.
    let mut dials = dialing::Schedule::new();
    for addr in &config.dial {
        dials.add(addr.clone(), dialing::Kind::Configured, None, Instant::now());
    }
    // Remembered peers that were reachable are dialled as well, so that a node
    // whose configured peer has gone can still find its way back in.
    let book_path = std::path::PathBuf::from(&config.peer_book);
    let mut book = match peerbook::PeerBook::load(&book_path) {
        Ok((book, skipped)) => {
            emit(&config, "peer-book-loaded",
                 &format!("remembered {} peers ({skipped} unreadable lines skipped)", book.len()),
                 &[("peers", &book.len().to_string()), ("skipped", &skipped.to_string())]);
            book
        }
        Err(error) => {
            emit(&config, "peer-book-loaded",
                 &format!("could not read {}: {error}; remembering nobody", config.peer_book),
                 &[("peers", "0"), ("skipped", "0"), ("error", &error.to_string())]);
            peerbook::PeerBook::default()
        }
    };
    for addr in book.addresses_to_dial() {
        dials.add(addr, dialing::Kind::Remembered, None, Instant::now());
    }
    // And, briefly, where peers that cannot be dialled were last seen: they
    // have been dialling this node while it was away, which holds their
    // routers open toward it, so this gets through at once instead of waiting
    // up to a minute for their next try. The operator's option 2, 2026-11-18.
    for (peer, addr) in book.seen_recently(now_millis() as u64, RETURN_DIAL_WITHIN.as_millis() as u64) {
        dials.add_briefly(addr, Some(peer), Instant::now(), dialing::FAST_AFTER_LOSS);
    }
    // Written at most every BOOK_SAVE_EVERY, and only when something changed.
    let mut book_dirty = false;
    let mut book_saved_at: Option<Instant> = None;
    // Which address reached which peer, so that losing the peer puts the
    // address back on the list. Without this a node dials until it succeeds
    // once and then never again: the first disconnection is permanent, and no
    // amount of waiting fixes it because nothing is trying.
    let mut reached_by: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    let mut dial_attempts: u32 = 0;
    let mut dial_retry = dial_retry_interval();
    // Where this node can currently see each peer. Observed, never claimed:
    // a peer's own idea of its address is worthless behind translation.
    let mut seen_at: HashMap<PeerId, Multiaddr> = HashMap::new();
    let mut announce = tokio::time::interval(ANNOUNCE_EVERY);
    let mut my_address: Option<Multiaddr> = None;
    let mut renew_mapping = tokio::time::interval(portmap::RENEW_EVERY);
    let mut say_digest = tokio::time::interval(DIGEST_EVERY);
    let mut last_repair: Option<Instant> = None;
    let mut work_summary = tokio::time::interval(WORK_SUMMARY_EVERY);
    let mut since_summary: HashMap<String, u64> = HashMap::new();
    let mut ticks_since_summary: u64 = 0;
    let mut mapping: Option<portmap::Mapping> = None;
    // Which protocol granted it, so the goodbye withdraws it the same way.
    let mut mapped_by: Option<portmap::Method> = None;
    // The renewal's answer, carried back from a task rather than awaited here.
    //
    // A router that does not speak NAT-PMP never replies, and asking costs the
    // full retry budget — six seconds — before it fails. Awaited inside the
    // select arm below, that six seconds is six seconds in which this node
    // sends no heartbeat and reads none, which is twice the detection window:
    // every peer concludes it died, and it concludes every peer died. Measured
    // on a laptop tethered to a phone, this fired every fifteen minutes for two
    // days and cost 94 needless job takeovers, each one immediately undone.
    //
    // So the ask happens elsewhere and the answer arrives as a message. The
    // loop keeps turning while the router is being waited on.
    let (mapping_tx, mut mapping_rx) =
        tokio::sync::mpsc::channel::<std::result::Result<(portmap::Mapping, portmap::Method), String>>(1);
    // One outstanding request at a time. The renewal interval is fifteen
    // minutes and a request gives up after six seconds, so this should never
    // engage — but if a router ever holds the socket open longer than the
    // interval, requests would otherwise pile up unboundedly.
    let mut renewing = false;

    let me = *swarm.local_peer_id();
    // Needs only to differ between two starts of the same node, never to be
    // secret, so the clock and the process id are enough.
    let run: u64 = (now_millis() as u64).wrapping_mul(1_000_003) ^ u64::from(std::process::id());
    emit(
        &config,
        "started",
        &format!("node {} is {} on mesh \"{}\"", config.label, me, config.mesh),
        &[("peer_id", &me.to_string()), ("mesh", &config.mesh), ("run", &run.to_string())],
    );

    let mut work = match &config.job {
        Some(path) => {
            let job = job::Job::load(path, job::DEFAULT_FUEL)?;
            emit(&config, "job-loaded",
                 &format!("holding {path}{}", if config.own { ", running it" } else { ", standing by" }),
                 &[("path", path), ("owner", if config.own { "true" } else { "false" })]);
            // What this node already knew existed, from before it was last
            // stopped. A ledger that started empty made every restart look
            // like the world had never been built in.
            let ledger = match ledger::Ledger::load_repairing(std::path::Path::new(&config.ledger)) {
                Ok((ledger, repair)) => {
                    match repair {
                        ledger::Repair::None => {}
                        ledger::Repair::TornTailTrimmed { bytes } => {
                            emit(&config, "ledger-repaired",
                                 &format!("cut {bytes} bytes of an interrupted append off the end \
                                           of {}, so the next append is readable", config.ledger),
                                 &[("path", &config.ledger), ("bytes", &bytes.to_string())]);
                        }
                        ledger::Repair::CorruptSetAside { at, kept, aside } => {
                            emit(&config, "ledger-corrupt",
                                 &format!("{} is damaged at byte {at}: moved to {aside}, and the \
                                           {kept} readable items written to a fresh file",
                                          config.ledger),
                                 &[("path", &config.ledger), ("at", &at.to_string()),
                                   ("kept", &kept.to_string()), ("aside", &aside)]);
                        }
                    }
                    if !ledger.is_empty() {
                        emit(&config, "ledger-loaded",
                             &format!("remembers {} things that already exist", ledger.len()),
                             &[("held", &ledger.len().to_string()), ("path", &config.ledger)]);
                    }
                    ledger
                }
                // Unreadable is not empty, and saying so matters: this node is
                // about to rebuild an inventory from whatever its peers happen
                // to mention, which will look like agreement long before it is.
                Err(error) => {
                    emit(&config, "ledger-unreadable",
                         &format!("{} could not be read ({error}) — starting from nothing, \
                                   and anything no peer mentions again is forgotten",
                                  config.ledger),
                         &[("path", &config.ledger), ("error", &error.to_string())]);
                    ledger::Ledger::new()
                }
            };
            Some(Work {
                job,
                ledger,
                // Not claimed yet, even when told to run it. Listening first
                // costs a few seconds once; claiming first costs duplicated
                // work every time a node restarts.
                owner: None,
                state: Vec::new(),
                tick: 0,
                owner_lost_at: None,
                owner_lost_label: None,
                unsaved: Vec::new(),
                last_checkpoint_at: None,
                resuming: None,
                speculating: None,
                claim_after: config
                    .own
                    .then(|| Instant::now() + config.detect * 3),
            })
        }
        None => None,
    };

    // Said in every heartbeat, so peers know whether this node could continue
    // their job. Fixed for the life of the process.
    let my_job: Option<[u8; 32]> = work.as_ref().map(|w| *w.job.id());
    // Recurring problems, said at most once a minute each.
    let mut quiet = Quiet::default();
    // Peers already told off for sending another job's checkpoints.
    let mut incompatible: HashSet<PeerId> = HashSet::new();

    let mut members: HashMap<PeerId, Member> = HashMap::new();
    let mut absences = Absences::default();
    // When each connected peer's current connection began, and which peers have
    // ever been members. Together they catch a connection to a known peer that
    // never carries a word — see `silent_connections`.
    let mut connected_at: HashMap<PeerId, Instant> = HashMap::new();
    let mut once_members: HashSet<PeerId> = HashSet::new();
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
            _ = dial_retry.tick(), if dials.len() > 0 => {
                dial_attempts += 1;
                // Anything already reaching a live peer is skipped. A retry
                // loop that keeps dialling a connected peer opens a new
                // connection every two seconds forever.
                let live: Vec<Multiaddr> = reached_by
                    .iter()
                    .filter(|(peer, _)| swarm.is_connected(peer))
                    .flat_map(|(_, addrs)| addrs.iter().cloned())
                    .collect();
                for addr in &live {
                    dials.remove(addr);
                }

                for addr in &dials.due(Instant::now()) {
                    // A punch to a peer already connected — through an
                    // introduction that is still open — would only open a
                    // second one. The attempt is spent; the backoff goes on.
                    if is_relayed(addr) && last_peer(addr).is_some_and(|p| swarm.is_connected(&p)) {
                        continue;
                    }
                    // Failures are expected and frequent: until the other side
                    // is also sending, there is nothing at the far end willing
                    // to answer. The attempt is worth making anyway, because
                    // the outgoing packet is what opens this side's path.
                    if let Some(ip) = ip_of(addr) {
                        dialled_ips.insert(ip, Instant::now());
                    }
                    let _ = swarm.dial(addr.clone());
                }
                if dial_attempts == 1 || dial_attempts % 15 == 0 {
                    emit(&config, "dialling",
                         &format!("still trying {} peer(s), attempt {dial_attempts}", dials.len()),
                         &[("peers", &dials.len().to_string()),
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
            _ = renew_mapping.tick(), if config.map_port && !renewing => {
                // Spawned, not awaited. See `mapping_rx` above for why.
                renewing = true;
                let port = config.port;
                let tx = mapping_tx.clone();
                tokio::spawn(async move {
                    // NAT-PMP first, then UPnP: whichever the router speaks.
                    let answer = portmap::within(portmap::MAPPING_TIMEOUT, portmap::first_granted(
                        async { portmap::request(port, portmap::LIFETIME_SECS).await.map_err(|e| e.to_string()) },
                        || upnp::request(port, portmap::LIFETIME_SECS),
                    )).await;
                    // The receiver lives as long as the loop, and the loop
                    // outlives this task in every case that matters. If it has
                    // gone, the node is shutting down and the answer is moot.
                    let _ = tx.send(answer).await;
                });
            }

            Some(answer) = mapping_rx.recv() => {
                renewing = false;
                match answer {
                    Ok((now, method)) => {
                        if mapping != Some(now) || mapped_by != Some(method) {
                            emit(&config, "port-mapped",
                                 &format!("the router forwards {}:{} to this node for {}s, by {}",
                                          now.external_ip, now.external_port, now.lifetime, method.name()),
                                 &[("external", &format!("{}:{}", now.external_ip, now.external_port)),
                                   ("method", method.name()),
                                   ("lifetime", &now.lifetime.to_string()),
                                   ("as_asked", if now.external_port == config.port { "true" } else { "false" })]);
                            mapping = Some(now);
                            mapped_by = Some(method);
                        }
                        // A grant after a failed renewal: what members proved
                        // before holds again.
                        if reachability.restore() {
                            for addr in reachability.confirmed.clone() {
                                swarm.add_external_address(addr.clone());
                                emit(&config, "reachable",
                                     &format!("the router forwards again, so {addr} is open again"),
                                     &[("addr", &addr.to_string()), ("by", "mapping")]);
                            }
                        }
                    }
                    Err(error) => {
                        // Reported every time rather than once. If a router
                        // stops renewing, reachability is being lost right now,
                        // and a single line an hour ago would not say so.
                        emit(&config, "port-map-failed", &error,
                             &[("error", &error),
                               ("was_mapped", if mapping.is_some() { "true" } else { "false" })]);
                        // The door this node held is closing: what members
                        // proved through it no longer holds.
                        if mapping.is_some() {
                            for addr in reachability.suspend() {
                                swarm.remove_external_address(&addr);
                                emit(&config, "unreachable",
                                     &format!("the router stopped forwarding, so {addr} may be closed"),
                                     &[("addr", &addr.to_string())]);
                            }
                        }
                        mapping = None;
                    }
                }
            }

            // Said by every node holding the job, owner or not. A standby's
            // ledger is as authoritative as anyone's — it is the same set —
            // and a standby is exactly the node most likely to have missed
            // something while it was away.
            _ = say_digest.tick(), if work.is_some() => {
                if let Some(w) = work.as_ref() {
                    // Published even when empty, and *especially* when empty.
                    // A node holding nothing is the one most in need of being
                    // sent something, and staying quiet until it has something
                    // to report is how it would stay empty: the only thing
                    // that prompts a peer to send history is hearing a digest
                    // that disagrees with its own.
                    let _ = swarm.behaviour_mut().gossipsub.publish(
                        topic.clone(),
                        encode_ledger_summary(&config.label,
                                              w.ledger.len() as u64,
                                              &w.ledger.summary()));
                }
            }

            _ = work_summary.tick(), if work.is_some() => {
                let Some(w) = work.as_ref() else { continue };
                if ticks_since_summary == 0 {
                    // Silence here is the interesting case: a node that holds a
                    // job and is not advancing it. Said once rather than
                    // repeatedly, so a standby does not narrate its own idleness
                    // forever.
                    continue;
                }
                let mut what: Vec<String> = since_summary
                    .iter()
                    .map(|(kind, count)| format!("{kind} ×{count}"))
                    .collect();
                what.sort();
                emit(&config, "working",
                     &format!("tick {} — {ticks_since_summary} ticks in the last minute; {}",
                              w.tick,
                              if what.is_empty() { "nothing produced".to_string() } else { what.join(", ") }),
                     &[("tick", &w.tick.to_string()),
                       ("ticks", &ticks_since_summary.to_string()),
                       ("ledger", &w.ledger.len().to_string())]);
                ticks_since_summary = 0;
                since_summary.clear();
            }

            _ = announce.tick() => {
                for (peer, addr) in &seen_at {
                    let _ = swarm.behaviour_mut().gossipsub
                        .publish(topic.clone(), encode_observed(&config.label, peer, addr));
                }
                // Where this node can be reached through its introducers. A
                // member that cannot reach it directly dials one of these, and
                // the introduction is what lets DCUtR time their punch.
                for addr in &circuit_addrs {
                    let _ = swarm.behaviour_mut().gossipsub
                        .publish(topic.clone(), encode_observed(&config.label, &me, addr));
                }
            }

            _ = heartbeat.tick() => {
                counter += 1;
                // Failing to publish is normal and uninteresting while this node
                // is the only one subscribed to the topic; `published` says so
                // only for the failures that are not that.
                published(&config, &mut quiet, "heartbeat", swarm.behaviour_mut().gossipsub
                    .publish(topic.clone(), encode_heartbeat(&config.label, counter, run, my_job.as_ref(), reachability.is_reachable())));
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
                        // Progress lives only in checkpoints, which live only
                        // in running nodes. If every holder stopped, the claim
                        // starts again from tick 0 with an inventory that says
                        // the work was already done: nothing is made twice,
                        // because the serials repeat, but every tick up to
                        // where it was is computed again. A Phase 0 limitation,
                        // said rather than hidden (review finding 8).
                        if work.tick == 0 && !work.ledger.is_empty() {
                            emit(&config, "cold-start",
                                 &format!("no peer held a checkpoint, so the job restarts from tick 0 \
                                           although {} things it made already exist; progress is not \
                                           kept on disk, so the work up to them is redone",
                                          work.ledger.len()),
                                 &[("held", &work.ledger.len().to_string())]);
                        }
                    }
                }

                // A speculating node runs the job without owning it. What it
                // publishes is indistinguishable from an owner's because it is
                // identical — same job, same tick, same derived serials — so
                // nothing downstream needs to know which this is.
                if work.owner != Some(me) && work.speculating.is_none() { continue }

                let mut inputs = Vec::new();
                inputs.extend_from_slice(&work.tick.to_le_bytes());
                inputs.extend_from_slice(&4u32.to_le_bytes());  // steel delivered
                inputs.extend_from_slice(&3u32.to_le_bytes());  // workers present

                let ticked = work.job.tick(&work.state, &inputs).and_then(|outcome| {
                    if outcome.state.len() > MAX_CHECKPOINT_STATE {
                        anyhow::bail!("the job's state grew to {} bytes, past the {} a checkpoint \
                                       can carry, so no standby could ever continue it",
                                      outcome.state.len(), MAX_CHECKPOINT_STATE);
                    }
                    Ok(outcome)
                });
                match ticked {
                    Ok(outcome) => {
                        work.state = outcome.state;
                        work.tick += 1;
                        if let Some(resuming) = work.resuming.take() {
                            let (resume, gap) = resuming.timings(Instant::now());
                            let gap_ms = gap.map(|g| g.as_millis().to_string())
                                .unwrap_or_else(|| "unknown".into());
                            emit(&config, "resumed",
                                 &format!("first tick after taking over: {} ms after deciding, \
                                           {gap_ms} ms since the last checkpoint",
                                          resume.as_millis()),
                                 &[("tick", &work.tick.to_string()),
                                   ("cause", resuming.cause),
                                   ("resume_ms", &resume.as_millis().to_string()),
                                   ("gap_ms", &gap_ms)]);
                        }
                        // Effects are reported with the tick that produced
                        // them, and that pairing is load-bearing rather than
                        // decorative. Two nodes may briefly continue the same
                        // job — a node that has not yet heard from a peer
                        // believes it is alone — and being deterministic, they
                        // produce identical effects. Wasted work is acceptable;
                        // widgets counted twice is not. Whatever applies these
                        // must therefore treat (job, tick) as the identity of
                        // an effect and ignore a repeat.
                        ticks_since_summary += 1;
                        let effects = String::from_utf8_lossy(&outcome.effects).into_owned();
                        // Counted rather than written out. What a reader wants
                        // to know is that work is happening and roughly how
                        // much; the individual ticks are noise that buries
                        // everything else.
                        for line in effects.lines() {
                            let kind = line.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                            *since_summary.entry(kind).or_insert(0) += 1;
                        }
                        let (fresh, rejected) = record_production(work, &effects);
                        if let Some(why) = rejected {
                            if let Some(repeats) = quiet.say("effects", Instant::now()) {
                                emit(&config, "effects-rejected",
                                     &format!("tick {}: {why} — nothing it produced was recorded", work.tick),
                                     &[("tick", &work.tick.to_string()), ("why", &why),
                                       ("repeats", &repeats.to_string())]);
                            }
                        }
                        // Checkpointed every tick because this state is tiny.
                        // Real work would checkpoint less often and trade a
                        // little replayed work for a lot less traffic.
                        published(&config, &mut quiet, "checkpoint", swarm.behaviour_mut().gossipsub.publish(
                            topic.clone(),
                            encode_checkpoint(&config.label, work.job.id(), work.tick, &work.state),
                        ));
                        // Separately from the checkpoint, and that separation
                        // is the design: the checkpoint is where the job *is*,
                        // small and sent constantly; this is what the job has
                        // *made*, and it is sent once per thing, forever.
                        publish_production(&mut swarm, &topic, &config, &mut work.unsaved, &fresh, &mut quiet);
                    }
                    Err(error) => {
                        emit(&config, "job-failed", &format!("{error:#}"),
                             &[("tick", &work.tick.to_string())]);
                        // Stop rather than spin: a job failing every tick would
                        // otherwise flood the mesh with identical complaints.
                        // The suspicion goes with the ownership, because a
                        // speculating node runs the job *without* owning it —
                        // since the tick guard admits it on `speculating`
                        // alone, clearing `owner` by itself stops nothing, and
                        // the node fails, complains and tries again every
                        // 200 ms for as long as it is left running. The
                        // job-failed line above is the record of this; calling
                        // it an abandoned speculation would claim the owner
                        // came back, which is not what happened.
                        work.owner = None;
                        stand_down(&mut work.speculating, &mut work.resuming);
                    }
                }
            }

            // Announce the departure rather than simply exiting, so peers learn
            // immediately instead of waiting out the detection window. This is
            // the whole difference between a planned handoff and a hole.
            _ = terminate.recv() => { return depart(&mut swarm, &topic, &config, mapped_by).await; }
            _ = interrupt.recv() => { return depart(&mut swarm, &topic, &config, mapped_by).await; }

            _ = sweep.tick() => {
                let now = Instant::now();
                // Open direct connections are contact for as long as they last.
                for addr in seen_at.values() {
                    if let Some(ip) = ip_of(addr) {
                        dialled_ips.insert(ip, now);
                    }
                }
                // Introductions: held on up to MAX_INTRODUCERS reachable members
                // this node is directly connected to, and none if it is
                // reachable itself. Each rides on that direct connection.
                {
                    let direct_peers = eligible_introducers(&direct.values().copied().collect(),
                                                            &introducer_refused, now);
                    let held: BTreeSet<PeerId> = introducers.keys().copied().collect();
                    let chosen = choose_introducers(reachability.is_reachable(), &members,
                                                    &direct_peers, &held, MAX_INTRODUCERS);
                    for peer in held.difference(&chosen) {
                        if let Some(id) = introducers.remove(peer) {
                            swarm.remove_listener(id);
                        }
                    }
                    for peer in chosen.difference(&held) {
                        let Some(addr) = seen_at.get(peer).filter(|a| !is_relayed(a)).cloned() else { continue };
                        match swarm.listen_on(circuit_listen_addr(&addr, *peer)) {
                            Ok(id) => {
                                introducers.insert(*peer, id);
                            }
                            Err(error) => {
                                introducer_refused.insert(*peer, now);
                                if let Some(repeats) = quiet.say("introducer", now) {
                                    emit(&config, "introducer-lost",
                                         &format!("could not ask for an introduction: {error}"),
                                         &[("peer_id", &peer.to_string()), ("error", &error.to_string()),
                                           ("repeats", &repeats.to_string())]);
                                }
                            }
                        }
                    }
                }
                if book_dirty && book_saved_at.is_none_or(|at| now.duration_since(at) >= BOOK_SAVE_EVERY) {
                    book_saved_at = Some(now);
                    match book.save(&book_path) {
                        Ok(()) => book_dirty = false,
                        Err(error) => {
                            if let Some(repeats) = quiet.say("peer-book", now) {
                                emit(&config, "peer-book-not-saved",
                                     &format!("could not write the peer book: {error}"),
                                     &[("error", &error.to_string()), ("repeats", &repeats.to_string())]);
                            }
                        }
                    }
                }
                let gone: Vec<PeerId> = members
                    .iter()
                    .filter(|(_, m)| now.duration_since(m.last_heard) > config.detect)
                    .map(|(peer, _)| *peer)
                    .collect();

                for peer in gone {
                    if let Some(member) = members.remove(&peer) {
                        let silent_for = now.duration_since(member.last_heard);
                        absences.vanished(peer, member.run, now);
                        // Let go of its connections as well as its membership.
                        // The transport keeps a dead QUIC connection until its
                        // own idle timeout, ten seconds — and a machine that
                        // restarts inside that arrives as a *second* connection
                        // to a peer already connected. gossipsub exchanges
                        // subscriptions only on the first, so the returning
                        // node never learns this one is listening and is never
                        // heard again. A box restarts its node after ten
                        // seconds, which made this the ordinary case: found by
                        // node/chaos-rig.sh, and checked by migration-rig.sh.
                        //
                        // Dropping a connection that was in fact still alive
                        // costs one redial, which silence has already started.
                        let _ = swarm.disconnect_peer_id(peer);
                        // Forgotten now rather than when the close completes, or the
                        // safety net below finds the connection in this same sweep and
                        // reports a stale connection that is already being dropped.
                        connected_at.remove(&peer);
                        // Reaching this point means no goodbye ever arrived: the
                        // machine was unplugged, lost power, lost its network, or
                        // hung. This is the case recovery has to be fast enough
                        // to survive, and the only one that costs a visible gap.
                        // Silence is the only warning a vanished machine gives,
                        // so it has to be what starts the search for it again
                        // — if it was directly connected. Silence through a
                        // forwarder says nothing about a direct path.
                        if direct.values().any(|p| *p == peer) {
                            redial_lost_peer(&config, &peer, &reached_by,
                                             &mut dials, &mut dial_attempts);
                        }

                        if let Some(w) = work.as_mut() {
                            if w.owner == Some(peer) {
                                w.owner = None;
                                w.owner_lost_at = Some(now);
                                w.owner_lost_label = Some(member.label.clone());
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
                        take_over_if_ours(&config, me, w, &members, "vanished");
                    }
                }

                // Suspicion, which is the same election run early. The owner is
                // quiet but not yet gone; this node starts continuing the job
                // anyway, and throws the work away if the owner proves alive.
                if let (Some(w), Some(after)) = (work.as_mut(), config.speculate_after) {
                    // First, the suspicions this sweep can put down. A suspect
                    // that is heard from again has answered the only question
                    // the speculation was asking — see [`suspicion_disproved`]
                    // for why waiting for its checkpoint is not enough.
                    if w.speculating.as_ref().is_some_and(|s| suspicion_disproved(s, &members)) {
                        if let Some(spec) = stand_down(&mut w.speculating, &mut w.resuming) {
                            let label = members
                                .get(&spec.about)
                                .map(|m| m.label.clone())
                                .unwrap_or_default();
                            let ticks_wasted = w.tick.saturating_sub(spec.from_tick);
                            emit(
                                &config,
                                "speculation-abandoned",
                                &format!(
                                    "{label} is talking again, so it was never gone — \
                                     {ticks_wasted} ticks computed on suspicion are discarded"
                                ),
                                &[
                                    ("peer_id", &spec.about.to_string()),
                                    ("label", &label),
                                    ("cause", "heard-again"),
                                    ("ticks_wasted", &ticks_wasted.to_string()),
                                    ("wasted_ms",
                                     &now.duration_since(spec.since).as_millis().to_string()),
                                ],
                            );
                        }
                    }

                    let owner = w.owner.filter(|owner| *owner != me);
                    if let Some(owner) = owner {
                        if w.speculating.is_none()
                            && Work::should_speculate(&me, &owner, &members, now, after, w.job.id())
                        {
                            let member = members.get(&owner);
                            let silent = member
                                .map(|m| now.duration_since(m.last_heard))
                                .unwrap_or_default();
                            let label = member.map(|m| m.label.clone()).unwrap_or_default();
                            w.speculating =
                                Some(Speculating { about: owner, since: now, from_tick: w.tick });
                            // The first tick reports itself as a resume, timed
                            // from here. Under speculation that tick happens
                            // before any decision, which is the whole gain.
                            w.resuming = Some(Resuming {
                                cause: "speculated",
                                decided: now,
                                last_checkpoint: w.last_checkpoint_at,
                            });
                            emit(
                                &config,
                                "speculating",
                                &format!(
                                    "{label} has said nothing for {:.0} ms — continuing its job \
                                     from tick {} without waiting to be sure",
                                    silent.as_secs_f64() * 1000.0,
                                    w.tick
                                ),
                                &[
                                    ("peer_id", &owner.to_string()),
                                    ("label", &label),
                                    ("silent_ms", &silent.as_millis().to_string()),
                                    ("tick", &w.tick.to_string()),
                                ],
                            );
                        }
                    }
                }

                for peer in silent_connections(&connected_at, &members, &once_members,
                                               now, config.detect * 2) {
                    connected_at.remove(&peer);
                    let _ = swarm.disconnect_peer_id(peer);
                    if direct.values().any(|p| *p == peer) {
                        redial_lost_peer(&config, &peer, &reached_by, &mut dials, &mut dial_attempts);
                    }
                    emit(&config, "silent-connection",
                         "connected to a former member that has said nothing — reconnecting",
                         &[("peer_id", &peer.to_string())]);
                }
            }

            event = swarm.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished { peer_id, connection_id, endpoint, .. } => {
                    connected_at.entry(peer_id).or_insert_with(Instant::now);
                    let relayed = connection_relayed(&endpoint);
                    // A member reached this node from an address it had not
                    // dialled: the door is open. See `unsolicited`.
                    // Which address it reached is what identify will say next:
                    // the listening address is only 0.0.0.0.
                    if proves_reachable(&endpoint, &dialled_ips, Instant::now()) {
                        proven_conns.insert(connection_id);
                    }
                    // Any direct connection, either way, is contact, and is
                    // where to look for this peer after a long absence.
                    if !relayed {
                        if let Some(ip) = ip_of(endpoint.get_remote_address()) {
                            dialled_ips.insert(ip, Instant::now());
                        }
                        if book.seen(peer_id, endpoint.get_remote_address(), now_millis() as u64) {
                            book_dirty = true;
                        }
                    }
                    if !relayed {
                        // Every address tagged with this peer stops being
                        // dialled, whichever side opened the connection. Not
                        // for an introduction: that closes at its limit, and
                        // the punch it was for must be tried again later.
                        dials.forget_peer(&peer_id);
                        direct.insert(connection_id, peer_id);
                    }
                    // Stop dialling this one. The address matched must be the
                    // address *we asked for*, not the one the connection ended
                    // up on: a peer dialled at one address commonly answers
                    // from another — a different interface, or the public
                    // address its translation assigned — and comparing against
                    // that leaves the peer on the list forever, quietly dialled
                    // for the rest of the run.
                    let reached = endpoint.get_remote_address().clone();
                    let before = dials.len();
                    if let (false, libp2p::core::ConnectedPoint::Dialer { address, .. }) = (relayed, &endpoint) {
                        dials.remove(address);
                        // Every address known to reach this peer comes off the
                        // list, not only the one that happened to succeed.
                        // Otherwise the others keep being dialled at a peer
                        // already sitting on the other end of a connection.
                        if let Some(known) = reached_by.get(&peer_id) {
                            for addr in known {
                                dials.remove(addr);
                            }
                        }
                        // Remembered so it can be dialled again later. A peer
                        // reached once is a peer worth trying to reach again,
                        // and a configured address is the most reliable thing a
                        // node has — it does not go stale the way an observed
                        // one does.
                        let known = reached_by.entry(peer_id).or_default();
                        if !known.contains(address) {
                            known.push(address.clone());
                        }
                        if book.dialled(peer_id, address, now_millis() as u64) {
                            book_dirty = true;
                        }
                    }
                    emit(&config, "connected",
                         &format!("reached {peer_id} at {reached}"),
                         &[("peer_id", &peer_id.to_string()),
                           ("addr", &reached.to_string()),
                           ("relayed", if relayed { "true" } else { "false" }),
                           ("after_attempts", &dial_attempts.to_string()),
                           ("still_pending", &dials.len().to_string())]);
                    if before > 0 && dials.len() == 0 {
                        emit(&config, "all-reached", "every peer given on the command line answered", &[]);
                    }

                    // Announced only when this is news. Announcing on every
                    // connection made a loop: a connection prompts an
                    // announcement, an announcement prompts everyone to dial,
                    // and each dial makes another connection. Two nodes
                    // produced dozens within a second, all to each other.
                    // Only direct addresses: where a member sees a peer, not
                    // which introducer carried a punch to it.
                    let is_news = !relayed && seen_at.get(&peer_id) != Some(&reached);
                    if !relayed {
                        seen_at.insert(peer_id, reached.clone());
                    }
                    if is_news {
                        // A newcomer is dialled by the whole mesh in the same
                        // moment, while it dials back — which is the only way
                        // two machines behind translation ever meet.
                        let _ = swarm.behaviour_mut().gossipsub
                            .publish(topic.clone(), encode_observed(&config.label, &peer_id, &reached));
                    }
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
                    if is_relayed(&address) {
                        circuit_addrs.insert(address.clone());
                    }
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
                        if let Some(ip) = ip_of(&addr) {
                            dialled_ips.insert(ip, Instant::now());
                        }
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
                        Message::Heartbeat { label, .. }
                        | Message::Departing { label }
                        | Message::Checkpoint { label, .. }
                        | Message::Observed { label, .. }
                        | Message::Produced { label, .. }
                        | Message::LedgerSummary { label, .. }
                        | Message::LedgerBucket { label, .. } => label.clone(),
                    };
                    let known = members.contains_key(&source);
                    if !known && !matches!(parsed, Message::Departing { .. }) {
                        members.insert(source, Member {
                            label: label.clone(),
                            joined: Instant::now(),
                            last_heard: Instant::now(),
                            disconnected: false,
                            run: None,
                            holding: Holding::Unknown,
                            reachable: false,
                        });
                        once_members.insert(source);
                        if book.named(source, &label) {
                            book_dirty = true;
                        }
                        emit(&config, "joined", &format!("{label} joined"),
                             &[("peer_id", &source.to_string()), ("label", &label)]);
                        // Answer immediately rather than waiting for the next
                        // interval. Until two nodes have heard from each other
                        // they each believe they are alone, and a node that
                        // believes it is alone will continue a job that someone
                        // else is also continuing.
                        counter += 1;
                        let _ = swarm.behaviour_mut().gossipsub
                            .publish(topic.clone(), encode_heartbeat(&config.label, counter, run, my_job.as_ref(), reachability.is_reachable()));
                        // And say what we hold, for the same reason. A node
                        // that has just arrived has missed everything made
                        // before it did, and waiting for the periodic digest
                        // would leave it ignorant of the world for a minute
                        // after joining it. The first digest each node sends
                        // goes out before it is connected to anybody, so this
                        // is in practice the one that starts a repair.
                        if let Some(w) = work.as_ref() {
                            let _ = swarm.behaviour_mut().gossipsub.publish(
                                topic.clone(),
                                encode_ledger_summary(&config.label,
                                                      w.ledger.len() as u64,
                                                      &w.ledger.summary()));
                        }
                    } else if let Some(member) = members.get_mut(&source) {
                        member.last_heard = Instant::now();
                        member.disconnected = false;
                    }

                    if let Message::Heartbeat { holding, .. } = &parsed {
                        if let Some(member) = members.get_mut(&source) {
                            member.holding = *holding;
                            member.reachable = member_reachable(&parsed);
                            if book.set_reachable(source, member.reachable) {
                                book_dirty = true;
                            }
                        }
                    }

                    match parsed {
                        Message::Heartbeat { run: Some(their_run), .. } => {
                            if let Some(member) = members.get_mut(&source) {
                                if member.run.is_some_and(|known| known != their_run) {
                                    // Restarted and back inside one detection
                                    // window, so it was never declared gone.
                                    // Nothing was taken over, and nothing here
                                    // would otherwise record that it happened.
                                    emit(&config, "restarted-unnoticed",
                                         &format!("{label} restarted without ever being missed"),
                                         &[("peer_id", &source.to_string()), ("label", &label)]);
                                }
                                member.run = Some(their_run);
                            }
                            if let Some(back) = absences.heard(&source, their_run, Instant::now()) {
                                emit(&config, "returned",
                                     &format!("{label} is back after {:.1}s — {}", back.away.as_secs_f64(),
                                              if back.restarted { "it had restarted" }
                                              else { "it never stopped: a FALSE ALARM" }),
                                     &[("peer_id", &source.to_string()), ("label", &label),
                                       ("restarted", if back.restarted { "true" } else { "false" }),
                                       ("away_ms", &back.away.as_millis().to_string())]);
                            }
                        }
                        Message::Heartbeat { run: None, .. } => {}

                        Message::Produced { items, .. } => {
                            // Merged whether or not this node runs the job. A
                            // standby that never advances a tick still holds a
                            // full inventory this way, which is the point: the
                            // record of what exists does not belong to whoever
                            // happens to be doing the work.
                            if let Some(w) = work.as_mut() {
                                let added = w.ledger.absorb(items);
                                if !added.is_empty() {
                                    // Written down, not merely believed. The
                                    // in-memory set and the file have to agree
                                    // or a restart quietly undoes the merge, and
                                    // a failure is said and retried exactly as
                                    // for this node's own production.
                                    let outcome = ledger::persist(
                                        std::path::Path::new(&config.ledger), &mut w.unsaved, &added);
                                    report_persisted(&config, &mut quiet, outcome);
                                }
                            }
                        }

                        Message::LedgerSummary { held, cells, .. } => {
                            let Some(w) = work.as_ref() else { continue };
                            let buckets = ledger::differing(&w.ledger.summary(), &cells);
                            // Both sides of a disagreement see it, and both
                            // would start a round. That is fine rather than
                            // wasteful: each sends only its own side of each
                            // differing cell, and union makes a repeat free.
                            if buckets.is_empty() || !repair_allowed(&mut last_repair) { continue }
                            emit(&config, "ledger-comparing",
                                 &format!("{label} holds {held} against this node's {} — \
                                           {} of {} buckets differ",
                                          w.ledger.len(), buckets.len(), ledger::FANOUT),
                                 &[("label", &label), ("their_held", &held.to_string()),
                                   ("our_held", &w.ledger.len().to_string()),
                                   ("buckets", &buckets.len().to_string())]);
                            for bucket in buckets.into_iter().take(REPAIR_BUCKETS_PER_ROUND) {
                                let _ = swarm.behaviour_mut().gossipsub.publish(
                                    topic.clone(),
                                    encode_ledger_bucket(&config.label, &source, bucket, false,
                                                         &w.ledger.bucket(bucket)));
                            }
                        }

                        Message::LedgerBucket { to, bucket, answer, cells, .. } => {
                            // Gossip reaches everyone; only the addressee acts.
                            if to != me { continue }
                            let Some(w) = work.as_ref() else { continue };
                            let ours = w.ledger.bucket(bucket);
                            let differ = ledger::differing(&ours, &cells);
                            if differ.is_empty() { continue }

                            let mut items = Vec::new();
                            for cell in &differ {
                                let found = w.ledger.items_in(bucket, *cell);
                                if !items.is_empty() && items.len() + found.len() > REPAIR_ITEMS_PER_BUCKET {
                                    break;
                                }
                                items.extend(found);
                            }
                            emit(&config, "ledger-repairing",
                                 &format!("sending {label} {} items from {} differing cells of bucket {bucket}",
                                          items.len(), differ.len()),
                                 &[("label", &label), ("bucket", &bucket.to_string()),
                                   ("cells", &differ.len().to_string()),
                                   ("items", &items.len().to_string())]);
                            for chunk in chunks_that_fit(&items, PRODUCED_BUDGET) {
                                published(&config, &mut quiet, "repair", swarm.behaviour_mut().gossipsub.publish(
                                    topic.clone(), encode_produced(&config.label, chunk)));
                            }
                            // The asker has sent its cells but not its items —
                            // it cannot know which of ours we lack until it
                            // sees our cells. An answer is not answered again.
                            if !answer {
                                let _ = swarm.behaviour_mut().gossipsub.publish(
                                    topic.clone(),
                                    encode_ledger_bucket(&config.label, &source, bucket, true, &ours));
                            }
                        }

                        Message::Departing { .. } => {
                            // Dropped for the same reason as a vanished peer's
                            // (see the sweep): the departing process exits
                            // without closing its connection, so it lingers
                            // for the transport's idle timeout, and a node that
                            // restarts inside that is never heard again.
                            let _ = swarm.disconnect_peer_id(source);
                            connected_at.remove(&source);
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
                                    w.owner_lost_label = Some(label.clone());
                                    take_over_if_ours(&config, me, w, &members, "announced");
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
                            // An address through an introducer is dialled as
                            // a punch: named with its peer, on the circuit
                            // schedule, and remembered so that losing a direct
                            // link to this peer can try again.
                            let relayed = is_relayed(&addr);
                            let addr = if relayed { dialable(&addr, peer) } else { addr };
                            if relayed {
                                let known = circuits_of.entry(peer).or_default();
                                if !known.contains(&addr) {
                                    known.push(addr.clone());
                                }
                            }
                            // Already connected, already trying, or already
                            // known to reach a peer we have — each is a reason
                            // not to dial again, and the last one matters most
                            // during a burst, when is_connected can still be
                            // false for a connection halfway through opening.
                            if swarm.is_connected(&peer)
                                || dials.contains(&addr)
                                || reached_by.values().any(|known| known.contains(&addr))
                            {
                                continue;
                            }
                            emit(&config, "learned-peer",
                                 &format!("{label} says {peer} is at {addr}"),
                                 &[("peer_id", &peer.to_string()), ("addr", &addr.to_string()),
                                   ("from", &label)]);
                            // Dial immediately as well as adding to the retry
                            // list. Everyone who heard this is dialling now,
                            // and arriving together is the entire point.
                            let kind = if relayed { dialing::Kind::Circuit } else { dialing::Kind::Observed };
                            if dials.add(addr.clone(), kind, Some(peer), Instant::now()) {
                                if let Some(ip) = ip_of(&addr) {
                                    dialled_ips.insert(ip, Instant::now());
                                }
                                let _ = swarm.dial(addr);
                            }
                        }

                        Message::Checkpoint { job, tick, state, .. } => {
                            let Some(w) = work.as_mut() else { continue };

                            // Another job's state is not this job's state, and
                            // its sender is not this job's owner.
                            if &job != w.job.id() {
                                if incompatible.insert(source) {
                                    emit(&config, "checkpoint-incompatible",
                                         &format!("{label} is running a different job; its checkpoints \
                                                   are ignored and it is not a candidate for this one"),
                                         &[("peer_id", &source.to_string()), ("label", &label)]);
                                }
                                continue;
                            }

                            // Somebody is running it, so this node has no cause
                            // to claim it.
                            w.claim_after = None;

                            // A checkpoint means somebody else is running this
                            // job, which is the answer to the suspicion that
                            // started this node running it. Resolved by the
                            // same rule two owners always use, so a speculator
                            // is not a special case: whoever is further along
                            // keeps it, and the other stops.
                            if let Some(spec) = w.speculating.as_ref() {
                                match resolve_speculation(spec, (w.tick, &me), (tick, &source)) {
                                    Speculation::Abandoned { ticks_wasted } => {
                                        let wasted_ms = Instant::now()
                                            .duration_since(spec.since)
                                            .as_millis();
                                        emit(&config, "speculation-abandoned",
                                             &format!("{label} is running it after all — \
                                                       {ticks_wasted} ticks computed on \
                                                       suspicion are discarded"),
                                             &[("peer_id", &source.to_string()),
                                               ("label", &label),
                                               ("cause", "checkpoint"),
                                               ("ticks_wasted", &ticks_wasted.to_string()),
                                               ("wasted_ms", &wasted_ms.to_string())]);
                                        w.speculating = None;
                                        // Nothing ticked after this point, so a
                                        // resume still owed is a resume that will
                                        // never happen.
                                        w.resuming = None;
                                    }
                                    Speculation::Outran => {
                                        // Further along than the node that came
                                        // back, so this one keeps the job and that
                                        // one yields when it hears this node's next
                                        // checkpoint. Speculation is over either
                                        // way; what differs is who owns the work.
                                        w.speculating = None;
                                        w.owner = Some(me);
                                        continue;
                                    }
                                }
                            }

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
                            w.owner_lost_label = None;
                            w.last_checkpoint_at = Some(Instant::now());
                            // Someone else is running it after all; a takeover
                            // still waiting for its first tick never got one.
                            w.resuming = None;
                            // Older checkpoints can arrive late; taking one
                            // would silently undo completed work.
                            if tick >= w.tick {
                                w.tick = tick;
                                w.state = state;
                            }
                        }
                    }
                }

                SwarmEvent::ConnectionClosed { peer_id, connection_id, num_established, endpoint, .. } => {
                    if !connection_relayed(&endpoint) {
                        if let Some(ip) = ip_of(endpoint.get_remote_address()) {
                            dialled_ips.insert(ip, Instant::now());
                        }
                    }
                    // A direct link lost: if this peer can be reached through
                    // an introducer, one punch is tried at once (each address
                    // is new to the schedule) and the circuit backoff follows.
                    let lost_direct = lost_last_direct(&mut direct, connection_id, peer_id);
                    if lost_direct {
                        for addr in circuits_of.get(&peer_id).into_iter().flatten() {
                            dials.add(addr.clone(), dialing::Kind::Circuit, Some(peer_id), Instant::now());
                        }
                    }
                    // Only the last connection closing means the transport is
                    // fully gone; a node may hold several at once, commonly one
                    // over QUIC and one over TCP.
                    if num_established == 0 {
                        connected_at.remove(&peer_id);
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
                        // one every time anything happens. Hurried only for a
                        // direct link lost, not an introduction ending.
                        if lost_direct {
                            redial_lost_peer(&config, &peer_id, &reached_by,
                                             &mut dials, &mut dial_attempts);
                        }
                    }
                }

                SwarmEvent::ExpiredListenAddr { address, .. } => {
                    circuit_addrs.remove(&address);
                }

                SwarmEvent::ListenerClosed { listener_id, reason, addresses, .. } => {
                    for address in &addresses {
                        circuit_addrs.remove(address);
                    }
                    if let Some(peer) = introducers.iter().find(|(_, id)| **id == listener_id).map(|(p, _)| *p) {
                        introducers.remove(&peer);
                        if reason.is_err() {
                            introducer_refused.insert(peer, Instant::now());
                        }
                        let error = match reason { Ok(()) => "closed".to_string(), Err(e) => e.to_string() };
                        emit(&config, "introducer-lost", &format!("an introducer stopped: {error}"),
                             &[("peer_id", &peer.to_string()),
                               ("label", &members.get(&peer).map(|m| m.label.clone()).unwrap_or_default()),
                               ("error", &error)]);
                    }
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::RelayClient(
                    relay::client::Event::ReservationReqAccepted { relay_peer_id, renewal: false, .. },
                )) => {
                    let label = members.get(&relay_peer_id).map(|m| m.label.clone()).unwrap_or_default();
                    emit(&config, "introducer-held",
                         &format!("{label} will introduce this node to members it cannot reach"),
                         &[("peer_id", &relay_peer_id.to_string()), ("label", &label)]);
                }

                // A member that reached this node unasked says where it found
                // it. That address is this node's own, as far as introducing
                // goes: the relay grants nothing until it knows one, because
                // it has to tell the other side where to reach it.
                SwarmEvent::Behaviour(MeshBehaviourEvent::Identify(identify::Event::Received {
                    connection_id, info, ..
                })) if proven_conns.remove(&connection_id) => {
                    let observed = info.observed_addr;
                    if ip_of(&observed).is_some_and(is_global) {
                        swarm.add_external_address(observed.clone());
                    }
                    if reachability.confirm(observed.clone()) {
                        emit(&config, "reachable",
                             &format!("a member reached this node at {observed} unasked: it can introduce others"),
                             &[("addr", &observed.to_string()), ("by", "connection")]);
                    }
                }

                // Every reachability test, whatever its outcome: a node that is
                // never confirmed reachable is otherwise silent about why.
                SwarmEvent::Behaviour(MeshBehaviourEvent::AutonatClient(autonat::v2::client::Event {
                    tested_addr, server, result, ..
                })) => {
                    let label = members.get(&server).map(|m| m.label.clone()).unwrap_or_default();
                    let outcome = match &result { Ok(()) => "reached".to_string(), Err(e) => e.to_string() };
                    emit(&config, "reachability-tested",
                         &format!("{label} tried to reach this node at {tested_addr}: {outcome}"),
                         &[("addr", &tested_addr.to_string()), ("label", &label),
                           ("ok", if result.is_ok() { "true" } else { "false" }), ("outcome", &outcome)]);
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Dcutr(dcutr::Event { remote_peer_id, result })) => {
                    let label = members.get(&remote_peer_id).map(|m| m.label.clone()).unwrap_or_default();
                    match result {
                        Ok(_) => emit(&config, "punch-succeeded",
                                      &format!("punched through to {label}: a direct link, no longer through anyone"),
                                      &[("peer_id", &remote_peer_id.to_string()), ("label", &label)]),
                        Err(error) => emit(&config, "punch-failed",
                                           &format!("could not punch through to {label}: {error}"),
                                           &[("peer_id", &remote_peer_id.to_string()), ("label", &label),
                                             ("error", &error.to_string())]),
                    }
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Ping(ping::Event {
                    peer, result: Ok(rtt), ..
                })) => {
                    emit(&config, "rtt", &format!("{peer} round trip {rtt:?}"),
                         &[("peer_id", &peer.to_string()),
                           ("rtt_ms", &rtt.as_millis().to_string())]);
                }

                // A member reached this node at an address it announced, on a
                // connection of its own: that, and nothing less, is reachable.
                SwarmEvent::ExternalAddrConfirmed { address } => {
                    if reachability.confirm(address.clone()) {
                        emit(&config, "reachable",
                             &format!("a member reached this node at {address}: it can introduce others"),
                             &[("addr", &address.to_string()), ("by", "autonat")]);
                    }
                }
                SwarmEvent::ExternalAddrExpired { address } => {
                    if reachability.expire(&address) {
                        emit(&config, "unreachable",
                             &format!("no member can reach this node at {address} any more"),
                             &[("addr", &address.to_string())]);
                    }
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
/// Records what a tick made, returning whatever the ledger had not already held.
///
/// The return is what gets announced and written down. It is usually everything
/// produced, but not always: a tick replayed after a takeover derives the same
/// serials as the node that ran it before, so re-running work the mesh already
/// knows about correctly announces nothing. That is the duplicate problem
/// dissolving rather than being handled.
fn record_production(work: &mut Work, effects: &str) -> (Vec<(String, ledger::Serial)>, Option<String>) {
    let totals = match production(effects) {
        Ok(totals) => totals,
        Err(why) => return (Vec::new(), Some(why)),
    };
    let mut fresh = Vec::new();
    for (kind, count) in totals {
        for ordinal in 0..count {
            let serial = ledger::Serial::derive(work.job.id(), work.tick, &kind, ordinal);
            if work.ledger.record(&kind, serial) {
                fresh.push((kind.clone(), serial));
            }
        }
    }
    (fresh, None)
}

/// Reads a tick's `produce` lines into a count per kind, or says why it
/// cannot.
///
/// Every line is checked before anything is recorded, so a tick is applied
/// whole or not at all. Counts are totalled per kind before serials are
/// derived: ordinals used to restart at zero on every line, so
/// `produce widget 2` then `produce widget 3` made three widgets, not five
/// (review finding 6). A kind on one line, which is all the factory writes,
/// derives exactly the serials it always did.
///
/// A missing count still means one. A count that is not a number used to mean
/// one as well, silently; it is now an error, as is a kind longer than
/// [`ledger::MAX_KIND`] or a tick producing more than
/// [`MAX_PRODUCED_PER_TICK`] in all.
fn production(effects: &str) -> Result<Vec<(String, u32)>, String> {
    let mut totals: Vec<(String, u64)> = Vec::new();
    let mut all = 0u64;
    for line in effects.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("produce") {
            continue;
        }
        let Some(kind) = parts.next() else {
            return Err(format!("`{line}` names nothing to produce"));
        };
        if kind.len() > ledger::MAX_KIND {
            return Err(format!("a kind of {} bytes, over the {}-byte limit", kind.len(), ledger::MAX_KIND));
        }
        let count = match parts.next() {
            None => 1,
            Some(n) => n.parse::<u32>().map_err(|_| format!("`{line}` has a count that is not a number"))? as u64,
        };
        if parts.next().is_some() {
            return Err(format!("`{line}` has more after its count"));
        }
        all += count;
        if all > MAX_PRODUCED_PER_TICK {
            return Err(format!("more than {MAX_PRODUCED_PER_TICK} things in one tick"));
        }
        match totals.iter_mut().find(|(k, _)| k == kind) {
            Some((_, total)) => *total += count,
            None => totals.push((kind.to_string(), count)),
        }
    }
    Ok(totals.into_iter().map(|(kind, n)| (kind, n as u32)).collect())
}

/// Room for a production message's items once the envelope is allowed for.
const PRODUCED_BUDGET: usize = GOSSIP_MAX_BYTES - GOSSIP_ENVELOPE - MAX_LABEL - 16;

/// Cuts items into runs that each fit one message: at most [`LEDGER_CHUNK`]
/// items, and at most `budget` bytes once encoded.
fn chunks_that_fit(items: &[(String, ledger::Serial)], budget: usize) -> Vec<&[(String, ledger::Serial)]> {
    let mut chunks = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, (kind, _)) in items.iter().enumerate() {
        let cost = 4 + kind.len() + 32;
        if i > start && (i - start == LEDGER_CHUNK || size + cost > budget) {
            chunks.push(&items[start..i]);
            start = i;
            size = 0;
        }
        size += cost;
    }
    if start < items.len() {
        chunks.push(&items[start..]);
    }
    chunks
}

/// Writes new items down and tells everyone about them.
///
/// Both halves matter and neither is enough alone: the file is what survives a
/// restart, and the announcement is what lets a node that was not running the
/// job know the thing exists at all.
fn publish_production(
    swarm: &mut libp2p::Swarm<MeshBehaviour>,
    topic: &gossipsub::IdentTopic,
    config: &Config,
    unsaved: &mut Vec<(String, ledger::Serial)>,
    fresh: &[(String, ledger::Serial)],
    quiet: &mut Quiet,
) {
    if fresh.is_empty() {
        return;
    }
    report_persisted(config, quiet, ledger::persist(std::path::Path::new(&config.ledger), unsaved, fresh));
    for chunk in chunks_that_fit(fresh, PRODUCED_BUDGET) {
        published(config, quiet, "production", swarm
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), encode_produced(&config.label, chunk)));
    }
}

/// Says what [`ledger::persist`] achieved, when there is anything to say.
///
/// A node whose ledger is not being written still works and still converges
/// with its peers — it just forgets everything when it restarts, which is
/// exactly the failure that is invisible until the restart. Local production
/// and a peer's both come through here, so both are reported the same way.
fn report_persisted(config: &Config, quiet: &mut Quiet, outcome: ledger::Persisted) {
    match outcome {
        ledger::Persisted::Nothing | ledger::Persisted::Saved { retried: 0 } => {}
        ledger::Persisted::Saved { retried } => {
            emit(config, "ledger-saved-again",
                 &format!("wrote {retried} items an earlier attempt could not"),
                 &[("path", &config.ledger), ("retried", &retried.to_string())]);
        }
        ledger::Persisted::NotSaved { error, pending } => {
            if let Some(repeats) = quiet.say("ledger", Instant::now()) {
                emit(config, "ledger-not-saved",
                     &format!("{error} — {pending} items held to retry"),
                     &[("path", &config.ledger), ("error", &error),
                       ("pending", &pending.to_string()), ("repeats", &repeats.to_string())]);
            }
        }
    }
}

/// How long a recurring problem stays quiet after being said.
const QUIET_FOR: Duration = Duration::from_secs(60);

/// Says a recurring problem at most once a minute, with a count of the repeats
/// in between, so a fault that recurs every tick is reported without burying
/// everything else in the log.
#[derive(Default)]
struct Quiet {
    last: HashMap<String, (Instant, u64)>,
}

impl Quiet {
    /// `Some(repeats since it was last said)` when it should be said now.
    fn say(&mut self, key: &str, now: Instant) -> Option<u64> {
        match self.last.get_mut(key) {
            Some((at, held)) if now.duration_since(*at) < QUIET_FOR => {
                *held += 1;
                None
            }
            Some((at, held)) => {
                let repeats = *held;
                *at = now;
                *held = 0;
                Some(repeats)
            }
            None => {
                self.last.insert(key.to_string(), (now, 0));
                Some(0)
            }
        }
    }
}

/// Reports a publish that failed for any reason other than nobody listening.
///
/// Every publish used to be `let _ =`. A node alone has no subscribers and
/// fails every publish, which is normal; but a message too large, or every
/// peer's queue full, is a checkpoint no standby receives while the job looks
/// perfectly healthy (review finding 9).
fn published(
    config: &Config,
    quiet: &mut Quiet,
    what: &str,
    result: Result<gossipsub::MessageId, gossipsub::PublishError>,
) {
    let error = match result {
        Ok(_)
        | Err(gossipsub::PublishError::NoPeersSubscribedToTopic)
        | Err(gossipsub::PublishError::Duplicate) => return,
        Err(error) => error,
    };
    if let Some(repeats) = quiet.say(&format!("publish-{what}"), Instant::now()) {
        emit(config, "publish-failed",
             &format!("a {what} could not be sent: {error}"),
             &[("what", what), ("error", &error.to_string()), ("repeats", &repeats.to_string())]);
    }
}

/// Puts a lost peer's addresses back on the list to be dialled.
///
/// Called both when the transport reports a connection closed and when a peer
/// simply goes silent, because those are different events and only the second
/// happens when a machine actually disappears. A process killed outright, a
/// laptop closing, a hotspot switched off — none of them close anything. The
/// connection stays open in the transport's opinion until an idle timeout
/// minutes away, so a mesh that waits for that has already given up.
fn redial_lost_peer(
    config: &Config,
    peer: &PeerId,
    reached_by: &HashMap<PeerId, Vec<Multiaddr>>,
    dials: &mut dialing::Schedule,
    attempts: &mut u32,
) {
    let Some(addresses) = reached_by.get(peer) else { return };
    let mut resumed = 0;
    let now = Instant::now();
    for addr in addresses {
        // The one event that brings an address forward: a direct connection
        // to its peer was lost, so the peer may be back within seconds.
        let kind = if config.dial.contains(addr) { dialing::Kind::Configured } else { dialing::Kind::Observed };
        dials.add(addr.clone(), kind, Some(*peer), now);
        dials.reset(addr, now);
        resumed += 1;
    }
    if resumed > 0 {
        *attempts = 0;
        emit(
            config,
            "redialling",
            &format!("lost a peer; trying its {resumed} known address(es) again"),
            &[
                ("peer_id", &peer.to_string()),
                ("addresses", &resumed.to_string()),
            ],
        );
    }
}

/// Connections to former members that have carried nothing for too long.
///
/// The safety net under the two disconnects that fix the restart race. However
/// a stale connection comes about, its signature is the same: the transport
/// says a peer that was once a member is connected, and gossip has not heard a
/// word from it. Dropping the connection lets a fresh one form, and on a fresh
/// first connection gossipsub exchanges subscriptions again.
///
/// Only former members, because a connection to a node of another mesh is
/// silent forever by design, and reconnecting to it would never end.
fn silent_connections(
    connected_at: &HashMap<PeerId, Instant>,
    members: &HashMap<PeerId, Member>,
    once_members: &HashSet<PeerId>,
    now: Instant,
    limit: Duration,
) -> Vec<PeerId> {
    connected_at
        .iter()
        .filter(|(peer, since)| {
            !members.contains_key(*peer)
                && once_members.contains(*peer)
                && now.duration_since(**since) > limit
        })
        .map(|(peer, _)| *peer)
        .collect()
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
    cause: &'static str,
) {
    let now = Instant::now();
    if !Work::should_take_over(&me, members, &work.job.id().clone()) {
        // Losing an election this node's own suspicion started still ends the
        // speculation. Returning before this ran — which a membership change
        // between suspecting and confirming is enough to cause — stranded the
        // node with `speculating: Some` and `owner: None`: still ticking, still
        // publishing, never named in any log as having taken anything over,
        // and now with no exit left, since the peer it was speculating about
        // is gone and will send no checkpoint.
        if let Some(spec) = stand_down(&mut work.speculating, &mut work.resuming) {
            let label = members.get(&spec.about).map(|m| m.label.clone()).unwrap_or_default();
            let ticks_wasted = work.tick.saturating_sub(spec.from_tick);
            emit(
                config,
                "speculation-abandoned",
                &format!(
                    "another survivor is continuing the job, so the {ticks_wasted} ticks \
                     computed on suspicion here are discarded"
                ),
                &[
                    ("peer_id", &spec.about.to_string()),
                    ("label", &label),
                    ("cause", "not-ours"),
                    ("ticks_wasted", &ticks_wasted.to_string()),
                    ("wasted_ms", &now.duration_since(spec.since).as_millis().to_string()),
                ],
            );
        }
        return;
    }
    let gap = work
        .owner_lost_at
        .map(|at| now.duration_since(at))
        .unwrap_or_default();

    // Already running it on suspicion: this decision changes who owns the job,
    // not what is being computed. No tick is re-run and no second resume is
    // reported — the work is done and published, and the resume that matters
    // was measured from the suspicion.
    let speculated = work.speculating.take();
    let speculated_ms = speculated
        .as_ref()
        .map(|s| now.duration_since(s.since).as_millis().to_string());

    work.owner = Some(me);
    work.owner_lost_at = None;
    let previous = work.owner_lost_label.take().unwrap_or_default();
    if resume_is_owed(speculated.is_some()) {
        work.resuming =
            Some(Resuming { cause, decided: now, last_checkpoint: work.last_checkpoint_at });
    }

    emit(
        config,
        "took-over",
        &format!(
            "continuing the job from tick {} — {:.0} ms after noticing{}",
            work.tick,
            gap.as_secs_f64() * 1000.0,
            match &speculated_ms {
                Some(ms) => format!(", having already been running it for {ms} ms"),
                None => String::new(),
            }
        ),
        &[
            ("tick", &work.tick.to_string()),
            ("cause", cause),
            // Which owner this continues. Empty when not known.
            ("previous", &previous),
            ("decision_ms", &gap.as_millis().to_string()),
            ("state_bytes", &work.state.len().to_string()),
            ("survivors", &(members.len() + 1).to_string()),
            ("speculated", if speculated_ms.is_some() { "true" } else { "false" }),
            ("speculated_ms", speculated_ms.as_deref().unwrap_or("0")),
        ],
    );
}

/// Whether a node that has just taken over still owes a resume.
///
/// A speculating node does not: its first tick happened when the suspicion
/// began and reported itself then, timed from there. Reporting a second one
/// here would count one takeover twice and measure it from a decision that
/// changed nothing about what was being computed.
fn resume_is_owed(was_speculating: bool) -> bool {
    !was_speculating
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
    mapped_by: Option<portmap::Method>,
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
    //
    // Bounded, because a router that does not answer would otherwise hold the
    // shutdown for the full six-second retry budget — delaying the goodbye this
    // function exists to deliver, and every restart along with it. The mapping
    // expires by itself within the hour, so the courtesy is not worth waiting on.
    if config.map_port {
        match mapped_by {
            Some(portmap::Method::Upnp) => {
                let _ = tokio::time::timeout(Duration::from_secs(1), upnp::release(config.port)).await;
            }
            _ => {
                let _ = tokio::time::timeout(Duration::from_secs(1), portmap::release(config.port)).await;
            }
        }
    }

    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            _ = swarm.select_next_some() => {}
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }

    // Then close every connection, and wait briefly for the closes to go out.
    // Exiting with them open leaves each peer holding a dead connection for the
    // transport's idle timeout — and a peer redialling during the drain above
    // gets a brand new one, which dies the same way a moment later. A node
    // that restarts inside that window is then never heard by that peer again.
    // Found by node/chaos-rig.sh; checked by migration-rig.sh.
    let peers: Vec<PeerId> = swarm.connected_peers().copied().collect();
    for peer in peers {
        let _ = swarm.disconnect_peer_id(peer);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while swarm.connected_peers().next().is_some() && tokio::time::Instant::now() < deadline {
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

    /// The job every member in these tests holds.
    const TEST_JOB: [u8; 32] = [3; 32];

    /// The mesh ran undisturbed for 41.1 hours before Run C, and when the peer
    /// finally left, `dial_retry`'s arm became eligible and fired 74,016 stored
    /// ticks at once: 16,873 connections in 86 seconds, 7.5 MB of log, and a
    /// `chaos.py` blinded for six minutes because its 2 MB tail window no
    /// longer reached the run's own `started` line.
    ///
    /// The interval's arm is guarded, so nothing polls it while every peer is
    /// reachable, and tokio's default `MissedTickBehavior::Burst` hands over
    /// every tick that elapsed unwatched. The healthier the mesh, the larger
    /// the burst — which is why two earlier runs looked clean: they began after
    /// runs full of disruptions, with backlogs seconds deep rather than days.
    /// `--no-mdns` must mean no mDNS behaviour, not an idle one: the idle one
    /// still joined 224.0.0.251 on every interface, LTE included.
    fn reachable_member(reachable: bool) -> Member {
        Member { label: "peer".into(), joined: Instant::now(), last_heard: Instant::now(),
                 disconnected: false, run: Some(1), holding: Holding::Job(TEST_JOB), reachable }
    }

    /// Peers in ascending order, so "the lowest" is checkable.
    fn sorted_peers(n: usize) -> Vec<PeerId> {
        let mut peers: Vec<PeerId> = (0..n).map(|_| PeerId::random()).collect();
        peers.sort();
        peers
    }

    /// Found in the NAT rig: a refused introduction was asked for again at
    /// every sweep, twice a second, for as long as the refusal lasted.
    #[test]
    fn an_introducer_that_refused_waits_a_minute_before_it_is_asked_again() {
        let t0 = Instant::now();
        let p = sorted_peers(2);
        let direct = HashSet::from([p[0], p[1]]);
        let refused = HashMap::from([(p[0], t0)]);
        assert_eq!(eligible_introducers(&direct, &refused, t0 + Duration::from_secs(1)), HashSet::from([p[1]]));
        assert_eq!(eligible_introducers(&direct, &refused, t0 + INTRODUCER_RETRY), direct);
        assert_eq!(INTRODUCER_RETRY, Duration::from_secs(60));
    }

    #[test]
    fn a_reachable_node_holds_no_introducers() {
        let p = sorted_peers(1);
        let members = HashMap::from([(p[0], reachable_member(true))]);
        let connected = HashSet::from([p[0]]);
        assert!(choose_introducers(true, &members, &connected, &BTreeSet::new(), MAX_INTRODUCERS).is_empty());
    }

    #[test]
    fn introducers_are_reachable_members_this_node_is_directly_connected_to() {
        let p = sorted_peers(3);
        let members = HashMap::from([
            (p[0], reachable_member(true)),  // reachable, but not connected
            (p[1], reachable_member(false)), // connected, but not reachable
            (p[2], reachable_member(true)),  // both
        ]);
        let connected = HashSet::from([p[1], p[2]]);
        assert_eq!(choose_introducers(false, &members, &connected, &BTreeSet::new(), MAX_INTRODUCERS),
                   BTreeSet::from([p[2]]));
    }

    #[test]
    fn at_most_three_introducers_and_the_ones_already_held_are_kept() {
        assert_eq!(MAX_INTRODUCERS, 3);
        let p = sorted_peers(5);
        let members: HashMap<PeerId, Member> = p.iter().map(|&q| (q, reachable_member(true))).collect();
        let connected: HashSet<PeerId> = p.iter().copied().collect();
        let held = BTreeSet::from([p[4]]);
        assert_eq!(choose_introducers(false, &members, &connected, &held, MAX_INTRODUCERS),
                   BTreeSet::from([p[0], p[1], p[4]]),
                   "the one held stays, and the lowest of the rest fill the other places");
    }

    #[test]
    fn an_introducer_that_stops_being_reachable_is_dropped() {
        let p = sorted_peers(2);
        let members = HashMap::from([(p[0], reachable_member(false)), (p[1], reachable_member(true))]);
        let connected = HashSet::from([p[0], p[1]]);
        let held = BTreeSet::from([p[0]]);
        assert_eq!(choose_introducers(false, &members, &connected, &held, MAX_INTRODUCERS),
                   BTreeSet::from([p[1]]));
    }

    #[test]
    fn a_circuit_address_is_recognised_and_a_direct_one_is_not() {
        let relay = PeerId::random();
        let direct: Multiaddr = "/ip4/204.210.210.200/udp/4001/quic-v1".parse().unwrap();
        assert!(!is_relayed(&direct));
        assert!(is_relayed(&circuit_listen_addr(&direct, relay)));
    }

    /// Found in the NAT rig: the receiving end of an introduction sees only
    /// `/p2p/<peer>` as the remote address. The circuit is in its local one, so
    /// a check of the remote alone counted the introduction as a direct link.
    #[test]
    fn a_connection_through_an_introducer_is_relayed_from_both_ends() {
        use libp2p::core::{ConnectedPoint, Endpoint, transport::PortUse};
        let relay = PeerId::random();
        let peer = PeerId::random();
        let circuit = circuit_listen_addr(&"/ip4/192.0.2.10/udp/4001/quic-v1".parse().unwrap(), relay);
        let receiving = ConnectedPoint::Listener {
            local_addr: circuit.clone(),
            send_back_addr: Multiaddr::empty().with(libp2p::multiaddr::Protocol::P2p(peer)),
        };
        let dialling = ConnectedPoint::Dialer {
            address: dialable(&circuit, peer),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };
        let direct = ConnectedPoint::Dialer {
            address: "/ip4/198.51.100.1/udp/4001/quic-v1".parse().unwrap(),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };
        assert!(connection_relayed(&receiving));
        assert!(connection_relayed(&dialling));
        assert!(!connection_relayed(&direct));
    }

    #[test]
    fn a_circuit_listen_address_names_the_relay_once() {
        let relay = PeerId::random();
        let bare: Multiaddr = "/ip4/204.210.210.200/udp/4001/quic-v1".parse().unwrap();
        let named = bare.clone().with(libp2p::multiaddr::Protocol::P2p(relay));
        let want: Multiaddr = format!("{bare}/p2p/{relay}/p2p-circuit").parse().unwrap();
        assert_eq!(circuit_listen_addr(&bare, relay), want);
        assert_eq!(circuit_listen_addr(&named, relay), want);
    }

    #[test]
    fn a_dialable_address_names_its_peer_once() {
        let (relay, peer) = (PeerId::random(), PeerId::random());
        let circuit = circuit_listen_addr(&"/ip4/192.0.2.10/udp/4001/quic-v1".parse().unwrap(), relay);
        let want: Multiaddr = format!("{circuit}/p2p/{peer}").parse().unwrap();
        assert_eq!(dialable(&circuit, peer), want);
        assert_eq!(dialable(&want, peer), want);
    }

    #[tokio::test]
    async fn the_swarm_builds_with_relaying_and_punching() {
        let mut config = default_config();
        config.mdns = false;
        assert!(build_swarm(&config, libp2p::identity::Keypair::generate_ed25519()).is_ok());
    }

    /// The relay is used only to introduce two members for a punch. Its
    /// default limits are the point: long enough to arrange the meeting, too
    /// short to become a path the mesh's traffic quietly depends on.
    #[test]
    fn introductions_keep_the_relays_default_limits() {
        let c = relay_config();
        assert_eq!(c.max_circuit_duration, Duration::from_secs(120));
        assert_eq!(c.max_circuit_bytes, 131_072);
    }

    /// Both of Run F's routers admit only replies, so a connection arriving
    /// from an address this node has not sent to proves the door is open. One
    /// from an address it just dialled proves nothing: that is a punch, or a
    /// blind dial met halfway.
    #[test]
    fn only_a_connection_from_an_address_not_dialled_recently_proves_reachability() {
        let t0 = Instant::now();
        let peer: std::net::IpAddr = "203.0.113.1".parse().unwrap();
        let mut dialled = HashMap::new();
        assert!(unsolicited(peer, &dialled, t0), "never dialled: it found us");
        dialled.insert(peer, t0);
        assert!(!unsolicited(peer, &dialled, t0 + Duration::from_secs(60)), "we sent to it a minute ago");
        assert!(unsolicited(peer, &dialled, t0 + UNSOLICITED_AFTER + Duration::from_secs(1)));
    }

    /// Review I1: a link older than ten minutes drops, the far side redials
    /// through the NAT flow still open toward it, and the receiving side took
    /// the arrival as proof it could be reached. Any recent contact with that
    /// address, not only a dial, makes an arrival from it prove nothing.
    #[test]
    fn an_arrival_from_a_recent_contact_proves_nothing() {
        use libp2p::core::{ConnectedPoint, Endpoint, transport::PortUse};
        let t0 = Instant::now();
        let mut contacts = HashMap::new();
        let arrival = |from: &str| ConnectedPoint::Listener {
            local_addr: "/ip4/0.0.0.0/udp/4001/quic-v1".parse().unwrap(),
            send_back_addr: format!("/ip4/{from}/udp/4001/quic-v1").parse().unwrap(),
        };
        assert!(proves_reachable(&arrival("203.0.113.1"), &contacts, t0));
        contacts.insert("203.0.113.1".parse().unwrap(), t0);
        assert!(!proves_reachable(&arrival("203.0.113.1"), &contacts, t0 + Duration::from_secs(300)));
        let dialled = ConnectedPoint::Dialer {
            address: "/ip4/198.51.100.1/udp/4001/quic-v1".parse().unwrap(),
            role_override: Endpoint::Dialer, port_use: PortUse::Reuse,
        };
        assert!(!proves_reachable(&dialled, &contacts, t0), "a connection this node opened proves nothing");
    }

    /// Review I2: a sibling on the same home network can always reach this
    /// node, which says nothing about anyone else.
    #[test]
    fn only_an_address_the_internet_routes_makes_a_node_reachable() {
        let mut r = Reachability::default();
        for private in ["10.0.0.5", "192.168.1.125", "172.20.10.2", "100.84.179.154", "127.0.0.1", "169.254.1.1"] {
            assert!(!r.confirm(format!("/ip4/{private}/udp/4001/quic-v1").parse().unwrap()), "{private}");
        }
        assert!(!r.confirm("/ip6/fe80::1/udp/4001/quic-v1".parse().unwrap()));
        assert!(!r.confirm("/ip6/fd00::1/udp/4001/quic-v1".parse().unwrap()));
        assert!(!r.is_reachable());
        assert!(r.confirm("/ip4/192.0.2.10/udp/4001/quic-v1".parse().unwrap()), "the rig's public range counts");
    }

    /// Review M1: one failed renewal withdrew reachability, and the next grant
    /// never restored it, so introductions stopped for good after one hiccup.
    #[test]
    fn reachability_withdrawn_with_a_lost_mapping_returns_with_the_next_grant() {
        let a: Multiaddr = "/ip4/204.210.210.200/udp/4001/quic-v1".parse().unwrap();
        let mut r = Reachability::default();
        r.confirm(a.clone());
        let withdrawn = r.suspend();
        assert_eq!(withdrawn, vec![a.clone()]);
        assert!(!r.is_reachable());
        assert!(r.restore(), "the grant brings it back");
        assert!(r.is_reachable());
        assert!(!r.restore(), "and only once");
    }

    /// Review I3: only losing the last direct connection to a peer hurries
    /// its redial — not an introduction ending, not silence through a forwarder.
    #[test]
    fn only_losing_the_last_direct_connection_counts_as_a_loss() {
        use libp2p::swarm::ConnectionId;
        let peer = PeerId::random();
        let (c1, c2, relayed) = (ConnectionId::new_unchecked(1), ConnectionId::new_unchecked(2), ConnectionId::new_unchecked(3));
        let mut direct = HashMap::from([(c1, peer), (c2, peer)]);
        assert!(!lost_last_direct(&mut direct, c1, peer), "another direct one remains");
        assert!(!lost_last_direct(&mut direct, relayed, peer), "an introduction ending is not a loss");
        assert!(lost_last_direct(&mut direct, c2, peer));
    }

    #[test]
    fn an_address_names_its_ip() {
        let quic: Multiaddr = "/ip4/192.0.2.10/udp/4001/quic-v1".parse().unwrap();
        assert_eq!(ip_of(&quic), Some("192.0.2.10".parse().unwrap()));
        let none: Multiaddr = "/dns4/example.org/udp/4001/quic-v1".parse().unwrap();
        assert_eq!(ip_of(&none), None);
    }

    /// Found in the NAT rig: holding an introduction makes libp2p confirm the
    /// relayed address as this node's own, and a node that took that as
    /// reachable dropped the very introductions that gave it the address.
    #[test]
    fn an_address_through_an_introducer_never_makes_a_node_reachable() {
        let relay = PeerId::random();
        let circuit = circuit_listen_addr(&"/ip4/192.0.2.10/udp/4001/quic-v1".parse().unwrap(), relay)
            .with(libp2p::multiaddr::Protocol::P2p(PeerId::random()));
        let mut r = Reachability::default();
        assert!(!r.confirm(circuit));
        assert!(!r.is_reachable());
    }

    #[test]
    fn reachable_once_any_address_is_confirmed_and_until_the_last_expires() {
        let a: Multiaddr = "/ip4/204.210.210.200/udp/4001/quic-v1".parse().unwrap();
        let b: Multiaddr = "/ip4/204.210.210.200/tcp/4001".parse().unwrap();
        let mut r = Reachability::default();
        assert!(!r.is_reachable());
        assert!(r.confirm(a.clone()), "the first confirmation makes it reachable");
        assert!(!r.confirm(b.clone()), "a second changes nothing that is said");
        assert!(!r.expire(&a), "still reachable at b");
        assert!(r.is_reachable());
        assert!(r.expire(&b), "the last one going is the change");
        assert!(!r.is_reachable());
    }

    /// AutoNAT's own default is every 5 s, which on a hotspot is data spent
    /// asking a question whose answer changes rarely.
    #[test]
    fn autonat_probes_no_more_than_once_a_minute() {
        assert!(AUTONAT_PROBE_EVERY >= Duration::from_secs(60));
        assert!(AUTONAT_MAX_CANDIDATES <= 4);
    }

    #[tokio::test]
    async fn no_mdns_builds_no_mdns_behaviour() {
        let me = libp2p::identity::Keypair::generate_ed25519().public().to_peer_id();
        assert!(!mdns_behaviour(false, me).expect("disabled always builds").is_enabled());
        assert!(mdns_behaviour(true, me).expect("enabled builds on this host").is_enabled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_mesh_does_not_store_up_dial_retries() {
        // riverboat's own account of it: was_present_ms=148031679.
        const THE_MESH_WAS_HEALTHY_FOR: Duration = Duration::from_millis(148_031_679);

        let mut retry = dial_retry_interval();
        retry.tick().await; // every interval yields its first tick immediately

        // Nothing polls it for 41 hours, because every peer is reachable.
        tokio::time::advance(THE_MESH_WAS_HEALTHY_FOR).await;

        // Then a peer leaves and the arm becomes eligible. Ten retries should
        // take the nine intervals that separate them; a stored-up backlog
        // delivers them with no time passing at all.
        let began = tokio::time::Instant::now();
        for _ in 0..10 {
            retry.tick().await;
        }
        let took = began.elapsed();

        assert!(
            took >= DIAL_RETRY * 9,
            "ten retries took {took:?}, not the {:?} that nine intervals cost: \
             {} ticks were stored up while the mesh was healthy and fired at once",
            DIAL_RETRY * 9,
            THE_MESH_WAS_HEALTHY_FOR.as_millis() / DIAL_RETRY.as_millis(),
        );
    }

    #[test]
    fn the_keepalive_leaves_margin_on_the_worst_measured_connection() {
        // At the friend's house the path into diamond kept an idle two-way
        // flow through 90 s and lost it at 120 s, in 138 inbound runs
        // (2026-11-14/16): diamond's own firewall, at Linux's default
        // connection-tracking timeout, not the router, which kept it through
        // 120 s. Shorter than the hotspot's 120-300 s. A keepalive without
        // real margin is one dropped packet away from a connection that dies
        // with no error anywhere, so two intervals, one of them lost, must
        // still fit inside the longest silence confirmed safe.
        let keepalive = Duration::from_millis(DEFAULT_KEEPALIVE_MS);
        let shortest_confirmed_safe = Duration::from_secs(90);
        assert!(keepalive * 2 <= shortest_confirmed_safe,
            "{keepalive:?} twice over is past {shortest_confirmed_safe:?}");
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
            parse_message(&encode_heartbeat("diamond", 42, 9, Some(&TEST_JOB), false)),
            Some(Message::Heartbeat { label: "diamond".into(), run: Some(9), holding: Holding::Job(TEST_JOB), reachable: Some(false) })
        );
        assert_eq!(
            parse_message(&encode_heartbeat("diamond", 42, 9, None, false)),
            Some(Message::Heartbeat { label: "diamond".into(), run: Some(9), holding: Holding::Nothing, reachable: Some(false) })
        );
        assert_eq!(
            parse_message(&encode_departing("diamond")),
            Some(Message::Departing { label: "diamond".into() })
        );
        assert_eq!(
            parse_message(&encode_checkpoint("diamond", &TEST_JOB, 7, b"opaque")),
            Some(Message::Checkpoint {
                label: "diamond".into(),
                job: TEST_JOB,
                tick: 7,
                state: b"opaque".to_vec()
            })
        );
    }

    #[test]
    fn a_silent_connection_to_a_former_member_is_dropped_and_only_that() {
        let now = Instant::now();
        let limit = Duration::from_secs(4);
        let long_ago = now - Duration::from_secs(10);
        let (stale, fresh, stranger, present) =
            (PeerId::random(), PeerId::random(), PeerId::random(), PeerId::random());

        let connected_at: HashMap<PeerId, Instant> = [
            (stale, long_ago),
            (fresh, now - Duration::from_secs(1)),
            (stranger, long_ago),
            (present, long_ago),
        ].into_iter().collect();
        let once: HashSet<PeerId> = [stale, fresh, present].into_iter().collect();
        let mut members = HashMap::new();
        members.insert(present, Member {
            label: "present".into(), joined: long_ago, last_heard: now,
            disconnected: false, run: None, holding: Holding::Job(TEST_JOB), reachable: false,
        });

        // A connection just made has not had time to speak; one to a node of
        // another mesh never will; one to a present member is speaking.
        assert_eq!(silent_connections(&connected_at, &members, &once, now, limit), vec![stale]);
    }

    #[test]
    fn a_heartbeat_from_before_runs_existed_still_counts() {
        // The boxes in the field run the older build until each is redeployed.
        // Rejecting its heartbeat as malformed would declare a live node dead.
        let mut old = vec![KIND_HEARTBEAT];
        put_str(&mut old, "diamond");
        old.extend_from_slice(&42u64.to_le_bytes());
        assert_eq!(
            parse_message(&old),
            Some(Message::Heartbeat { label: "diamond".into(), run: None, holding: Holding::Unknown, reachable: None })
        );
    }

    /// A heartbeat from a build that had runs but not jobs: alive, run known,
    /// and not a candidate until it says what it holds.
    #[test]
    fn a_heartbeat_from_before_jobs_were_said_holds_unknown() {
        let mut old = vec![KIND_HEARTBEAT];
        put_str(&mut old, "diamond");
        old.extend_from_slice(&42u64.to_le_bytes());
        old.extend_from_slice(&9u64.to_le_bytes());
        assert_eq!(
            parse_message(&old),
            Some(Message::Heartbeat { label: "diamond".into(), run: Some(9), holding: Holding::Unknown, reachable: None })
        );
    }

    #[test]
    fn reachability_survives_a_round_trip() {
        for (job, holding) in [(Some(&TEST_JOB), Holding::Job(TEST_JOB)), (None, Holding::Nothing)] {
            for reachable in [true, false] {
                assert_eq!(
                    parse_message(&encode_heartbeat("riverboat", 1, 2, job, reachable)),
                    Some(Message::Heartbeat { label: "riverboat".into(), run: Some(2), holding, reachable: Some(reachable) })
                );
            }
        }
    }

    /// A node built before heartbeats said whether they could be reached is
    /// never asked to introduce anyone: assuming it could would send others to
    /// reserve introductions on a machine that may not be able to make them.
    #[test]
    fn a_heartbeat_from_before_reachability_was_said_is_unreachable() {
        let mut old = vec![KIND_HEARTBEAT];
        put_str(&mut old, "riverboat");
        old.extend_from_slice(&1u64.to_le_bytes());
        old.extend_from_slice(&2u64.to_le_bytes());
        old.push(1);
        old.extend_from_slice(&TEST_JOB);
        let parsed = parse_message(&old);
        assert_eq!(
            parsed,
            Some(Message::Heartbeat { label: "riverboat".into(), run: Some(2), holding: Holding::Job(TEST_JOB), reachable: None })
        );
        assert!(!member_reachable(&parsed.unwrap()));
    }

    /// The old checkpoint kind carried no job, so it cannot be checked, and
    /// a node that misread it would take another job's state as its own.
    #[test]
    fn the_retired_checkpoint_kind_is_ignored() {
        let mut old = vec![3u8];
        put_str(&mut old, "diamond");
        old.extend_from_slice(&7u64.to_le_bytes());
        old.extend_from_slice(&6u32.to_le_bytes());
        old.extend_from_slice(b"opaque");
        assert_eq!(parse_message(&old), None);
    }

    fn holding(h: Holding) -> Member {
        Member { label: "peer".into(), joined: Instant::now(), last_heard: Instant::now(),
                 disconnected: false, run: Some(1), holding: h, reachable: false }
    }

    /// Finding 5's first case: the lowest identifier in the mesh holds no job.
    /// It used to win every election and do nothing, so the job stopped.
    #[test]
    fn a_member_without_the_job_cannot_block_one_that_has_it() {
        let mut peers = vec![PeerId::random(), PeerId::random()];
        peers.sort();
        let (jobless, survivor) = (peers[0], peers[1]);
        let mut members = HashMap::new();
        members.insert(jobless, holding(Holding::Nothing));
        assert!(Work::should_take_over(&survivor, &members, &TEST_JOB));
        members.insert(jobless, holding(Holding::Unknown));
        assert!(Work::should_take_over(&survivor, &members, &TEST_JOB), "unknown is not a candidate either");
    }

    /// Finding 5's second case: two members holding different job files are
    /// not candidates for each other's job.
    #[test]
    fn a_member_holding_a_different_job_is_not_a_candidate() {
        let mut peers = vec![PeerId::random(), PeerId::random(), PeerId::random()];
        peers.sort();
        let (other_job, me, same_job) = (peers[0], peers[1], peers[2]);
        let mut members = HashMap::new();
        members.insert(other_job, holding(Holding::Job([9; 32])));
        members.insert(same_job, holding(Holding::Job(TEST_JOB)));
        assert!(Work::should_take_over(&me, &members, &TEST_JOB));
        // And the one holding the same job, if lower, still wins.
        members.insert(other_job, holding(Holding::Job(TEST_JOB)));
        assert!(!Work::should_take_over(&me, &members, &TEST_JOB));
    }

    #[test]
    fn a_suspect_is_only_succeeded_by_a_member_that_can_run_it() {
        let mut peers = vec![PeerId::random(), PeerId::random(), PeerId::random()];
        peers.sort();
        let (jobless, me, owner) = (peers[0], peers[1], peers[2]);
        let now = Instant::now();
        let after = Duration::from_millis(500);
        let mut members = HashMap::new();
        members.insert(jobless, holding(Holding::Nothing));
        let mut quiet_owner = holding(Holding::Job(TEST_JOB));
        quiet_owner.last_heard = now - Duration::from_millis(600);
        members.insert(owner, quiet_owner);
        assert!(Work::should_speculate(&me, &owner, &members, now, after, &TEST_JOB));
    }

    #[test]
    fn repeated_lines_of_one_kind_make_distinct_items() {
        // Finding 6: two lines of one kind restarted the ordinal and made
        // three widgets instead of five.
        assert_eq!(production("produce widget 2\nproduce widget 3\n"),
                   Ok(vec![("widget".to_string(), 5)]));
        assert_eq!(production("produce widget 2\nproduce gear 1\nproduce widget 1"),
                   Ok(vec![("widget".to_string(), 3), ("gear".to_string(), 1)]));
    }

    #[test]
    fn the_factorys_one_line_derives_the_serials_it_always_did() {
        assert_eq!(production("produce widget 4\n"), Ok(vec![("widget".to_string(), 4)]));
        assert_eq!(production("produce widget\n"), Ok(vec![("widget".to_string(), 1)]));
        assert_eq!(production("idle\n"), Ok(vec![]));
    }

    #[test]
    fn a_tick_that_asks_for_too_much_or_nonsense_produces_nothing() {
        // Finding 2: a few bytes of effect asked for four billion items.
        assert!(production("produce widget 4294967295").is_err());
        assert!(production(&format!("produce widget {}", MAX_PRODUCED_PER_TICK + 1)).is_err());
        let near = MAX_PRODUCED_PER_TICK / 2 + 1;
        assert!(production(&format!("produce a {near}\nproduce b {near}")).is_err(), "the total counts");
        assert!(production("produce widget lots").is_err(), "not a number is not one");
        assert!(production("produce widget 2 extra").is_err());
        assert!(production(&format!("produce {} 1", "k".repeat(ledger::MAX_KIND + 1))).is_err());
        assert!(production("produce").is_err());
    }

    #[test]
    fn long_kinds_are_chunked_to_fit_a_message() {
        // Finding 9: 512 items of a 256-byte kind came to 150 KB.
        let kind = "k".repeat(ledger::MAX_KIND);
        let items: Vec<(String, ledger::Serial)> = (0..1500u32)
            .map(|n| (kind.clone(), ledger::Serial::derive(&TEST_JOB, 1, &kind, n)))
            .collect();
        let chunks = chunks_that_fit(&items, PRODUCED_BUDGET);
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), items.len());
        let label = "l".repeat(MAX_LABEL);
        for chunk in &chunks {
            assert!(encode_produced(&label, chunk).len() + GOSSIP_ENVELOPE <= GOSSIP_MAX_BYTES);
        }
        // Ordinary kinds still go 512 at a time.
        let widgets = serials(&(0..1200).collect::<Vec<_>>());
        assert_eq!(chunks_that_fit(&widgets, PRODUCED_BUDGET)[0].len(), LEDGER_CHUNK);
    }

    #[test]
    fn the_largest_checkpoint_fits_one_message() {
        let state = vec![0u8; MAX_CHECKPOINT_STATE];
        let label = "l".repeat(MAX_LABEL);
        assert!(encode_checkpoint(&label, &TEST_JOB, u64::MAX, &state).len() + GOSSIP_ENVELOPE
                <= GOSSIP_MAX_BYTES);
    }

    #[test]
    fn a_recurring_problem_is_said_once_a_minute_with_its_count() {
        let mut quiet = Quiet::default();
        let t0 = Instant::now();
        assert_eq!(quiet.say("x", t0), Some(0));
        assert_eq!(quiet.say("x", t0 + Duration::from_secs(1)), None);
        assert_eq!(quiet.say("x", t0 + Duration::from_secs(2)), None);
        assert_eq!(quiet.say("y", t0 + Duration::from_secs(2)), Some(0), "kinds are separate");
        assert_eq!(quiet.say("x", t0 + QUIET_FOR), Some(2));
    }

    #[test]
    fn a_return_after_a_restart_is_not_a_false_alarm() {
        let peer = PeerId::random();
        let at = Instant::now();
        let mut absences = Absences::default();
        absences.vanished(peer, Some(1), at);
        let back = absences.heard(&peer, 2, at + Duration::from_secs(12)).unwrap();
        assert_eq!(back, Return { restarted: true, away: Duration::from_secs(12) });
    }

    #[test]
    fn a_return_without_a_restart_is_a_false_alarm() {
        // Same run: the process never stopped, so the takeover it caused was
        // work migrated for nothing.
        let peer = PeerId::random();
        let at = Instant::now();
        let mut absences = Absences::default();
        absences.vanished(peer, Some(7), at);
        assert!(!absences.heard(&peer, 7, at + Duration::from_secs(30)).unwrap().restarted);
    }

    #[test]
    fn a_return_is_reported_once_and_only_for_the_absent() {
        let peer = PeerId::random();
        let at = Instant::now();
        let mut absences = Absences::default();
        assert_eq!(absences.heard(&peer, 7, at), None, "never gone, so never back");
        absences.vanished(peer, Some(7), at);
        assert!(absences.heard(&peer, 7, at).is_some());
        assert_eq!(absences.heard(&peer, 7, at), None, "every later heartbeat would repeat it");
    }

    #[test]
    fn the_visible_gap_runs_from_the_last_checkpoint_not_the_decision() {
        // Measuring from the decision would leave out detection, which is most
        // of what a player waits through.
        let checkpoint = Instant::now();
        let resuming = Resuming {
            cause: "vanished",
            decided: checkpoint + Duration::from_millis(2_300),
            last_checkpoint: Some(checkpoint),
        };
        let (resume, gap) = resuming.timings(checkpoint + Duration::from_millis(2_450));
        assert_eq!(resume, Duration::from_millis(150));
        assert_eq!(gap, Some(Duration::from_millis(2_450)));
    }

    #[test]
    fn a_speculative_first_tick_is_timed_from_the_suspicion() {
        // The visible gap is what a player waits through, so it runs from the
        // last checkpoint heard to the first state produced — and under
        // speculation that first state is produced before anybody has decided
        // the owner is gone.
        let checkpoint = Instant::now();
        let suspected = checkpoint + Duration::from_millis(500);
        let resuming = Resuming {
            cause: "speculated",
            decided: suspected,
            last_checkpoint: Some(checkpoint),
        };
        let (resume, gap) = resuming.timings(suspected + Duration::from_millis(100));
        assert_eq!(resume, Duration::from_millis(100));
        assert_eq!(gap, Some(Duration::from_millis(600)),
                   "600 ms is the whole point: detection is no longer inside it");
    }

    #[test]
    fn confirming_a_speculation_does_not_resume_twice() {
        // The first tick already happened and already reported itself. Arming
        // another resume here would report two for one takeover, the second
        // measured from a decision that changed nothing about the work.
        assert!(!resume_is_owed(true), "already running it: the resume was reported at that tick");
        assert!(resume_is_owed(false), "nothing has ticked yet, so the resume is still owed");
    }

    /// Two random peer ids sorted so a test can talk about "lower" and
    /// "higher" without caring which random value landed where.
    fn ordered_peers() -> (PeerId, PeerId) {
        let (a, b) = (PeerId::random(), PeerId::random());
        if a < b { (a, b) } else { (b, a) }
    }

    #[test]
    fn a_speculator_behind_the_sender_abandons_and_counts_from_where_it_began() {
        // A speculator resumed from an older checkpoint, so an owner that
        // never stopped is normally ahead. The speculator yields, and reports
        // how many ticks it ran on a suspicion that turned out wrong.
        let (me, owner) = ordered_peers();
        let spec = Speculating { about: owner, since: Instant::now(), from_tick: 40 };
        assert_eq!(
            resolve_speculation(&spec, (52, &me), (60, &owner)),
            Speculation::Abandoned { ticks_wasted: 12 },
        );
    }

    #[test]
    fn a_speculator_ahead_of_the_sender_outruns_it() {
        // The rule does not care which of the two was speculating: whoever is
        // further along keeps the job, even when that is the speculator.
        let (me, owner) = ordered_peers();
        let spec = Speculating { about: owner, since: Instant::now(), from_tick: 10 };
        assert_eq!(resolve_speculation(&spec, (50, &me), (30, &owner)), Speculation::Outran);
    }

    #[test]
    fn a_tie_goes_to_the_lower_identifier_in_either_direction() {
        let (lower, higher) = ordered_peers();

        // `me` holds the lower identifier and ticks are tied: this node wins.
        let spec = Speculating { about: higher, since: Instant::now(), from_tick: 20 };
        assert_eq!(
            resolve_speculation(&spec, (20, &lower), (20, &higher)),
            Speculation::Outran,
        );

        // `me` holds the higher identifier and ticks are tied: the sender wins.
        let spec = Speculating { about: lower, since: Instant::now(), from_tick: 20 };
        assert_eq!(
            resolve_speculation(&spec, (20, &higher), (20, &lower)),
            Speculation::Abandoned { ticks_wasted: 0 },
        );
    }

    #[test]
    fn a_speculation_that_never_ticked_reports_zero_wasted_not_an_underflow() {
        // This node's tick never moved past where speculation began, and the
        // sender is further along, so this abandons — with nothing wasted,
        // not a panic from `from_tick` subtracting past the current tick.
        let (me, owner) = ordered_peers();
        let spec = Speculating { about: owner, since: Instant::now(), from_tick: 40 };
        assert_eq!(
            resolve_speculation(&spec, (40, &me), (41, &owner)),
            Speculation::Abandoned { ticks_wasted: 0 },
        );
    }

    fn serials(ticks: &[u64]) -> Vec<(String, ledger::Serial)> {
        ticks
            .iter()
            .map(|tick| {
                ("widget".to_string(), ledger::Serial::derive(&[7; 32], *tick, "widget", 0))
            })
            .collect()
    }

    #[test]
    fn productions_and_digests_survive_a_round_trip() {
        let items = serials(&[1, 2, 3]);
        assert_eq!(
            parse_message(&encode_produced("diamond", &items)),
            Some(Message::Produced { label: "diamond".into(), items })
        );

        let mut cells = vec![ledger::Cell::default(); ledger::FANOUT];
        cells[3] = ledger::Cell { count: 9, xor: 0xdead_beef };
        assert_eq!(
            parse_message(&encode_ledger_summary("diamond", 1234, &cells)),
            Some(Message::LedgerSummary { label: "diamond".into(), held: 1234, cells: cells.clone() })
        );
        let to = PeerId::random();
        assert_eq!(
            parse_message(&encode_ledger_bucket("diamond", &to, 200, true, &cells)),
            Some(Message::LedgerBucket { label: "diamond".into(), to, bucket: 200, answer: true, cells })
        );
    }

    #[test]
    fn a_summary_cut_short_is_not_a_message() {
        // Compared against a full one, a truncated summary would report every
        // missing position as a difference and start repairs for all of them.
        let summary = encode_ledger_summary("diamond", 1, &vec![ledger::Cell::default(); ledger::FANOUT]);
        assert_eq!(parse_message(&summary[..summary.len() - 1]), None);
    }

    #[test]
    fn the_retired_whole_set_digest_is_ignored() {
        // Kind 6, from nodes not yet redeployed. Acting on it would mean
        // guessing at a format this build no longer understands.
        let mut old = vec![6u8];
        put_str(&mut old, "diamond");
        old.extend_from_slice(&1u64.to_le_bytes());
        old.extend_from_slice(&[0u8; 32]);
        assert_eq!(parse_message(&old), None);
    }

    #[test]
    fn a_summary_fits_comfortably_in_one_gossip_message() {
        let summary = encode_ledger_summary("a-long-node-label", u64::MAX,
                                            &vec![ledger::Cell::default(); ledger::FANOUT]);
        assert!(summary.len() < 4_096, "summary is {} bytes", summary.len());
    }

    #[test]
    fn an_announcement_of_nothing_is_not_a_message() {
        // Otherwise "malformed" and "meaningless" arrive looking identical,
        // and a parser that accepts both can no longer reject either.
        assert_eq!(parse_message(&encode_produced("diamond", &[])), None);
    }

    #[test]
    fn a_production_is_never_mistaken_for_another_kind_of_message() {
        // The nightmare in a hand-rolled codec: a new message kind that an
        // older parser reads as an old one. A heartbeat that is really an
        // inventory would make a departed node look present.
        let produced = encode_produced("diamond", &serials(&[1]));
        assert!(matches!(parse_message(&produced), Some(Message::Produced { .. })));
        let cells = vec![ledger::Cell::default(); ledger::FANOUT];
        let summary = encode_ledger_summary("diamond", 1, &cells);
        assert!(matches!(parse_message(&summary), Some(Message::LedgerSummary { .. })));
    }

    #[test]
    fn two_nodes_separated_and_reunited_end_up_holding_the_same_things() {
        // The whole point of choosing deltas over a ledger that rides in the
        // checkpoint. Neither node is authoritative and neither asks what the
        // other has; they announce what they made and union what they hear.
        let mut alone = ledger::Ledger::new();
        let mut apart = ledger::Ledger::new();
        for (kind, serial) in serials(&[1, 2, 3]) {
            alone.record(&kind, serial);
        }
        for (kind, serial) in serials(&[3, 4, 5]) {
            apart.record(&kind, serial);
        }
        assert_ne!(alone.summary(), apart.summary(), "should start disagreeing");

        // Each sends everything it holds — what a repair does for every cell
        // that differs, and here every cell does — and each absorbs the
        // other's. Tick 3 was made by both and is one item.
        let from_alone = encode_produced("alone", &alone.iter()
            .map(|(k, s)| (k.to_string(), *s)).collect::<Vec<_>>());
        let from_apart = encode_produced("apart", &apart.iter()
            .map(|(k, s)| (k.to_string(), *s)).collect::<Vec<_>>());
        let Some(Message::Produced { items: theirs, .. }) = parse_message(&from_apart) else {
            panic!("did not parse");
        };
        let Some(Message::Produced { items: ours, .. }) = parse_message(&from_alone) else {
            panic!("did not parse");
        };
        alone.absorb(theirs);
        apart.absorb(ours);

        assert_eq!(alone.summary(), apart.summary(), "should have converged");
        assert_eq!(alone.len(), 5, "tick 3 was made twice and exists once");
    }

    #[test]
    fn a_second_repair_waits() {
        // A node joining an established mesh disagrees with everyone at once.
        // Without a floor between repairs, every peer answers with a full
        // ledger simultaneously.
        let mut last = None;
        assert!(repair_allowed(&mut last), "the first repair should go");
        assert!(!repair_allowed(&mut last), "an immediate second should not");
        last = Some(Instant::now() - REPAIR_NO_SOONER_THAN);
        assert!(repair_allowed(&mut last), "one should go again after the wait");
    }

    #[test]
    fn a_repair_is_broken_into_messages_a_peer_will_accept() {
        // An oversized publish is dropped by gossipsub without complaint, so a
        // repair that is too big does not fail — it silently never arrives.
        let many = serials(&(0..LEDGER_CHUNK as u64 * 2 + 7).collect::<Vec<_>>());
        let chunks: Vec<_> = many.chunks(LEDGER_CHUNK).collect();
        assert_eq!(chunks.len(), 3, "should be split, not sent whole");
        for chunk in chunks {
            let encoded = encode_produced("diamond", chunk);
            assert!(encoded.len() < 64 * 1024,
                    "a chunk came to {} bytes, near gossipsub's cap", encoded.len());
            assert!(matches!(parse_message(&encoded), Some(Message::Produced { .. })));
        }
    }

    #[test]
    fn checkpoints_carry_bytes_that_are_not_text() {
        // State is opaque, so the wire format has to survive anything a job
        // chooses to put in it — the reason this is framed rather than
        // delimited.
        let state: Vec<u8> = (0u8..=255).collect();
        let parsed = parse_message(&encode_checkpoint("n", &TEST_JOB, 1, &state)).expect("parses");
        assert_eq!(parsed, Message::Checkpoint { label: "n".into(), job: TEST_JOB, tick: 1, state });
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
            run: None,
            holding: Holding::Job(TEST_JOB),
            reachable: false,
        };

        let mut winners = 0;
        for me in &peers {
            let others: HashMap<PeerId, Member> = peers
                .iter()
                .filter(|p| *p != me)
                .map(|p| (*p, member("peer")))
                .collect();
            if Work::should_take_over(me, &others, &TEST_JOB) {
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
            run: None,
            holding: Holding::Job(TEST_JOB),
            reachable: false,
        });
        assert!(Work::should_take_over(&me, &members, &TEST_JOB));
    }

    #[test]
    fn a_lone_survivor_takes_over() {
        // The last node standing has nobody to lose the comparison to, and the
        // job should continue rather than stop because no election was possible.
        assert!(Work::should_take_over(&PeerId::random(), &HashMap::new(), &TEST_JOB));
    }

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("tab\there"), "\"tab\\u0009here\"");
    }

    #[test]
    fn asking_the_router_takes_longer_than_a_peer_takes_to_be_declared_dead() {
        // The reason the renewal is spawned rather than awaited in the select
        // loop, recorded as an assertion so that it cannot quietly stop being
        // true. A router that never answers costs the full retry budget, and
        // that budget is longer than the detection window — so awaiting it
        // inline makes the node miss enough heartbeats to be declared dead by
        // every peer, and to declare every peer dead in turn.
        //
        // Measured, not theorised: on a laptop tethered to a phone this fired
        // every fifteen minutes for two days, producing 96 false departures and
        // 94 needless job takeovers, none of which were real.
        let detect = Duration::from_millis(DEFAULT_DETECT_MS);
        assert!(
            portmap::WORST_CASE > detect,
            "a failing renewal ({:?}) now fits inside the detection window ({:?}); \
             if that is genuinely true the spawn is no longer load-bearing, but \
             check why before deleting it",
            portmap::WORST_CASE,
            detect
        );
    }

    fn member_last_heard(at: Instant) -> Member {
        Member { label: "peer".into(), joined: at, last_heard: at, disconnected: false, run: Some(1), holding: Holding::Job(TEST_JOB), reachable: false }
    }

    #[test]
    fn speculation_waits_for_the_owner_to_go_quiet() {
        let now = Instant::now();
        let after = Duration::from_millis(500);
        let (me, owner) = (PeerId::random(), PeerId::random());
        let mut members = HashMap::new();
        members.insert(owner, member_last_heard(now - Duration::from_millis(200)));

        assert!(!Work::should_speculate(&me, &owner, &members, now, after, &TEST_JOB),
                "200 ms of silence is one lost packet, not a missing machine");

        members.insert(owner, member_last_heard(now - Duration::from_millis(600)));
        assert!(Work::should_speculate(&me, &owner, &members, now, after, &TEST_JOB));
    }

    #[test]
    fn only_the_successor_speculates_and_the_suspect_does_not_count() {
        // The election must exclude the node being suspected. Including it
        // would mean that in a two-node mesh the higher identifier is never
        // eligible, and speculation would appear simply not to work.
        let now = Instant::now();
        let after = Duration::from_millis(500);
        let quiet = now - Duration::from_secs(1);
        let mut ids = [PeerId::random(), PeerId::random(), PeerId::random()];
        ids.sort();
        let [low, middle, high] = ids;

        let mut members = HashMap::new();
        members.insert(low, member_last_heard(quiet));
        members.insert(high, member_last_heard(now));

        // `low` owns the job and has gone quiet. `middle` is the lowest of the
        // survivors, so it speculates and `high` does not.
        assert!(Work::should_speculate(&middle, &low, &members, now, after, &TEST_JOB));

        let mut seen_by_high = HashMap::new();
        seen_by_high.insert(low, member_last_heard(quiet));
        seen_by_high.insert(middle, member_last_heard(now));
        assert!(!Work::should_speculate(&high, &low, &seen_by_high, now, after, &TEST_JOB));
    }

    #[test]
    fn a_suspect_that_speaks_again_ends_the_suspicion() {
        // The case a lossy link produces and a dead machine does not: the
        // owner's heartbeats arrive while its checkpoints are dropped. It
        // never vanishes, so no takeover confirms the suspicion, and it never
        // checkpoints, so nothing answers it. Left running, the speculator is
        // a shadow owner forever.
        let now = Instant::now();
        let owner = PeerId::random();
        let spec = Speculating { about: owner, since: now, from_tick: 40 };

        let mut members = HashMap::new();
        members.insert(owner, member_last_heard(now - Duration::from_millis(600)));
        assert!(!suspicion_disproved(&spec, &members),
                "still silent: the suspicion stands");

        members.insert(owner, member_last_heard(now + Duration::from_millis(100)));
        assert!(suspicion_disproved(&spec, &members),
                "heard since the suspicion was raised: it was never gone");
    }

    #[test]
    fn a_suspect_that_has_vanished_does_not_disprove_its_own_suspicion() {
        // Gone from the membership means the sweep has declared it lost, and
        // the takeover — not this — is what ends the speculation. Answering
        // yes here would abandon the work a moment before it is confirmed.
        let owner = PeerId::random();
        let spec = Speculating { about: owner, since: Instant::now(), from_tick: 1 };
        assert!(!suspicion_disproved(&spec, &HashMap::new()));
    }

    #[test]
    fn standing_down_ends_the_speculation_and_the_resume_it_owed() {
        // Every way out of speculation that is not a takeover comes through
        // here, because each of them stops the ticking: a resume left armed
        // would be reported by whatever ticks next, timed from a suspicion
        // that was put down long before.
        let now = Instant::now();
        let mut speculating = Some(Speculating { about: PeerId::random(), since: now, from_tick: 7 });
        let mut resuming = Some(Resuming {
            cause: "speculated",
            decided: now,
            last_checkpoint: Some(now),
        });

        let spec = stand_down(&mut speculating, &mut resuming);
        assert_eq!(spec.map(|s| s.from_tick), Some(7), "the caller is told how far it had got");
        assert!(speculating.is_none());
        assert!(resuming.is_none(), "nothing ticks after this, so nothing resumes");

        // A node that was not speculating keeps whatever resume it is owed:
        // an ordinary takeover arms one and must still report it.
        let mut none = None;
        let mut owed = Some(Resuming { cause: "vanished", decided: now, last_checkpoint: None });
        assert!(stand_down(&mut none, &mut owed).is_none());
        assert!(owed.is_some(), "an ordinary takeover's resume is not a speculation's");
    }

    #[test]
    fn a_peer_that_is_not_a_member_is_not_speculated_about() {
        // Its silence cannot be measured, so there is nothing to suspect.
        let now = Instant::now();
        let (me, stranger) = (PeerId::random(), PeerId::random());
        assert!(!Work::should_speculate(&me, &stranger, &HashMap::new(), now,
                                        Duration::from_millis(500), &TEST_JOB));
    }

    #[test]
    fn suspicion_falls_strictly_inside_detection() {
        // Speculation that fired at or after detection would never happen: the
        // takeover it is meant to precede would already have occurred.
        let detect = Duration::from_millis(DEFAULT_DETECT_MS);
        assert!(detect / 4 < detect);
        assert!(detect / 4 >= Duration::from_millis(DEFAULT_HEARTBEAT_MS) / 2,
                "a trigger below half a heartbeat would suspect a node that is answering normally");
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
