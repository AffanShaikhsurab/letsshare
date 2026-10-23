//! Hardware probe / control CLI for LetShare.
//!
//! ```text
//! cargo run -p letsshare-probe            # report
//! cargo run -p letsshare-probe -- start   # bring the hotspot up
//! cargo run -p letsshare-probe -- stop    # bring it down
//! ```

use letsshare_core::{Band, Capability, Hotspot, Policy, Recorder, Sample, Supervisor};
use std::env;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cmd = env::args().nth(1).unwrap_or_else(|| "report".into());

    match cmd.as_str() {
        "start" => start(),
        "stop" => stop(),
        "band" => band(env::args().nth(2)),
        "tune" => tune(),
        "watch" => watch(),
        "soak" => soak(),
        "demo" => demo(),
        "diagnose" => diagnose(),
        _ => report(),
    }
}

fn report() -> Result<(), Box<dyn std::error::Error>> {
    println!("== LetShare hardware probe ==\n");

    println!("-- connection profiles --");
    for p in letsshare_core::tethering::profiles()? {
        println!(
            "  {:<32} {:>6}  {}",
            p.name,
            if p.wlan { "wlan" } else { "other" },
            if p.connected { "connected" } else { "-" }
        );
    }

    let cap = Hotspot::capability()?;
    println!("\n-- capability --\n  {:?} ({})", cap, cap.reason());
    if cap != Capability::Enabled {
        return Ok(());
    }

    let hs = Hotspot::auto()?;
    println!("\n-- uplink --\n  {}", hs.uplink());
    println!("\n-- current state --");
    println!("  oper      : {:?}", hs.state()?);
    println!("  clients   : {}/{}", hs.client_count()?, hs.max_clients()?);

    let cfg = hs.config()?;
    println!("\n-- access point config --");
    println!("  ssid      : {}", cfg.ssid);
    println!("  band      : {}", cfg.band.label());
    println!(
        "  passphrase: {}",
        if cfg.passphrase.is_empty() {
            "<empty>"
        } else {
            "<set>"
        }
    );

    Ok(())
}

fn start() -> Result<(), Box<dyn std::error::Error>> {
    let hs = Hotspot::auto()?;
    println!("uplink : {}", hs.uplink());
    println!("before : {:?}", hs.state()?);

    let outcome = hs.start()?;
    println!("start  : {:?} {}", outcome.status, outcome.message);

    // Verify: state must actually be On, not just "Success".
    let state = hs.state()?;
    println!(
        "after  : {:?}  (clients {}/{})",
        state,
        hs.client_count()?,
        hs.max_clients()?
    );
    Ok(())
}

fn stop() -> Result<(), Box<dyn std::error::Error>> {
    let hs = Hotspot::auto()?;
    let outcome = hs.stop()?;
    println!("stop   : {:?} {}", outcome.status, outcome.message);
    println!("after  : {:?}", hs.state()?);
    Ok(())
}

fn band(arg: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let target = match arg.as_deref() {
        Some("2.4") => Band::Ghz24,
        Some("5") => Band::Ghz5,
        Some("6") => Band::Ghz6,
        _ => Band::Auto,
    };
    let hs = Hotspot::auto()?;
    let cfg = hs.config()?;
    hs.configure(&cfg.ssid, &cfg.passphrase, target)?;
    println!("band set to {}", target.label());
    Ok(())
}

fn tune() -> Result<(), Box<dyn std::error::Error>> {
    let hs = Hotspot::auto()?;
    println!("idle timeout was enabled: {}", hs.idle_timeout_enabled()?);
    hs.disable_idle_timeout()?;
    println!("idle timeout now       : {}", hs.idle_timeout_enabled()?);

    let bands: Vec<_> = hs.supported_bands()?.iter().map(|b| b.label()).collect();
    println!("supported AP bands     : {}", bands.join(", "));
    println!("max clients            : {}", hs.max_clients()?);
    Ok(())
}

/// Write one file that explains what this machine is doing.
fn diagnose() -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;
    use std::process::Command;

    let mut out = String::new();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let _ = writeln!(out, "LetShare diagnose bundle  (unix ts {ts})\n");

    let _ = writeln!(out, "== LetShare view ==");
    if let Ok(hs) = Hotspot::auto() {
        let _ = writeln!(out, "uplink      : {}", hs.uplink());
        let _ = writeln!(out, "state       : {:?}", hs.state());
        let _ = writeln!(
            out,
            "clients     : {}/{}",
            hs.client_count()?,
            hs.max_clients()?
        );
        let _ = writeln!(out, "idle timeout: {}", hs.idle_timeout_enabled()?);
        if let Ok(cfg) = hs.config() {
            let _ = writeln!(out, "ssid        : {}", cfg.ssid);
            let _ = writeln!(out, "band        : {}", cfg.band.label());
        }
        let bands: Vec<_> = hs
            .supported_bands()
            .unwrap_or_default()
            .iter()
            .map(|b| b.label())
            .collect();
        let _ = writeln!(out, "ap bands    : {}", bands.join(", "));
    } else {
        let _ = writeln!(out, "no uplink available");
    }
    let _ = writeln!(
        out,
        "gateway up  : {}",
        letsshare_core::health::ics_gateway_present()
    );
    let _ = writeln!(
        out,
        "connectivity: {:?}",
        letsshare_core::health::classify(Duration::from_millis(900))
    );

    // Raw platform dumps. Deliberately no SSID/passphrase in ours; netsh does
    // include profile names, which is acceptable for a local file.
    let dumps: &[(&str, &[&str])] = &[
        ("wlan interfaces", &["wlan", "show", "interfaces"]),
        ("wlan drivers", &["wlan", "show", "drivers"]),
        ("wlan networks", &["wlan", "show", "networks", "mode=bssid"]),
        ("ipconfig", &["int", "ip", "show", "config"]),
    ];

    for (title, args) in dumps {
        let _ = writeln!(out, "\n== {title} ==");
        match Command::new("netsh").args(*args).output() {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                let _ = writeln!(out, "{text}");
            }
            Err(e) => {
                let _ = writeln!(out, "<failed: {e}>");
            }
        }
    }

    let path = std::env::temp_dir().join(format!("letsshare-diagnose-{ts}.txt"));
    std::fs::write(&path, out)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Live test: bring up a known hotspot and record everything a phone does.
