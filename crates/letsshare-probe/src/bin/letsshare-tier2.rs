//! letsshare-tier2: supervised owned-stack runtime.
//!
//! ```text
//! letsshare-tier2 run     # supervisor (used by the logon task)
//! letsshare-tier2 status  # one-shot health report
//! letsshare-tier2 down    # stop children and remove NAT/address
//! ```

use letsshare_core::tier2::{Config, Stack};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "status".into());

    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .try_init();

    let cfg = Config::from_current_exe();
    let mut stack = Stack::new(cfg);

    match cmd.as_str() {
        "run" => {
            stack.run();
            Ok(())
        }
        "down" => {
            stack.down();
            println!("stopped");
            Ok(())
        }
        "restart" => {
            // Ask the running supervisor (which is elevated) to restart a
            // component. We cannot signal it directly, so we drop a file.
            let what = std::env::args().nth(2).unwrap_or_else(|| "all".into());
            let path = letsshare_core::tier2::control_file();
            std::fs::write(&path, format!("restart {what}"))?;
            println!("sent: restart {what}");
            Ok(())
        }
        "logs" => {
            let path = letsshare_core::tier2::supervisor_log();
            match std::fs::read_to_string(&path) {
                Ok(s) => {
                    for line in s.lines().rev().take(20).collect::<Vec<_>>().iter().rev() {
                        println!("{line}");
                    }
                    Ok(())
                }
                Err(e) => {
                    eprintln!("no log at {}: {e}", path.display());
                    Ok(())
                }
            }
        }
        "status" => {
            let s = stack.status();
            println!("{}", s.render());
            println!("healthy: {}", s.healthy());
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: letsshare-tier2 [run|status|down]");
            std::process::exit(2);
        }
    }
}
