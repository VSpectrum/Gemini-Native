//! Launch a browser the machine already has, then poll for the session cookies.
//!
//! Process creation and cookie reads run on the blocking pool. The wait between
//! reads is `tokio::time::sleep`, so the sign-in wait does not occupy a
//! worker or a blocking thread.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::cookies::{poll_for_cookies, CookieSource};
use super::devtools::{self, chromium_cookies};
use super::readers::{self, read_firefox_cookies, read_safari_cookies, webdriver_call};
use super::BrowserError;
use crate::reap;
use crate::safety;
use crate::status::{ActivityStep, Reporter};

pub(crate) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|err| panic!("background task panicked: {err}"))
}

fn executable(path: &Path) -> bool {
    path.is_file() && {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(path)
                .map(|meta| meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub fn find_browsers() -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let mut seen = Vec::new();
    let mut add = |kind: &str, path: Option<PathBuf>| {
        let Some(path) = path else { return };
        let text = path.to_string_lossy().to_string();
        if seen.iter().any(|item: &String| item == &text) || !executable(&path) {
            return;
        }
        seen.push(text);
        found.push((kind.to_string(), path));
    };

    if cfg!(target_os = "macos") {
        for (kind, path) in [
            ("chromium", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            ("chromium", "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            ("chromium", "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
            ("chromium", "/Applications/Chromium.app/Contents/MacOS/Chromium"),
            ("firefox", "/Applications/Firefox.app/Contents/MacOS/firefox"),
            ("safari", "/Applications/Safari.app/Contents/MacOS/Safari"),
        ] {
            add(kind, Some(PathBuf::from(path)));
        }
    } else if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let program = std::env::var("PROGRAMFILES").unwrap_or_else(|_| r"C:\Program Files".into());
        let program_x86 =
            std::env::var("PROGRAMFILES(X86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        for (kind, path) in [
            ("chromium", PathBuf::from(&program).join(r"Google\Chrome\Application\chrome.exe")),
            ("chromium", PathBuf::from(&program_x86).join(r"Google\Chrome\Application\chrome.exe")),
            ("chromium", PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe")),
            ("chromium", PathBuf::from(&program).join(r"Microsoft\Edge\Application\msedge.exe")),
            ("chromium", PathBuf::from(&program_x86).join(r"Microsoft\Edge\Application\msedge.exe")),
            ("chromium", PathBuf::from(&local).join(r"BraveSoftware\Brave-Browser\Application\brave.exe")),
            ("firefox", PathBuf::from(&program).join(r"Mozilla Firefox\firefox.exe")),
            ("firefox", PathBuf::from(&program_x86).join(r"Mozilla Firefox\firefox.exe")),
        ] {
            add(kind, Some(path));
        }
    }

    if !cfg!(windows) {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "brave-browser",
            "chromium",
            "chromium-browser",
        ] {
            add("chromium", which(name));
        }
        add("firefox", which("firefox"));
    }
    found
}

fn browser_label(kind: &str, exe: &Path) -> String {
    if kind == "safari" {
        return "Safari".into();
    }
    if kind == "firefox" {
        return "Firefox".into();
    }
    let name = exe.file_name().and_then(|name| name.to_str()).unwrap_or("");
    match name {
        "Google Chrome" | "chrome.exe" | "google-chrome" | "google-chrome-stable" => "Chrome",
        "Microsoft Edge" | "msedge.exe" | "microsoft-edge" => "Edge",
        "Brave Browser" | "brave.exe" | "brave-browser" => "Brave",
        "Chromium" | "chromium" | "chromium-browser" => "Chromium",
        "firefox" | "firefox.exe" => "Firefox",
        _ => "browser",
    }
    .to_string()
}

fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn spawn_browser(argv: &[String], browser_pid: &AtomicU32) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
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
    let child = cmd.spawn()?;
    browser_pid.store(child.id(), Ordering::SeqCst);
    reap::arm(std::process::id(), child.id());
    Ok(child)
}

fn terminate_child(child: &mut std::process::Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    reap::stop_process_group(child.id());
    let _ = child.wait();
}

fn wait_for_exit(proc: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if proc.try_wait().ok().flatten().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn finish_browser(proc: &mut std::process::Child, port: Option<u16>) {
    if proc.try_wait().ok().flatten().is_some() {
        // Chrome already left, usually to restart after the Google login.
        // Killing the process group here also kills that restarted window.
        return;
    }
    let Some(port) = port else {
        terminate_child(proc);
        return;
    };
    devtools::close_devtools_browser(port);
    if wait_for_exit(proc, Duration::from_secs(5)) {
        return;
    }
    reap::ask_process_group_to_exit(proc.id());
    if wait_for_exit(proc, Duration::from_secs(3)) {
        return;
    }
    terminate_child(proc);
}

/// Reaps the sign-in process if the request is cancelled mid-poll.
/// The happy path calls [`ChildGuard::shutdown`] on the blocking pool instead,
/// so a graceful DevTools close does not run on the IO worker.
///
/// Drop clears the shared pid when that slot still names this process.
/// Every exit, including an early driver error, goes through drop, so Cancel
/// cannot later signal a recycled id. A newer sign-in pid is left in place.
struct ChildGuard {
    child: Option<std::process::Child>,
    browser_pid: Arc<AtomicU32>,
    owned_pid: u32,
}

impl ChildGuard {
    fn new(child: std::process::Child, browser_pid: Arc<AtomicU32>) -> Self {
        let owned_pid = child.id();
        Self {
            child: Some(child),
            browser_pid,
            owned_pid,
        }
    }

    fn get_mut(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("browser process")
    }

    fn shutdown(mut self, port: Option<u16>) {
        if let Some(mut child) = self.child.take() {
            finish_browser(&mut child, port);
        }
    }

    fn release_pid(&self) {
        let _ = self.browser_pid.compare_exchange(
            self.owned_pid,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_child(&mut child);
        }
        self.release_pid();
    }
}

/// How `/json/list` answered. `Unavailable` means the port did not respond.
#[derive(Clone, Copy)]
enum TargetList {
    HasTargets,
    Empty,
    Unavailable,
}

/// An empty target list is normal before the first page exists, and again
/// while Google's sign-in page is navigating. On a slow machine that gap
/// lasts longer than a few seconds. The window counts as closed only after
/// a target has been seen and the list stays empty.
const EMPTY_TARGET_GRACE: Duration = Duration::from_secs(90);

/// The spawned pid can exit while the window is still on screen. macOS Chrome
/// does that when the launcher hands off, and signing in to Chrome restarts
/// the browser. Both leave a stretch where the pid is dead and the debugging
/// port is down. Keep polling through that stretch.
const PROCESS_EXIT_GRACE: Duration = Duration::from_secs(60);

struct ProcessExitGrace {
    down_since: Option<Instant>,
}

impl ProcessExitGrace {
    fn new() -> Self {
        Self { down_since: None }
    }

    fn still_open(&mut self, spawned_alive: bool, port_open: bool, now: Instant) -> bool {
        if spawned_alive || port_open {
            self.down_since = None;
            return true;
        }
        let since = *self.down_since.get_or_insert(now);
        now.saturating_duration_since(since) < PROCESS_EXIT_GRACE
    }
}

struct ChromiumWindow {
    saw_target: bool,
    empty_since: Option<Instant>,
}

impl ChromiumWindow {
    fn new() -> Self {
        Self {
            saw_target: false,
            empty_since: None,
        }
    }

    fn still_open(&mut self, process_alive: bool, targets: TargetList, now: Instant) -> bool {
        if !process_alive {
            return false;
        }
        match targets {
            TargetList::HasTargets => {
                self.saw_target = true;
                self.empty_since = None;
                true
            }
            TargetList::Unavailable => true,
            TargetList::Empty if !self.saw_target => true,
            TargetList::Empty => {
                let since = *self.empty_since.get_or_insert(now);
                now.saturating_duration_since(since) < EMPTY_TARGET_GRACE
            }
        }
    }
}

struct ChromiumSource<'a> {
    proc: &'a mut std::process::Child,
    port: u16,
    profile: PathBuf,
    window: ChromiumWindow,
    exit_grace: ProcessExitGrace,
}

impl CookieSource for ChromiumSource<'_> {
    async fn read_cookies(&mut self) -> Vec<super::cookies::Cookie> {
        let port = self.port;
        blocking(move || chromium_cookies(port).unwrap_or_default()).await
    }

    async fn still_open(&mut self) -> Result<bool, BrowserError> {
        let spawned_alive = self.proc.try_wait().ok().flatten().is_none();
        if !spawned_alive {
            self.refresh_port().await;
        }
        let port = self.port;
        let probed = blocking(move || devtools::devtools_pages(port)).await;
        let now = Instant::now();
        if !self.exit_grace.still_open(spawned_alive, probed.is_ok(), now) {
            return Ok(false);
        }
        // Pid and port are both down. Chrome may be restarting into the same
        // profile; closing the window here is what cut sign-in off.
        if !spawned_alive && probed.is_err() {
            return Ok(true);
        }
        let targets = match probed {
            Ok(pages) if !pages.is_empty() => TargetList::HasTargets,
            Ok(_) => TargetList::Empty,
            Err(_) => TargetList::Unavailable,
        };
        // A dead launcher pid is not a closed window while DevTools answers.
        Ok(self.window.still_open(true, targets, now))
    }
}

impl ChromiumSource<'_> {
    async fn refresh_port(&mut self) {
        let path = self.profile.join("DevToolsActivePort");
        if let Some(port) = blocking(move || read_devtools_port_file(&path)).await {
            self.port = port;
        }
    }
}

