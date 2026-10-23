//! WinRT tethering control (`Windows.Networking.NetworkOperators`).
//!
//! These are safe wrappers over `NetworkOperatorTetheringManager`. Every call
//! here was verified against real hardware (Intel AX203 / Win11 26200) to work
//! without administrator elevation.

use windows::core::HSTRING;
use windows::Networking::Connectivity::{
    ConnectionProfile, NetworkConnectivityLevel, NetworkInformation,
};
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringAccessPointConfiguration, NetworkOperatorTetheringManager,
    NetworkOperatorTetheringSessionAccessPointConfiguration, TetheringCapability,
    TetheringOperationStatus, TetheringOperationalState, TetheringWiFiAuthenticationKind,
    TetheringWiFiBand, TetheringWiFiPerformancePriority,
};

/// Errors surfaced by the tethering layer.
#[derive(Debug, thiserror::Error)]
pub enum HotspotError {
    #[error("WinRT call failed: {0}")]
    WinRt(#[from] windows::core::Error),

    #[error("tethering is not available on this machine: {0:?}")]
    Unsupported(Capability),

    #[error("no Wi-Fi connection profile found to tether")]
    NoUplinkProfile,

    #[error("hotspot start returned {0:?}: {1}")]
    StartFailed(String, String),

    #[error("hotspot stop returned {0:?}: {1}")]
    StopFailed(String, String),
}

/// Whether the platform can tether at all, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Enabled,
    DisabledByGroupPolicy,
    DisabledByHardware,
    DisabledByOperator,
    DisabledBySku,
    DisabledByService,
    DisabledByRequiredApp,
    DisabledBySystemCapability,
    Unknown,
}

impl Capability {
    pub fn is_available(self) -> bool {
        matches!(self, Capability::Enabled)
    }

    /// Human-readable reason, suitable for showing in the UI.
    pub fn reason(self) -> &'static str {
        match self {
            Capability::Enabled => "Available",
            Capability::DisabledByGroupPolicy => "Blocked by group policy",
            Capability::DisabledByHardware => "Wi-Fi adapter cannot run an access point",
            Capability::DisabledByOperator => "Blocked by the mobile operator",
            Capability::DisabledBySku => "Not supported by this Windows edition",
            Capability::DisabledByService => "The Mobile Hotspot service is not running",
            Capability::DisabledByRequiredApp => "A required app is not installed",
            Capability::DisabledBySystemCapability => "Blocked by system capability policy",
            Capability::Unknown => "Unknown capability state",
        }
    }
}

impl From<TetheringCapability> for Capability {
    fn from(value: TetheringCapability) -> Self {
        match value {
            TetheringCapability::Enabled => Capability::Enabled,
            TetheringCapability::DisabledByGroupPolicy => Capability::DisabledByGroupPolicy,
            TetheringCapability::DisabledByHardwareLimitation => Capability::DisabledByHardware,
            TetheringCapability::DisabledByOperator => Capability::DisabledByOperator,
            TetheringCapability::DisabledBySku => Capability::DisabledBySku,
            TetheringCapability::DisabledBySystemCapability => {
                Capability::DisabledBySystemCapability
            }
            TetheringCapability::DisabledByRequiredAppNotInstalled => {
                Capability::DisabledByRequiredApp
            }
            _ => Capability::Unknown,
        }
    }
}

/// Operational state of the hotspot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperState {
    Off,
    On,
    InTransition,
    Unknown,
}

impl From<TetheringOperationalState> for OperState {
    fn from(value: TetheringOperationalState) -> Self {
        match value {
            TetheringOperationalState::On => OperState::On,
            TetheringOperationalState::Off => OperState::Off,
            TetheringOperationalState::InTransition => OperState::InTransition,
            _ => OperState::Unknown,
        }
    }
}

/// Radio band for the access point. `Auto` lets the driver choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Band {
    #[default]
    Auto,
    Ghz24,
    Ghz5,
    Ghz6,
}

