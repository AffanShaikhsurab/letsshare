> **Status note.** This is the engineering notebook written while building the project.
> Some sections describe intent or rejected experiments rather than shipped behaviour.
> README.md is the source of truth for what actually runs.

# LetShare — implementation plan

A small, fast, reliable Wi-Fi sharing app for Windows, written in Rust.

The goal: your laptop receives Wi-Fi on a good radio and hands it to your phone
in rooms where the phone's own radio gives up. Windows already has "Mobile
hotspot". It is unreliable. LetShare makes it reliable.

---

## 1. The core insight

**Do not rebuild the hotspot.** Windows already has a complete, kernel-backed AP
stack. Rebuilding it is the single biggest way to fail.

```
LetShare  ──►  WinRT tethering API  ──►  icssvc  ──►  wlansvc ──►  Wi-Fi Direct
                                                            virtual adapter (the AP)
                                              └─────►  SharedAccess (ICS)
                                                       NAT + DHCP + DNS proxy
                                                       on 192.168.137.1/24
```

Everything below the WinRT line is Microsoft's, runs in kernel/LocalService, and
works. What is *missing* is a supervisor: something that verifies the hotspot is
actually usable and repairs it when it silently isn't. That is the product.

This is the same bet Tailscale made on WireGuard: own the orchestration and the
reliability, not the packet path.

---

## 2. What we verified on this machine (not theory)

Every row below was measured on the target laptop — Windows 11 build 26200,
Intel Wi-Fi 6 AX203, Windows 10 Home Single Language SKU string — not read from
a blog.

| Claim | Result | Evidence |
|---|---|---|
| WinRT tethering API usable | **Yes** | `GetTetheringCapabilityFromConnectionProfile` → `Enabled` |
| Needs administrator? | **No** | started/stopped with `Elevated = False` |
| Start/stop works | **Yes** | `StartTetheringAsync` → `Success`, state `Off → On → Off` |
| AP is a real AP | **Yes** | `Microsoft Wi-Fi Direct Virtual Adapter` appeared, `192.168.137.1/24` |
| ICS DHCP server alive | **Yes** | `netstat` shows `192.168.137.1:67` + `:68`, DNS on `:53` |
| Forwarding + route | **Yes** | `Forwarding=Enabled` on WFD adapter, route `192.168.137.0/24` |
| Band is scriptable | **Yes** | `Band` set to 5 GHz and accepted; `IsBandSupported` present |
| Concurrent 2.4 uplink + 5 GHz AP | **Yes** | uplink on ch 1 @ 2.4 GHz while AP ran 5 GHz |
| 6 GHz AP available | **No** | `supported_bands` = 2.4, 5 (AX203 is not 6E) |
| Legacy `netsh wlan set hostednetwork` | **Dead** | `Hosted network supported: No` |
| Max clients | **8** | `MaxClientCount` |
| Idle teardown exists | **Yes** | `IsNoConnectionsTimeoutEnabled`; `DisableNoConnectionsTimeout` works |
| Per-session config | **Yes** | `StartTetheringAsync2(SessionAccessPointConfiguration)` |
| AP-over-station priority | **Yes** | `TetheringWiFiPerformancePriority::TetheringOverStation` |
| Rust can drive all of it | **Yes** | `letsshare-core` + `letsshare-probe` compile and run |
| Operation result is trustworthy | **No** | start returned `WiFiDeviceOff` while state was already `On` |
| `StartTetheringAsync2` (session config) works here | **No** | refused with `WiFiDeviceOff`; baseline call succeeds |

Two findings that change the design:

1. **No elevation required.** The privileged work happens in `icssvc` /
   `SharedAccess`. We do not need a Windows service just to start a hotspot.
   That removes most of the complexity people assume this app needs.
2. **`StartTetheringAsync2` takes a per-session config.** We can set SSID, band,
   WPA mode and priority *for this session only*, leaving the user's saved
   Windows hotspot settings untouched. Safe to experiment, trivial to roll back.

Two more that were found by hitting them:

3. **The operation result lies.** `StartTetheringAsync` returned
   `WiFiDeviceOff` while `TetheringOperationalState` was already `On`. The
   hotspot had started; the return code said otherwise. Every operation is now
   followed by a bounded state verification, and the verified state wins. This
   single habit is most of the difference between "glitchy" and "reliable".
