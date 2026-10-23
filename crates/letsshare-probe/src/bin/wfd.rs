//! Probe: can this machine run a bare Wi-Fi Direct *legacy* access point?
//!
//! This is the prerequisite for owning the IP layer. If the legacy AP starts,
//! we can supply our own DHCP/DNS/NAT and drop ICS entirely. If it does not,
//! ICS is not optional and the architecture must work around it.
//!
//! Run: `cargo run -p letsshare-probe --bin wfd`
//! Reversible: the AP is stopped before exit.

use std::time::Duration;
use windows::core::HSTRING;
use windows::Devices::WiFiDirect::{
    WiFiDirectAdvertisementListenStateDiscoverability, WiFiDirectAdvertisementPublisher,
    WiFiDirectAdvertisementPublisherStatus,
};
use windows::Security::Credentials::PasswordCredential;

fn status(p: &WiFiDirectAdvertisementPublisher) -> String {
    match p.Status() {
        Ok(s) if s == WiFiDirectAdvertisementPublisherStatus::Started => "started".into(),
        Ok(s) if s == WiFiDirectAdvertisementPublisherStatus::Aborted => "ABORTED".into(),
        Ok(s) if s == WiFiDirectAdvertisementPublisherStatus::Stopped => "stopped".into(),
        Ok(s) if s == WiFiDirectAdvertisementPublisherStatus::Created => "created".into(),
        Ok(s) => format!("{s:?}"),
        Err(e) => format!("status error: {e}"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("== Wi-Fi Direct legacy AP probe ==\n");

    let args: Vec<String> = std::env::args().collect();
    let hold = args.iter().any(|a| a == "--hold");
    let ssid = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("LETSSHARE_SSID").ok())
        .unwrap_or_else(|| "LetShare-WFD-Test".to_string());
    let passphrase_text = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .nth(1)
        .cloned()
        .or_else(|| std::env::var("LETSSHARE_PASSPHRASE").ok())
        .unwrap_or_else(|| letsshare_core::tier2::DEFAULT_PASSPHRASE.to_string());

    if passphrase_text == letsshare_core::tier2::DEFAULT_PASSPHRASE {
        eprintln!("warning: using the published default Wi-Fi passphrase");
    }

    let publisher = WiFiDirectAdvertisementPublisher::new()?;
    let advertisement = publisher.Advertisement()?;
    let legacy = advertisement.LegacySettings()?;

    legacy.SetIsEnabled(true)?;
    legacy.SetSsid(&HSTRING::from(ssid.as_str()))?;

    let passphrase = PasswordCredential::new()?;
    passphrase.SetPassword(&HSTRING::from(passphrase_text.as_str()))?;
    legacy.SetPassphrase(&passphrase)?;

    advertisement.SetIsAutonomousGroupOwnerEnabled(true)?;
    advertisement
        .SetListenStateDiscoverability(WiFiDirectAdvertisementListenStateDiscoverability::Normal)?;

    println!("legacy AP enabled, autonomous GO on");
    println!("starting...");
    publisher.Start()?;
    println!("status immediately : {}", status(&publisher));

    for i in 1..=3 {
        std::thread::sleep(Duration::from_secs(2));
        println!("status after {}s    : {}", i * 2, status(&publisher));
    }

    if status(&publisher) == "started" {
        println!("\nRESULT: legacy AP is available on this hardware.");
        if hold {
            println!("holding the AP up; press ctrl-c to stop\n");
            loop {
                std::thread::sleep(Duration::from_secs(10));
                let s = status(&publisher);
                println!("status: {s}");
                // If the AP aborts (radio taken, Mobile Hotspot started, resume
                // glitch) exit non-zero so the supervisor restarts us.
                if s != "started" {
                    eprintln!("AP is no longer started ({s}); exiting for supervisor restart");
                    std::process::exit(1);
                }
            }
        }
        println!("Check with: netsh wlan show interfaces  (look for a second interface)");
        std::thread::sleep(Duration::from_secs(4));
    } else {
        println!("\nRESULT: legacy AP did not come up (see status above).");
    }

    publisher.Stop()?;
    std::thread::sleep(Duration::from_millis(500));
    println!("final status       : {}", status(&publisher));
    Ok(())
}