/// Stops the "Sign in to Chrome?" step from restarting the window after the
/// Google page login. Written only for a profile that does not have
/// Preferences yet, so an existing profile file is left as Chrome wrote it.
fn disarm_chrome_signin_restart(profile: &Path) -> std::io::Result<()> {
    let dir = profile.join("Default");
    fs::create_dir_all(&dir)?;
    super::restrict_dir(&dir);
    let path = dir.join("Preferences");
    if path.exists() {
        return Ok(());
    }
    let prefs = serde_json::json!({
        "signin": { "interception_enabled": false }
    });
    let bytes = serde_json::to_vec(&prefs).map_err(std::io::Error::other)?;
    super::write_private(&path, &bytes)
}

struct FirefoxSource<'a> {
    proc: &'a mut std::process::Child,
    db_path: PathBuf,
    started: Instant,
}

impl CookieSource for FirefoxSource<'_> {
    async fn read_cookies(&mut self) -> Vec<super::cookies::Cookie> {
        let path = self.db_path.clone();
        blocking(move || {
            if !path.exists() {
                return Vec::new();
            }
            read_firefox_cookies(&path).unwrap_or_default()
        })
        .await
    }

    async fn still_open(&mut self) -> Result<bool, BrowserError> {
        if self.proc.try_wait().ok().flatten().is_none() {
            return Ok(true);
        }
        if self.started.elapsed() < Duration::from_secs(3) {
            return Err(BrowserError::Launch("browser exited immediately".into()));
        }
        Ok(false)
    }
}