4. **The newer API is not the better API.** `StartTetheringAsync2` was refused
   consistently on this adapter while the baseline `StartTetheringAsync` worked.
   We try the session path, then fall back. A wrapper that only knows the modern
   call would fail on hardware where the modern call is broken.

### The environment is the hard part

At the time of testing the uplink was **RSSI −82 dBm, 35 % signal, 14 Mbps TX,
2.4 GHz, channel 1**, with only three visible APs, two of them on channel 11.
The laptop itself is at the edge. That is precisely the use case — and it also
means upstream quality, not our software, sets the ceiling.

---

## 3. Reality check on the ambitious parts

Worth being blunt before writing code, because three of the ideas in the brief
are physically or contractually impossible on Windows without a signed kernel
driver.

### Band switching (2.4 ↔ 5 GHz) — **yes, and it matters**

`Band` is a real, supported property with `IsBandSupported` preflight and two
documented failure codes: `RadioRestriction` (driver refuses the band) and
`BandInterference` (AP band conflicts with the station link). We can do this
properly: probe, choose, fall back.

**Channel selection is not scriptable.** There is no API to pin the SoftAP
channel; the driver picks. So "avoid congested channels" collapses to "choose the
best band", which is most of the real-world benefit anyway.

### "Amplify the network" — **no, but the laptop-as-relay is real**

Software cannot add antenna gain, MIMO order, or receiver sensitivity. What is
true: a laptop usually has a better radio, bigger antennas, and no hand/head
absorption, so it holds a link where a phone drops. Putting the laptop between
router and phone is a genuine improvement. It is a placement story, not a tuning
story, and the app should say so instead of promising magic.

Software can genuinely help with: band choice, warning when the *uplink* is the
bottleneck (as here, at −82 dBm), and telling the user "move the laptop two
metres left" instead of "move the phone".

### True repeater / extender mode — **no**

Three independent blockers:

1. One radio cannot be on two channels at once, so any repeater is same-channel
   and halves throughput.
2. Windows' AP is a layer-3 construct. Microsoft explicitly prohibits layer-2
   bridging between the AP adapter and any other adapter.
3. A real 802.11 repeater must retransmit at L2 with its own BSSID — that needs
   an NDIS miniport, i.e. a WHQL-signed kernel driver.

Do not ship "extender" language. Ship "relay": the laptop is a client of the
router and an AP for the phone. That is honest and it works.

### Replacing ICS with our own NAT/DHCP — **no, and it would be slower**

ICS NAT is kernel-mode and is not the bottleneck; the radio is. A userland
`smoltcp` NAT through WinTun would add per-packet copies, our own DHCP, our own
DNS forwarder, our own ARP, and a task per flow — for maybe tens of Mbps and a
lot of fragility. It buys nothing at these rates.

The only defensible reason to go userland later is per-client policy/AQM, and
Windows has no fq_codel/cake equivalent to attach it to. Keep it as a research
spike, never as v1.

### IPv6 — **not available on the private side**

ICS does IPv4 NAT only. No NAT66, no DHCPv6-PD. Clients get IPv4. Say so.

---

## 4. The seven bugs we actually fix

This is the product spec. Each is something the built-in UI does wrong.

| # | Windows failure | LetShare fix |
|---|---|---|
| 1 | Hotspot turns off ~5 min after the last client leaves | `DisableNoConnectionsTimeout()` at bind time |
| 2 | Manager is bound to one profile; roam or Wi-Fi→Ethernet silently breaks NAT | Re-bind on uplink change; treat "uplink identity" as a watched signal |
| 3 | Reports "On" while the ICS gateway or DHCP is wedged | Verify `192.168.137.1/24` exists + real reachability, not just an API result |
| 4 | Screen lock / resume kills the radio; no event fires | Power request + resume re-arm; verify after wake |
| 5 | 5 GHz silently rejected, UI still says on | `IsBandSupported` preflight + handle `RadioRestriction`/`BandInterference` explicitly |
| 6 | Stale ICS state after driver update/VPN removal | Repair path: stop → clear → restart, with backoff and a circuit breaker |
| 7 | Upstream LAN also on `192.168.137.0/24` breaks both directions | Detect the collision and refuse with a clear message instead of failing mysteriously |
| 8 | `ClientCount` is built from leases, not associations | Treat "associated but no lease" as a first-class failure and restart the allocator |
| 9 | ICS DNS proxy binds but answers nothing; DHCP and NAT still work | Probe DNS *through the gateway*, then bind the gateway address and forward upstream |

