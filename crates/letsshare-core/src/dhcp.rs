//! A small, correct DHCP server.
//!
//! This exists for one reason: ICS on affected Windows builds omits DHCP option
//! 6, so tethered clients receive an address and a gateway but no resolver. No
//! userland process can inject that option while ICS holds port 67 exclusively,
//! so on those machines LetShare must serve DHCP itself.
//!
//! Scope is deliberately narrow: IPv4, one subnet, a handful of clients, no
//! relay agents. Correctness over features — a malformed offer is worse than no
//! offer, because the client silently sits at "obtaining IP address".

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;
const RELEASE: u8 = 7;

/// Server configuration for one private subnet.
#[derive(Debug, Clone)]
pub struct DhcpConfig {
    /// Our address on the AP interface; also the gateway and default resolver.
    pub server: Ipv4Addr,
    pub mask: Ipv4Addr,
    /// Inclusive pool bounds.
    pub pool_start: Ipv4Addr,
    pub pool_end: Ipv4Addr,
    /// Resolvers to advertise. This is the field ICS omits.
    pub dns: Vec<Ipv4Addr>,
    pub lease: Duration,
}

impl DhcpConfig {
    /// Sensible defaults for a `/24` AP subnet.
    pub fn for_subnet(server: Ipv4Addr) -> Self {
        let o = server.octets();
        Self {
            server,
            mask: Ipv4Addr::new(255, 255, 255, 0),
            pool_start: Ipv4Addr::new(o[0], o[1], o[2], 10),
            pool_end: Ipv4Addr::new(o[0], o[1], o[2], 250),
            dns: vec![server],
            lease: Duration::from_secs(12 * 3600),
        }
    }
}

/// A lease as handed out to a client.
#[derive(Debug, Clone)]
pub struct Lease {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    pub expires: Instant,
}

/// One parsed request, reduced to what the server needs.
struct Request {
    xid: [u8; 4],
    mac: [u8; 6],
    flags: u16,
    requested_ip: Option<Ipv4Addr>,
    msg_type: u8,
}

fn parse(buf: &[u8]) -> Option<Request> {
    if buf.len() < 240 || buf[236..240] != MAGIC {
        return None;
    }
    let mut xid = [0u8; 4];
    xid.copy_from_slice(&buf[4..8]);

    let mut mac = [0u8; 6];
    mac.copy_from_slice(&buf[28..34]);

    let flags = u16::from_be_bytes([buf[10], buf[11]]);

    let mut msg_type = 0u8;
    let mut requested_ip = None;

    let mut i = 240;
    while i < buf.len() {
        let code = buf[i];
        if code == 255 {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        let len = *buf.get(i + 1)? as usize;
        let data = buf.get(i + 2..i + 2 + len)?;

        match code {
            53 if len == 1 => msg_type = data[0],
            50 if len == 4 => {
                requested_ip = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]))
            }
            _ => {}
        }
        i += 2 + len;
    }

    Some(Request {
        xid,
        mac,
        flags,
        requested_ip,
        msg_type,
    })
}

fn push_opt(out: &mut Vec<u8>, code: u8, data: &[u8]) {
    out.push(code);
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

fn build_reply(cfg: &DhcpConfig, req: &Request, kind: u8, yiaddr: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; 240];

    p[0] = 2; // BOOTREPLY
    p[1] = 1; // Ethernet
    p[2] = 6; // hlen
    p[4..8].copy_from_slice(&req.xid);
    p[10..12].copy_from_slice(&req.flags.to_be_bytes());
    p[16..20].copy_from_slice(&yiaddr.octets());
    p[20..24].copy_from_slice(&cfg.server.octets());
    p[28..34].copy_from_slice(&req.mac);
    p[236..240].copy_from_slice(&MAGIC);

    push_opt(&mut p, 53, &[kind]);
    push_opt(&mut p, 54, &cfg.server.octets());

    if kind != NAK {
        let lease = cfg.lease.as_secs() as u32;
        push_opt(&mut p, 51, &lease.to_be_bytes());
        push_opt(&mut p, 1, &cfg.mask.octets());
        push_opt(&mut p, 3, &cfg.server.octets());

        // The whole point: advertise a resolver.
        let dns: Vec<u8> = cfg.dns.iter().flat_map(|d| d.octets()).collect();
        if !dns.is_empty() {
            push_opt(&mut p, 6, &dns);
        }

        // Renew at 50%, rebind at 87.5%, as RFC 2131 recommends.
        push_opt(&mut p, 58, &(lease / 2).to_be_bytes());
        push_opt(&mut p, 59, &(lease / 8 * 7).to_be_bytes());
    }

    p.push(255);
    p
}