struct SafariFileSource {
    path: PathBuf,
}

impl CookieSource for SafariFileSource {
    async fn read_cookies(&mut self) -> Vec<super::cookies::Cookie> {
        let path = self.path.clone();
        blocking(move || read_safari_cookies(&path).unwrap_or_default()).await
    }

    async fn still_open(&mut self) -> Result<bool, BrowserError> {
        Ok(blocking(safari_is_running).await)
    }
}

struct DriverSource<'a> {
    proc: &'a mut std::process::Child,
    port: u16,
    session: String,
}

impl CookieSource for DriverSource<'_> {
    async fn read_cookies(&mut self) -> Vec<super::cookies::Cookie> {
        let port = self.port;
        let session = self.session.clone();
        blocking(move || {
            webdriver_call(
                port,
                "GET",
                &format!("/session/{session}/cookie"),
                None,
                Duration::from_secs(5),
            )
            .ok()
            .and_then(|result| result.get("value").cloned())
            .and_then(|value| serde_json::from_value::<Vec<serde_json::Value>>(value).ok())
            .map(super::cookies::cookies_from_json)
            .unwrap_or_default()
        })
        .await
    }

    async fn still_open(&mut self) -> Result<bool, BrowserError> {
        Ok(self.proc.try_wait().ok().flatten().is_none())
    }
}

/// Chrome writes `DevToolsActivePort` only when `--remote-debugging-port=0`.
/// A fixed port still listens; the file is simply absent. Prefer the file
/// when Chrome chose the port, otherwise use the port we asked for once it
/// accepts connections.
fn chosen_devtools_port(
    file_port: Option<u16>,
    requested: u16,
    requested_ready: bool,
) -> Option<u16> {
    file_port.or(requested_ready.then_some(requested))
}

fn tcp_port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(200),
    )
    .is_ok()
}

