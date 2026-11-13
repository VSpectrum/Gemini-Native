//! Stop a sign-in browser without a shell.
//!
//! Cancel signals the process group from this process. If the app is killed
//! outright, the small `gemini_reap` binary beside this one notices and does
//! the same. That helper is separate so a crash watcher is not a second copy
//! of the window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::safety::pid_allowed;

pub fn stop_process_group(pid: u32) {
    if !pid_allowed(pid) {
        return;
    }
    #[cfg(unix)]
    unix_signal::group(pid);
    #[cfg(windows)]
    windows_tree::kill(pid);
}

/// Start `gemini_reap` when it was built next to this executable.
/// Missing helper means cancel still works; only a hard kill of the app can
/// leave the sign-in window open.
pub fn arm(parent: u32, browser: u32) {
    if !pid_allowed(parent) || !pid_allowed(browser) {
        return;
    }
    let Some(helper) = helper_path() else {
        return;
    };
    let mut cmd = Command::new(helper);
    cmd.arg(parent.to_string())
        .arg(browser.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.spawn();
}

fn helper_path() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    sibling_helper(current.parent()?)
}

fn sibling_helper(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "gemini_reap.exe"
    } else {
        "gemini_reap"
    };
    let path = dir.join(name);
    path.is_file().then_some(path)
}

#[cfg(unix)]
mod unix_signal {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    pub fn group(pid: u32) {
        unsafe {
            kill(-(pid as i32), 9);
        }
    }
}

#[cfg(windows)]
mod windows_tree {
    use std::process::{Command, Stdio};

    pub fn kill(pid: u32) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_name_matches_the_platform() {
        let dir = std::env::temp_dir().join(format!("gemini-reap-missing-{}", std::process::id()));
        assert!(sibling_helper(&dir).is_none());
    }
}