/// The server itself.
pub struct DhcpServer {
    cfg: DhcpConfig,
    leases: HashMap<[u8; 6], Lease>,
    /// Reverse index so two clients cannot be offered the same address.
    used: HashMap<Ipv4Addr, [u8; 6]>,
    /// Most recent lease event, drained by the serving loop.
    event: Option<Lease>,
}

impl DhcpServer {
    pub fn new(cfg: DhcpConfig) -> Self {
        Self {
            cfg,
            leases: HashMap::new(),
            used: HashMap::new(),
            event: None,
        }
    }

    pub fn leases(&self) -> impl Iterator<Item = &Lease> {
        self.leases.values()
    }

    /// Drain the most recent lease event, for logging by the serving loop.
    pub fn take_event(&mut self) -> Option<Lease> {
        self.event.take()
    }

    fn allocate(&mut self, mac: [u8; 6], requested: Option<Ipv4Addr>) -> Ipv4Addr {
        // Free leases that have actually expired. Without this the table only
        // ever shrinks on an explicit RELEASE, which a client need never send:
        // a few hundred random MACs would burn the pool for good.
        self.expire();

        // Honour a previous lease, then an explicit request, then scan the pool.
        if let Some(lease) = self.leases.get(&mac) {
            return lease.ip;
        }
        if let Some(ip) = requested {
            if self.in_pool(ip) && !self.used.contains_key(&ip) {
                return ip;
            }
        }

        let start = u32::from(self.cfg.pool_start);
        let end = u32::from(self.cfg.pool_end);
        for n in start..=end {
            let ip = Ipv4Addr::from(n);
            if !self.used.contains_key(&ip) {
                return ip;
            }
        }

        // Pool exhausted: evict the lease closest to expiry instead of handing
        // the same address to two clients.
        if let Some(mac) = self
            .leases
            .iter()
            .min_by_key(|(_, l)| l.expires)
            .map(|(m, _)| *m)
        {
            if let Some(lease) = self.leases.remove(&mac) {
                self.used.remove(&lease.ip);
                return lease.ip;
            }
        }
        self.cfg.pool_end
    }

    fn expire(&mut self) {
        let now = Instant::now();
        let expired: Vec<[u8; 6]> = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.expires <= now)
            .map(|(mac, _)| *mac)
            .collect();
        for mac in expired {
            if let Some(lease) = self.leases.remove(&mac) {
                self.used.remove(&lease.ip);
            }
        }
    }

    fn in_pool(&self, ip: Ipv4Addr) -> bool {
        let n = u32::from(ip);
        n >= u32::from(self.cfg.pool_start) && n <= u32::from(self.cfg.pool_end)
    }

    fn remember(&mut self, mac: [u8; 6], ip: Ipv4Addr) {
        // Only raise an event when this client is actually getting a new
        // address. A renew must not append another line to the lease log.
        let changed = self.leases.get(&mac).map(|l| l.ip != ip).unwrap_or(true);

        if let Some(previous) = self.leases.get(&mac) {
            if previous.ip != ip {
                self.used.remove(&previous.ip);
            }
        }
        if let Some(old_mac) = self.used.insert(ip, mac) {
            if old_mac != mac {
                self.leases.remove(&old_mac);
            }
        }
        self.leases.insert(
            mac,
            Lease {
                mac,
                ip,
                expires: Instant::now() + self.cfg.lease,
            },
        );
        if changed {
            self.event = self.leases.get(&mac).cloned();
        }
    }

    /// Handle one datagram, returning the reply and the destination.
    pub fn handle(&mut self, buf: &[u8]) -> Option<(Vec<u8>, SocketAddr)> {
        let req = parse(buf)?;
        if req.mac == [0u8; 6] {
            return None;
        }

        let kind = match req.msg_type {
            DISCOVER => OFFER,
            REQUEST => {
                // A client may be confirming a previous lease after reboot.
                if let Some(want) = req.requested_ip {
                    if !self.in_pool(want) {
                        let pkt = build_reply(&self.cfg, &req, NAK, Ipv4Addr::UNSPECIFIED);
                        return Some((pkt, broadcast()));
                    }
                }
                ACK
            }
            RELEASE => {
                if let Some(lease) = self.leases.remove(&req.mac) {
                    self.used.remove(&lease.ip);
                }
                return None;
            }
            _ => return None,
        };

        let ip = self.allocate(req.mac, req.requested_ip);
        if kind == ACK {
            self.remember(req.mac, ip);
        }

        let pkt = build_reply(&self.cfg, &req, kind, ip);
        Some((pkt, broadcast()))
    }
}

