//! Asking the router for a door, and keeping it open.
//!
//! A mesh with nobody reachable cannot admit anyone it is not already connected
//! to: hole punching needs both sides to know each other's live address, which
//! needs an introducer, which needs somebody reachable. A forwarded port is how
//! the first somebody comes to exist.
//!
//! Most consumer routers will open one on request, under a setting usually
//! labelled UPnP. **The mapping expires**, which is the part that matters here:
//! a node that asks once and forgets stops being reachable an hour later,
//! silently, while appearing entirely healthy.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;

const NAT_PMP_PORT: u16 = 5351;

/// How long each mapping is requested for.
pub const LIFETIME_SECS: u32 = 3600;

/// How often to ask again.
///
/// A quarter of the lifetime, so three consecutive failures — a router
/// rebooting, a moment of packet loss — pass before reachability is actually
/// lost. Renewing at the last moment would make any single failure fatal.
pub const RENEW_EVERY: Duration = Duration::from_secs(LIFETIME_SECS as u64 / 4);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapping {
    pub external_ip: Ipv4Addr,
    pub external_port: u16,
    pub lifetime: u32,
}

/// Asks the router to forward `port`, returning what it agreed to.
pub async fn request(port: u16, lifetime: u32) -> io::Result<Mapping> {
    let gateway = default_gateway()?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let router = SocketAddr::from((gateway, NAT_PMP_PORT));

    let mut reply = [0u8; 32];

    // What address the router holds. Asked separately because a router that
    // answers this and refuses the mapping is a different situation from one
    // that does not speak the protocol at all.
    let len = ask(&socket, router, &[0, 0], &mut reply).await?;
    if len < 12 || reply[1] != 128 || u16::from_be_bytes([reply[2], reply[3]]) != 0 {
        return Err(io::Error::other("the router would not say its address"));
    }
    let external_ip = Ipv4Addr::new(reply[8], reply[9], reply[10], reply[11]);

    // Ask for the same external port as the internal one. A router that agrees
    // gives this node a predictable address that peers can remember across
    // restarts; one that assigns a different port says so, and then the address
    // has to be learned rather than assumed.
    let mut message = vec![0u8, 1]; // version 0, map UDP
    message.extend_from_slice(&[0, 0]);
    message.extend_from_slice(&port.to_be_bytes());
    message.extend_from_slice(&port.to_be_bytes());
    message.extend_from_slice(&lifetime.to_be_bytes());

    let len = ask(&socket, router, &message, &mut reply).await?;
    if len < 16 || reply[1] != 129 {
        return Err(io::Error::other("the router gave no usable answer"));
    }
    let code = u16::from_be_bytes([reply[2], reply[3]]);
    if code != 0 {
        return Err(io::Error::other(format!("the router refused: code {code}")));
    }

    Ok(Mapping {
        external_ip,
        external_port: u16::from_be_bytes([reply[10], reply[11]]),
        lifetime: u32::from_be_bytes([reply[12], reply[13], reply[14], reply[15]]),
    })
}

/// Gives the port back.
///
/// A lifetime of zero deletes the mapping. Not required — it would lapse on its
/// own — but leaving a door open in someone's router after leaving is untidy in
/// the same way as vanishing without saying goodbye.
pub async fn release(port: u16) -> io::Result<()> {
    request(port, 0).await.map(|_| ())
}

async fn ask(
    socket: &UdpSocket,
    router: SocketAddr,
    message: &[u8],
    reply: &mut [u8],
) -> io::Result<usize> {
    for _ in 0..3 {
        socket.send_to(message, router).await?;
        match tokio::time::timeout(Duration::from_secs(2), socket.recv_from(reply)).await {
            Ok(Ok((len, from))) if from.ip() == router.ip() => return Ok(len),
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the router did not answer — it may not speak NAT-PMP, or may have it switched off",
    ))
}

/// The router this machine sends through, from the kernel's routing table.
fn default_gateway() -> io::Result<Ipv4Addr> {
    let routes = std::fs::read_to_string("/proc/net/route")?;
    for line in routes.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (_iface, destination, gateway) = (fields.next(), fields.next(), fields.next());
        if destination == Some("00000000") {
            if let Some(raw) = gateway.and_then(|hex| u32::from_str_radix(hex, 16).ok()) {
                // The kernel writes these little-endian.
                return Ok(Ipv4Addr::from(raw.swap_bytes()));
            }
        }
    }
    Err(io::Error::other("no default route"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_leaves_room_for_failures() {
        // Renewing at the last moment makes one lost packet fatal. Three
        // attempts should fit inside a mapping's life.
        let lifetime = Duration::from_secs(LIFETIME_SECS as u64);
        assert!(RENEW_EVERY * 3 <= lifetime);
    }

    #[test]
    fn a_gateway_is_read_from_the_routing_table() {
        // Only meaningful where there is a default route, which a build machine
        // has and a sealed test environment may not.
        if let Ok(gateway) = default_gateway() {
            assert!(!gateway.is_unspecified(), "read a nonsense gateway");
        }
    }
}
