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

use std::collections::{HashMap, HashSet};
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

/// How many items to put in one message.
///
/// Gossipsub refuses anything oversized, and a refused repair is invisible —
/// it does not fail loudly, it simply never arrives. Sized well inside the
/// default cap so that a long kind name cannot push a chunk over it.
const LEDGER_CHUNK: usize = 512;

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
    /// `run` identifies one start of a node's process. The identity survives a
    /// restart by design (§8.1), which is exactly why it cannot say whether a
    /// peer that went silent and came back had restarted or had merely been
    /// unreachable — and those are different failures that must not be counted
    /// together.
    Heartbeat { label: String, run: Option<u64> },
    Departing { label: String },
    Checkpoint { label: String, tick: u64, state: Vec<u8> },
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

fn encode_heartbeat(label: &str, counter: u64, run: u64) -> Vec<u8> {
    let mut out = vec![KIND_HEARTBEAT];
    put_str(&mut out, label);
    out.extend_from_slice(&counter.to_le_bytes());
    out.extend_from_slice(&run.to_le_bytes());
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
            Some(Message::Heartbeat { label, run })
        }
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
    fn should_take_over(me: &PeerId, members: &HashMap<PeerId, Member>) -> bool {
        Self::lowest_among(me, members.keys())
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
    ) -> bool {
        members
            .get(owner)
            .is_some_and(|suspect| now.duration_since(suspect.last_heard) >= after)
            && Self::lowest_among(me, members.keys().filter(|peer| *peer != owner))
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

/// Which of a speculating node and the checkpoint's sender keeps the job.
///
/// Deliberately just [`Work::theirs`] plus the arithmetic for how much was
/// wasted: a speculator is not a special case of the two-owners rule, it is
/// an instance of it. Kept separate from the `Message::Checkpoint` handler so
/// the decision can be tested without driving the event loop.
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
        speculate_after: None,
        json: false,
        job: None,
        job_tick: Duration::from_millis(DEFAULT_JOB_TICK_MS),
        keepalive: Duration::from_millis(DEFAULT_KEEPALIVE_MS),
        mesh: "default".to_string(),
        port: DEFAULT_PORT,
        identity: "/var/lib/mesh-node/identity".to_string(),
        ledger: "/var/lib/mesh-node/ledger".to_string(),
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

async fn run(config: Config) -> Result<()> {
    let keypair = load_or_create_identity(&config)?;
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
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
    let mut say_digest = tokio::time::interval(DIGEST_EVERY);
    let mut last_repair: Option<Instant> = None;
    let mut work_summary = tokio::time::interval(WORK_SUMMARY_EVERY);
    let mut since_summary: HashMap<String, u64> = HashMap::new();
    let mut ticks_since_summary: u64 = 0;
    let mut mapping: Option<portmap::Mapping> = None;
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
        tokio::sync::mpsc::channel::<std::result::Result<portmap::Mapping, String>>(1);
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
            let ledger = match ledger::Ledger::load(std::path::Path::new(&config.ledger)) {
                Ok(ledger) => {
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
            _ = dial_retry.tick(), if !pending.is_empty() => {
                dial_attempts += 1;
                // Anything already reaching a live peer is skipped. A retry
                // loop that keeps dialling a connected peer opens a new
                // connection every two seconds forever.
                let live: Vec<Multiaddr> = reached_by
                    .iter()
                    .filter(|(peer, _)| swarm.is_connected(peer))
                    .flat_map(|(_, addrs)| addrs.iter().cloned())
                    .collect();
                pending.retain(|addr| !live.contains(addr));

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
            _ = renew_mapping.tick(), if config.map_port && !renewing => {
                // Spawned, not awaited. See `mapping_rx` above for why.
                renewing = true;
                let port = config.port;
                let tx = mapping_tx.clone();
                tokio::spawn(async move {
                    let answer = portmap::request(port, portmap::LIFETIME_SECS)
                        .await
                        .map_err(|error| error.to_string());
                    // The receiver lives as long as the loop, and the loop
                    // outlives this task in every case that matters. If it has
                    // gone, the node is shutting down and the answer is moot.
                    let _ = tx.send(answer).await;
                });
            }

            Some(answer) = mapping_rx.recv() => {
                renewing = false;
                match answer {
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
                        emit(&config, "port-map-failed", &error,
                             &[("error", &error),
                               ("was_mapped", if mapping.is_some() { "true" } else { "false" })]);
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
            }

            _ = heartbeat.tick() => {
                counter += 1;
                // Failing to publish is normal and uninteresting while this node
                // is the only one subscribed to the topic.
                let _ = swarm.behaviour_mut().gossipsub
                    .publish(topic.clone(), encode_heartbeat(&config.label, counter, run));
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

                // A speculating node runs the job without owning it. What it
                // publishes is indistinguishable from an owner's because it is
                // identical — same job, same tick, same derived serials — so
                // nothing downstream needs to know which this is.
                if work.owner != Some(me) && work.speculating.is_none() { continue }

                let mut inputs = Vec::new();
                inputs.extend_from_slice(&work.tick.to_le_bytes());
                inputs.extend_from_slice(&4u32.to_le_bytes());  // steel delivered
                inputs.extend_from_slice(&3u32.to_le_bytes());  // workers present

                match work.job.tick(&work.state, &inputs) {
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
                        let fresh = record_production(work, &effects);
                        // Checkpointed every tick because this state is tiny.
                        // Real work would checkpoint less often and trade a
                        // little replayed work for a lot less traffic.
                        let _ = swarm.behaviour_mut().gossipsub.publish(
                            topic.clone(),
                            encode_checkpoint(&config.label, work.tick, &work.state),
                        );
                        // Separately from the checkpoint, and that separation
                        // is the design: the checkpoint is where the job *is*,
                        // small and sent constantly; this is what the job has
                        // *made*, and it is sent once per thing, forever.
                        publish_production(&mut swarm, &topic, &config, &fresh);
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
                        // so it has to be what starts the search for it again.
                        redial_lost_peer(&config, &peer, &reached_by,
                                         &mut pending, &mut dial_attempts);

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
                            && Work::should_speculate(&me, &owner, &members, now, after)
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
                    redial_lost_peer(&config, &peer, &reached_by, &mut pending, &mut dial_attempts);
                    emit(&config, "silent-connection",
                         "connected to a former member that has said nothing — reconnecting",
                         &[("peer_id", &peer.to_string())]);
                }
            }

            event = swarm.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    connected_at.entry(peer_id).or_insert_with(Instant::now);
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
                        // Every address known to reach this peer comes off the
                        // list, not only the one that happened to succeed.
                        // Otherwise the others keep being dialled at a peer
                        // already sitting on the other end of a connection.
                        if let Some(known) = reached_by.get(&peer_id) {
                            pending.retain(|addr| !known.contains(addr));
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

                    // Announced only when this is news. Announcing on every
                    // connection made a loop: a connection prompts an
                    // announcement, an announcement prompts everyone to dial,
                    // and each dial makes another connection. Two nodes
                    // produced dozens within a second, all to each other.
                    let is_news = seen_at.get(&peer_id) != Some(&reached);
                    seen_at.insert(peer_id, reached.clone());
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
                        });
                        once_members.insert(source);
                        emit(&config, "joined", &format!("{label} joined"),
                             &[("peer_id", &source.to_string()), ("label", &label)]);
                        // Answer immediately rather than waiting for the next
                        // interval. Until two nodes have heard from each other
                        // they each believe they are alone, and a node that
                        // believes it is alone will continue a job that someone
                        // else is also continuing.
                        counter += 1;
                        let _ = swarm.behaviour_mut().gossipsub
                            .publish(topic.clone(), encode_heartbeat(&config.label, counter, run));
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
                                    // or a restart quietly undoes the merge.
                                    let _ = ledger::append(
                                        std::path::Path::new(&config.ledger), &added);
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
                            for chunk in items.chunks(LEDGER_CHUNK) {
                                let _ = swarm.behaviour_mut().gossipsub.publish(
                                    topic.clone(), encode_produced(&config.label, chunk));
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
                            // Already connected, already trying, or already
                            // known to reach a peer we have — each is a reason
                            // not to dial again, and the last one matters most
                            // during a burst, when is_connected can still be
                            // false for a connection halfway through opening.
                            if swarm.is_connected(&peer)
                                || pending.contains(&addr)
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
                            let _ = swarm.dial(addr.clone());
                            pending.push(addr);
                        }

                        Message::Checkpoint { tick, state, .. } => {
                            let Some(w) = work.as_mut() else { continue };

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

                SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
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
                        // one every time anything happens.
                        redial_lost_peer(&config, &peer_id, &reached_by,
                                         &mut pending, &mut dial_attempts);
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
/// Records what a tick made, returning whatever the ledger had not already held.
///
/// The return is what gets announced and written down. It is usually everything
/// produced, but not always: a tick replayed after a takeover derives the same
/// serials as the node that ran it before, so re-running work the mesh already
/// knows about correctly announces nothing. That is the duplicate problem
/// dissolving rather than being handled.
fn record_production(work: &mut Work, effects: &str) -> Vec<(String, ledger::Serial)> {
    let mut fresh = Vec::new();
    for line in effects.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("produce") {
            continue;
        }
        let Some(kind) = parts.next() else { continue };
        let count: u32 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(1);

        for ordinal in 0..count {
            let serial = ledger::Serial::derive(work.job.id(), work.tick, kind, ordinal);
            if work.ledger.record(kind, serial) {
                fresh.push((kind.to_string(), serial));
            }
        }
    }
    fresh
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
    fresh: &[(String, ledger::Serial)],
) {
    if fresh.is_empty() {
        return;
    }
    if let Err(error) = ledger::append(std::path::Path::new(&config.ledger), fresh) {
        // Said once per occurrence rather than swallowed. A node whose ledger
        // is not being written still works and still converges with its peers
        // — it just forgets everything when it restarts, which is exactly the
        // failure that is invisible until the restart.
        emit(config, "ledger-not-saved", &format!("{error}"),
             &[("path", &config.ledger), ("error", &error.to_string())]);
    }
    for chunk in fresh.chunks(LEDGER_CHUNK) {
        let _ = swarm
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), encode_produced(&config.label, chunk));
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
    pending: &mut Vec<Multiaddr>,
    attempts: &mut u32,
) {
    let Some(addresses) = reached_by.get(peer) else { return };
    let mut resumed = 0;
    for addr in addresses {
        if !pending.contains(addr) {
            pending.push(addr.clone());
            resumed += 1;
        }
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
    if !Work::should_take_over(&me, members) {
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
        let _ = tokio::time::timeout(Duration::from_secs(1), portmap::release(config.port)).await;
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
            parse_message(&encode_heartbeat("diamond", 42, 9)),
            Some(Message::Heartbeat { label: "diamond".into(), run: Some(9) })
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
            disconnected: false, run: None,
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
            Some(Message::Heartbeat { label: "diamond".into(), run: None })
        );
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
            run: None,
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
            run: None,
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
        Member { label: "peer".into(), joined: at, last_heard: at, disconnected: false, run: Some(1) }
    }

    #[test]
    fn speculation_waits_for_the_owner_to_go_quiet() {
        let now = Instant::now();
        let after = Duration::from_millis(500);
        let (me, owner) = (PeerId::random(), PeerId::random());
        let mut members = HashMap::new();
        members.insert(owner, member_last_heard(now - Duration::from_millis(200)));

        assert!(!Work::should_speculate(&me, &owner, &members, now, after),
                "200 ms of silence is one lost packet, not a missing machine");

        members.insert(owner, member_last_heard(now - Duration::from_millis(600)));
        assert!(Work::should_speculate(&me, &owner, &members, now, after));
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
        assert!(Work::should_speculate(&middle, &low, &members, now, after));

        let mut seen_by_high = HashMap::new();
        seen_by_high.insert(low, member_last_heard(quiet));
        seen_by_high.insert(middle, member_last_heard(now));
        assert!(!Work::should_speculate(&high, &low, &seen_by_high, now, after));
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
                                        Duration::from_millis(500)));
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