impl From<Band> for TetheringWiFiBand {
    fn from(value: Band) -> Self {
        match value {
            Band::Auto => TetheringWiFiBand::Auto,
            Band::Ghz24 => TetheringWiFiBand::TwoPointFourGigahertz,
            Band::Ghz5 => TetheringWiFiBand::FiveGigahertz,
            Band::Ghz6 => TetheringWiFiBand::SixGigahertz,
        }
    }
}

impl From<TetheringWiFiBand> for Band {
    fn from(value: TetheringWiFiBand) -> Self {
        match value {
            TetheringWiFiBand::TwoPointFourGigahertz => Band::Ghz24,
            TetheringWiFiBand::FiveGigahertz => Band::Ghz5,
            TetheringWiFiBand::SixGigahertz => Band::Ghz6,
            _ => Band::Auto,
        }
    }
}

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Band::Auto => "Auto",
            Band::Ghz24 => "2.4 GHz",
            Band::Ghz5 => "5 GHz",
            Band::Ghz6 => "6 GHz",
        }
    }
}

/// A saved Wi-Fi profile that could serve as the uplink.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub connected: bool,
    pub wlan: bool,
}

/// Hotspot configuration as stored by Windows.
#[derive(Debug, Clone)]
pub struct HotspotConfig {
    pub ssid: String,
    pub passphrase: String,
    pub band: Band,
    pub auth: String,
}

/// Outcome of a start/stop request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpStatus {
    Success,
    AlreadyOn,
    /// Driver refused the requested band.
    RadioRestriction,
    /// Requested AP band conflicts with the station link (e.g. 6 GHz AP + 5 GHz STA).
    BandInterference,
    /// Uplink is associated but has no real internet (captive portal, no DHCP).
    NetworkLimitedConnectivity,
    /// Reported while the radio is mid-transition. Frequently spurious.
    WifiDeviceOff,
    MobileBroadbandDeviceOff,
    BluetoothDeviceOff,
    OperationInProgress,
    Other(String),
}

impl OpStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, OpStatus::Success | OpStatus::AlreadyOn)
    }

    /// Worth retrying soon rather than counting as a hard failure.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            OpStatus::OperationInProgress | OpStatus::WifiDeviceOff
        )
    }

    pub fn describe(&self) -> String {
        match self {
            OpStatus::Success => "Success".into(),
            OpStatus::AlreadyOn => "Already on".into(),
            OpStatus::RadioRestriction => {
                "The Wi-Fi driver refused this band (RadioRestriction)".into()
            }
            OpStatus::BandInterference => {
                "This band conflicts with the current Wi-Fi link (BandInterference)".into()
            }
            OpStatus::NetworkLimitedConnectivity => "The uplink has no real internet access".into(),
            OpStatus::WifiDeviceOff => "Windows reported the Wi-Fi device as off".into(),
            OpStatus::MobileBroadbandDeviceOff => "Mobile broadband device is off".into(),
            OpStatus::BluetoothDeviceOff => "Bluetooth device is off".into(),
            OpStatus::OperationInProgress => "Another tethering operation is in progress".into(),
            OpStatus::Other(s) => format!("Unexpected result: {s}"),
        }
    }
}

impl From<TetheringOperationStatus> for OpStatus {
    fn from(value: TetheringOperationStatus) -> Self {
        match value {
            TetheringOperationStatus::Success => OpStatus::Success,
            TetheringOperationStatus::AlreadyOn => OpStatus::AlreadyOn,
            TetheringOperationStatus::RadioRestriction => OpStatus::RadioRestriction,
            TetheringOperationStatus::BandInterference => OpStatus::BandInterference,
            TetheringOperationStatus::NetworkLimitedConnectivity => {
                OpStatus::NetworkLimitedConnectivity
            }
            TetheringOperationStatus::WiFiDeviceOff => OpStatus::WifiDeviceOff,
            TetheringOperationStatus::MobileBroadbandDeviceOff => {
                OpStatus::MobileBroadbandDeviceOff
            }
            TetheringOperationStatus::BluetoothDeviceOff => OpStatus::BluetoothDeviceOff,
            TetheringOperationStatus::OperationInProgress => OpStatus::OperationInProgress,
            other => OpStatus::Other(format!("{other:?}")),
        }
    }
}