fn demo() -> Result<(), Box<dyn std::error::Error>> {
    const SSID: &str = "LetShare-Test";
    const PASS: &str = "letsshare123";

    let hs = Hotspot::auto()?;
    // Persist credentials so the baseline start path matches the session path.
    hs.configure(SSID, PASS, Band::Auto)?;

    println!("==============================================");
    println!("  SSID     : {SSID}");
    println!("  password : {PASS}");
    println!("  band     : Auto (let Windows choose)");
    println!("  uplink   : {}", hs.uplink());
    println!("==============================================\n");
    println!("Connect your phone now. Leave it connected.\n");

    run_session(0, Some(SSID.to_string()), Some(PASS.to_string()))
}

/// Long-running flight recorder: leaves evidence for intermittent failures.
///
/// `soak` runs forever, `soak 90` runs for 90 minutes. Reproduce the failure
/// with your phone, then send the CSV.
fn soak() -> Result<(), Box<dyn std::error::Error>> {
    let minutes: u64 = env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    run_session(minutes, None, None)
}

fn run_session(
    minutes: u64,
    ssid: Option<String>,
    passphrase: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = soak_dir();
    let rec = Recorder::create(&dir)?;
    let deadline = if minutes > 0 {
        Some(Instant::now() + Duration::from_secs(minutes * 60))
    } else {
        None
    };

    println!("LetShare flight recorder");
    println!("  file    : {}", rec.path().display());
    println!("  cadence : 3s");
    println!(
        "  end     : {}",
        if minutes > 0 { "see below" } else { "ctrl-c" }
    );
    if minutes > 0 {
        println!("  minutes : {minutes}");
    }
    println!("\nReproduce the failure with your phone. A line prints whenever");
    println!("any signal changes. Send the CSV when it breaks.\n");

    let mut sup = Supervisor::new(Policy {
        ssid: ssid.clone(),
        passphrase,
        poll: Duration::from_secs(3),
        // Keep AP-first off during diagnosis so we do not confound the radio.
        ap_first: false,
        ..Policy::default()
    });
    sup.turn_on();

    let mut last = String::new();
    let t0 = Instant::now();
    loop {
        let snap = sup.tick();
        let sample = Sample::capture(sup.hotspot(), snap.phase.label(), ssid.as_deref());
        let _ = rec.append(&sample);

        let sig = format!(
            "{:?}|{}|{}|{}|{:?}|{}|{}|{}|{}",
            snap.state,
            snap.clients.len(),
            snap.gateway_present as u8,
            snap.dhcp_listening as u8,
            snap.connectivity,
            if sample.ap.band.is_empty() {
                "-"
            } else {
                &sample.ap.band
            },
            sample
                .ap
                .channel
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            sample.ap_clients.len(),
            sample
                .radio
                .rssi
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".into())
        );

        if sig != last {
            println!(
                "[{:>5}s] {:?} clients={} gw={} dhcp={} net={:?} | AP band={} ch={} leases={} | uplink rssi={} rate={} :: {}",
                t0.elapsed().as_secs(),
                snap.state,
                snap.clients.len(),
                if snap.gateway_present { "up" } else { "MISSING" },
                if snap.dhcp_listening { "serving" } else { "DEAD" },
                snap.connectivity,
                if sample.ap.band.is_empty() { "?" } else { &sample.ap.band },
                sample
                    .ap
                    .channel
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                sample.ap_clients.len(),
                sample
                    .radio
                    .rssi
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "?".into()),
                sample
                    .radio
                    .tx_mbps
                    .map(|r| format!("{r:.0}M"))
                    .unwrap_or_else(|| "?".into()),
                snap.note
            );
            last = sig;
        }

        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }

    println!("\nwrote {}", rec.path().display());
    Ok(())
}

fn soak_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|p| std::path::PathBuf::from(p).join("LetsShare").join("logs"))
        .unwrap_or_else(std::env::temp_dir)
}

/// Run the supervisor and print a status line whenever something changes.
fn watch() -> Result<(), Box<dyn std::error::Error>> {
    let mut sup = Supervisor::new(Policy {
        poll: Duration::from_secs(3),
        ..Policy::default()
    });
    sup.turn_on();
    println!("supervising (ctrl-c to stop)\n");

    let mut last = String::new();
    loop {
        let snap = sup.tick();
        let line = format!(
            "[{:<10}] {:?} uplink={} gw={} net={:?} clients={}/{} fails={} :: {}",
            snap.phase.label(),
            snap.state,
            if snap.uplink.is_empty() {
                "-"
            } else {
                &snap.uplink
            },
            if snap.gateway_present {
                "up"
            } else {
                "MISSING"
            },
            snap.connectivity,
            snap.clients.len(),
            snap.max_clients,
            snap.failures,
            snap.note
        );
        if line != last {
            println!("{line}");
            last = line;
        }
        std::thread::sleep(sup.policy().poll);
    }
}
