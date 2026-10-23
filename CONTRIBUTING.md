# Contributing to LetShare

Thanks for looking. This is a small, Windows-specific tool, so the most useful
contributions are usually precise bug reports and small, tested changes.

## Before you start

- **Platform.** The supported target is Windows 11 with a Wi-Fi adapter that
  exposes a Wi-Fi Direct AP role. Windows 10 2004+ is untested. macOS and Linux
  are out of scope: the whole point is the Windows tethering stack.
- **Hardware.** The reference machine is an Intel AX203. Other adapters vary in
  whether they expose the legacy AP role and which bands they allow. Say which
  adapter you tested on.
- **Elevation.** The stack configures an adapter address and creates a WinNat
  instance. Development requires an elevated shell for those steps.

## Build and test

```powershell
cargo build --release -p letsshare-probe
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Every binary lives in `target\release`. The release profile is tuned for size
and speed (`lto = "thin"`, `panic = "abort"`, one codegen unit).

## Run it while developing

Do not point your working copy at the installed task. Build, then deploy:

```powershell
# elevated
.\scripts\clean-restart.ps1
```

That stops the running stack, rebuilds, copies the binaries to
`%ProgramFiles%\LetShare`, reinstalls both scheduled tasks and starts them.
`.\scripts\deploy-and-verify.ps1` additionally exercises the watchdog.

## What to change, and what not to

- Keep the supervisor's rule: **verify by function, not by handle.** "The child
  process exists" is not proof that DHCP answers or that DNS resolves.
- Health checks must prove the client path (lease, resolver, route), not the
  host's own internet access.
- The DHCP server must stay bound to the AP address. Binding `0.0.0.0:67` makes
  it a rogue DHCP server on whatever network the laptop is attached to.
- Do not add a dependency without a licence that MIT can absorb. Run
  `cargo deny check licenses` if you add one.
- Keep PowerShell files ASCII. `Set-Content` and editors have produced smart
  quotes that break parsing before.

## Reporting bugs

Include:

1. `target\release\letsshare-tier2.exe status`
2. The last ~30 lines of `%TEMP%\letsshare-supervisor.log`
3. `Get-NetAdapter` and `netsh wlan show interfaces`
4. Adapter model, Windows build, and whether the uplink is 2.4 or 5 GHz

For anything that looks security-sensitive, do not open a public issue. See
[SECURITY.md](SECURITY.md).

## Pull requests

- One logical change per PR, with a sentence on what you measured.
- Tests for parsing, allocation and anything with an off-by-one.
- `cargo fmt` and `clippy -D warnings` clean.
- By contributing you agree your work is licensed under the MIT licence in
  [LICENSE](LICENSE).
