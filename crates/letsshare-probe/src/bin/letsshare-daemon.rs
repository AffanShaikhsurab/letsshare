//! Windowless supervisor entry point for the logon task.
//!
//! The console binary (`letsshare-tier2`) is for humans. This variant allocates
//! no console, so the scheduled task can run it without a window appearing.
//! All reporting goes to `%TEMP%\letsshare-supervisor.log`, with the child
//! processes logging to `%TEMP%\letsshare-<component>.log`.

#![windows_subsystem = "windows"]

use letsshare_core::tier2::{Config, Stack};

fn main() {
    // A windowless process has nowhere to print a panic. Record it.
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write as _;
        let path = letsshare_core::tier2::supervisor_log();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "panic: {info}");
        }
    }));

    let cfg = Config::from_current_exe();
    let mut stack = Stack::new(cfg);
    stack.run();
}
