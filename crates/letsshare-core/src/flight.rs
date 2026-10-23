//! Flight recorder.
//!
//! An intermittent failure is only fixable once it leaves evidence. This module
//! appends one CSV row per sample — radio quality, hotspot state, client count,
//! and whether the IP layer is actually serving — so a failure that happens once
//! an hour can be reconstructed afterwards instead of guessed at.

use crate::health;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::tethering::{Hotspot, OperState};

/// Wi-Fi link quality of the *uplink*, parsed from `netsh`.
#[derive(Debug, Clone, Default)]
pub struct Radio {
    pub signal_pct: Option<u32>,
    pub rssi: Option<i32>,
    pub band: String,
    pub channel: Option<u32>,
    pub rx_mbps: Option<f64>,
    pub tx_mbps: Option<f64>,
}

/// What the access point actually came up on, learned by scanning for our own
/// SSID. The tethering API reports the *configured* band, not the operating
/// one, so this is the only way to know the truth.
#[derive(Debug, Clone, Default)]
pub struct ApRadio {
    pub band: String,
    pub channel: Option<u32>,
    pub signal_pct: Option<u32>,
}

/// Find our own SSID in a scan and report the band/channel it is beaconing on.
pub fn read_ap_radio(ssid: &str) -> ApRadio {
    let mut out = ApRadio::default();
    let Ok(scan) = Command::new("netsh")
        .args(["wlan", "show", "networks", "mode=bssid"])
        .output()
    else {
        return out;
    };

    let text = String::from_utf8_lossy(&scan.stdout);
    let mut in_target = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("SSID ") {
            // "SSID 1 : Name"
            let name = rest.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
            in_target = name == ssid;
            continue;
        }
        if !in_target {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            match key.trim() {
                "Band" => out.band = value.trim().to_string(),
                "Channel" => out.channel = value.trim().parse().ok(),
                "Signal" => out.signal_pct = value.trim().trim_end_matches('%').parse().ok(),
                _ => {}
            }
        }
    }
    out
}

/// IPv4 neighbours on the ICS subnet: proof that a client actually got an
/// address and ARP'd, not merely associated.
pub fn ap_neighbours() -> Vec<String> {
    let Ok(out) = Command::new("arp").arg("-a").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let ip = parts.next()?;
            let mac = parts.next()?;
            if ip.starts_with("192.168.137.") && ip != "192.168.137.1" && ip != "192.168.137.255" {
                Some(format!("{ip}={mac}"))
            } else {
                None
            }
        })
        .collect()
}

/// Read the connected interface's link quality.
///
/// `netsh` is the pragmatic source here: it is always present, needs no
/// elevation, and reports the RSSI/rate/band trio that `WlanQueryInterface`
/// would otherwise need a chunk of unsafe code to reach.
pub fn read_radio() -> Radio {
    let mut radio = Radio::default();
    let Ok(out) = Command::new("netsh")
        .args(["wlan", "show", "interfaces"])
        .output()
    else {
        return radio;
    };

    let mut connected = false;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        if key == "State" {
            connected = value.eq_ignore_ascii_case("connected");
            continue;
        }
        if !connected {
            continue;
        }

        match key {
            "Signal" => radio.signal_pct = value.trim_end_matches('%').trim().parse().ok(),
            "Rssi" => radio.rssi = value.parse().ok(),
            "Band" => radio.band = value.to_string(),
            "Channel" => radio.channel = value.parse().ok(),
            "Receive rate (Mbps)" => radio.rx_mbps = value.parse().ok(),
            "Transmit rate (Mbps)" => radio.tx_mbps = value.parse().ok(),
            _ => {}
        }
    }
    radio
}

/// One observation.
#[derive(Debug, Clone)]
pub struct Sample {
    pub ts: u64,
    pub phase: String,
    pub state: String,
    pub clients: u32,
    pub gateway: bool,
    pub dhcp: bool,
    pub net: String,
    pub radio: Radio,
    /// Band/channel the AP is really beaconing on.
    pub ap: ApRadio,
    /// IPv4 neighbours that actually obtained an address.
    pub ap_clients: Vec<String>,
}

impl Sample {
    /// Gather one sample. Never fails: a broken probe is recorded as unknown.
    pub fn capture(hs: Option<&Hotspot>, phase: &str, ssid: Option<&str>) -> Self {
        let (state, clients) = match hs {
            Some(h) => (
                format!("{:?}", h.state().unwrap_or(OperState::Unknown)),
                h.client_count().unwrap_or(0),
            ),
            None => ("Unknown".to_string(), 0),
        };
        let gateway = health::ics_gateway_present();
        let dhcp = if gateway && clients == 0 {
            health::ics_dhcp_listening()
        } else {
            true
        };

        Self {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            phase: phase.to_string(),
            state,
            clients,
            gateway,
            dhcp,
            net: format!(
                "{:?}",
                health::classify(std::time::Duration::from_millis(800))
            ),
            radio: read_radio(),
            ap: ssid.map(read_ap_radio).unwrap_or_default(),
            ap_clients: ap_neighbours(),
        }
    }

    fn csv(&self) -> String {
        let mut s = String::with_capacity(200);
        let _ = write!(
            s,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            self.ts,
            self.phase,
            self.state,
            self.clients,
            self.gateway as u8,
            self.dhcp as u8,
            self.net,
            opt(self.radio.signal_pct),
            opt(self.radio.rssi),
            if self.radio.band.is_empty() {
                "-"
            } else {
                &self.radio.band
            },
            opt(self.radio.channel),
            opt_f(self.radio.rx_mbps),
            opt_f(self.radio.tx_mbps),
            if self.ap.band.is_empty() {
                "-"
            } else {
                &self.ap.band
            },
            opt(self.ap.channel),
            if self.ap_clients.is_empty() {
                "-".to_string()
            } else {
                self.ap_clients.join("|")
            },
        );
        s
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

fn opt_f(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "-".into())
}

/// Append-only CSV writer.
pub struct Recorder {
    path: PathBuf,
}

impl Recorder {
    /// Create a recorder in `dir`, writing a header immediately.
    pub fn create(dir: &std::path::Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("soak-{ts}.csv"));
        std::fs::write(
            &path,
            "ts,phase,state,clients,gateway,dhcp,net,signal,rssi,band,channel,rx_mbps,tx_mbps,ap_band,ap_channel,ap_clients\n",
        )?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn append(&self, sample: &Sample) -> std::io::Result<()> {
        let mut f = OpenOptions::new().append(true).open(&self.path)?;
        writeln!(f, "{}", sample.csv())
    }
}
