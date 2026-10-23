> Historical test record from the development machine.
> Identifiers have been sanitized; the numbers are from the runs described.

# Tier 2 live audit — 2026-10-23 16:07–16:09 (UTC)

Read-only, non-elevated. Host `test-laptop`, user `tester`, uplink Wi-Fi `192.168.1.10` (gw `192.168.1.1`).

| # | Check | Result | Evidence |
|---|-------|--------|----------|
| 1 | AP adapter has exactly one IPv4 = 192.168.50.1/24, no 192.168.137.1 | **PASS** | ifIndex 23 `Local Area Connection* 11` (Microsoft Wi-Fi Direct Virtual Adapter #3) → `192.168.50.1/24`, PrefixOrigin Manual, AddressState Preferred, sole IPv4. No `192.168.137.1` on any adapter (`ipconfig /all` match: none). `169.254.137.1/16` sits on *Ethernet* (APIPA, unrelated). |
| 2 | Supervisor/wfd/dhcpd/dnsrescue exist, stable start times | **FAIL** | All four up but the generation is ~1 min old: daemon 44632 @16:08:44, wfd 33764 @16:08:45, dhcpd 22988 @16:08:46, dnsrescue 39552 @16:08:48. The prior generation (48864/49124/48448/8620 @16:06:04–07) disappeared mid-audit (~16:08:2x) and the task recorded `0x41306` (terminated) for that run. No shutdown/exit line in any log. |
| 3 | UDP 67 bound by dhcpd | **PASS (caveat)** | `UDP 0.0.0.0:67` → PID 22988 = `dhcpd`. Caveat: bound `0.0.0.0`, not `192.168.50.1` as specified. |
| 4 | DNS on 192.168.50.1 answers | **PASS** | `Resolve-DnsName` via 192.168.50.1 returned A records for example.com, cloudflare.com, github.com. `dnsrescue.log` logs those exact names; supervisor `dns_queries` rose 338→356. Serving, not just bound. |
| 5 | WinNat "LetShare" Active, non-zero sessions | **PASS** | `LetShare` Active, `192.168.50.0/24`. 63 sessions, all `InternalSourceAddress=192.168.50.10`. Sample: `192.168.50.10:49761 → 139.178.198.128:443` (ext `192.168.1.10:54303`, TCP); `192.168.50.10:49342 → 17.248.162.234:443` (ext `…:63148`, UDP); `192.168.50.10:500 → 49.44.190.247:500` (ext `…:63136`). |
| 6 | Client present on AP interface | **PASS (note)** | Neighbour on ifIndex 23: `192.168.50.10`, MAC `AA-BB-CC-DD-EE-FF`, State **Reachable**. Note: stale `192.168.137.3 / 00-00-00-00-00-00 / Unreachable` also present (ICS leftover, see F5). |
| 7 | No conflicting DHCP for 192.168.50 | **PASS (caveat)** | `dhcpd` is the only socket serving the 50-subnet. ICS (`SharedAccess`) **is Running** and holds `192.168.137.1:67` plus `0.0.0.0:53` (PID 3984), but no interface has `192.168.137.1`, so that DHCP socket is bound to a dead address and cannot answer 50-subnet clients. Its `0.0.0.0:53` proxy is shadowed by dnsrescue's more-specific `192.168.50.1:53`. |
| 8 | Routing sanity | **PASS** | Default `0.0.0.0/0 → 192.168.1.1` via ifIndex 10 (Wi-Fi). `192.168.50.0/24` on-link via ifIndex 23. |
| 9 | Logs | **PASS (errors present)** | Supervisor tail: `ap=up dhcp=up dns=up address=up nat=up leases=5 dns_queries=356`. dhcpd tail: `lease aa:bb:cc:dd:ee:ff -> 192.168.50.10` / `dhcp listening on 0.0.0.0:67` / `dns [192.168.50.1] | lease 43200s`. **14× `Os { code: 10048, AddrInUse }`** at the top of `letsshare-dhcpd.log`; 6 startup banners. One transient `dns=DOWN` (dns_queries=319). No panics. |
| 10 | Lease file | **PASS (note)** | 5 entries, all identical: `lease aa:bb:cc:dd:ee:ff -> 192.168.50.10` (grew 4→5 during audit). No timestamps, no expiry, no dedupe. |

## Real problems, by severity

**F1 — HIGH — `dhcpd` is a rogue DHCP server on the uplink.** `dhcp.rs:282` binds `0.0.0.0:67`, and `DhcpServer::handle()` (line 239+) validates only the MAC, never the ingress interface or the client's subnet. Replies broadcast to `255.255.255.255:68`. A firewall allow rule for `dhcpd.exe` exists on **Private, Public**, plus the built-in "Core Networking – DHCP-In" (Any/Allow); the uplink is on the **Public** profile. Any DISCOVER on the home Wi-Fi can therefore be offered `192.168.50.x / gw 192.168.50.1 / dns 192.168.50.1`. Fix: bind `192.168.50.1:67` (or `SO_BINDTODEVICE`/`IP_PKTINFO` and drop non-AP ingress).

**F2 — HIGH — stack died mid-audit and restarted silently.** At 16:07:40 the 16:06 generation was healthy; by ~16:08:30 all four PIDs were gone, `:67` was unbound, the phone's ARP went `Unreachable`, and DNS/DHCP were dead for roughly 20–40 s until 16:08:44. Task history for that run: `0x41306` (terminated). The task has only a `LogonTrigger`, so the 16:08:44 run was an external `Start-ScheduledTask`, i.e. an operator restart — but because the daemon writes no exit reason, a crash and a kill are indistinguishable from logs. That is the observability gap to close.

**F3 — MEDIUM — ICS still running and racing for port 67.** `SharedAccess` is Running with `ScopeAddress 192.168.137.1`; the 14 `AddrInUse` errors prove `dhcpd` lost the port race at some point and only survived by retrying. Today it is harmless only because two things hold: the broken `192.168.137.1:67` socket is bound to a non-existent address, and dnsrescue's `192.168.50.1:53` beats ICS's `0.0.0.0:53`. If ICS re-plumbs `192.168.137.1` or grabs `0.0.0.0:67`, dhcpd's bind fails again; if dnsrescue ever binds wildcard, the old no-DNS bug returns. Stop/disable ICS.

**F4 — LOW — lease file is append-only junk.** 5 duplicate lines for one MAC, no timestamps/expiry; supervisor reports `leases=5` for one client. Dedupe and timestamp.

**F5 — LOW — stale leftovers.** ARP `192.168.137.3` with `00-00-00-00-00-00` on the AP interface; `169.254.137.1` APIPA on Ethernet. Cosmetic residue from the ICS era.

**F6 — LOW — one `dns=DOWN` health blip** (dns_queries=319) while DNS worked throughout: the check is flaky, not the resolver.

**Verdict:** the data path genuinely works — client leased, NAT translating, DNS forwarding real (verified by the resolver's own query log, not just a bound socket). But two high-severity items stand: `dhcpd` will hand out addresses on the uplink LAN, and one full-stack restart happened during the watch with no recorded cause.
