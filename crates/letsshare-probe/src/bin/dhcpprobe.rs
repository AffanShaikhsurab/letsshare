//! DHCP probe: ask the ICS allocator what it actually offers a new client.
//!
//! Sends a real DHCPDISCOVER with a synthetic MAC and decodes the OFFER. This
//! answers the question a phone's UI cannot: does the lease include a gateway
//! and, critically, a DNS server?
//!
//! Run: `cargo run -p letsshare-probe --bin dhcpprobe`

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const FAKE_MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

fn build_discover(xid: u32) -> Vec<u8> {
    let mut p = vec![0u8; 240];

    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // Ethernet
    p[2] = 6; // hlen
    p[3] = 0; // hops
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    // secs = 0
    p[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast flag

    // ciaddr, yiaddr, siaddr, giaddr stay zero
    p[28..34].copy_from_slice(&FAKE_MAC);
    // sname (64) and file (128) stay zero

    p[236..240].copy_from_slice(&MAGIC);

    // DHCP Message Type = DISCOVER
    p.extend_from_slice(&[53, 1, 1]);
    // Parameter request list: mask, router, dns, domain, lease, server id, mtu
    p.extend_from_slice(&[55, 7, 1, 3, 6, 15, 51, 54, 26]);
    p.push(255);
    p
}

fn ip(data: &[u8]) -> String {
    if data.len() == 4 {
        Ipv4Addr::new(data[0], data[1], data[2], data[3]).to_string()
    } else {
        data.iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn decode(reply: &[u8]) {
    if reply.len() < 240 || reply[236..240] != MAGIC {
        println!("  not a DHCP packet");
        return;
    }
    let yiaddr = Ipv4Addr::new(reply[16], reply[17], reply[18], reply[19]);
    println!("  offered address : {yiaddr}");
    println!("  server          : {}", ip(&reply[20..24]));

    let mut i = 240;
    while i < reply.len() {
        let code = reply[i];
        if code == 255 {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        let Some(len) = reply.get(i + 1).copied() else {
            break;
        };
        let data = reply.get(i + 2..i + 2 + len as usize).unwrap_or(&[]);

        // Raw line for every option, so a decoder bug can never masquerade as
        // a missing option.
        println!("  opt {code:<3} len {len:<3} {:02x?}", data);

        match code {
            1 => println!("  subnet mask     : {}", ip(data)),
            3 => println!("  router (gateway): {}", ip(data)),
            6 => {
                let servers: Vec<String> = data.chunks(4).map(ip).collect();
                println!("  DNS servers     : {}", servers.join(", "));
            }
            15 => println!("  domain          : {}", String::from_utf8_lossy(data)),
            51 => {
                let secs = data
                    .iter()
                    .take(4)
                    .fold(0u32, |acc, b| (acc << 8) | *b as u32);
                println!(
                    "  lease time      : {secs}s ({:.1} h)",
                    secs as f32 / 3600.0
                );
            }
            54 => println!("  server id       : {}", ip(data)),
            _ => {}
        }
        i += 2 + len as usize;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("== DHCP probe: what does ICS offer? ==\n");

    let sock = UdpSocket::bind("0.0.0.0:68")?;
    sock.set_broadcast(true)?;
    sock.set_read_timeout(Some(Duration::from_secs(4)))?;

    let xid: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0x1234_5678);

    let packet = build_discover(xid);
    println!("sending DISCOVER with synthetic MAC 02:11:22:33:44:55");

    let targets: Vec<String> = if std::env::args().count() > 1 {
        std::env::args().skip(1).collect()
    } else {
        vec![
            "255.255.255.255:67".to_string(),
            "192.168.137.1:67".to_string(),
            "192.168.50.1:67".to_string(),
        ]
    };

    for target in &targets {
        let addr: SocketAddr = target.parse()?;
        match sock.send_to(&packet, addr) {
            Ok(n) => println!("  sent {n} bytes to {target}"),
            Err(e) => println!("  send to {target} failed: {e}"),
        }
    }

    println!("\nwaiting for OFFER...");
    let mut buf = [0u8; 1500];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                println!("\nreply from {from} ({n} bytes)");
                decode(&buf[..n]);
            }
            Err(e) => {
                println!("\nno reply: {e}");
                println!("(the allocator may only answer on the private interface)");
                break;
            }
        }
    }

    Ok(())
}
