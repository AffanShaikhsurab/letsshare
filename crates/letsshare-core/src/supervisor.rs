//! The supervisor: the reason this app exists.
//!
//! Windows can start a hotspot. It cannot keep one honest. This state machine
//! watches the thing it started, verifies that it is actually usable, and
//! repairs it with bounded restarts instead of leaving the user with a pretty
//! toggle that says "On".

use crate::health::{self, Connectivity};
use crate::tethering::{Band, Client, Hotspot, OperState};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Tunables. Defaults are deliberately conservative.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Radio band to request. `Auto` lets the driver decide.
    pub band: Band,
    /// Optional SSID override. `None` keeps whatever Windows has stored.
    pub ssid: Option<String>,
    /// Optional passphrase override.
    pub passphrase: Option<String>,
    /// Ask the driver to favour the AP over the station link.
    pub ap_first: bool,
    /// Try WPA3 when the adapter supports it.
    pub wpa3: bool,
    /// Loop period.
    pub poll: Duration,
    /// First restart delay; doubles per consecutive failure.
    pub backoff_base: Duration,
    /// Restart delay ceiling.
    pub backoff_max: Duration,
    /// Consecutive ICS-layer observations (gateway or DHCP) before a restart.
    pub gateway_strikes: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            band: Band::Auto,
            ssid: None,
            passphrase: None,
            ap_first: true,
            wpa3: false,
            poll: Duration::from_secs(3),
            backoff_base: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            gateway_strikes: 2,
        }
    }
}

/// What the supervisor believes it is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// User has not asked for a hotspot.
    Idle,
    /// Bringing it up.
    Starting,
    /// On and verified reachable.
    Running,
    /// On but something is wrong (uplink down, DNS broken, no gateway).
    Degraded,
    /// Waiting out a backoff before retrying.
    Restarting,
    /// Gave up for now; `reason` says why.
    Failed,
}

impl Phase {
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Starting => "starting",
            Phase::Running => "running",
            Phase::Degraded => "degraded",
            Phase::Restarting => "restarting",
            Phase::Failed => "failed",
        }
    }
}

/// A point-in-time view for the UI.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub phase: Phase,
    pub desired: bool,
    pub state: OperState,
    pub uplink: String,
    pub gateway_present: bool,
    pub dhcp_listening: bool,
    pub connectivity: Connectivity,
    pub clients: Vec<Client>,
    pub max_clients: u32,
    pub failures: u32,
    pub idle_timeout: bool,
    pub note: String,
}

/// Read-only facts about the machine, gathered per tick.
#[derive(Debug, Clone)]
struct Probe {
    state: OperState,
    clients: Vec<Client>,
    max_clients: u32,
    gateway_present: bool,
    dhcp_listening: bool,
    connectivity: Connectivity,
    idle_timeout: bool,
}

fn jitter(base: Duration, failures: u32) -> Duration {
    let exp = failures.min(6);
    let scaled = base.saturating_mul(1u32 << exp);
    let capped = scaled.min(Duration::from_secs(60));
    // Deterministic-enough jitter without pulling in an RNG crate.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pct = 80 + (nanos % 41); // 80..=120
    capped.mul_f64(pct as f64 / 100.0)
}

/// Owns the hotspot lifecycle for one uplink.
pub struct Supervisor {
    policy: Policy,
    desired: bool,
    phase: Phase,
    hs: Option<Hotspot>,
    uplink: String,
    failures: u32,
    retry_at: Option<Instant>,
    gateway_strikes: u32,
    prepared: bool,
    note: String,
}

impl Supervisor {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            desired: false,
            phase: Phase::Idle,
            hs: None,
            uplink: String::new(),
            failures: 0,
            retry_at: None,
            gateway_strikes: 0,
            prepared: false,
            note: String::new(),
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn policy_mut(&mut self) -> &mut Policy {
        &mut self.policy
    }