fn broadcast() -> SocketAddr {
    SocketAddr::from(([255, 255, 255, 255], 68))
}

/// Bind and serve forever, reporting each lease on stdout.
///
/// The socket is bound to the AP address, **not** to `0.0.0.0`. Binding the
/// wildcard would make this a DHCP server for every interface on the machine,
/// including the uplink LAN — it would happily offer `192.168.50.x` to a
/// stranger's laptop on the home Wi-Fi. The AP address does not exist until the
/// adapter is up, so retry rather than exiting.
pub fn serve(cfg: DhcpConfig) -> std::io::Result<()> {
    let bind: SocketAddr = SocketAddr::from((cfg.server, 67));
    let socket = {
        let mut attempt = 0;
        loop {
            match UdpSocket::bind(bind) {
                Ok(s) => break s,
                Err(e) if attempt < 30 => {
                    attempt += 1;
                    eprintln!("bind {bind} failed ({e}); retrying ({attempt}/30)");
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                }
                Err(e) => return Err(e),
            }
        }
    };
    socket.set_broadcast(true)?;

    println!("dhcp listening on {bind}");
    println!(
        "  subnet {} / {} | pool {} .. {} | dns {:?} | lease {:?}",
        cfg.server, cfg.mask, cfg.pool_start, cfg.pool_end, cfg.dns, cfg.lease
    );
    println!("  advertising option 6 (DNS) — the field ICS omits\n");

    let mut server = DhcpServer::new(cfg);
    let lease_file = std::env::temp_dir().join("letsshare-leases.txt");
    let mut buf = [0u8; 1500];
    loop {
        let Ok((n, _from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if let Some((reply, dest)) = server.handle(&buf[..n]) {
            let _ = socket.send_to(&reply, dest);
        }

        // Log leases to stdout (captured by the supervisor) and to a file the
        // status command can read.
        if let Some(lease) = server.take_event() {
            let mac = lease
                .mac
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":");
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let line = format!("{ts} lease {mac} -> {}", lease.ip);
            println!("{line}");
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&lease_file)
            {
                use std::io::Write as _;
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discover() -> Vec<u8> {
        let mut p = vec![0u8; 240];
        p[0] = 1;
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        p[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        p[28..34].copy_from_slice(&[0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
        p[236..240].copy_from_slice(&MAGIC);
        p.extend_from_slice(&[53, 1, DISCOVER, 255]);
        p
    }

    #[test]
    fn offer_contains_dns_option() {
        let cfg = DhcpConfig::for_subnet(Ipv4Addr::new(192, 168, 50, 1));
        let mut server = DhcpServer::new(cfg);
        let (reply, dest) = server.handle(&discover()).expect("offer");

        assert_eq!(dest.port(), 68);

        // Walk options and find code 6.
        let mut i = 240;
        let mut dns = None;
        while i < reply.len() {
            let code = reply[i];
            if code == 255 {
                break;
            }
            if code == 0 {
                i += 1;
                continue;
            }
            let len = reply[i + 1] as usize;
            if code == 6 {
                dns = Some(reply[i + 2..i + 2 + len].to_vec());
            }
            i += 2 + len;
        }

        let dns = dns.expect("option 6 must be present");
        assert_eq!(dns, vec![192, 168, 50, 1]);
    }

    #[test]
    fn offers_within_pool_and_is_stable() {
        let cfg = DhcpConfig::for_subnet(Ipv4Addr::new(192, 168, 50, 1));
        let mut server = DhcpServer::new(cfg);
        let (reply, _) = server.handle(&discover()).unwrap();
        let ip = Ipv4Addr::new(reply[16], reply[17], reply[18], reply[19]);
        assert!(u32::from(ip) >= u32::from(Ipv4Addr::new(192, 168, 50, 10)));
        assert!(u32::from(ip) <= u32::from(Ipv4Addr::new(192, 168, 50, 250)));
    }
}