fn debugging_port_ready(port: u16) -> bool {
    tcp_port_open(port) && devtools::devtools_pages(port).is_ok()
}

/// Chrome's profile lock is a symlink whose text is `hostname-pid`. A live
/// pid means a window for this profile is already open, so a new launch exits
/// immediately and must not be treated as "try the next browser".
fn running_singleton(profile: &Path) -> Option<u32> {
    let target = fs::read_link(profile.join("SingletonLock")).ok()?;
    let pid = target
        .to_string_lossy()
        .rsplit('-')
        .next()?
        .parse()
        .ok()?;
    process_alive(pid).then_some(pid)
}

fn process_alive(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let rc = unsafe { kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

async fn wait_for_devtools_port(
    profile: &Path,
    proc: &mut std::process::Child,
    requested_port: u16,
) -> Result<u16, BrowserError> {
    let port_file = profile.join("DevToolsActivePort");
    let started = Instant::now();
    let deadline = started + Duration::from_secs(20);
    loop {
        let path = port_file.clone();
        let file_port = blocking(move || read_devtools_port_file(&path)).await;
        let ready = if file_port.is_some() {
            false
        } else {
            blocking(move || debugging_port_ready(requested_port)).await
        };
        if let Some(port) = chosen_devtools_port(file_port, requested_port, ready) {
            return Ok(port);
        }
        let exited = proc.try_wait().ok().flatten().is_some();
        if exited && started.elapsed() < Duration::from_secs(2) {
            if running_singleton(profile).is_some() {
                return Err(BrowserError::Message(
                    "Chrome is already open for sign-in. Finish logging in there, or quit that window and send the message again.".into(),
                ));
            }
            return Err(BrowserError::Launch(
                "browser exited before the debugging port was ready".into(),
            ));
        }
        if exited {
            // The window was on screen. A launch error here starts the next
            // browser, which is how a Chrome sign-in was followed by Firefox.
            return Err(BrowserError::Message(
                "Chrome closed before the debugging port was ready. Send the message again.".into(),
            ));
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(BrowserError::Message(
        "Chrome stayed open, but its debugging port never started. Send the message again.".into(),
    ))
}

fn read_devtools_port_file(path: &Path) -> Option<u16> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().next()?.trim().parse().ok()
}

async fn login_chromium(
    exe: &Path,
    profile_dir: &Path,
    timeout_secs: u64,
    browser_pid: &Arc<AtomicU32>,
    label: &str,
    report: &Reporter,
) -> Result<(String, String), BrowserError> {
    report.report(ActivityStep::OpeningBrowser(label.to_string()));
    let profile = profile_dir.join("chromium");
    let profile_for_create = profile.clone();
    blocking(move || -> Result<(), String> {
        fs::create_dir_all(&profile_for_create).map_err(|err| err.to_string())?;
        super::restrict_dir(&profile_for_create);
        disarm_chrome_signin_restart(&profile_for_create).map_err(|err| err.to_string())?;
        Ok(())
    })
    .await
    .map_err(BrowserError::Launch)?;

    let port = blocking(free_port)
        .await
        .map_err(|err| BrowserError::Launch(err.to_string()))?;
    let argv = vec![
        exe.to_string_lossy().to_string(),
        format!("--user-data-dir={}", profile.display()),
        format!("--remote-debugging-address=127.0.0.1"),
        format!("--remote-debugging-port={port}"),
        format!("--remote-allow-origins=http://127.0.0.1:{port}"),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "https://gemini.google.com/".into(),
    ];
    let argv_for_spawn = argv.clone();
    let pid = Arc::clone(browser_pid);
    let mut guard = ChildGuard::new(
        blocking(move || spawn_browser(&argv_for_spawn, &pid))
            .await
            .map_err(|err| BrowserError::Launch(err.to_string()))?,
        Arc::clone(browser_pid),
    );
    eprintln!(
        "Sign in to Gemini in the browser window. It uses its own profile, stays open while you sign in, and closes itself once the session is stored."
    );
    report.report(ActivityStep::WaitingForSignIn(label.to_string()));
    let devtools = wait_for_devtools_port(&profile, guard.get_mut(), port).await;
    let port_for_close = devtools.as_ref().ok().copied();
    let result = match devtools {
        Ok(devtools_port) => {
            let mut source = ChromiumSource {
                proc: guard.get_mut(),
                port: devtools_port,
                profile: profile.clone(),
                window: ChromiumWindow::new(),
                exit_grace: ProcessExitGrace::new(),
            };
            poll_for_cookies(
                &mut source,
                Duration::from_secs(timeout_secs),
                Duration::from_secs(1),
                Duration::from_secs(1),
                || report.report(ActivityStep::ReadingSessionCookies),
            )
            .await
        }
        Err(err) => Err(err),
    };
    blocking(move || guard.shutdown(port_for_close)).await;
    result
}

async fn login_firefox(
    exe: &Path,
    profile_dir: &Path,
    timeout_secs: u64,
    browser_pid: &Arc<AtomicU32>,
    label: &str,
    report: &Reporter,
) -> Result<(String, String), BrowserError> {
    report.report(ActivityStep::OpeningBrowser(label.to_string()));
    let profile = profile_dir.join("firefox");
    let profile_for_create = profile.clone();
    blocking(move || -> Result<(), String> {
        fs::create_dir_all(&profile_for_create).map_err(|err| err.to_string())?;
        super::restrict_dir(&profile_for_create);
        let user_js = profile_for_create.join("user.js");
        if !user_js.exists() {
            let _ = fs::write(
                &user_js,
                "user_pref(\"browser.shell.checkDefaultBrowser\", false);\n\
                 user_pref(\"datareporting.policy.dataSubmissionEnabled\", false);\n",
            );
        }
        Ok(())
    })
    .await
    .map_err(BrowserError::Launch)?;

    let argv = vec![
        exe.to_string_lossy().to_string(),
        "--profile".into(),
        profile.display().to_string(),
        "--no-remote".into(),
        "--new-instance".into(),
        "https://gemini.google.com/".into(),
    ];
    let argv_for_spawn = argv.clone();
    let pid = Arc::clone(browser_pid);
    let mut guard = ChildGuard::new(
        blocking(move || spawn_browser(&argv_for_spawn, &pid))
            .await
            .map_err(|err| BrowserError::Launch(err.to_string()))?,
        Arc::clone(browser_pid),
    );
    eprintln!("Sign in to Gemini in the browser window. It closes itself once the session is stored.");
    report.report(ActivityStep::WaitingForSignIn(label.to_string()));
    let db_path = profile.join("cookies.sqlite");
    let started = Instant::now();
    let result = {
        let mut source = FirefoxSource {
            proc: guard.get_mut(),
            db_path,
            started,
        };
        poll_for_cookies(
            &mut source,
            Duration::from_secs(timeout_secs),
            Duration::from_secs(1),
            Duration::from_secs(1),
            || report.report(ActivityStep::ReadingSessionCookies),
        )
        .await
    };
    blocking(move || guard.shutdown(None)).await;
    result
}

fn safari_is_running() -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.processes().values().any(|process| {
        let name = process.name();
        name.eq_ignore_ascii_case("Safari") || name.eq_ignore_ascii_case("com.apple.Safari")
    })
}

fn readable_safari_cookies() -> Option<(String, String)> {
    let path = readers::safari_cookie_file()?;
    let cookies = read_safari_cookies(&path).ok()?;
    let (sid, sidts) = super::cookies::extract_sid_and_sidts(&cookies);
    Some((sid?, sidts?))
}

async fn wait_for_local_port(
    port: u16,
    proc: &mut std::process::Child,
    timeout: Duration,
) -> Result<(), BrowserError> {
    let deadline = Instant::now() + timeout;
    loop {
        if proc.try_wait().ok().flatten().is_some() {
            return Err(BrowserError::Launch(
                "safaridriver exited before it was ready".into(),
            ));
        }
        let connected = blocking(move || {
            std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().unwrap(),
                Duration::from_millis(500),
            )
            .is_ok()
        })
        .await;
        if connected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::Launch(
                "timed out waiting for safaridriver".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn login_safari_driver(
    timeout_secs: u64,
    browser_pid: &Arc<AtomicU32>,
    report: &Reporter,
) -> Result<(String, String), BrowserError> {
    let driver = PathBuf::from("/usr/bin/safaridriver");
    if !executable(&driver) {
        return Err(BrowserError::Launch(
            "Safari is installed, but safaridriver is missing. Set GEMINI_PSID and GEMINI_PSIDTS to sign in.".into(),
        ));
    }
    let port = blocking(free_port)
        .await
        .map_err(|err| BrowserError::Launch(err.to_string()))?;
    let pid = Arc::clone(browser_pid);
    let mut guard = ChildGuard::new(
        blocking(move || {
            spawn_browser(
                &[
                    driver.to_string_lossy().to_string(),
                    "-p".into(),
                    port.to_string(),
                ],
                &pid,
            )
        })
        .await
        .map_err(|err| BrowserError::Launch(err.to_string()))?,
        Arc::clone(browser_pid),
    );

    if let Err(err) = wait_for_local_port(port, guard.get_mut(), Duration::from_secs(20)).await {
        blocking(move || guard.shutdown(None)).await;
        return Err(err);
    }
    let created = match blocking(move || {
        webdriver_call(
            port,
            "POST",
            "/session",
            Some(r#"{"capabilities":{"alwaysMatch":{"browserName":"safari"}}}"#),
            Duration::from_secs(30),
        )
    })
    .await
    {
        Ok(created) => created,
        Err(err) => {
            blocking(move || guard.shutdown(None)).await;
            return Err(err);
        }
    };
    let value = created.get("value").cloned().unwrap_or(created);
    let id = value
        .get("sessionId")
        .and_then(|item| item.as_str())
        .map(str::to_string);
    let Some(id) = id else {
        blocking(move || guard.shutdown(None)).await;
        return Err(BrowserError::Launch(
            "Safari did not start a sign-in window. In Safari, open Settings > Advanced, \
             turn on 'Show features for web developers', then Develop > Allow Remote Automation."
                .into(),
        ));
    };
    if !safety::session_id_ok(&id) {
        blocking(move || guard.shutdown(None)).await;
        return Err(BrowserError::Launch(
            "Safari returned an unexpected session id.".into(),
        ));
    }
    if let Err(err) = blocking({
        let id = id.clone();
        move || {
            webdriver_call(
                port,
                "POST",
                &format!("/session/{id}/url"),
                Some(r#"{"url":"https://gemini.google.com/"}"#),
                Duration::from_secs(30),
            )
        }
    })
    .await
    {
        let _ = blocking({
            let id = id.clone();
            move || {
                webdriver_call(
                    port,
                    "DELETE",
                    &format!("/session/{id}"),
                    None,
                    Duration::from_secs(5),
                )
            }
        })
        .await;
        blocking(move || guard.shutdown(None)).await;
        return Err(err);
    }
    eprintln!("Sign in to Gemini in the Safari window. It closes itself once the session is stored.");
    report.report(ActivityStep::WaitingForSignIn("Safari".into()));
    let result = {
        let mut source = DriverSource {
            proc: guard.get_mut(),
            port,
            session: id.clone(),
        };
        poll_for_cookies(
            &mut source,
            Duration::from_secs(timeout_secs),
            Duration::from_secs(1),
            Duration::from_secs(1),
            || report.report(ActivityStep::ReadingSessionCookies),
        )
        .await
    };
    let _ = blocking({
        let id = id.clone();
        move || {
            webdriver_call(
                port,
                "DELETE",
                &format!("/session/{id}"),
                None,
                Duration::from_secs(5),
            )
        }
    })
    .await;
    blocking(move || guard.shutdown(None)).await;
    result
}

async fn login_safari(
    timeout_secs: u64,
    browser_pid: &Arc<AtomicU32>,
    report: &Reporter,
) -> Result<(String, String), BrowserError> {
    if let Some(saved) = blocking(readable_safari_cookies).await {
        report.report(ActivityStep::ReadingSessionCookies);
        return Ok(saved);
    }
    let path = readers::safari_cookie_file();
    let file_readable = match &path {
        Some(path) => {
            let path = path.clone();
            blocking(move || read_safari_cookies(&path).is_ok()).await
        }
        None => false,
    };
    match login_safari_driver(timeout_secs, browser_pid, report).await {
        Ok(pair) => return Ok(pair),
        Err(err) if !file_readable => {
            return Err(BrowserError::Launch(format!(
                "{err} Safari's cookie file could not be read either. \
                 Turn on Develop > Allow Remote Automation, or set GEMINI_PSID and GEMINI_PSIDTS."
            )));
        }
        // The driver guard has reaped safaridriver and released its pid.
        // Cancel during the file poll must not signal that id.
        Err(_) => {}
    }
    let Some(path) = path else {
        return Err(BrowserError::Launch("Safari cookie file is missing.".into()));
    };
    eprintln!("Sign in to Gemini in Safari. Your normal Safari window stays open.");
    report.report(ActivityStep::OpeningBrowser("Safari".into()));
    report.report(ActivityStep::WaitingForSignIn("Safari".into()));
    let _ = blocking(|| {
        Command::new("open")
            .args(["-a", "Safari", "https://gemini.google.com/"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    })
    .await;
    let mut source = SafariFileSource { path };
    poll_for_cookies(
        &mut source,
        Duration::from_secs(timeout_secs),
        Duration::from_secs(1),
        Duration::from_secs(1),
        || report.report(ActivityStep::ReadingSessionCookies),
    )
    .await
}

pub async fn collect_session_cookies(
    profile_dir: &Path,
    timeout_secs: u64,
    browsers: Option<Vec<(String, PathBuf)>>,
    browser_pid: &Arc<AtomicU32>,
    report: &Reporter,
) -> Result<(String, String), BrowserError> {
    let browsers = match browsers {
        Some(browsers) => browsers,
        None => blocking(find_browsers).await,
    };
    if browsers.is_empty() {
        return Err(BrowserError::Message(
            "No Chrome, Edge, Brave, Chromium, Firefox, or Safari is installed. \
             Install one of them and send the message again to sign in. \
             Or set GEMINI_PSID and GEMINI_PSIDTS."
                .into(),
        ));
    }
    let mut failures = Vec::new();
    for (kind, exe) in browsers {
        let label = browser_label(&kind, &exe);
        let result = if kind == "firefox" {
            login_firefox(&exe, profile_dir, timeout_secs, browser_pid, &label, report).await
        } else if kind == "safari" {
            login_safari(timeout_secs, browser_pid, report).await
        } else {
            login_chromium(&exe, profile_dir, timeout_secs, browser_pid, &label, report).await
        };
        match result {
            Ok(pair) => return Ok(pair),
            Err(BrowserError::Launch(message)) => {
                failures.push(format!(
                    "{}: {message}",
                    exe.file_name().and_then(|name| name.to_str()).unwrap_or("browser")
                ));
            }
            Err(other) => return Err(other),
        }
    }
    Err(BrowserError::Message(format!(
        "Could not open a browser for sign-in.\n{}",
        failures.join("\n")
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        chosen_devtools_port, collect_session_cookies, disarm_chrome_signin_restart, spawn_browser,
        tcp_port_open, ChildGuard, ChromiumWindow, ProcessExitGrace, TargetList,
        EMPTY_TARGET_GRACE, PROCESS_EXIT_GRACE,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::status::Reporter;

    #[tokio::test]
    async fn missing_browser_explains_the_next_step() {
        let err = collect_session_cookies(
            Path::new("/tmp/does-not-matter"),
            5,
            Some(vec![]),
            &Arc::new(AtomicU32::new(0)),
            &Reporter::noop(),
        )
        .await
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("No Chrome"));
        assert!(text.contains("GEMINI_PSID"));
    }

    fn linger_argv() -> Vec<String> {
        if cfg!(windows) {
            vec!["ping".into(), "-n".into(), "8".into(), "127.0.0.1".into()]
        } else {
            vec!["sleep".into(), "8".into()]
        }
    }

    #[test]
    fn shutting_down_on_an_early_error_clears_the_browser_pid() {
        let slot = Arc::new(AtomicU32::new(0));
        let child = spawn_browser(&linger_argv(), &slot).expect("linger");
        let owned = child.id();
        assert_eq!(slot.load(Ordering::SeqCst), owned);
        ChildGuard::new(child, Arc::clone(&slot)).shutdown(None);
        assert_eq!(slot.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dropping_the_guard_clears_the_browser_pid() {
        let slot = Arc::new(AtomicU32::new(0));
        let child = spawn_browser(&linger_argv(), &slot).expect("linger");
        {
            let _guard = ChildGuard::new(child, Arc::clone(&slot));
            assert_ne!(slot.load(Ordering::SeqCst), 0);
        }
        assert_eq!(slot.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn releasing_a_finished_browser_leaves_a_newer_pid() {
        let slot = Arc::new(AtomicU32::new(0));
        let child = spawn_browser(&linger_argv(), &slot).expect("linger");
        let guard = ChildGuard::new(child, Arc::clone(&slot));
        slot.store(424_242, Ordering::SeqCst);
        guard.shutdown(None);
        assert_eq!(slot.load(Ordering::SeqCst), 424_242);
    }

    #[test]
    fn empty_target_list_does_not_mean_a_live_window_closed() {
        let mut window = ChromiumWindow::new();
        let start = Instant::now();
        assert!(window.still_open(true, TargetList::Empty, start));
        assert!(window.still_open(
            true,
            TargetList::Empty,
            start + EMPTY_TARGET_GRACE + Duration::from_secs(30),
        ));
        assert!(!window.still_open(false, TargetList::Empty, start));
    }

    #[test]
    fn one_empty_devtools_read_does_not_close_a_window_that_is_still_there() {
        let mut window = ChromiumWindow::new();
        let start = Instant::now();
        assert!(window.still_open(true, TargetList::HasTargets, start));
        assert!(window.still_open(
            true,
            TargetList::Empty,
            start + Duration::from_millis(200),
        ));
        assert!(window.still_open(true, TargetList::HasTargets, start + Duration::from_secs(1)));
    }

    #[test]
    fn a_window_that_stays_gone_is_closed() {
        let mut window = ChromiumWindow::new();
        let start = Instant::now();
        assert!(window.still_open(true, TargetList::HasTargets, start));
        let gone = start + Duration::from_secs(1);
        assert!(window.still_open(true, TargetList::Empty, gone));
        assert!(window.still_open(
            true,
            TargetList::Empty,
            gone + EMPTY_TARGET_GRACE - Duration::from_millis(1),
        ));
        assert!(!window.still_open(true, TargetList::Empty, gone + EMPTY_TARGET_GRACE));
    }

    #[test]
    fn a_dead_process_is_closed_even_when_devtools_still_lists_targets() {
        let mut window = ChromiumWindow::new();
        let now = Instant::now();
        assert!(!window.still_open(false, TargetList::HasTargets, now));
        assert!(!window.still_open(false, TargetList::Unavailable, now));
    }

    #[test]
    fn a_devtools_miss_does_not_close_a_running_browser() {
        let mut window = ChromiumWindow::new();
        assert!(window.still_open(true, TargetList::Unavailable, Instant::now()));
    }

    #[test]
    fn a_dead_launcher_with_a_live_port_is_still_signing_in() {
        let mut grace = ProcessExitGrace::new();
        assert!(grace.still_open(false, true, Instant::now()));
    }

    #[test]
    fn a_browser_restart_gap_does_not_close_the_window() {
        let mut grace = ProcessExitGrace::new();
        let start = Instant::now();
        assert!(grace.still_open(false, false, start));
        assert!(grace.still_open(
            false,
            false,
            start + PROCESS_EXIT_GRACE - Duration::from_millis(1),
        ));
        assert!(!grace.still_open(false, false, start + PROCESS_EXIT_GRACE));
    }

    #[test]
    fn the_port_coming_back_starts_the_restart_gap_over() {
        let mut grace = ProcessExitGrace::new();
        let start = Instant::now();
        assert!(grace.still_open(false, false, start));
        assert!(grace.still_open(false, true, start + Duration::from_secs(5)));
        let again = start + PROCESS_EXIT_GRACE + Duration::from_secs(10);
        assert!(grace.still_open(false, false, again));
        assert!(grace.still_open(false, false, again + Duration::from_secs(1)));
    }

    #[test]
    fn a_fixed_debugging_port_counts_without_the_port_file() {
        assert_eq!(chosen_devtools_port(None, 9333, true), Some(9333));
        assert_eq!(chosen_devtools_port(None, 9333, false), None);
        assert_eq!(chosen_devtools_port(Some(9334), 9333, true), Some(9334));
    }

    #[test]
    fn a_listening_port_is_ready_for_chrome() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_port_open(port));
    }

    #[test]
    fn a_fresh_profile_does_not_ask_chrome_to_restart_on_signin() {
        let dir = std::env::temp_dir().join(format!(
            "gemini-signin-pref-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        disarm_chrome_signin_restart(&dir).unwrap();
        let path = dir.join("Default").join("Preferences");
        let prefs: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(prefs["signin"]["interception_enabled"], false);

        std::fs::write(&path, r#"{"profile":{"name":"Gemini"}}"#).unwrap();
        disarm_chrome_signin_restart(&dir).unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.contains("Gemini"));
        assert!(!kept.contains("interception_enabled"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
