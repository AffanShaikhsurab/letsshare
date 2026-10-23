//! DNS rescue: answer DNS on the AP gateway address when the built-in ICS
//! proxy has wedged.
//!
//! Windows' Internet Connection Sharing runs a DNS proxy that binds `0.0.0.0:53`
//! non-exclusively. When it dies — which is observable: the socket stays bound
//! but answers nothing — every tethered client loses name resolution while DHCP
//! and NAT keep working. The phone then reports "connected" with no internet and
//! silently falls back to cellular.
//!
//! Because the ICS socket is not exclusive, a more specific bind wins for the
//! gateway address. This forwarder takes that address over and forwards upstream.
//!
//! Responses are cached for their own TTL. A phone opening a page asks for the
//! same handful of names repeatedly, and every miss costs a full round trip to
//! the uplink resolver; on a weak 2.4 GHz link that is the difference between a
//! page that feels instant and one that stutters.
//!
//! Run: `dnsrescue [listen_addr] [upstream_addr]`

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Upper bound on concurrent upstream lookups. One client must not be able to
/// spawn threads without limit.
const MAX_IN_FLIGHT: usize = 256;
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

struct InFlight;

impl InFlight {
    fn acquire() -> Option<Self> {
        if IN_FLIGHT.fetch_add(1, Ordering::Relaxed) >= MAX_IN_FLIGHT {
            IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
            None
        } else {
            Some(InFlight)
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Random-ish transaction id. Combined with a fresh source port this is what
/// makes off-path answer injection impractical.
fn next_id() -> u16 {
    static SEQ: AtomicU32 = AtomicU32::new(0x9E37_79B9);
    static JITTER: AtomicU16 = AtomicU16::new(0);
    let x = SEQ.fetch_add(0x9E37_79B9, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let j = JITTER.fetch_add(1, Ordering::Relaxed);
    ((x ^ nanos.rotate_left(11) ^ u32::from(j)) >> 8) as u16
}

/// How many names to remember. Small enough to stay irrelevant in memory,
/// large enough for a phone's working set.
const CACHE_CAP: usize = 1024;
/// Never trust an upstream TTL longer than this; a stale name outliving its
/// real record is worse than one extra lookup.
const CACHE_MAX_TTL: u64 = 300;
const CACHE_MIN_TTL: u64 = 5;

struct Entry {
    reply: Vec<u8>,
    expires: Instant,
}

type Cache = Arc<Mutex<HashMap<Vec<u8>, Entry>>>;

/// Pull the queried name out of a DNS question section.
fn qname(query: &[u8]) -> Option<String> {
    if query.len() < 13 {
        return None;
    }
    let mut i = 12usize;
    let mut out = String::new();
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 || len & 0xC0 != 0 {
            break;
        }
        i += 1;
        let part = query.get(i..i + len)?;
        out.push_str(&String::from_utf8_lossy(part));
        out.push('.');
        i += len;
        if out.len() > 253 {
            break;
        }
    }
    Some(out)
}

/// Offset just past the question section: the name plus QTYPE and QCLASS.
fn question_end(msg: &[u8]) -> Option<usize> {
    let mut i = 12usize;
    loop {
        let len = *msg.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        i += 1 + len;
    }
    Some(i + 4)
}

/// Cache key: the question bytes with the name lowercased. The transaction id
/// lives in the header, so it must not be part of the key.
fn cache_key(query: &[u8]) -> Option<Vec<u8>> {
    let end = question_end(query)?;
    let mut key = query.get(12..end)?.to_vec();
    // Lowercase only the name labels, leaving QTYPE/QCLASS untouched.
    let mut i = 0usize;
    while i < key.len() {
        let len = key[i] as usize;
        if len == 0 || len & 0xC0 != 0 {
            break;
        }
        for b in key.iter_mut().skip(i + 1).take(len) {
            *b = b.to_ascii_lowercase();
        }
        i += 1 + len;
    }
    Some(key)
}

fn skip_name(msg: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let b = *msg.get(i)?;
        if b == 0 {
            return Some(i + 1);
        }
        if b & 0xC0 == 0xC0 {
            return Some(i + 2);
        }
        i += 1 + b as usize;
    }
}

/// Smallest TTL across the answer records, clamped to something sane.
fn min_ttl(msg: &[u8]) -> u64 {
    let Some(mut i) = question_end(msg) else {
        return CACHE_MIN_TTL;
    };
    let Some(count) = msg.get(6..8) else {
        return CACHE_MIN_TTL;
    };
    let answers = u16::from_be_bytes([count[0], count[1]]) as usize;
    let mut ttl: Option<u64> = None;
    for _ in 0..answers {
        let Some(name_end) = skip_name(msg, i) else {
            break;
        };
        // type(2), class(2), ttl(4), rdlen(2)
        let Some(rec) = msg.get(name_end..name_end + 10) else {
            break;
        };
        let this = u32::from_be_bytes([rec[4], rec[5], rec[6], rec[7]]) as u64;
        ttl = Some(ttl.map_or(this, |t| t.min(this)));
        let rdlen = u16::from_be_bytes([rec[8], rec[9]]) as usize;
        i = name_end + 10 + rdlen;
    }
    ttl.unwrap_or(CACHE_MIN_TTL)
        .clamp(CACHE_MIN_TTL, CACHE_MAX_TTL)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listen: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.50.1:53".to_string())
        .parse()?;
    let upstream: SocketAddr = std::env::args()
        .nth(2)
        .or_else(|| std::env::var("LETSSHARE_UPSTREAM_DNS").ok())
        .unwrap_or_else(|| "1.1.1.1:53".to_string())
        .parse()?;

    // The adapter address is tentative for a moment after it is assigned, so a
    // bind can fail with EADDRNOTAVAIL even though the address exists. Retry
    // instead of exiting and leaving clients without a resolver.
    let socket = {
        let mut attempt = 0;
        loop {
            match UdpSocket::bind(listen) {
                Ok(s) => break Arc::new(s),
                Err(e) if attempt < 20 => {
                    attempt += 1;
                    eprintln!("bind {listen} failed ({e}); retrying ({attempt}/20)");
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => return Err(e.into()),
            }
        }
    };
    println!("dns rescue listening on {listen}");
    println!("forwarding to         {upstream}");
    println!("(ICS keeps NAT and DHCP; only name resolution is ours)\n");

    let cache: Cache = Arc::new(Mutex::new(HashMap::new()));
    let mut buf = [0u8; 1500];

    loop {
        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("recv error: {e}");
                continue;
            }
        };
        if len < 12 {
            continue;
        }

        let name = qname(&buf[..len]).unwrap_or_else(|| "?".into());
        let query = buf[..len].to_vec();
        let key = cache_key(&query);

        // Cache hit: answer immediately, with the client's own transaction id.
        if let Some(key) = key.as_ref() {
            let hit = {
                let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
                match c.get(key) {
                    Some(e) if e.expires > Instant::now() => Some(e.reply.clone()),
                    Some(_) => {
                        c.remove(key);
                        None
                    }
                    None => None,
                }
            };
            if let Some(mut reply) = hit {
                println!("{from} -> {name} (cache)");
                reply[0] = query[0];
                reply[1] = query[1];
                let _ = socket.send_to(&reply, from);
                continue;
            }
        }

        println!("{from} -> {name}");

        let socket = Arc::clone(&socket);
        let cache = Arc::clone(&cache);

        // One short-lived thread per query. At tethered-client volumes this is
        // cheaper than the bookkeeping needed to multiplex, and it keeps a slow
        // upstream from stalling everyone else.
        std::thread::spawn(move || {
            let Some(_slot) = InFlight::acquire() else {
                return;
            };

            // Our own transaction id, a fresh source port, and a connected
            // socket: only a reply from the configured upstream, carrying our
            // id and echoing our question, is accepted. Forwarding the client
            // query verbatim let any host inject a cached answer.
            let id = next_id();
            let mut upstream_query = query.clone();
            upstream_query[0..2].copy_from_slice(&id.to_be_bytes());

            let Ok(up) = UdpSocket::bind("0.0.0.0:0") else {
                return;
            };
            let _ = up.set_read_timeout(Some(Duration::from_secs(3)));
            if up.connect(upstream).is_err() {
                return;
            }
            if up.send(&upstream_query).is_err() {
                return;
            }
            let mut reply = [0u8; 4096];
            if let Ok(n) = up.recv(&mut reply) {
                if n < 12 || u16::from_be_bytes([reply[0], reply[1]]) != id {
                    return;
                }
                let (Some(qe), Some(re)) =
                    (question_end(&upstream_query), question_end(&reply[..n]))
                else {
                    return;
                };
                if re != qe || !reply[12..re].eq_ignore_ascii_case(&upstream_query[12..qe]) {
                    return;
                }

                // Hand the client its own transaction id.
                reply[0..2].copy_from_slice(&query[0..2]);
                let reply = &reply[..n];

                // Only cache real answers: rcode 0 with at least one record.
                let rcode = reply.get(3).map(|b| b & 0x0F).unwrap_or(1);
                let answers = reply
                    .get(6..8)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
                    .unwrap_or(0);
                if rcode == 0 && answers > 0 {
                    if let Some(key) = key {
                        let ttl = min_ttl(reply);
                        let entry = Entry {
                            reply: reply.to_vec(),
                            expires: Instant::now() + Duration::from_secs(ttl),
                        };
                        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
                        if c.len() >= CACHE_CAP {
                            c.retain(|_, e| e.expires > Instant::now());
                            if c.len() >= CACHE_CAP {
                                c.clear();
                            }
                        }
                        c.insert(key, entry);
                    }
                }
                let _ = socket.send_to(reply, from);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn key_ignores_transaction_id_and_case() {
        let a = query("Example.COM", 1);
        let mut b = query("example.com", 1);
        b[0] = 0xAB;
        b[1] = 0xCD;
        assert_eq!(cache_key(&a), cache_key(&b));
        assert!(cache_key(&a).is_some());
    }

    #[test]
    fn key_separates_record_types() {
        let a = query("example.com", 1);
        let aaaa = query("example.com", 28);
        assert_ne!(cache_key(&a), cache_key(&aaaa));
    }

    #[test]
    fn malformed_query_has_no_key() {
        assert!(cache_key(&[0u8; 4]).is_none());
        assert!(qname(&[0u8; 4]).is_none());
    }
}