Bug 3 is the important one. "Started successfully" and "works" are different
claims, and only the second one is worth showing the user.

---

## 5. Architecture

Deliberately small. Three crates, one process, one optional service later.

### 5.1 Data plane tiers

The access point and the router are separable problems, and the right split
depends on the machine. Rather than pick one data plane dogmatically, LetShare
picks the highest tier the hardware supports — and can fall back at runtime.

| Tier | AP | NAT/DHCP/DNS | Needs admin | Band control | When |
|---|---|---|---|---|---|
| **0 — Shared** | WinRT tethering | ICS (kernel) | no | **yes** | default, one radio |
| **1 — Dedicated** | WinRT tethering pinned to a second adapter | ICS (kernel) | no | **yes** | a second Wi-Fi adapter exists |
| **2 — Owned** | Wi-Fi Direct legacy AP | our DHCP + our DNS + WinNat | **yes** (service) | **no** | last resort only — see below |

Tier 2 prerequisites, now researched:

- **WinNat ships on Home.** `winnat.sys`, the `NetNat` module and `MSFT_NetNat`
  are present and running on this exact SKU (Home Single Language, 26200). No
  Hyper-V role required. The cmdlet takes an address prefix, not a virtual
  switch, so an addressed AP adapter can be the internal side — but that needs
  an elevated spike to confirm, not a doc read.
- **WinNat is a host-global singleton.** One internal prefix per machine, and
  other NATs (WSL2, Docker, Hyper-V) may already own it. Detect with
  `Get-NetNat` and fall back to ICS rather than fighting for it.
- **ICS must be torn down first.** It hands clients its own gateway/DNS, so our
  DHCP/DNS would be ignored and traffic would be double-translated.
- **Programmatic control is WMI**, not PowerShell: `MSFT_NetNat` in
  `root\StandardCimv2`. Shelling out to `New-NetNat` costs 200–500 ms and is
  brittle.
- **WinNat gives no DHCP, DNS, or IPv6.** We supply the first two, and must
  block or ignore the third.

One correction to an earlier assumption: **WinTun cannot see traffic arriving
on the AP adapter** — it is a synthetic L3 endpoint, not a hook. A userland NAT
would need WinDivert (≈30–50 % of a core at 300 Mbps, plus AV/EDR friction), and
true WFP redirect/reinject is kernel-only. That is why userland NAT is excluded
on the numbers rather than merely deprioritised.

### 5.1.1 Why Tier 2 is a last resort, not a plan

The legacy AP *works* — we started it on this hardware while the station link
stayed up. Feasible is not the same as viable. Research into the API and its
field history turned up four costs that no amount of engineering removes:

1. **No band or channel control, ever.** `WiFiDirectLegacySettings` exposes only
   `IsEnabled`, `Ssid`, `Passphrase`. Microsoft's answer (Dec 2024) is explicit:
   the driver and OS choose the channel, "there is no public api can change the
   channel". A request to add it has been open since 2021 with no answer. The AP
   follows the station's channel, and with no association the driver tends to
   pick 2.4 GHz. **You cannot guarantee a 2.4-only device can see it, and you
   cannot escape a congested channel.** The tethering manager, by contrast, has
   `Band` with `IsBandSupported` for 2.4/5/6 GHz.
2. **No client list and no RSSI.** There is no query for associated clients. The
   only signal is `ConnectionRequested` → `WiFiDirectDevice.FromIdAsync`, whose
   ids look like `WiFiDirect#AA:BB:..._LegacyPendingRequest`. Device lists must
   come from `GetIpNetTable2`, and disconnect events are unreliable.
3. **A two-minute trap.** If `FromIdAsync` is not called for every
   `ConnectionRequested`, Windows silently drops legacy clients after roughly
   two minutes. That is a platform behaviour, not a bug in a wrapper — and it is
   exactly the class of failure this app exists to eliminate.