    pub fn desired(&self) -> bool {
        self.desired
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The bound hotspot handle, if any. Used by the flight recorder.
    pub fn hotspot(&self) -> Option<&Hotspot> {
        self.hs.as_ref()
    }

    /// Ask for the hotspot to be up. Idempotent.
    pub fn turn_on(&mut self) {
        if !self.desired {
            self.desired = true;
            self.retry_at = None;
            self.failures = 0;
            self.phase = Phase::Starting;
            self.note = "starting".into();
        }
    }

    /// Ask for the hotspot to be down. Idempotent.
    pub fn turn_off(&mut self) {
        self.desired = false;
        self.retry_at = None;
        self.gateway_strikes = 0;
    }

    fn bound(&mut self) -> Result<(), String> {
        match Hotspot::auto() {
            Ok(hs) => {
                let name = hs.uplink().to_string();
                if name != self.uplink {
                    tracing::info!(uplink = %name, "uplink bound");
                    self.uplink = name;
                    self.prepared = false;
                    self.failures = 0;
                    self.retry_at = None;
                }
                self.hs = Some(hs);
                Ok(())
            }
            Err(e) => {
                self.note = format!("no usable uplink: {e}");
                Err(self.note.clone())
            }
        }
    }

    /// One-time driver configuration for this uplink binding.
    fn prepare(&mut self) -> Result<(), String> {
        if self.prepared {
            return Ok(());
        }
        let Some(hs) = &self.hs else {
            return Err("not bound".into());
        };

        // The single highest-value call in the whole API: without it Windows
        // tears the hotspot down five minutes after the last client leaves.
        if let Err(e) = hs.disable_idle_timeout() {
            tracing::warn!(error = %e, "could not disable idle timeout");
        }

        self.prepared = true;
        Ok(())
    }

    fn probe(&self) -> Result<Probe, String> {
        let hs = self.hs.as_ref().ok_or("not bound")?;
        let state = hs.state().map_err(|e| e.to_string())?;
        let clients = hs.clients().unwrap_or_default();
        let gateway_present = health::ics_gateway_present();
        // Only meaningful when nobody has a lease: if a client is connected,
        // DHCP demonstrably worked. This is the check the built-in UI lacks.
        let dhcp_listening = if gateway_present && clients.is_empty() {
            health::ics_dhcp_listening()
        } else {
            true
        };
        Ok(Probe {
            state,
            clients,
            max_clients: hs.max_clients().unwrap_or(0),
            gateway_present,
            dhcp_listening,
            connectivity: health::classify(Duration::from_millis(600)),
            idle_timeout: hs.idle_timeout_enabled().unwrap_or(false),
        })
    }

    fn schedule_retry(&mut self, reason: &str) {
        self.failures = self.failures.saturating_add(1);
        let delay = jitter(self.policy.backoff_base, self.failures - 1);
        self.retry_at = Some(Instant::now() + delay);
        self.phase = Phase::Restarting;
        self.note = format!("{reason}; retrying in {:.0}s", delay.as_secs_f32());
        tracing::warn!(reason, retry_in = ?delay, failures = self.failures, "hotspot needs repair");
    }

    /// Advance the state machine and return a fresh snapshot.
    pub fn tick(&mut self) -> Snapshot {
        if !self.desired {
            if let Some(hs) = &self.hs {
                if matches!(hs.state(), Ok(OperState::On)) {
                    let _ = hs.stop();
                }
            }
            self.phase = Phase::Idle;
            self.note = "off".into();
            return self.snapshot_defaults();
        }

        if let Err(e) = self.bound() {
            self.phase = Phase::Failed;
            self.note = e;
            return self.snapshot_defaults();
        }

        if let Err(e) = self.prepare() {
            self.schedule_retry(&e);
            return self.snapshot_defaults();
        }

        let probe = match self.probe() {
            Ok(p) => p,
            Err(e) => {
                self.schedule_retry(&e);
                return self.snapshot_defaults();
            }
        };

        match probe.state {
            OperState::On => {
                if !probe.gateway_present {
                    self.gateway_strikes += 1;
                    self.phase = Phase::Degraded;
                    self.note = format!(
                        "AP is on but the ICS gateway {} is missing ({}/{})",
                        health::ICS_GATEWAY,
                        self.gateway_strikes,
                        self.policy.gateway_strikes
                    );
                    if self.gateway_strikes >= self.policy.gateway_strikes {
                        if let Some(hs) = &self.hs {
                            let _ = hs.stop();
                        }
                        self.gateway_strikes = 0;
                        self.prepared = false;
                        self.schedule_retry("ICS gateway did not come up");
                    }
                } else if !probe.dhcp_listening {
                    // AP is up and addressed, but nothing is handing out leases.
                    // This is the failure the built-in UI cannot see: the phone
                    // associates, says "connected", and gets no address, while
                    // Windows shows zero devices because it counts leases.
                    self.gateway_strikes += 1;
                    self.phase = Phase::Degraded;
                    self.note = format!(
                        "AP is up but the DHCP allocator is not serving ({}/{})",
                        self.gateway_strikes, self.policy.gateway_strikes
                    );
                    if self.gateway_strikes >= self.policy.gateway_strikes {
                        if let Some(hs) = &self.hs {
                            let _ = hs.stop();
                        }
                        self.gateway_strikes = 0;
                        self.prepared = false;
                        self.schedule_retry("ICS DHCP allocator stopped serving");
                    }
                } else {
                    self.gateway_strikes = 0;
                    self.failures = 0;
                    match probe.connectivity {
                        Connectivity::Good => {
                            self.phase = Phase::Running;
                            self.note = format!(
                                "{} client(s) sharing {}",
                                probe.clients.len(),
                                self.uplink
                            );
                        }
                        Connectivity::DnsBroken => {
                            self.phase = Phase::Degraded;
                            self.note = "uplink has a route but DNS is failing".into();
                        }
                        Connectivity::Offline => {
                            self.phase = Phase::Degraded;
                            self.note =
                                "uplink is offline (upstream problem, not the hotspot)".into();
                        }
                    }
                }
            }
            OperState::InTransition => {
                self.phase = Phase::Starting;
                self.note = "Windows is transitioning the radio".into();
            }
            OperState::Off | OperState::Unknown => {
                if let Some(at) = self.retry_at {
                    if Instant::now() < at {
                        self.phase = Phase::Restarting;
                        return self.snapshot(probe);
                    }
                }
                self.phase = Phase::Starting;
                self.note = "starting hotspot".into();
                let session = crate::tethering::SessionConfig {
                    ssid: self.policy.ssid.clone(),
                    passphrase: self.policy.passphrase.clone(),
                    band: self.policy.band,
                    ap_first: self.policy.ap_first,
                    wpa3: self.policy.wpa3,
                };
                let result = self
                    .hs
                    .as_ref()
                    .ok_or_else(|| "not bound".to_string())
                    .and_then(|hs| hs.start_with(&session).map_err(|e| e.to_string()));

                match result {
                    Ok(_) => {
                        self.retry_at = None;
                        self.prepared = false;
                        self.note = "started, verifying".into();
                    }
                    Err(e) => {
                        self.schedule_retry(&e);
                    }
                }
            }
        }

        self.snapshot(probe)
    }

    fn snapshot_defaults(&self) -> Snapshot {
        Snapshot {
            phase: self.phase.clone(),
            desired: self.desired,
            state: OperState::Unknown,
            uplink: self.uplink.clone(),
            gateway_present: health::ics_gateway_present(),
            dhcp_listening: true,
            connectivity: Connectivity::Offline,
            clients: Vec::new(),
            max_clients: 0,
            failures: self.failures,
            idle_timeout: false,
            note: self.note.clone(),
        }
    }

    fn snapshot(&self, probe: Probe) -> Snapshot {
        Snapshot {
            phase: self.phase.clone(),
            desired: self.desired,
            state: probe.state,
            uplink: self.uplink.clone(),
            gateway_present: probe.gateway_present,
            dhcp_listening: probe.dhcp_listening,
            connectivity: probe.connectivity,
            clients: probe.clients,
            max_clients: probe.max_clients,
            failures: self.failures,
            idle_timeout: probe.idle_timeout,
            note: self.note.clone(),
        }
    }
}
