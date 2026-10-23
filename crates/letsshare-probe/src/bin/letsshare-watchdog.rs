//! Windowless watchdog for the `LetShare` logon task.
//!
//! Task Scheduler gives up on a task after its restart budget is spent, so the
//! task alone cannot be trusted to stay up. This binary is registered as a
//! second task ("LetShare-Watchdog") that ticks every few minutes and repairs
//! the one failure the main task cannot: the task object is `Running` while no
//! `letsshare-daemon` process exists (a crash that the restart logic did not
//! pick up, or a budget that ran out).
//!
//! It is a no-op whenever the daemon is alive, so it is safe to run alongside a
//! healthy stack. `schtasks` is used directly, never PowerShell, and every
//! spawn is console-suppressed: this process has no window to show anything in.
//! Actions are recorded in `%TEMP%\letsshare-watchdog.log`; a healthy tick
//! writes nothing at all.
//!
//! Killing a parent on Windows does not kill its children. A daemon that died
//! therefore leaves `wfd`, `dhcpd` and `dnsrescue` alive, still holding the AP
//! and ports 67/53. A freshly started daemon would then spawn duplicates that
//! cannot bind and would churn forever. So before starting the task, the
//! watchdog clears those orphans; the stack is already down at that point.

#![windows_subsystem = "windows"]

use std::io::Write as _;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Same reasoning as the supervisor: a windowless process spawning a console
/// program would flash a new console window onto the desktop.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The logon task that owns the stack.
const TASK_NAME: &str = "LetShare";

/// Image name the task's action runs (`letsshare-daemon.exe run`).
const DAEMON_IMAGE: &str = "letsshare-daemon.exe";

/// A manually started supervisor owns the stack too; never fight it.
const CLI_IMAGE: &str = "letsshare-tier2.exe";

/// Children of the daemon. Orphaned when it dies, so they must go first.
const HELPERS: [&str; 3] = ["wfd.exe", "dhcpd.exe", "dnsrescue.exe"];

/// How long to wait for the daemon process to appear after starting the task.
const START_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    // A windowless process has nowhere to print a panic. Record it.
    std::panic::set_hook(Box::new(|info| {
        log(&format!("panic: {info}"));
    }));

    match daemon_running() {
        Some(true) => {}
        // The process table could not be read. Repairing on a guess could kill
        // a working stack, so do nothing and let the next tick try again.
        None => {}
        Some(false) => {
            // `letsshare-tier2 run` is a legitimate supervisor too. If one is
            // alive (or even a status command happens to be mid-flight), leave
            // the stack alone and try again next tick.
            if process_running(CLI_IMAGE) == Some(true) {
                return;
            }
            repair();
        }
    }
}

/// Start the task, ending it first if it is stuck in `Running`.
fn repair() {
    // The daemon is gone, so its children are unreachable. Clear them before a
    // new daemon tries to bind the same ports.
    let orphans = kill_orphans();
    if !orphans.is_empty() {
        log(&format!("killed orphaned helpers: {}", orphans.join(", ")));
    }

    let mut notes: Vec<&str> = Vec::new();

    if task_is_running() {
        if end_task() {
            notes.push("ended the stuck task (state Running, no daemon process)");
            std::thread::sleep(Duration::from_secs(2));
        } else {
            notes.push("failed to end the stuck task");
        }
    }

    if !run_task() {
        notes.push("schtasks /run failed");
        log(&notes.join("; "));
        std::process::exit(1);
    }

    if daemon_appears() {
        notes.push("started the task");
        log(&notes.join("; "));
    } else {
        notes.push("task started but no daemon process appeared before the timeout");
        log(&notes.join("; "));
        std::process::exit(1);
    }
}

