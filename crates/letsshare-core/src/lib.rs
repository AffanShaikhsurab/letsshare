//! Core control surface for LetShare.
//!
//! LetShare does not reimplement Wi-Fi hotspotting. Windows already ships a
//! working AP stack (Wi-Fi Direct virtual adapter + ICS NAT/DHCP + icssvc).
//! What it lacks is a *supervisor*: something that watches the hotspot and
//! repairs it. This crate is that control surface.

pub mod dhcp;
pub mod flight;
pub mod health;
pub mod supervisor;
pub mod tethering;
pub mod tier2;

pub use dhcp::{DhcpConfig, DhcpServer};
pub use flight::{Radio, Recorder, Sample};
pub use health::Connectivity;
pub use supervisor::{Phase, Policy, Snapshot, Supervisor};
pub use tethering::{
    Band, Capability, Client, Hotspot, HotspotConfig, HotspotError, OpStatus, OperState, Profile,
    SessionConfig, StartOutcome,
};

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, HotspotError>;
