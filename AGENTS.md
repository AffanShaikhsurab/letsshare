# LetShare — agent notes

## Subagent model (required)

All subagents must run on **DeepSeek V4.1 Flash 1017 via Fireworks**:

```
model: fireworks/accounts/fireworks/models/deepseek-v4p1-flash-1017
```

Pass this explicitly on every `subagent` call. Do not rely on a default.
Verified working with a real subagent call on 2026-10-23.

## What this app is

Shares a laptop's Wi-Fi uplink to phones. Windows' own Mobile Hotspot is the
baseline; this exists because it is unreliable.

## Hard-won facts (do not re-derive)

- The WinRT tethering API works **unelevated** and is the Tier 0 path.
- The legacy `netsh wlan set hostednetwork` path is dead on modern drivers.
- ICS on build 26200 hands out leases with **no DHCP option 6** (no DNS), creates
  no firewall rules, and its DNS proxy binds `:53` without answering. Service
  restarts do not fix it. Verified with a synthetic DHCPDISCOVER.
- ICS holds `192.168.137.1:67` exclusively, so the missing option cannot be
  injected from userland. A working resolver can be bound to the gateway address
  because ICS binds `0.0.0.0:53` non-exclusively.
- Tier 2 therefore runs its own stack: legacy Wi-Fi Direct AP + our DHCP (with
  option 6) + our DNS + WinNat on `192.168.50.0/24`.
- Tier 2 needs elevation for the adapter address and `New-NetNat`. WinNat is a
  host-global singleton and collides with WSL/Docker/Hyper-V.
- Never trust an operation result. `StartTetheringAsync` has returned
  `WiFiDeviceOff` while the hotspot was already `On`. Verify state afterwards.
- `StartTetheringAsync2` (session config) is refused on the AX203; fall back to
  the baseline call.
- Health checks must prove the **client's** path (lease, resolver, route), not the
  host's internet.

## Layout

```
crates/letsshare-core/   tethering, health, supervisor (Tier 0), dhcp, tier2, flight
crates/letsshare-probe/  CLI + helper binaries (wfd, dhcpd, dnsrescue, letsshare-tier2)
scripts/                 elevated setup, repair, autostart
```

## Operational

- Tier 2 supervisor: `target\release\letsshare-tier2.exe run`
- Status: `target\release\letsshare-tier2.exe status`
- Logon task: `LetShare` (RunLevel Highest). Install: `scripts/install-autostart.ps1`.
- Restart budget: 999 attempts, 1 minute apart. A second task,
  `LetShare-Watchdog`, runs at logon and every 5 minutes and restarts the first
  one if no `letsshare-daemon` process exists (`%TEMP%\letsshare-watchdog.log`).
- Killing the daemon does **not** kill `wfd`/`dhcpd`/`dnsrescue` on Windows. They
  keep ports 67/53 and the AP. The watchdog clears them before restarting, and
  the uninstaller clears them too.
- Task Scheduler rejects `P99999999DT23H59M59S` at registration even though
  PowerShell builds that trigger. Use a concrete window (3650 days).
- Logs: `%TEMP%\letsshare-*.log`, leases in `%TEMP%\letsshare-leases.txt`.
