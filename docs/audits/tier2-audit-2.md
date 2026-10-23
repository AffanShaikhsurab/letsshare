> Historical test record from the development machine.
> Identifiers have been sanitized; the numbers are from the runs described.

# Tier 2 audit 2 — verification of six fixes

Verified 2026-10-23 16:18–16:21 (UTC), read-only pass. Nothing started, stopped, or elevated.

| # | Issue | Verdict |
|---|---|---|
| F1 | dhcpd wide bind | **PASS** |
| F2 | silent death | **PASS** |
| F3 | ICS port 67 race | **PASS** |
| F4 | lease log junk | **PASS** (legacy lines remain) |
| F5 | stale leftovers | **PASS** (residue inert) |
| F6 | flaky DNS check | **PASS** |

## F1 — bind scoped to the AP

```
UDP    192.168.50.1:67        *:*                                    5316
```
Exactly one match for `:67` in `netstat -ano -p UDP`. `Get-Process -Id 5316` → `dhcpd`. No `0.0.0.0:67` exists anywhere. Parent is PID 44716 (`letsshare-daemon.exe`), so the daemon owns the child.

AP-side serving: `dhcpprobe.exe "255.255.255.255:67"` got a reply — but from **192.168.1.1** (the uplink router), offering 192.168.1.54 with `opt 6 len 4 [c0,a8,01,01]` → `DNS servers: 192.168.1.1`. That is the *proof of the fix*: dhcpd stayed silent on the uplink LAN. A probe to `192.168.50.255:67` timed out; that is expected, since a broadcast emitted by the host leaves the AP adapter and is not looped back to the local listener (the probe itself prints "the allocator may only answer on the private interface"). Live service is instead evidenced by the lease record for `aa:bb:cc:dd:ee:ff -> 192.168.50.10` at 16:11:58 with `dns [192.168.50.1]`, and `dns_queries` climbing 561 → 591 during the pass.

## F2 — heartbeat and panics

7 `heartbeat pid=44716 uptime_ok` lines total. In a 70 s observation window exactly **1** new heartbeat arrived among 19 new lines → 60 s cadence holds. Last heartbeat fell ~22 s before the log's final write (`2026-10-23T16:21:10`), i.e. ≈16:20:48, under 30 s old at the time of reading. `Select-String 'panic:'` → **no matches**. Caveat: the heartbeat line carries no timestamp, so recency is inferred from file mtime and line position.

## F3 — ICS

`Get-Service SharedAccess` → `Running / Automatic`, but **no address and no port**: `netstat` has no `192.168.137.1:67`, and no adapter holds 192.168.137.1 at all (AP iface `Local Area Connection* 11` = `192.168.50.1/24`). ICS is not sharing, so nothing races. Even if Mobile Hotspot were enabled and ICS took 192.168.137.1:67, dhcpd's bind is a different address — **no possible collision**.

## F4 — leases

`letsshare-leases.txt`, 7 lines: six legacy duplicates `lease aa:bb:cc:dd:ee:ff -> 192.168.50.10`, then one new line `1792752118 lease aa:bb:cc:dd:ee:ff -> 192.168.50.10`. Unix timestamp present and correct (1792752118 → 2026-10-23 16:11:58 UTC). `status` reports `leases=1`, equal to distinct addresses (1), not line count (7). Old records were not rewritten, so the file is still unbounded history.

## F5 — leftovers

Still present, all inert:
- `192.168.137.3 / 00-00-00-00-00-00 / Unreachable` on AP iface — zero MAC, no address 192.168.137.x on any adapter, no route, so it can carry no traffic.
- `Ethernet = 169.254.137.1/16`, plus APIPA on `Local Area Connection` (169.254.205.176) and `LAC*8` (169.254.4.227) — link-local only, no gateway, no route entry.
Routing table for the share is clean: `192.168.50.0/24 → 0.0.0.0 via LAC*11`, default via Wi-Fi 192.168.1.1, `Get-NetNat LetShare 192.168.50.0/24 Active=True`.

## F6 — DNS check

Five runs, 2 s apart, all `dns=up`, `healthy: true`:

```
16:19:36 dns_queries=561 | 16:19:40 563 | 16:19:44 565 | 16:19:47 566 | 16:19:50 568
```
Independent query agreed: `Resolve-DnsName example.com -Server 192.168.50.1` → 172.66.147.243, 104.20.23.154. `dnsrescue.log` shows the resolver actively answering from 192.168.50.1. No disagreement between check and reality.

## Overall

Stack is healthy: ap/dhcp/dns/address/nat all up, NAT active, resolver answering, DHCP lease served 8 minutes before the pass. **No remaining HIGH-severity problem found.** Phone client is **not connected** — the neighbour entry for 192.168.50.10 is `Unreachable` with a zero MAC, i.e. its lease is idle/expired-connection, as expected.

One MEDIUM residual, not in the original six: the running dhcpd (PID 5316, started 16:16:00) is **not** writing `letsshare-dhcpd.log` (mtime 16:14:23, earlier than the process start), so any restart after 16:16 leaves no trace — the same class of blind spot F2 fixed for the supervisor. The log's 11 `dhcp listening` lines are from earlier manual runs, not a current restart loop (PID stable 5+ min).
