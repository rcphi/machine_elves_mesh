//! Phase 0 mesh node.
//!
//! Does one thing: forms an overlay with other nodes, keeps track of who is in
//! it, and notices when someone disappears — distinguishing a machine that left
//! cleanly from one that was unplugged.
//!
//! Jobs, checkpointing, and migration come later. Membership has to be
//! trustworthy first, because everything else is built on knowing who is here.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, mdns, noise, ping, tcp, yamux, Multiaddr, PeerId};

/// The topic every node publishes its heartbeat and its goodbye to.
const HEARTBEAT_TOPIC: &str = "machine-elves/heartbeat/0.1";

/// Prefix marking a message as "I am still here".
const MSG_HEARTBEAT: &str = "hb";

/// Prefix marking a message as "I am leaving on purpose".
///
/// This is what §9.6 calls a graceful drain, and it has to be an announcement
/// rather than anything the transport layer reports. A killed process still has
/// its sockets closed tidily by the kernel, so a clean close says nothing about
/// whether the departure was orderly — only the node itself knows that.
const MSG_DEPARTING: &str = "bye";

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
}

/// What is known about one other node.
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
        config.listen.push("/ip4/0.0.0.0/udp/0/quic-v1".parse()?);
        config.listen.push("/ip4/0.0.0.0/tcp/0".parse()?);
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

fn print_usage() {
    println!("mesh-node — forms an overlay and tracks who is in it");
    println!();
    println!("  --label <name>       identify this node in the output");
    println!("  --listen <multiaddr> address to listen on (repeatable)");
    println!("  --peer <multiaddr>   address of a node to dial (repeatable)");
    println!("  --heartbeat-ms <n>   how often to announce presence (default {DEFAULT_HEARTBEAT_MS})");
    println!("  --detect-ms <n>      silence before a peer is presumed gone (default {DEFAULT_DETECT_MS})");
    println!("  --json               emit machine-readable events");
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
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/machine-elves/0.1".into(),
                    key.public(),
                )),
                ping: ping::Behaviour::new(ping::Config::new()),
            })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let topic = gossipsub::IdentTopic::new(HEARTBEAT_TOPIC);
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    for addr in &config.listen {
        swarm.listen_on(addr.clone())?;
    }
    for addr in &config.dial {
        swarm.dial(addr.clone())?;
    }

    let me = *swarm.local_peer_id();
    emit(
        &config,
        "started",
        &format!("node {} is {}", config.label, me),
        &[("peer_id", &me.to_string())],
    );

    let mut members: HashMap<PeerId, Member> = HashMap::new();
    let mut heartbeat = tokio::time::interval(config.heartbeat);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    // Checked several times per detection window so that a departure is
    // reported close to when it actually crossed the threshold, rather than up
    // to a whole window late.
    let mut sweep = tokio::time::interval(config.detect / 4);
    let mut counter: u64 = 0;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                counter += 1;
                let payload = format!("{MSG_HEARTBEAT}|{}|{counter}|{}", config.label, now_millis());
                // Failing to publish is normal and uninteresting while this node
                // is the only one subscribed to the topic.
                let _ = swarm.behaviour_mut().gossipsub.publish(topic.clone(), payload.as_bytes());
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
            }

            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    emit(&config, "listening", &format!("listening on {address}"),
                         &[("addr", &address.to_string())]);
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                    for (peer, addr) in peers {
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        let _ = swarm.dial(addr);
                    }
                }

                SwarmEvent::Behaviour(MeshBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. }
                )) => {
                    let Some(source) = message.source else { continue };
                    let text = String::from_utf8_lossy(&message.data);
                    let Some((kind, label)) = parse_message(&text) else { continue };

                    if kind == MSG_DEPARTING {
                        if let Some(member) = members.remove(&source) {
                            // An announced departure needs no detection window:
                            // the mesh knows at once and can hand work over
                            // before anything stalls.
                            emit(&config, "left",
                                 &format!("{} left, announced", member.label),
                                 &[("peer_id", &source.to_string()),
                                   ("label", &member.label),
                                   ("was_present_ms",
                                    &Instant::now().duration_since(member.joined)
                                        .as_millis().to_string())]);
                        }
                        continue;
                    }

                    if kind != MSG_HEARTBEAT { continue }

                    match members.get_mut(&source) {
                        Some(member) => {
                            member.last_heard = Instant::now();
                            member.disconnected = false;
                        }
                        None => {
                            members.insert(source, Member {
                                label: label.clone(),
                                joined: Instant::now(),
                                last_heard: Instant::now(),
                                disconnected: false,
                            });
                            emit(&config, "joined", &format!("{label} joined"),
                                 &[("peer_id", &source.to_string()), ("label", &label)]);
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
    let payload = format!("{MSG_DEPARTING}|{}", config.label);
    let published = swarm
        .behaviour_mut()
        .gossipsub
        .publish(topic.clone(), payload.as_bytes())
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

    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            _ = swarm.select_next_some() => {}
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    Ok(())
}

/// Splits a gossip payload into its kind and the sender's label.
///
/// Returns `None` for anything unrecognised, so a future version publishing
/// message kinds this one has never heard of is ignored rather than
/// misinterpreted as a heartbeat.
fn parse_message(text: &str) -> Option<(&str, String)> {
    let mut parts = text.split('|');
    let kind = parts.next()?;
    if kind != MSG_HEARTBEAT && kind != MSG_DEPARTING {
        return None;
    }
    let label = parts.next().filter(|l| !l.is_empty())?;
    Some((kind, label.to_string()))
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
    fn reads_heartbeats_and_goodbyes() {
        assert_eq!(
            parse_message("hb|diamond|42|1787360517214"),
            Some(("hb", "diamond".to_string()))
        );
        assert_eq!(
            parse_message("bye|diamond"),
            Some(("bye", "diamond".to_string()))
        );
    }

    #[test]
    fn ignores_messages_it_does_not_understand() {
        // A later version publishing a new kind must not be mistaken for a
        // heartbeat, which would make a departed node look present.
        assert_eq!(parse_message("checkpoint|diamond|..."), None);
        assert_eq!(parse_message(""), None);
        assert_eq!(parse_message("hb"), None);
        assert_eq!(parse_message("hb|"), None);
    }

    #[test]
    fn a_goodbye_is_never_mistaken_for_a_heartbeat() {
        // The whole distinction between a 4ms handover and a 3s hole rests on
        // these two never being confused.
        let (kind, _) = parse_message("bye|beta").expect("parses");
        assert_ne!(kind, MSG_HEARTBEAT);
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
