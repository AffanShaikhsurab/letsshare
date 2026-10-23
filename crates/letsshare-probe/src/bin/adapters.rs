//! Probe: how many Wi-Fi radios does this machine have?
//!
//! Two radios means we can pin the access point to one and the uplink to the
//! other, which removes single-radio time-sharing entirely — the single biggest
//! speed lever available to this app. One radio means we must time-share and
//! should let the Windows stack choose the band.
//!
//! Run: `cargo run -p letsshare-probe --bin adapters`

use windows::Devices::WiFi::WiFiAdapter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("== Wi-Fi adapter probe ==\n");

    let adapters = WiFiAdapter::FindAllAdaptersAsync()?.join()?;
    let count = adapters.Size()?;
    println!("Wi-Fi adapters found: {count}\n");

    for i in 0..count {
        let adapter = adapters.GetAt(i)?;
        let network_adapter = adapter.NetworkAdapter()?;
        let id = network_adapter.NetworkAdapterId()?;
        println!("  [{i}] adapter id   : {id:?}");
        println!(
            "      inbound max  : {:?}",
            network_adapter.InboundMaxBitsPerSecond()
        );
        println!(
            "      outbound max : {:?}",
            network_adapter.OutboundMaxBitsPerSecond()
        );
    }

    println!();
    if count >= 2 {
        println!("RESULT: {count} radios — Tier 1 (dedicated AP radio) is possible.");
        println!(
            "        Pin the AP to one radio with CreateFromConnectionProfileWithTargetAdapter."
        );
    } else if count == 1 {
        println!("RESULT: 1 radio — Tier 0 only. Station and AP must time-share,");
        println!("        so expect roughly half the negotiated rate and let the");
        println!("        driver choose the band rather than forcing 5 GHz.");
    } else {
        println!("RESULT: no Wi-Fi adapter found.");
    }

    Ok(())
}