4. **It is preempted by Mobile Hotspot**, which "takes precedence over all Wi-Fi
   Direct scenarios". The two cannot coexist, so this is a sequential fallback
   at best, never a concurrent one.

Add the field record: Intel's 2025 drivers removed SoftAP-class Wi-Fi Direct on
the AX201, and the only Rust wrapper (`wifidirect-legacy-ap`, 2023) has no
`Drop`, panics inside COM callbacks, and never retains the listener. FlyingCarpet
moves gigabytes over it and still collects start failures, "unable to obtain IP",
2.4-only throughput and connection drops.

**Conclusion:** Tier 2 buys control of the lease layer by giving up band control,
client visibility, and driver stability. That is a bad trade for a consumer app.
It stays in the plan as a feature-flagged escape hatch for machines where the
hotspot API is unavailable — not as an architecture we build toward.

Why this ordering:

- **Tier 1 is the real speed win.** One radio must time-share between station and
  AP; the measured rate swings on this machine were 7 → 115 Mbps within seconds.
  A second radio removes the contention entirely. `CreateFromConnectionProfileWithTargetAdapter`
  already supports pinning the AP adapter, so this is a supported API, not a hack.
- **Tier 2 is the reliability escape hatch, not the default.** It gives us the
  lease layer (which is where the reported failures live) but costs three
  things: elevation, band control, and a dependency on `New-NetNat`.
- **Userland NAT is excluded on the numbers.** A Wintun + userspace stack costs
  roughly 0.5 CPU cores per 100 Mbps and caps near 500 Mbps; kernel NAT costs
  essentially nothing. There is no reliability argument that justifies that bill.

Verified on the target machine:

| Question | Answer |
|---|---|
| Legacy AP starts while STA stays connected | **yes** |
| Own DHCP can bind UDP/67 unelevated | **yes** |
| Own DNS can bind `:53` on a specific address while ICS holds `0.0.0.0:53` | **yes** |
| Raw sockets for unicast DHCP replies | **blocked** — use broadcast or `CreateIpNetEntry2` |
| Address the AP adapter | **needs admin** |
| `New-NetNat` | **needs admin**, and is a per-host singleton |
| Second Wi-Fi radio present here | **no** |

### 5.2 Crate layout

```
crates/
  letsshare-core/     # all logic, no UI
    tethering.rs      # WinRT wrapper (verified surface)
    health.rs         # cheap probes: gateway, DHCP allocator, route, DNS
    flight.rs         # CSV flight recorder for intermittent failures
    supervisor.rs     # state machine, backoff, repair
  letsshare-probe/    # CLI: report / start / stop / tune / watch / soak / diagnose
  letsshare-app/      # tray UI (planned)
```

### Process model

**v1: one process, no elevation, tray resident.** Verified viable: tethering
works unelevated.

**v2: add a service only for what genuinely needs it** — restoring a stale ICS
pairing, setting the WFD adapter's network category to Private, installing a
scheduled-task autostart. Keep the UI unprivileged and talk to the service over
a named pipe with a security descriptor. This is Tailscale's split
(`tailscaled` + GUI) and it is the right shape *when* we need privilege — not
before.

### State machine

```
        turn_on
Idle ──────────► Starting ──► Running
                     ▲            │
                     │            │ gateway missing / uplink flap / DNS dead
                  Restarting ◄────┤
                     │            ▼
                  (backoff)    Degraded
                     └──────────► Failed   (circuit breaker open)
```

Per tick (3 s) we gather a `Probe`: operational state, client list, whether the
ICS gateway exists, connectivity class (Good / DnsBroken / Offline), and whether
the idle timeout came back. The state machine only acts on transitions.

Backoff: 2 s doubling to 60 s, ±20 % jitter. Circuit breaker after repeated
failures so we do not thrash a user's radio; reset on legitimate external change
(uplink changed, user toggled, driver re-enumerated).

### Health probes

Cheap and blunt, in `health.rs`:

- **Gateway present** — bind a UDP socket to `192.168.137.1:0`. Succeeds only if
  the address is local. Dependency-free, instant.
- **Internet** — TCP connect to anycast resolvers by IP literal, so a broken DNS
  cannot masquerade as a dead route.
- **DNS** — resolve a canary name.
- Classify to `Good | DnsBroken | Offline`, because those need different fixes.

