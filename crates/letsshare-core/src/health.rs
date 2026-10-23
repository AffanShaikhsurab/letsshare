//! Cheap, dependency-free health probes.
//!
//! These are deliberately blunt: they answer "would a phone work right now?"
//! rather than "did an API return success?". A hotspot can report `On` while
//! being useless, which is exactly the failure mode LetShare exists to catch.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::Command;
use std::time::Duration;

/// The gateway address ICS always assigns to its private interface.
pub const ICS_GATEWAY: &str = "192.168.137.1";

/// True when the host actually owns the ICS gateway address, i.e. the shared
/// adapter is up and addressed. Binding a UDP socket to the address only
/// succeeds if it is local.
pub fn ics_gateway_present() -> bool {
    UdpSocket::bind((ICS_GATEWAY, 0)).is_ok()
}

/// True when the ICS DHCP allocator is bound and able to answer.
///
/// This is the signal the built-in UI lacks: a hotspot can be `On`, own its
/// gateway address, and still serve no leases. Clients then associate, show
/// "connected", and get nothing — while Windows reports zero devices because
/// its client list is built from leases, not associations.
///
/// Parsing the endpoint table is diagnostic-grade, not elegant. It is cheap at
/// supervisor cadence and it catches the exact failure `ClientCount` cannot see.
pub fn ics_dhcp_listening() -> bool {
    Command::new("netstat")
        .args(["-ano", "-p", "UDP"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.contains("192.168.137.1:67"))
        })
        .unwrap_or(false)
}

/// TCP reachability check against well-known anycast resolvers.
/// Deliberately IP-literal so a broken DNS server does not mask a live route.
pub fn internet_reachable(timeout: Duration) -> bool {
    const PROBES: [&str; 3] = ["1.1.1.1:443", "8.8.8.8:443", "9.9.9.9:443"];
    PROBES.iter().any(|p| {
        p.parse::<SocketAddr>()
            .ok()
            .and_then(|addr| TcpStream::connect_timeout(&addr, timeout).ok())
            .is_some()
    })
}

/// True when name resolution works through the host's configured DNS.
pub fn dns_resolves(host: &str, timeout: Duration) -> bool {
    let _ = timeout;
    (host, 443u16)
        .to_socket_addrs()
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// Classify the uplink. Distinguishes "no route" from "DNS broken", which need
/// different fixes and different messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    /// Route and DNS both work.
    Good,
    /// IP traffic works but DNS does not: clients will see "no internet".
    DnsBroken,
    /// No IP route at all.
    Offline,
}

pub fn classify(timeout: Duration) -> Connectivity {
    if !internet_reachable(timeout) {
        return Connectivity::Offline;
    }
    if dns_resolves("www.msftconnecttest.com", timeout) {
        Connectivity::Good
    } else {
        Connectivity::DnsBroken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_probe_does_not_panic() {
        let _ = ics_gateway_present();
    }

    #[test]
    fn classify_returns_a_variant() {
        let c = classify(Duration::from_millis(200));
        assert!(matches!(
            c,
            Connectivity::Good | Connectivity::DnsBroken | Connectivity::Offline
        ));
    }
}
