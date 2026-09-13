//! Does iroh get a direct connection where our own hole punching could not?
//!
//! Measured, not assumed. Our probe found diamond's router filters by address
//! *and* port, which is why neither machine could accept an unsolicited packet
//! and why the mesh needs a member with a forwarded port. iroh claims ~90%
//! direct connections in production. That claim is either true on *this* pair
//! of machines or it is not, and no amount of reading settles it.
//!
//! The number that matters is not "did it connect" — a relay guarantees that.
//! It is whether the connection ends up **direct**, because a relayed one has
//! reintroduced exactly the always-on third party we are trying to do without.
//!
//!     iroh-probe listen [--portmap]
//!     iroh-probe connect <endpoint-id-printed-by-listen> [--portmap]
//!
//! Port mapping is off unless `--portmap` is given. iroh otherwise asks the
//! router for a forwarded port over UPnP/PCP/NAT-PMP, and diamond's router is
//! already known to grant one — so a direct result would report a door the
//! router opened, which we have, rather than hole punching, which we lack.

use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::{presets, Builder, Endpoint, PortmapperConfig};
use iroh::{EndpointAddr, EndpointId, Watcher};

const ALPN: &[u8] = b"machine-elves/iroh-spike/0";

/// How long to keep watching after the connection is up.
///
/// A connection often starts on the relay and upgrades to direct once hole
/// punching completes, so the first reading is the least interesting one. This
/// waits long enough to see whether the upgrade happens.
const WATCH: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
    let portmap = std::env::args().any(|a| a == "--portmap");
    let mut args = std::env::args().skip(1).filter(|a| a != "--portmap");
    println!("port mapping: {}", if portmap { "ON — a direct result may be a router-opened port" } else { "off — a direct result is hole punching" });
    match args.next().unwrap_or_default().as_str() {
        "listen" => listen(portmap).await,
        "connect" => {
            let id = args.next().context("connect needs an endpoint id")?;
            connect(&id, portmap).await
        }
        _ => {
            eprintln!("usage: iroh-probe listen [--portmap] | iroh-probe connect <endpoint-id> [--portmap]");
            std::process::exit(2);
        }
    }
}

fn builder(portmap: bool) -> Builder {
    let config = if portmap { PortmapperConfig::default() } else { PortmapperConfig::Disabled };
    Endpoint::builder(presets::N0).portmapper_config(config)
}

async fn listen(portmap: bool) -> Result<()> {
    let endpoint = builder(portmap)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;

    println!("endpoint id: {}", endpoint.id());
    println!("waiting for the relay to be ready...");
    endpoint.online().await;
    report_own_addr(&endpoint, "after coming online").await;
    println!("online. run this on the other machine:");
    println!();
    println!("    ./iroh-probe connect {}", endpoint.id());
    println!();

    // Keeps accepting. A connect aborted halfway — or run from the wrong
    // network and stopped — must not use up the listener for the real attempt.
    loop {
        let incoming = endpoint.accept().await.context("endpoint closed")?;
        let conn = match incoming.await {
            Ok(conn) => conn,
            Err(err) => {
                println!("incoming connection failed: {err}");
                continue;
            }
        };
        let remote = conn.remote_id();
        println!("connected from {remote}");

        // Read whatever they send, so the connection is genuinely carrying data
        // rather than merely established.
        if let Ok(mut stream) = conn.accept_uni().await {
            let bytes = stream.read_to_end(64 * 1024).await.unwrap_or_default();
            println!("received {} bytes", bytes.len());
        }

        watch(&endpoint, remote).await;
        println!();
        println!("still listening. connect again with the same command.");
        println!();
    }
}

async fn connect(id: &str, portmap: bool) -> Result<()> {
    let remote: EndpointId = id.parse().context("that is not an endpoint id")?;
    let endpoint = builder(portmap).bind().await?;
    println!("endpoint id: {}", endpoint.id());
    endpoint.online().await;
    report_own_addr(&endpoint, "after coming online").await;

    println!("connecting to {remote}...");
    let started = std::time::Instant::now();
    let conn = endpoint.connect(EndpointAddr::from(remote), ALPN).await?;
    println!("connected in {:?}", started.elapsed());

    let mut stream = conn.open_uni().await?;
    stream.write_all(b"hello from the other side").await?;
    stream.finish()?;

    watch(&endpoint, remote).await;
    Ok(())
}

/// What this machine believes its own reachable addresses to be.
///
/// The distinction that matters when a connection stays on the relay: a peer
/// that never learned a direct address for itself has nothing to advertise, so
/// hole punching was never attempted rather than attempted and failed. Those
/// are different problems with different fixes, and the connection log alone
/// cannot tell them apart.
async fn report_own_addr(endpoint: &Endpoint, when: &str) {
    let addr = { let mut w = endpoint.watch_addr(); w.get() };
    println!("own addresses {when}: {addr:?}");
    println!("  bound sockets: {:?}", endpoint.bound_sockets());
}

/// Reports how the connection is actually carried, repeatedly.
///
/// Repeatedly because the answer changes: iroh will happily start on a relay
/// and upgrade to a direct path a few seconds later. A single reading taken at
/// connect time would report the relay and miss the entire point.
async fn watch(endpoint: &Endpoint, remote: EndpointId) {
    let deadline = std::time::Instant::now() + WATCH;
    let mut last = String::new();
    let mut direct_at_end = false;
    while std::time::Instant::now() < deadline {
        if let Some(info) = endpoint.remote_info(remote).await {
            let mut active = Vec::new();
            let mut known = Vec::new();
            for addr in info.addrs() {
                let text = format!("{:?}", addr.addr());
                match addr.usage() {
                    iroh::endpoint::TransportAddrUsage::Active => active.push(text),
                    _ => known.push(text),
                }
            }
            // Only the active paths decide the verdict. A direct address that is
            // merely known — tried once, then abandoned for the relay — is
            // exactly the failure this tool exists to catch.
            direct_at_end = active.iter().any(|a| a.starts_with("Ip("));
            let now = format!("ACTIVE {active:?}  (also known: {known:?})");
            if now != last {
                println!("[{:>5.1}s] {now}", started_secs(deadline));
                last = now;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    report_own_addr(endpoint, "at the end").await;
    println!();
    println!("VERDICT: {}", if direct_at_end {
        "a direct path was in use — no relay needed for the traffic"
    } else {
        "relay only — hole punching did not succeed between these two machines"
    });
}

fn started_secs(deadline: std::time::Instant) -> f64 {
    WATCH.as_secs_f64() - deadline.saturating_duration_since(std::time::Instant::now()).as_secs_f64()
}