Deliberately *not* in v1: ARP/neighbour analysis, per-client byte counters,
ETW. They are expensive relative to what they tell us.

### Config policy

Use `StartTetheringAsync2` with a session config. Never mutate the user's saved
hotspot unless they explicitly ask. Everything is reversible by construction.

---

## 6. UI

A tray icon plus one small window. Nothing else.

- Tray icon encodes state: off / starting / running / degraded / failed.
- Window: one big switch, the SSID + QR code to join, current clients, and one
  line of plain language about what is wrong.
- A single **Diagnose** button that produces one zip: our logs, the journal,
  `netsh wlan show interfaces`, `netsh wlan show networks mode=bssid`,
  `ipconfig /all`, `Get-NetAdapter`, and recent SharedAccess/icssvc event log
  entries. SSID and passphrase redacted. Support without ping-pong.
- No dashboards, no graphs in v1. The user has one question: "does my phone have
  internet?".

Framework: **Tauri v2** if we want the diagnostics surface cheaply (WebView2 is
already on Win11, tray/autostart/notification/updater plugins are maintained),
with the window destroyed when hidden so idle cost stays near-native. Fallback
is **egui + `tray-icon`** if we decide the extra webview is not worth it — pure
Rust, ~80 ms startup.

Either way: **never keep the window alive in the background.**

---

## 7. Roadmap

### M0 — proof (done)
Workspace, WinRT wrapper, hardware probe, verified start/stop/tune on real
hardware. `letsshare-probe watch` already supervises.

### M1 — trustworthy core
- Journal of intent + last-applied config (`%ProgramData%\LetsShare`), atomic
  write, two generations.
- Reconcile-on-start: compare journal to reality, never trust either alone.
- `192.168.137.0/24` collision detection.
- Uplink-change detection and re-bind.
- Unit tests around the state machine with a fake clock.

### M2 — survive the real world
- Power request so the radio is not gated mid-session.
- Resume handling: re-verify, re-arm, re-bind.
- Circuit breaker + user-visible "why".
- Repair for stale ICS pairing (needs elevation; that is when the service
  appears).
- Band auto-selection: score 2.4 vs 5 from a scan, respect `IsBandSupported`,
  fall back cleanly. (Note: Wi-Fi scan APIs may need location consent on 24H2+;
  degrade to `Auto` if denied.)

### M3 — usable app
- Tray UI + QR join + client list + Diagnose bundle.
- Autostart at login (per-user), single instance.
- First-run flow that explains the uplink/bottleneck honestly.

### M4 — optional
- Signed MSI + updater.
- Service split for privileged repairs.
- Per-client throttle via Policy-based QoS (blunt, but real).
- Wi-Fi 6E/6 GHz when the hardware has it.

---

## 8. Risks and open questions

| Risk | Why it matters | Mitigation |
|---|---|---|
| Build-specific ICS regressions (26200 has a known one) | Wrapping cannot fix a broken DHCP allocator | Verify, detect, report; keep a documented "known-bad build" path |
| Driver rejects concurrent STA+AP | Hotspot starts but uplink collapses | Detect via gateway + uplink health; prefer `Auto` band; surface it |
| Scan APIs denied without location consent | Band auto-selection degrades to blind | Try, fall back to `Auto`, say so |
| Third-party firewalls / VPN filter drivers | Break ICS DHCP outright | Detect, name the likely culprit, do not pretend |
| Wi-Fi Direct scenarios preempt each other | Screen casting can take the radio | Detect, warn, do not fight |
| Antivirus flagging an unsigned network app | Blocks adoption | Sign early, keep the binary small and boring |
| Vendor drivers removing SoftAP/WFD support (Intel AX201, 2025) | A machine that worked yesterday stops working | Runtime capability probe, never a platform assumption; ICS remains the fallback |

**Open questions to settle before M2:** does `StartTetheringAsync2` behave
identically from a LocalSystem service; does `EnableSharing`'s one-time
confirmation prompt ever surface from a service; and is the 5-minute idle
teardown actually the top complaint in the field or just the loudest one online.

---

## 9. Prior art — what exists, what to reuse, what to avoid

The field is thinner than it looks. Every famous Windows hotspot app (mHotspot,
MyPublicWiFi, Connectify) is closed source. The open-source field tops out around
250 stars. That is an opportunity, but it also means there is no project to
simply depend on.

