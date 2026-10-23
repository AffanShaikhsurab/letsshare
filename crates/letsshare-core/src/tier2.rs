//! Tier 2: an owned IP layer, supervised.
//!
//! On some Windows builds ICS hands out leases without a DNS option, so clients
//! associate and then fail silently. Nothing in userland can inject the missing
//! option while ICS owns port 67, so this stack replaces it:
//!
//! ```text
//! Wi-Fi Direct legacy AP  ->  our DHCP (option 6)  ->  our DNS  ->  WinNat
//! ```
//!
//! Everything here is a child process we own, plus two pieces of system state
//! (the adapter address and the NAT instance) that only an elevated process can
//! repair. The loop below keeps all five alive and re-arms after sleep/resume.

use crate::tethering::OperState;
use std::fs::OpenOptions;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// The supervisor runs under `windows_subsystem = "windows"`, so it has no
/// console. Any console program it spawns would otherwise get a brand-new
/// console window flashed onto the desktop — visible as a blue rectangle
/// appearing every few seconds, and a new window for every child process.
/// Always suppress that.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a command that can never flash a console window.
fn hidden(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Published in the README and the source tree. Anyone in radio range knows it.
/// It exists so a checkout runs without configuration; it is not a secret.
pub const DEFAULT_PASSPHRASE: &str = "letsshare123";

/// Configuration for one owned stack.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory containing the helper binaries.
    pub root: PathBuf,
    pub ap_ip: Ipv4Addr,
    /// Internal prefix handed to WinNat.
    pub prefix: String,
    pub nat_name: String,
    pub ssid: String,
    pub passphrase: String,
    /// Resolver to forward to. `None` means use the uplink adapter's DNS.
    pub dns_upstream: Option<Ipv4Addr>,
    pub poll: Duration,
}

impl Config {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        // A default passphrase is published with this repository, so it must be
        // treated as a placeholder, never as a secret. Real deployments set the
        // environment variables; the supervisor warns when it sees the default.
        let ssid =
            std::env::var("LETSSHARE_SSID").unwrap_or_else(|_| "LetShare-WFD-Test".to_string());
        let passphrase = std::env::var("LETSSHARE_PASSPHRASE")
            .unwrap_or_else(|_| DEFAULT_PASSPHRASE.to_string());
        Self {
            root: root.into(),
            ap_ip: Ipv4Addr::new(192, 168, 50, 1),
            prefix: "192.168.50.0/24".to_string(),
            nat_name: "LetShare".to_string(),
            ssid,
            passphrase,
            dns_upstream: None,
            poll: Duration::from_secs(3),
        }
    }

    /// Root next to the running executable.
    pub fn from_current_exe() -> Self {
        let root = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(root)
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.exe"))
    }
}

/// One supervised child process.
struct Managed {
    name: &'static str,
    exe: PathBuf,
    args: Vec<String>,
    child: Option<Child>,
    failures: u32,
    next_try: Instant,
    log: PathBuf,
}

impl Managed {
    fn new(name: &'static str, exe: PathBuf, args: Vec<String>) -> Self {
        let log = std::env::temp_dir().join(format!("letsshare-{name}.log"));
        Self {
            name,
            exe,
            args,
            child: None,
            failures: 0,
            next_try: Instant::now(),
            log,
        }
    }