/// Per-session AP settings. Applied with `StartTetheringAsync2`, which means
/// the user's saved Windows hotspot configuration is never mutated.
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub ssid: Option<String>,
    pub passphrase: Option<String>,
    pub band: Band,
    /// Ask the driver to favour the AP over the station link.
    pub ap_first: bool,
    /// Prefer WPA3 where the driver allows it.
    pub wpa3: bool,
}

/// A device currently associated with the hotspot.
#[derive(Debug, Clone)]
pub struct Client {
    pub mac: String,
    pub hostnames: Vec<String>,
}

/// Outcome of a start/stop request.
#[derive(Debug, Clone)]
pub struct StartOutcome {
    pub status: OpStatus,
    pub message: String,
}

fn enum_str<T: std::fmt::Debug>(value: T) -> String {
    format!("{value:?}")
}

/// Enumerate connection profiles, most useful uplink first.
pub fn profiles() -> Result<Vec<Profile>, HotspotError> {
    let list = NetworkInformation::GetConnectionProfiles()?;
    let mut out = Vec::with_capacity(list.Size()? as usize);

    for i in 0..list.Size()? {
        let p: ConnectionProfile = list.GetAt(i)?;
        let name = p.ProfileName()?.to_string();
        let wlan = p.IsWlanConnectionProfile()?;
        let connected = matches!(
            p.GetNetworkConnectivityLevel()?,
            NetworkConnectivityLevel::InternetAccess
                | NetworkConnectivityLevel::ConstrainedInternetAccess
        );
        out.push(Profile {
            name,
            connected,
            wlan,
        });
    }

    // Prefer a WLAN profile that actually has internet.
    out.sort_by_key(|p| (!(p.wlan && p.connected), !p.connected, p.name.clone()));
    Ok(out)
}

/// Pick the best uplink profile: connected WLAN first, then any connected.
pub fn best_uplink() -> Result<Profile, HotspotError> {
    profiles()?
        .into_iter()
        .find(|p| p.wlan && p.connected)
        .ok_or(HotspotError::NoUplinkProfile)
}

/// Handle to the system tethering manager, bound to one uplink profile.
pub struct Hotspot {
    inner: NetworkOperatorTetheringManager,
    uplink: String,
}

impl Hotspot {
    /// Bind to a specific uplink profile by name.
    pub fn for_profile(name: &str) -> Result<Self, HotspotError> {
        let list = NetworkInformation::GetConnectionProfiles()?;
        let wanted = HSTRING::from(name);

        for i in 0..list.Size()? {
            let p: ConnectionProfile = list.GetAt(i)?;
            if p.ProfileName()? == wanted {
                let inner = NetworkOperatorTetheringManager::CreateFromConnectionProfile(&p)?;
                return Ok(Self {
                    inner,
                    uplink: name.to_string(),
                });
            }
        }
        Err(HotspotError::NoUplinkProfile)
    }

    /// Bind to the best available uplink.
    pub fn auto() -> Result<Self, HotspotError> {
        let uplink = best_uplink()?;
        Self::for_profile(&uplink.name)
    }

    pub fn uplink(&self) -> &str {
        &self.uplink
    }

    /// Static capability probe, before any manager exists.
    pub fn capability() -> Result<Capability, HotspotError> {
        let uplink = best_uplink()?;
        Self::capability_for(&uplink.name)
    }