Three techniques exist on Windows, and they are **not** interchangeable:

1. `netsh wlan set hostednetwork` — deprecated, and dead on modern drivers
   (this laptop reports `Hosted network supported: No`). A museum piece.
2. **`NetworkOperatorTetheringManager`** (WinRT) — what Settings uses. Gives you
   the AP *and* the router: ICS NAT, DHCP, DNS, client enumeration, band control.
   **This is what LetShare uses.**
3. `WiFiDirectAdvertisementPublisher` legacy AP — a bare L2 AP with no IP layer.
   You must bring your own NAT, which on Windows means ICS again or a driver.

Worth noting explicitly: `wifidirect-legacy-ap` is the only real Rust SoftAP
crate (BSD-4-Clause, ~5 stars, last touched 2023). We are *not* using it, because
technique 3 hands us a network we would then have to route — strictly more work
for strictly less capability than technique 2.

| Project | Lang | What it gives us | Verdict |
|---|---|---|---|
| [jfhr/mobile-hotspot](https://github.com/jfhr/mobile-hotspot) | C# | Clean `NetworkOperatorTetheringManager` usage; polls `GetTetheringClients()` on a 10 s cadence with a delta model | **Copy the design**, not the code |
| [loxia01/PSInternetConnectionSharing](https://github.com/loxia01/PSInternetConnectionSharing) | PS | The clearest documentation of `INetSharingManager` COM that exists: disable-all → set public/private → verify | **Spec for our repair path** |
| [lakinduakash/linux-wifi-hotspot](https://github.com/lakinduakash/linux-wifi-hotspot) | Shell | Same-card uplink+AP, and "channel follows client" so the AP does not fight the uplink | **Architecture lesson** |
| [oblique/create_ap](https://github.com/oblique/create_ap) / [garywill/linux-router](https://github.com/garywill/linux-router) | Shell | Reliability by *supervising* four children; `--daemon`, reconnect logic | **Model for our supervisor** |
| [simon0302010/wifi-scan](https://github.com/simon0302010/wifi-scan) | Rust | Native `WlanAPI` scanning | **Candidate dep for band preflight** |
| [Mygod/VPNHotspot](https://github.com/Mygod/VPNHotspot) | Kotlin | Rootless tethering when you cannot get NAT | Not needed — ICS exists |

Android's stack is the best mental model: it keeps **three independent state
machines** — upstream selection, AP, DHCP server — because those three fail
independently and need separate repair. Windows gives us the same three
(adapter, AP, ICS) with no unified state, which is precisely why the built-in UI
is bad at reporting what went wrong. Our supervisor mirrors that split.

Three anti-patterns we will not repeat:

1. **Hosted-network wrappers** — silently "succeed" while producing a dead AP.
2. **Restart-the-whole-stack keepalives** (e.g. `PersistentHotspot`, 100 stars,
   exists solely to defeat the idle timeout). The correct fix is one API call,
   not a 60-second restart loop. This is the single clearest example of the
   flakiness we are trying to remove.
3. **Assuming ICS is configurable.** It hardcodes `192.168.137.1/24` and owns its
   DHCP/DNS. Needing a different subnet means abandoning ICS for `New-NetNat` +
   your own DHCP — and the two refuse to coexist. We stay on ICS.

One claim from the research worth correcting: `NetworkOperatorTetheringManager`
is documented as needing the restricted `wiFiControl` capability, but that
applies to **packaged (MSIX/UWP)** apps. We verified an unpackaged Win32 binary
driving it with no manifest and no elevation. It becomes relevant only if we
ever ship through the Store.

---

## 10. Non-goals

- Not a repeater/extender.
- Not a VPN, not a proxy, not traffic shaping in v1.
- Not cross-platform. Windows only, done properly.
- Not a replacement for ICS, DHCP, DNS, or NAT.
- No kernel driver unless a specific feature proves impossible without one.

---

## 11. Appendix: symptom → cause, and the decision gate

### Observed live failure (first capture)

Reproduced on 2026-10-23 with a real phone on the target laptop:

```
clients = 1        phone leased 192.168.137.34
gateway = up       192.168.137.1/24 on the WFD adapter
dhcp    = serving  UDP 192.168.137.1:67 bound
forwarding = on    both interfaces
ICS pairing = correct (public = Wi-Fi, private = WFD)
host can ping the phone (35-114 ms)
DNS via 192.168.137.1:53 -> no response
DNS via 127.0.0.1:53     -> no response
DNS via 192.168.1.1:53  -> works
```

So: association, DHCP, addressing, ARP and L3 were all healthy. **Only the
ICS DNS proxy was dead** — bound to `0.0.0.0:53`, answering nothing, and with no
ICS firewall rules created at all, which indicates ICS never finished
initialising. A full tethering stop/start did **not** repair it; the client
re-leased and the proxy stayed dead.

### Root cause, isolated

A synthetic DHCPDISCOVER against the live allocator returns a complete offer —
and no DNS option:

```
opt 53 OFFER | 54 192.168.137.1 | 1 255.255.255.0 | 3 192.168.137.1
opt 58/59 timers | 51 lease 168h | 46 NetBIOS | 15 domain <empty>
(no option 6)
```

Every option is enumerated in the raw dump, so this is not a decoder artefact.
Clients therefore receive an address and a gateway but **no resolver**, which is
precisely "connected, no internet": Android associates, cannot resolve anything,
marks the network unvalidated, hides the Wi-Fi icon and falls back to cellular.

Confirmed independently: a working resolver bound to `192.168.137.1:53` (possible
because ICS binds `0.0.0.0:53` non-exclusively) received **zero** queries from
the phone, even after a forced reassociation. The phone was never told to use it.

Why it cannot be patched from userland:

- ICS holds `192.168.137.1:67` **exclusively** — a second bind fails, so the
  missing option cannot be injected into DHCP.
- ICS also binds `0.0.0.0:53`; the gateway address can be taken over, but only
  helps clients that were told to use it.
- No ICS firewall rules exist at all, so ICS setup stopped part-way.

This host is build **26200**, which has a documented ICS regression affecting the
allocator and DNS. The conclusion is not "restart harder" — it is that on such
builds ICS cannot supply the IP layer, and LetShare must own DHCP and DNS.

### Tier 2 result: owned stack works end to end

Brought up on the same hardware, with ICS fully off:

```
legacy AP            started, uplink stayed connected
AP adapter           192.168.50.1/24
our DHCP             phone leased 192.168.50.10
our DNS              served github/apple/google lookups for that client
WinNat               66 active translations 192.168.50.10 <-> 192.168.1.10
traffic              134.7 KB up / 757.7 KB down in 10 s
host -> phone RTT    4-7 ms   (was 35-114 ms under ICS)
```

No static DNS was configured on the client. The lease carried option 6 because
we wrote the packet. That is the defect closed.

Remaining hardening for Tier 2: retry the DNS bind (the address is tentative for
a moment after assignment, which killed the first start), log leases, and put the
whole stack under the same supervisor and journal used for Tier 0.

### Confirmed by the user

Setting a static DNS of `192.168.137.1` on the phone made the connection work
immediately. The resolver we bound then served **84 real queries** from the
phone (App Store telemetry, CDN hostnames, editor APIs). That proves:

- NAT and forwarding are fine.
- Routing between AP and uplink is fine.
- The DNS forwarder itself is correct.
- **The single defect is the absent DHCP option 6.** Nothing else was wrong.

So the repair surface is precisely one field. The remaining question is how to
deliver it automatically, since ICS owns `192.168.137.1:67` exclusively:

1. Restart `SharedAccess` + `icssvc` (elevated) and re-probe the offer.
2. If the proxy returns but the option does not, set the AP adapter's DNS so
   ICS has a resolver to advertise — guarding against a forwarding loop.
3. If neither works, own DHCP (Tier 2), which needs ICS out of the way.

That is the exact shape of "connected, no internet": everything below DNS works,
so the phone shows associated, then silently falls back to cellular because its
connectivity probe cannot resolve.

The surgical fix, verified on the same session: ICS binds `0.0.0.0:53`
non-exclusively, so a more specific bind wins for the gateway address. Binding
`192.168.137.1:53` and forwarding upstream restored resolution immediately while
ICS kept NAT and DHCP. This is `dnsrescue`, and it is the model for the whole
product: **do not replace the platform, repair the one broken component.**

### Symptom table

| What you see | What it means |
|---|---|
| Phone says "connected", Windows shows no device | Association succeeded (L2). DHCP failed. `ClientCount` counts **leases**, not associations — so Windows honestly shows zero. |
| Nothing loads despite "connected" | No address, therefore no gateway, therefore no DNS. Same single root cause. |
| Works again after restarting the network | A restart forces ICS to re-pair and makes the client send a fresh DHCP DISCOVER, so it works until the next expiry. |
| Dies again after roughly an hour | A lease timer. Renewal at T1 is not being answered; at expiry the client falls off. |
| Speed collapses, band drops to 2.4 GHz | Single radio time-sharing STA+AP. Measured swings on this machine: TX 7 → 115 Mbps within minutes at −76 dBm. The uplink is the ceiling. |

This is why the supervisor verifies **leases**, not just state. A hotspot that is
`On`, owns `192.168.137.1`, and refuses to hand out addresses is the exact case
the built-in UI calls healthy.

### The gate before committing to an architecture

Run `letsshare-probe soak` for an hour with the phone connected and reproduce
the failure. The CSV decides the design:

| CSV signature at failure | Cause | Response |
|---|---|---|
| `gateway=0` | ICS adapter lost its address | Auto-restart (already implemented) |
| `gateway=1, dhcp=0` | Allocator stopped serving | Auto-restart allocator (implemented) |
| `gateway=1, dhcp=1, clients=1, net=Offline` | Uplink died | Do **not** restart; report upstream |
| Clients drop to 0 hourly, then recover | Lease renewal broken | Scheduled renew nudge first; Tier 2 only if that fails |
| Phone associates, `clients` stays 0 | Leases never issued | Tier 2, with its costs understood (see 5.1.1) |
| Everything healthy but throughput low | Radio/uplink physics | Band policy, placement advice |

If most rows are the first two, Plan A (supervise ICS) is correct and sufficient.
If the failures are the last two, ICS is structurally broken on this machine and
only Plan B — our own IP layer over a Wi-Fi Direct legacy AP — actually fixes it.

---

## 12. Why Rust

Not for speed — the bottleneck is the radio. For the reasons that actually
matter here: a single small binary with no runtime, no GC pauses while a
3-second poll loop runs for days, deterministic teardown, and `windows` crate
bindings that reach every API we need with no FFI glue. The app should be
invisible: a few MB, a few MB of RAM, instant start, and never in the way.

---

## 13. Deployed operations reference (what actually runs)

```
Task "LetShare" (at logon, RunLevel Highest, hidden)
  letsshare-daemon.exe run          windowless supervisor
    wfd.exe --hold <ssid> <pass>    legacy Wi-Fi Direct AP
    dhcpd.exe 192.168.50.1          DHCP on 192.168.50.1:67 (option 6 present)
    dnsrescue.exe 192.168.50.1:53   forwarder with a TTL-aware cache
  WinNat "LetShare"                 192.168.50.0/24 -> uplink

Task "LetShare-Watchdog" (at logon, then every 5 minutes)
  letsshare-watchdog.exe            no-op while the daemon runs; otherwise
                                    clears orphaned children, ends a stuck
                                    task, starts it, waits for the process
```

Control and inspection:

```powershell
letsshare-tier2.exe status            # verified by function, not by handle
letsshare-tier2.exe restart dhcp|dns|ap|all
letsshare-tier2.exe logs
```

### Post-audit hardening

An independent audit of the live stack found two high-severity issues, both
fixed and re-verified:

- **dhcpd bound `0.0.0.0:67`** and would have offered `192.168.50.x` leases to
  devices on the uplink LAN. It now binds `192.168.50.1:67` only. A broadcast
  DISCOVER from the AP still reaches it, with option 6.
- **Silent restarts had no recorded cause.** The daemon now writes a 60-second
  heartbeat and a panic hook to `%TEMP%\letsshare-supervisor.log`, so a gap is
  evidence and a crash is distinguishable from a kill.

Also addressed: the DNS health probe retries once with a longer timeout (it was
reporting DOWN while DNS worked); lease logging deduplicates and timestamps; and
the supervisor stands down when Windows Mobile Hotspot is on, because that
preempts every Wi-Fi Direct scenario and fighting it only produces a restart
loop.
