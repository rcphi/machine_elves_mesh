//! Asking the router for a port by UPnP.
//!
//! NAT-PMP alone missed the friend's router, which answers neither it nor, as
//! it turned out, UPnP; but most routers that open a port at all speak one of
//! the two, and the operator's aim is a game that needs no router setup from
//! its players. So the node asks both (see [`portmap::first_granted`]).
//!
//! **The gateway is asked directly first.** UPnP's discovery step is normally
//! multicast, and a host kept away from its household — diamond's
//! `isolate-lan` rejects all outgoing multicast, the gateway exempt — cannot
//! send it. Many routers answer only the multicast form, so that is tried
//! second, and where the host forbids it the attempt simply fails.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use igd_next::{PortMappingProtocol, SearchOptions};

use crate::portmap;

/// How long each search waits for an answer: unicast, then multicast.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(2);

/// UPnP's multicast discovery address.
const SSDP: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900);

/// The searches to try, in order: the gateway itself if known, then multicast.
pub(crate) fn search_options(gateway: Option<Ipv4Addr>) -> Vec<SearchOptions> {
    let search = |to: SocketAddr| SearchOptions {
        broadcast_address: to,
        timeout: Some(SEARCH_TIMEOUT),
        single_search_timeout: Some(SEARCH_TIMEOUT),
        ..SearchOptions::default()
    };
    let mut options = Vec::new();
    if let Some(gateway) = gateway {
        options.push(search(SocketAddr::new(IpAddr::V4(gateway), 1900)));
    }
    options.push(search(SSDP));
    options
}

/// The address this host uses to reach `gateway`. Nothing is sent.
fn local_ip_towards(gateway: Ipv4Addr) -> std::io::Result<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect((gateway, 1900))?;
    match socket.local_addr()?.ip() {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(_) => Err(std::io::Error::other("no IPv4 route to the gateway")),
    }
}

/// Asks the gateway to forward UDP `port` to this host's same port, for
/// `lifetime` seconds.
pub async fn request(port: u16, lifetime: u32) -> Result<portmap::Mapping, String> {
    let gateway = portmap::default_gateway().ok();
    let mut errors = Vec::new();
    for options in search_options(gateway) {
        let to = options.broadcast_address;
        let found = match igd_next::aio::tokio::search_gateway(options).await {
            Ok(found) => found,
            Err(error) => {
                errors.push(format!("{to}: {error}"));
                continue;
            }
        };
        let local = match gateway.map(local_ip_towards) {
            Some(Ok(ip)) => ip,
            Some(Err(error)) => return Err(error.to_string()),
            None => return Err("no default gateway to forward from".to_string()),
        };
        let external_ip = match found.get_external_ip().await.map_err(|e| e.to_string())? {
            IpAddr::V4(ip) => ip,
            IpAddr::V6(ip) => return Err(format!("the router gave an IPv6 address, {ip}")),
        };
        found
            .add_port(PortMappingProtocol::UDP, port, SocketAddr::new(IpAddr::V4(local), port),
                      lifetime, "machine elves mesh")
            .await
            .map_err(|e| format!("the router refused: {e}"))?;
        return Ok(portmap::Mapping { external_ip, external_port: port, lifetime });
    }
    Err(errors.join("; "))
}

/// Withdraws the mapping, if the router can be found again quickly.
pub async fn release(port: u16) {
    let gateway = portmap::default_gateway().ok();
    for options in search_options(gateway) {
        if let Ok(found) = igd_next::aio::tokio::search_gateway(options).await {
            let _ = found.remove_port(PortMappingProtocol::UDP, port).await;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Duration;

    #[test]
    fn the_gateway_is_asked_directly_before_multicast() {
        let o = search_options(Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].broadcast_address, "192.168.1.1:1900".parse().unwrap());
        assert_eq!(o[1].broadcast_address, "239.255.255.250:1900".parse().unwrap());
    }

    #[test]
    fn with_no_gateway_only_multicast_is_tried() {
        let o = search_options(None);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].broadcast_address, "239.255.255.250:1900".parse().unwrap());
    }

    /// A router that never answers must not hold anything up for long: the
    /// renewal that asks runs beside the node, but it should still finish.
    #[test]
    fn the_whole_upnp_search_is_bounded() {
        let total: Duration = search_options(Some(Ipv4Addr::LOCALHOST)).iter().map(|o| o.timeout.unwrap()).sum();
        assert!(total <= Duration::from_secs(6), "{total:?}");
        assert!(search_options(None).iter().all(|o| o.single_search_timeout == Some(SEARCH_TIMEOUT)));
    }
}