    fn alive(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                tracing::warn!(component = self.name, ?status, "child exited");
                self.child = None;
                false
            }
            Ok(None) => true,
            Err(e) => {
                tracing::warn!(component = self.name, error = %e, "child wait failed");
                self.child = None;
                false
            }
        }
    }

    /// Start the child if it is not running. Returns true if it was (re)started.
    fn ensure(&mut self) -> bool {
        if self.alive() {
            return false;
        }
        if Instant::now() < self.next_try {
            return false;
        }
        if !self.exe.exists() {
            tracing::error!(component = self.name, path = %self.exe.display(), "missing binary");
            return false;
        }

        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .ok();
        let err = log.as_ref().and_then(|f| f.try_clone().ok());

        let mut cmd = Command::new(&self.exe);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.args(&self.args)
            .stdin(Stdio::null())
            .stdout(log.map(Stdio::from).unwrap_or_else(Stdio::null))
            .stderr(err.map(Stdio::from).unwrap_or_else(Stdio::null));

        match cmd.spawn() {
            Ok(child) => {
                tracing::info!(component = self.name, pid = child.id(), "started");
                self.child = Some(child);
                self.failures = 0;
                true
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                let delay = backoff(self.failures);
                self.next_try = Instant::now() + delay;
                tracing::error!(component = self.name, error = %e, ?delay, "spawn failed");
                false
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn backoff(failures: u32) -> Duration {
    let secs = 1u64 << failures.min(5);
    Duration::from_secs(secs.min(30))
}

/// Path of the supervisor's own log (it runs windowless under a task, so
/// stdout is not visible anywhere).
pub fn supervisor_log() -> PathBuf {
    std::env::temp_dir().join("letsshare-supervisor.log")
}

/// Path the CLI writes control commands to.
pub fn control_file() -> PathBuf {
    std::env::temp_dir().join("letsshare-control.txt")
}

fn append_line(path: &Path, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

/// Snapshot of what is actually true, not what we intended.
#[derive(Debug, Clone)]
pub struct Status {
    pub ap: bool,
    pub dhcp: bool,
    pub dns: bool,
    pub address: bool,
    pub nat: bool,
    pub leases: usize,
    pub dns_queries: usize,
}

impl Status {
    pub fn healthy(&self) -> bool {
        self.ap && self.dhcp && self.dns && self.address && self.nat
    }

    pub fn render(&self) -> String {
        format!(
            "ap={} dhcp={} dns={} address={} nat={} leases={} dns_queries={}",
            mark(self.ap),
            mark(self.dhcp),
            mark(self.dns),
            mark(self.address),
            mark(self.nat),
            self.leases,
            self.dns_queries
        )
    }
}

fn mark(v: bool) -> &'static str {
    if v {
        "up"
    } else {
        "DOWN"
    }
}

fn count_matching(path: &Path, needle: &str) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| l.contains(needle)).count())
        .unwrap_or(0)
}

/// Is something already bound to this exact UDP address?
///
/// One syscall, no child process. The previous version spawned `netstat` on
/// every tick, which flashed a console window and cost tens of milliseconds.
/// A successful bind means the port was free, so it is dropped immediately.
fn port_bound(ip: Ipv4Addr, port: u16) -> bool {
    match UdpSocket::bind(SocketAddr::from((ip, port))) {
        Ok(sock) => {
            drop(sock);
            false
        }
        Err(_) => true,
    }
}