/// Build a command that can never flash a console window.
fn hidden(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Kill the daemon's orphaned children. Returns the images actually stopped.
fn kill_orphans() -> Vec<&'static str> {
    let mut killed = Vec::new();
    for image in HELPERS {
        if process_running(image) != Some(true) {
            continue;
        }
        if hidden("taskkill")
            .args(["/IM", image, "/F"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
        {
            killed.push(image);
        }
    }
    killed
}

/// Is a process with this image name alive? `None` means "could not tell".
///
/// `tasklist` is the pragmatic source here: always present, needs no
/// elevation, and its CSV output for a filter match carries the image name, so
/// the answer does not depend on the localized "no tasks" message.
fn process_running(image: &str) -> Option<bool> {
    let filter = format!("IMAGENAME eq {image}");
    let out = hidden("tasklist")
        .args(["/FI", &filter, "/NH", "/FO", "CSV"])
        .output()
        .ok()?;
    Some(parse_tasklist(&String::from_utf8_lossy(&out.stdout), image))
}

fn daemon_running() -> Option<bool> {
    process_running(DAEMON_IMAGE)
}

/// A filter hit lists the image name; a miss prints a localized INFO line that
/// does not, so the name alone is the signal.
fn parse_tasklist(output: &str, image: &str) -> bool {
    output
        .to_ascii_lowercase()
        .contains(&image.to_ascii_lowercase())
}

/// Does the scheduled task report state `Running`? A task in that state with no
/// daemon process is stuck and must be ended before it can be started again.
/// `None` means the report did not contain a status line.
fn task_is_running() -> bool {
    let Ok(out) = hidden("schtasks")
        .args(["/query", "/tn", TASK_NAME, "/fo", "list", "/v"])
        .output()
    else {
        return false;
    };
    matches!(
        parse_task_status(&String::from_utf8_lossy(&out.stdout)),
        Some(true)
    )
}

fn parse_task_status(list_output: &str) -> Option<bool> {
    for line in list_output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("status") {
            return Some(value.trim().eq_ignore_ascii_case("running"));
        }
    }
    None
}

fn end_task() -> bool {
    schtasks(&["/end", "/tn", TASK_NAME])
}

fn run_task() -> bool {
    schtasks(&["/run", "/tn", TASK_NAME])
}

fn schtasks(args: &[&str]) -> bool {
    hidden("schtasks")
        .args(args)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Poll until the daemon process shows up. Never trust the start result alone.
fn daemon_appears() -> bool {
    let polls = START_TIMEOUT.as_millis().div_ceil(500) as usize;
    for _ in 0..polls {
        std::thread::sleep(Duration::from_millis(500));
        if daemon_running() == Some(true) {
            return true;
        }
    }
    false
}

fn log_path() -> PathBuf {
    std::env::temp_dir().join("letsshare-watchdog.log")
}

/// Append one line, but only when the watchdog actually changed something.
fn log(msg: &str) {
    let stamp = utc_stamp(unix_secs());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = writeln!(f, "{stamp} {msg}");
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `1970-01-01T00:00:00Z` style stamp without pulling in a time crate.
fn utc_stamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;

    // Howard Hinnant's civil_from_days: days since the epoch -> Y/M/D.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400;
    let year = if month <= 2 { year + 1 } else { year };

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem / 60) % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_task_status, parse_tasklist, utc_stamp};

    #[test]
    fn epoch_formats_as_utc() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn leap_day_formats_as_utc() {
        assert_eq!(utc_stamp(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn tasklist_hit_is_detected() {
        let hit = "\"letsshare-daemon.exe\",\"10032\",\"Console\",\"2\",\"9,416 K\"\r\n";
        assert!(parse_tasklist(hit, "letsshare-daemon.exe"));
        let miss = "INFO: No tasks are running which match the specified criteria.\r\n";
        assert!(!parse_tasklist(miss, "letsshare-daemon.exe"));
        // An orphaned helper must never be mistaken for the daemon.
        assert!(!parse_tasklist(hit, "wfd.exe"));
        let wfd = "\"wfd.exe\",\"123\",\"Console\",\"2\",\"1,000 K\"\r\n";
        assert!(parse_tasklist(wfd, "wfd.exe"));
    }

    #[test]
    fn task_status_is_parsed() {
        let running = "Folder: \\\r\n\
                       HostName:                             TEST-LAPTOP\r\n\
                       TaskName:                             \\LetShare\r\n\
                       Next Run Time:                        N/A\r\n\
                       Status:                               Running\r\n\
                       Logon Mode:                           Interactive/Background\r\n";
        assert_eq!(parse_task_status(running), Some(true));

        let ready = running.replace("Running", "Ready");
        assert_eq!(parse_task_status(&ready), Some(false));

        // No status line at all (query failed, unusual output): unknown.
        assert_eq!(
            parse_task_status("ERROR: The system cannot find the file specified.\r\n"),
            None
        );
    }
}