    pub fn capability_for(name: &str) -> Result<Capability, HotspotError> {
        let list = NetworkInformation::GetConnectionProfiles()?;
        let wanted = HSTRING::from(name);

        for i in 0..list.Size()? {
            let p: ConnectionProfile = list.GetAt(i)?;
            if p.ProfileName()? == wanted {
                let cap =
                    NetworkOperatorTetheringManager::GetTetheringCapabilityFromConnectionProfile(
                        &p,
                    )?;
                return Ok(cap.into());
            }
        }
        Err(HotspotError::NoUplinkProfile)
    }

    pub fn state(&self) -> Result<OperState, HotspotError> {
        Ok(self.inner.TetheringOperationalState()?.into())
    }

    pub fn client_count(&self) -> Result<u32, HotspotError> {
        Ok(self.inner.ClientCount()?)
    }

    pub fn max_clients(&self) -> Result<u32, HotspotError> {
        Ok(self.inner.MaxClientCount()?)
    }

    pub fn config(&self) -> Result<HotspotConfig, HotspotError> {
        let cfg: NetworkOperatorTetheringAccessPointConfiguration =
            self.inner.GetCurrentAccessPointConfiguration()?;
        Ok(HotspotConfig {
            ssid: cfg.Ssid()?.to_string(),
            passphrase: cfg.Passphrase()?.to_string(),
            band: cfg.Band()?.into(),
            auth: enum_str(cfg.AuthenticationKind()?),
        })
    }

    /// Apply SSID / passphrase / band. Takes effect on the next start, or
    /// immediately if the hotspot is already running (Windows will bounce it).
    pub fn configure(&self, ssid: &str, passphrase: &str, band: Band) -> Result<(), HotspotError> {
        let cfg = NetworkOperatorTetheringAccessPointConfiguration::new()?;
        cfg.SetSsid(&HSTRING::from(ssid))?;
        cfg.SetPassphrase(&HSTRING::from(passphrase))?;
        if cfg.IsBandSupported(band.into())? {
            cfg.SetBand(band.into())?;
        }
        self.inner.ConfigureAccessPointAsync(&cfg)?.join()?;
        Ok(())
    }

    /// Which bands the driver will accept for the AP.
    pub fn supported_bands(&self) -> Result<Vec<Band>, HotspotError> {
        let cfg = self.inner.GetCurrentAccessPointConfiguration()?;
        let mut out = Vec::new();
        for b in [Band::Ghz24, Band::Ghz5, Band::Ghz6] {
            if cfg.IsBandSupported(b.into())? {
                out.push(b);
            }
        }
        Ok(out)
    }

    /// True when Windows will tear the hotspot down 5 minutes after the last
    /// client leaves. This is the single most common "it turned itself off".
    pub fn idle_timeout_enabled(&self) -> Result<bool, HotspotError> {
        Ok(NetworkOperatorTetheringManager::IsNoConnectionsTimeoutEnabled()?)
    }

    /// Kill the idle teardown. Idempotent.
    pub fn disable_idle_timeout(&self) -> Result<(), HotspotError> {
        if self.idle_timeout_enabled()? {
            NetworkOperatorTetheringManager::DisableNoConnectionsTimeout()?;
            tracing::info!("disabled no-connections idle timeout");
        }
        Ok(())
    }

