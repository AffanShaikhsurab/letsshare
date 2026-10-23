//! Standalone DHCP server for the owned (Tier 2) data plane.
//!
//! `dhcpd [server_ip]` — defaults to 192.168.50.1 and derives the pool and DNS
//! from that address. Binds UDP 67 (unprivileged on Windows) and advertises
//! option 6, which ICS does not.

use letsshare_core::dhcp::{serve, DhcpConfig};
use std::net::Ipv4Addr;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server: Ipv4Addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.50.1".to_string())
        .parse()?;

    serve(DhcpConfig::for_subnet(server))?;
    Ok(())
}
