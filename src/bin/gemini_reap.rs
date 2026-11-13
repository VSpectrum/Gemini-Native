//! Small process that outlives the window.
//!
//! `gemini_native_client` starts this with the app pid and the sign-in browser
//! pid. It sleeps, and if the app is gone while the browser is still running,
//! it stops that browser's process group. It does not link the UI.

use std::thread;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let parent = args.next().and_then(|value| value.parse().ok()).unwrap_or(0);
    let browser = args.next().and_then(|value| value.parse().ok()).unwrap_or(0);
    std::process::exit(watch(parent, browser));
}

fn watch(parent: u32, browser: u32) -> i32 {
    if !pid_allowed(parent) || !pid_allowed(browser) {
        return 2;
    }
    // The parent has to be alive now. Otherwise this could be pointed at a
    // pid that is already gone and used to signal some other process.
    if !alive(parent) {
        return 0;
    }
    loop {
        thread::sleep(Duration::from_secs(2));
        match decide(true, alive(parent), alive(browser)) {
            Action::Wait => {}
            Action::Kill => {
                signal_group(browser);
                return 0;
            }
            Action::Exit => return 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Wait,
    Kill,
    Exit,
}

fn decide(saw_parent_alive: bool, parent_alive: bool, browser_alive: bool) -> Action {
    if !browser_alive {
        return Action::Exit;
    }
    if parent_alive {
        return Action::Wait;
    }
    if saw_parent_alive {
        Action::Kill
    } else {
        Action::Exit
    }
}

fn pid_allowed(pid: u32) -> bool {
    pid > 1 && pid <= i32::MAX as u32
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    if !pid_allowed(pid) {
        return false;
    }
    let rc = unsafe { kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(unix)]
fn signal_group(pid: u32) {
    if pid_allowed(pid) {
        unsafe {
            kill(-(pid as i32), 9);
        }
    }
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    if !pid_allowed(pid) {
        return false;
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(windows)]
fn signal_group(pid: u32) {
    if !pid_allowed(pid) {
        return;
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_parent_keeps_waiting() {
        assert_eq!(decide(true, true, true), Action::Wait);
    }

    #[test]
    fn a_dead_parent_stops_the_browser() {
        assert_eq!(decide(true, false, true), Action::Kill);
    }

    #[test]
    fn a_parent_we_never_saw_does_not_get_a_kill() {
        assert_eq!(decide(false, false, true), Action::Exit);
    }

    #[test]
    fn a_closed_browser_ends_the_watch() {
        assert_eq!(decide(true, true, false), Action::Exit);
        assert_eq!(decide(true, false, false), Action::Exit);
    }

    #[test]
    fn init_and_zero_are_never_signaled() {
        assert!(!pid_allowed(0));
        assert!(!pid_allowed(1));
        assert!(pid_allowed(std::process::id()));
    }
}