/// Does a resolver actually answer on this address? A bound socket proves
/// nothing, as ICS demonstrated. Retried once because a single dropped datagram
/// must not be reported as an outage.
fn dns_answers(ip: Ipv4Addr) -> bool {
    for attempt in 0..2 {
        if dns_answers_once(ip) {
            return true;
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    false
}

fn dns_answers_once(ip: Ipv4Addr) -> bool {
    let mut q: Vec<u8> = vec![
        0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    for label in ["example", "com"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0x00, 0x00, 0x01, 0x00, 0x01]);

    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else {
        return false;
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(1500)));
    if sock.send_to(&q, (ip, 53)).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    matches!(sock.recv_from(&mut buf), Ok((n, _)) if n > 12)
}

/// Distinct addresses handed out, not lines in the log.
fn count_unique_leases(path: &Path) -> usize {
    let mut set = std::collections::HashSet::new();
    if let Ok(s) = std::fs::read_to_string(path) {
        for line in s.lines() {
            if let Some((_, ip)) = line.rsplit_once("-> ") {
                set.insert(ip.trim().to_string());
            }
        }
    }
    set.len()
}

/// The supervisor.
pub struct Stack {
    cfg: Config,
    ap: Managed,
    dhcp: Managed,
    dns: Managed,
    last_repair: Instant,
    last_upstream_check: Instant,
    last_heartbeat: Instant,
    last_ics_check: Instant,
    ics_standdown: Option<Instant>,
    nat_ok: bool,
    upstream: Ipv4Addr,
}

impl Stack {
    pub fn new(cfg: Config) -> Self {
        let upstream = cfg.dns_upstream.unwrap_or_else(uplink_dns);
        let ap = Managed::new(
            "wfd",
            cfg.bin("wfd"),
            vec![
                "--hold".to_string(),
                cfg.ssid.clone(),
                cfg.passphrase.clone(),
            ],
        );
        let dhcp = Managed::new("dhcpd", cfg.bin("dhcpd"), vec![cfg.ap_ip.to_string()]);
        let dns = Managed::new(
            "dnsrescue",
            cfg.bin("dnsrescue"),
            vec![format!("{}:53", cfg.ap_ip), format!("{upstream}:53")],
        );

        Self {
            cfg,
            ap,
            dhcp,
            dns,
            last_repair: Instant::now() - Duration::from_secs(60),
            last_upstream_check: Instant::now() - Duration::from_secs(60),
            last_heartbeat: Instant::now(),
            last_ics_check: Instant::now() - Duration::from_secs(60),
            ics_standdown: None,
            nat_ok: false,
            upstream,
        }
    }

    pub fn upstream(&self) -> Ipv4Addr {
        self.upstream
    }

    /// True when the host owns the AP address.
    pub fn address_present(&self) -> bool {
        UdpSocket::bind((self.cfg.ap_ip, 0)).is_ok()
    }

    /// Ask Windows whether the NAT instance exists. Spawns PowerShell, so this
    /// is rate-limited by the caller.
    fn nat_present(&self) -> bool {
        let script = format!(
            "if (Get-NetNat -Name '{}' -ErrorAction SilentlyContinue) {{ 'yes' }} else {{ 'no' }}",
            self.cfg.nat_name
        );
        hidden("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("yes"))
            .unwrap_or(false)
    }

    /// Repair the two pieces only an elevated process can touch.
    fn repair(&mut self) {
        if Instant::now() - self.last_repair < Duration::from_secs(10) {
            return;
        }
        self.last_repair = Instant::now();

        let script = format!(
            "$ErrorActionPreference='SilentlyContinue';\
             $ap = Get-NetAdapter | Where-Object {{ $_.InterfaceDescription -like '*Wi-Fi Direct*' -and $_.Status -eq 'Up' }} | Select-Object -First 1;\
             if (-not $ap) {{ Write-Output 'NO_AP'; exit 2 }};\
             $foreign = Get-NetIPAddress -InterfaceIndex $ap.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue |\
               Where-Object {{ $_.IPAddress -eq '192.168.137.1' }};\
             if ($foreign) {{\
               $foreign | ForEach-Object {{ Remove-NetIPAddress -InterfaceIndex $ap.ifIndex -IPAddress $_.IPAddress -Confirm:$false -ErrorAction SilentlyContinue }};\
               Write-Output 'ADDRESS_CLEANED'\
             }};\
             if (-not (Get-NetIPAddress -InterfaceIndex $ap.ifIndex -IPAddress {ip} -ErrorAction SilentlyContinue)) {{\
               New-NetIPAddress -InterfaceIndex $ap.ifIndex -IPAddress {ip} -PrefixLength 24 -ErrorAction SilentlyContinue | Out-Null;\
               Write-Output 'ADDRESS_SET'\
             }};\
             if (-not (Get-NetNat -Name '{nat}' -ErrorAction SilentlyContinue)) {{\
               New-NetNat -Name '{nat}' -InternalIPInterfaceAddressPrefix '{prefix}' -ErrorAction SilentlyContinue | Out-Null;\
               Write-Output 'NAT_SET'\
             }};\
             Write-Output 'NAT_OK'",
            ip = self.cfg.ap_ip,
            nat = self.cfg.nat_name,
            prefix = self.cfg.prefix
        );

        match hidden("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let text = text.trim();
                if text.contains("ADDRESS_SET") || text.contains("NAT_SET") {
                    tracing::warn!(result = %text, "repaired system state");
                }
                if text.contains("NO_AP") {
                    tracing::warn!("no AP adapter present yet");
                }
                // The pass already asked Windows for the NAT; do not spawn a
                // second PowerShell just to ask the same question again.
                self.nat_ok = text.contains("NAT_OK") || text.contains("NAT_SET");
            }
            Err(e) => tracing::debug!(error = %e, "repair probe failed (probably not elevated)"),
        }
    }

    /// Follow the uplink resolver when the host changes network.
    fn refresh_upstream(&mut self) {
        if Instant::now() - self.last_upstream_check < Duration::from_secs(60) {
            return;
        }
        self.last_upstream_check = Instant::now();

        let current = uplink_dns();
        if current != self.upstream {
            tracing::warn!(from = %self.upstream, to = %current, "uplink resolver changed");
            append_line(
                &supervisor_log(),
                &format!("upstream {} -> {}", self.upstream, current),
            );
            self.upstream = current;
            self.dns.args = vec![format!("{}:53", self.cfg.ap_ip), format!("{current}:53")];
            // Restart with the new upstream on the next tick.
            self.dns.stop();
        }
    }

    /// Windows Mobile Hotspot preempts every Wi-Fi Direct scenario, so fighting
    /// it just produces a restart loop. Detect it and stand down.
    fn ics_is_on(&mut self) -> bool {
        if Instant::now() - self.last_ics_check < Duration::from_secs(15) {
            return self.ics_standdown.is_some();
        }
        self.last_ics_check = Instant::now();
        crate::tethering::Hotspot::auto()
            .ok()
            .and_then(|h| h.state().ok())
            .map(|s| s == OperState::On)
            .unwrap_or(false)
    }

    /// One supervision pass.
    pub fn tick(&mut self) {
        if self.ics_is_on() {
            if self.ics_standdown.is_none() {
                tracing::warn!("Windows Mobile Hotspot is on; standing down");
                append_line(&supervisor_log(), "standdown: Windows Mobile Hotspot is on");
                self.ap.stop();
                self.dhcp.stop();
                self.dns.stop();
                self.ics_standdown = Some(Instant::now());
            }
            return;
        }
        if self.ics_standdown.take().is_some() {
            tracing::info!("Windows Mobile Hotspot is off; resuming");
            append_line(&supervisor_log(), "resume: Windows Mobile Hotspot is off");
        }

        self.refresh_upstream();

        let ap_started = self.ap.ensure();
        if ap_started {
            // The adapter is new, so sockets bound to the old address are dead.
            // Stop them; the respawn below will wait for the address to exist.
            self.dhcp.stop();
            self.dns.stop();
            self.last_repair = Instant::now() - Duration::from_secs(60);
        }
        let _ = self.dhcp.ensure();
        let _ = self.dns.ensure();

        let address = self.address_present();

        // Periodic maintenance, not just failure repair: adapters can come back
        // with stale addresses from an earlier ICS session, and the NAT can be
        // dropped by another tool. A pass every 30s keeps the state canonical.
        // It is the only place that asks Windows about the NAT, so the status
        // line below never has to spawn anything.
        let due = Instant::now() - self.last_repair >= Duration::from_secs(30);
        if !address || !self.nat_ok || due {
            self.repair();
        }
    }

    /// Full status, asking Windows about the NAT. Used by the CLI, where one
    /// PowerShell spawn is invisible and worth the accuracy.
    pub fn status(&self) -> Status {
        self.status_inner(true)
    }

    /// Status for the supervisor's own log line. The NAT flag comes from the
    /// last maintenance pass, so a tick never spawns a process.
    fn status_cached(&self) -> Status {
        self.status_inner(false)
    }

    fn status_inner(&self, live_nat: bool) -> Status {
        let address = self.address_present();
        Status {
            // Report what is true, not which children we happen to own: the
            // stack may have been started by a task or an earlier supervisor.
            ap: address,
            // Only a server bound to the AP address counts. Checking the
            // wildcard would also accept ICS, which is exactly what we replace.
            dhcp: address && port_bound(self.cfg.ap_ip, 67),
            dns: dns_answers(self.cfg.ap_ip),
            address,
            nat: if live_nat {
                self.nat_present()
            } else {
                self.nat_ok
            },
            leases: count_unique_leases(&std::env::temp_dir().join("letsshare-leases.txt")),
            dns_queries: count_matching(
                &std::env::temp_dir().join("letsshare-dnsrescue.log"),
                " -> ",
            ),
        }
    }

    /// Apply a pending control command, if any. This is how the CLI can ask a
    /// running (elevated) supervisor to restart one component.
    fn apply_control(&mut self) {
        let path = control_file();
        let Ok(cmd) = std::fs::read_to_string(&path) else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let cmd = cmd.trim().to_lowercase();
        match cmd.as_str() {
            "restart ap" => self.ap.stop(),
            "restart dhcp" => self.dhcp.stop(),
            "restart dns" => self.dns.stop(),
            "restart all" => {
                self.ap.stop();
                self.dhcp.stop();
                self.dns.stop();
            }
            _ => return,
        }
        tracing::warn!(command = %cmd, "control command applied");
        append_line(&supervisor_log(), &format!("control: {cmd}"));
    }

    pub fn run(&mut self) {
        if self.cfg.passphrase == DEFAULT_PASSPHRASE {
            tracing::warn!(
                "using the published default Wi-Fi passphrase; set LETSSHARE_PASSPHRASE"
            );
            append_line(
                &supervisor_log(),
                "warning: default Wi-Fi passphrase in use (set LETSSHARE_PASSPHRASE)",
            );
        }
        tracing::info!(
            ap = %self.cfg.ssid,
            ip = %self.cfg.ap_ip,
            upstream = %self.upstream,
            "tier2 supervisor starting"
        );
        append_line(
            &supervisor_log(),
            &format!(
                "start ap={} ip={} upstream={}",
                self.cfg.ssid, self.cfg.ap_ip, self.upstream
            ),
        );

        let mut last_line = String::new();
        loop {
            self.apply_control();
            self.tick();

            // A heartbeat makes a silent death distinguishable from a kill: a
            // gap in the log is evidence, and its absence is not.
            if Instant::now() - self.last_heartbeat >= Duration::from_secs(60) {
                self.last_heartbeat = Instant::now();
                append_line(
                    &supervisor_log(),
                    &format!("heartbeat pid={} uptime_ok", std::process::id()),
                );
            }

            let s = self.status_cached();
            let line = s.render();
            if line != last_line {
                println!("{line}");
                append_line(&supervisor_log(), &line);
                last_line = line;
            }
            std::thread::sleep(self.cfg.poll);
        }
    }

    /// Tear everything down, including system state.
    pub fn down(&mut self) {
        self.ap.stop();
        self.dhcp.stop();
        self.dns.stop();
        let script = format!(
            "Remove-NetNat -Name '{}' -Confirm:$false -ErrorAction SilentlyContinue; Remove-NetIPAddress -IPAddress {} -Confirm:$false -ErrorAction SilentlyContinue",
            self.cfg.nat_name, self.cfg.ap_ip
        );
        let _ = hidden("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output();
    }
}

/// Read the resolver of whichever interface currently carries the default
/// route. Re-read periodically: a laptop roams, and a stale upstream is a
/// silent outage for every tethered client.
fn uplink_dns() -> Ipv4Addr {
    let script = "$if = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias; if ($if) { (Get-DnsClientServerAddress -InterfaceAlias $if -AddressFamily IPv4 -ErrorAction SilentlyContinue).ServerAddresses | Where-Object { $_ } | Select-Object -First 1 }";
    if let Ok(out) = hidden("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Ok(ip) = text.trim().parse::<Ipv4Addr>() {
            return ip;
        }
    }
    Ipv4Addr::new(1, 1, 1, 1)
}
