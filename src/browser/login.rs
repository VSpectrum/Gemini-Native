//! Launch a browser the machine already has, then poll for the session cookies.
//!
//! Process creation and cookie reads run on the blocking pool. The wait between
//! reads is `tokio::time::sleep`, so a five-minute sign-in does not occupy a
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

fn finish_browser(proc: &mut std::process::Child, port: Option<u16>) {
    if let Some(port) = port {
        devtools::close_devtools_browser(port);
    }
    terminate_child(proc);
}

/// Reaps the sign-in process if the request is cancelled mid-poll.
/// The happy path calls [`ChildGuard::shutdown`] on the blocking pool instead,
/// so a graceful DevTools close does not run on the IO worker.
struct ChildGuard {
    child: Option<std::process::Child>,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    fn get_mut(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("browser process")
    }

    fn shutdown(mut self, port: Option<u16>) {
        if let Some(mut child) = self.child.take() {
            finish_browser(&mut child, port);
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_child(&mut child);
        }
    }
}

struct ChromiumSource<'a> {
    proc: &'a mut std::process::Child,
    port: u16,
    opened: Instant,
}

impl CookieSource for ChromiumSource<'_> {
    async fn read_cookies(&mut self) -> Vec<super::cookies::Cookie> {
        let port = self.port;
        blocking(move || chromium_cookies(port).unwrap_or_default()).await
    }

    async fn still_open(&mut self) -> Result<bool, BrowserError> {
        if self.proc.try_wait().ok().flatten().is_some() {
            return Ok(false);
        }
        if self.opened.elapsed() < Duration::from_secs(5) {
            return Ok(true);
        }
        let port = self.port;
        match blocking(move || devtools::devtools_pages(port)).await {
            Ok(pages) => Ok(!pages.is_empty()),
            Err(_) => Ok(self.proc.try_wait().ok().flatten().is_none()),
        }
    }
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

async fn wait_for_devtools_port(
    profile: &Path,
    proc: &mut std::process::Child,
) -> Result<u16, BrowserError> {
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if proc.try_wait().ok().flatten().is_some() {
            return Err(BrowserError::Launch(
                "browser exited before the debugging port was ready".into(),
            ));
        }
        let path = port_file.clone();
        if let Some(port) = blocking(move || read_devtools_port_file(&path)).await {
            return Ok(port);
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if proc.try_wait().ok().flatten().is_some() {
        return Err(BrowserError::Launch(
            "browser exited before the debugging port was ready".into(),
        ));
    }
    Err(BrowserError::Launch(
        "timed out waiting for the debugging port".into(),
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
        "--disable-blink-features=AutomationControlled".into(),
        "https://gemini.google.com/".into(),
    ];
    let argv_for_spawn = argv.clone();
    let pid = Arc::clone(browser_pid);
    let mut guard = ChildGuard::new(
        blocking(move || spawn_browser(&argv_for_spawn, &pid))
            .await
            .map_err(|err| BrowserError::Launch(err.to_string()))?,
    );
    eprintln!("Sign in to Gemini in the browser window. It closes itself once the session is stored.");
    report.report(ActivityStep::WaitingForSignIn(label.to_string()));
    let devtools = wait_for_devtools_port(&profile, guard.get_mut()).await;
    let port_for_close = devtools.as_ref().ok().copied();
    let result = match devtools {
        Ok(devtools_port) => {
            let mut source = ChromiumSource {
                proc: guard.get_mut(),
                port: devtools_port,
                opened: Instant::now(),
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
    browser_pid.store(0, Ordering::SeqCst);
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
    browser_pid.store(0, Ordering::SeqCst);
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
    browser_pid.store(0, Ordering::SeqCst);
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
    use super::collect_session_cookies;
    use std::path::Path;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

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
}