    pub fn start_with(&self, session: &SessionConfig) -> Result<StartOutcome, HotspotError> {
        match self.start_session(session) {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                // Verified on hardware: the session-config path can be refused
                // outright (reported as WiFiDeviceOff) while the baseline call
                // starts the AP fine. Never lose the start over a nicer API.
                tracing::warn!(error = %e, "session-config start failed, using baseline start");
                let mut outcome = self.start()?;
                outcome.message = format!(
                    "{} (session config rejected, used baseline start)",
                    outcome.message
                );
                Ok(outcome)
            }
        }
    }

    fn start_session(&self, session: &SessionConfig) -> Result<StartOutcome, HotspotError> {
        let cfg = NetworkOperatorTetheringSessionAccessPointConfiguration::new()?;

        if let Some(ssid) = &session.ssid {
            cfg.SetSsid(&HSTRING::from(ssid.as_str()))?;
        }
        if let Some(pass) = &session.passphrase {
            cfg.SetPassphrase(&HSTRING::from(pass.as_str()))?;
        }

        let band: TetheringWiFiBand = session.band.into();
        if cfg.IsBandSupported(band)? {
            cfg.SetBand(band)?;
        } else {
            tracing::warn!(
                band = session.band.label(),
                "band not supported, letting driver choose"
            );
        }

        if session.wpa3 {
            let kind = TetheringWiFiAuthenticationKind::Wpa3;
            if cfg.IsAuthenticationKindSupported(kind)? {
                cfg.SetAuthenticationKind(kind)?;
            }
        }

        if session.ap_first {
            cfg.SetPerformancePriority(TetheringWiFiPerformancePriority::TetheringOverStation)?;
        }

        let result = self
            .inner
            .StartTetheringAsync2(&cfg)
            .and_then(|op| op.join());
        match result {
            Ok(result) => self.settle(result, 1_000),
            Err(e) => Err(HotspotError::StartFailed(
                "session start call failed".into(),
                e.to_string(),
            )),
        }
    }

    /// Wait briefly for the radio to settle, then report what is actually true.
    ///
    /// Verified on hardware: `StartTetheringAsync` can return `WiFiDeviceOff`
    /// while the hotspot is in fact already up. The operation result is not
    /// trustworthy mid-transition, so we ask the OS instead of believing it.
    fn settle(
        &self,
        result: windows::Networking::NetworkOperators::NetworkOperatorTetheringOperationResult,
        wait_ms: u64,
    ) -> Result<StartOutcome, HotspotError> {
        let status: OpStatus = result.Status()?.into();
        let message = result.AdditionalErrorMessage()?.to_string();

        if status.is_ok() {
            return Ok(StartOutcome { status, message });
        }

        let attempts = (wait_ms / 100).max(1);
        for _ in 0..attempts {
            if matches!(self.state(), Ok(OperState::On)) {
                tracing::warn!(reported = ?status, "start reported failure but state is On");
                return Ok(StartOutcome {
                    status: OpStatus::Success,
                    message: format!("{message} (reported {status:?}, verified On)"),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Err(HotspotError::StartFailed(status.describe(), message))
    }

    /// Devices currently associated with the AP.
    pub fn clients(&self) -> Result<Vec<Client>, HotspotError> {
        let list = self.inner.GetTetheringClients()?;
        let mut out = Vec::new();
        for i in 0..list.Size()? {
            let c = list.GetAt(i)?;
            let hostnames = match c.HostNames() {
                Ok(names) => (0..names.Size().unwrap_or(0))
                    .into_iter()
                    .filter_map(|j| names.GetAt(j).ok())
                    .filter_map(|h| h.DisplayName().ok())
                    .map(|h| h.to_string())
                    .collect(),
                Err(_) => Vec::new(),
            };
            out.push(Client {
                mac: c.MacAddress().map(|m| m.to_string()).unwrap_or_default(),
                hostnames,
            });
        }
        Ok(out)
    }

    pub fn start(&self) -> Result<StartOutcome, HotspotError> {
        let result = self.inner.StartTetheringAsync()?.join()?;
        self.settle(result, 3_000)
    }

    pub fn stop(&self) -> Result<StartOutcome, HotspotError> {
        let result = self.inner.StopTetheringAsync()?.join()?;
        let status: OpStatus = result.Status()?.into();
        let message = result.AdditionalErrorMessage()?.to_string();

        if status.is_ok() {
            return Ok(StartOutcome { status, message });
        }

        // Same lesson as start: verify, do not trust the result code.
        for _ in 0..30 {
            if matches!(self.state(), Ok(OperState::Off)) {
                return Ok(StartOutcome {
                    status: OpStatus::Success,
                    message: format!("{message} (reported {status:?}, verified Off)"),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Err(HotspotError::StopFailed(status.describe(), message))
    }
}
