//! Sign-in without a bundled browser.
//!
//! Chrome, Edge, Brave, Chromium, and Firefox get a profile under
//! `~/.gemini_local/browser_profile`. Safari's cookie file is read in place
//! and never written back.

use base64::{engine::general_purpose::STANDARD, Engine};
use sha1::{Digest, Sha1};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const COOKIE_URLS: &[&str] = &[
    "https://gemini.google.com/",
    "https://accounts.google.com/",
    "https://www.google.com/",
];

#[derive(Debug)]
pub enum BrowserError {
    Launch(String),
    Message(String),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserError::Launch(text) | BrowserError::Message(text) => f.write_str(text),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
}

pub fn extract_sid_and_sidts(cookies: &[Cookie]) -> (Option<String>, Option<String>) {
    let mut sid = None;
    let mut sidts = None;
    for cookie in cookies {
        let value = cookie.value.trim();
        if value.is_empty() || !cookie.domain.ends_with("google.com") {
            continue;
        }
        if cookie.name == "__Secure-1PSID" {
            sid = Some(value.to_string());
        } else if cookie.name == "__Secure-1PSIDTS" {
            sidts = Some(value.to_string());
        }
    }
    (sid, sidts)
}

pub fn wait_for_google_cookies<F, S, N, C>(
    mut fetch: F,
    mut still_open: S,
    timeout: f64,
    interval: f64,
    mut sleep_fn: N,
    mut clock: C,
) -> Result<(String, String), BrowserError>
where
    F: FnMut() -> Vec<Cookie>,
    S: FnMut() -> Result<bool, BrowserError>,
    N: FnMut(f64),
    C: FnMut() -> f64,
{
    let start = clock();
    loop {
        if !still_open()? {
            return Err(BrowserError::Message(
                "Browser window was closed before sign-in completed.".into(),
            ));
        }
        if clock() - start > timeout {
            return Err(BrowserError::Message(
                "Timed out waiting for sign-in cookies (5 minutes exceeded).".into(),
            ));
        }
        let (sid, sidts) = extract_sid_and_sidts(&fetch());
        if sid.is_some() && sidts.is_some() {
            sleep_fn(1.0);
            if !still_open()? {
                return Err(BrowserError::Message(
                    "Browser window was closed before sign-in completed.".into(),
                ));
            }
            let (sid2, sidts2) = extract_sid_and_sidts(&fetch());
            return Ok((
                sid2.or(sid).unwrap_or_default(),
                sidts2.or(sidts).unwrap_or_default(),
            ));
        }
        sleep_fn(interval);
    }
}

fn home_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
    } else {
        std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
    }
}

pub fn cache_dir() -> PathBuf {
    home_dir().join(".gemini_local")
}

pub fn cookie_file() -> PathBuf {
    cache_dir().join("cookies.json")
}

pub fn profile_dir() -> PathBuf {
    cache_dir().join("browser_profile")
}

pub fn media_dir() -> PathBuf {
    cache_dir().join("media")
}

fn executable(path: &Path) -> bool {
    path.is_file() && {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(path).map(|meta| meta.permissions().mode() & 0o111 != 0).unwrap_or(false)
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
        let program_x86 = std::env::var("PROGRAMFILES(X86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
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

pub fn collect_session_cookies(
    profile_dir: &Path,
    timeout_secs: u64,
    browsers: Option<Vec<(String, PathBuf)>>,
    browser_pid: &AtomicU32,
) -> Result<(String, String), BrowserError> {
    let browsers = browsers.unwrap_or_else(find_browsers);
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
        let result = if kind == "firefox" {
            login_firefox(&exe, profile_dir, timeout_secs, browser_pid)
        } else if kind == "safari" {
            login_safari(timeout_secs, browser_pid)
        } else {
            login_chromium(&exe, profile_dir, timeout_secs, browser_pid)
        };
        match result {
            Ok(pair) => return Ok(pair),
            Err(BrowserError::Launch(message)) => {
                failures.push(format!("{}: {message}", exe.file_name().and_then(|n| n.to_str()).unwrap_or("browser")));
            }
            Err(other) => return Err(other),
        }
    }
    Err(BrowserError::Message(format!(
        "Could not open a browser for sign-in.\n{}",
        failures.join("\n")
    )))
}

fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn production_wait<F, S>(fetch: F, still_open: S, timeout_secs: u64) -> Result<(String, String), BrowserError>
where
    F: FnMut() -> Vec<Cookie>,
    S: FnMut() -> Result<bool, BrowserError>,
{
    wait_for_google_cookies(
        fetch,
        still_open,
        timeout_secs as f64,
        1.0,
        |seconds| thread::sleep(Duration::from_secs_f64(seconds.max(0.0))),
        monotonic,
    )
}

fn free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
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
    tie_browser_to_parent(std::process::id(), child.id());
    Ok(child)
}

fn tie_browser_to_parent(parent: u32, browser: u32) {
    let script = format!(
        "while kill -0 {parent} 2>/dev/null && kill -0 {browser} 2>/dev/null; do sleep 0.5; done; \
         if ! kill -0 {parent} 2>/dev/null && kill -0 {browser} 2>/dev/null; then \
         kill -TERM {browser} 2>/dev/null; sleep 1; kill -KILL {browser} 2>/dev/null; fi"
    );
    let mut cmd = Command::new("sh");
    cmd.args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

fn terminate_child(child: &mut std::process::Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn login_chromium(
    exe: &Path,
    profile_dir: &Path,
    timeout_secs: u64,
    browser_pid: &AtomicU32,
) -> Result<(String, String), BrowserError> {
    let profile = profile_dir.join("chromium");
    fs::create_dir_all(&profile).map_err(|err| BrowserError::Launch(err.to_string()))?;
    let port = free_port().map_err(|err| BrowserError::Launch(err.to_string()))?;
    let argv = vec![
        exe.to_string_lossy().to_string(),
        format!("--user-data-dir={}", profile.display()),
        format!("--remote-debugging-port={port}"),
        "--remote-allow-origins=*".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-blink-features=AutomationControlled".into(),
        "https://gemini.google.com/".into(),
    ];
    let mut proc = spawn_browser(&argv, browser_pid).map_err(|err| BrowserError::Launch(err.to_string()))?;
    eprintln!("Sign in to Gemini in the browser window. It closes itself once the session is stored.");
    let devtools = wait_for_devtools_port(&profile, &mut proc);
    let port_for_close = devtools.as_ref().ok().copied();
    let result = (|| {
        let devtools_port = devtools?;
        let opened = Instant::now();
        production_wait(
            || chromium_cookies(devtools_port).unwrap_or_default(),
            || {
                if proc.try_wait().ok().flatten().is_some() {
                    return Ok(false);
                }
                if opened.elapsed() < Duration::from_secs(5) {
                    return Ok(true);
                }
                match devtools_pages(devtools_port) {
                    Ok(pages) => Ok(!pages.is_empty()),
                    Err(_) => Ok(proc.try_wait().ok().flatten().is_none()),
                }
            },
            timeout_secs,
        )
    })();
    close_chromium(&mut proc, port_for_close);
    browser_pid.store(0, Ordering::SeqCst);
    result
}

fn login_firefox(
    exe: &Path,
    profile_dir: &Path,
    timeout_secs: u64,
    browser_pid: &AtomicU32,
) -> Result<(String, String), BrowserError> {
    let profile = profile_dir.join("firefox");
    fs::create_dir_all(&profile).map_err(|err| BrowserError::Launch(err.to_string()))?;
    let user_js = profile.join("user.js");
    if !user_js.exists() {
        let _ = fs::write(
            &user_js,
            "user_pref(\"browser.shell.checkDefaultBrowser\", false);\n\
             user_pref(\"datareporting.policy.dataSubmissionEnabled\", false);\n",
        );
    }
    let argv = vec![
        exe.to_string_lossy().to_string(),
        "--profile".into(),
        profile.display().to_string(),
        "--no-remote".into(),
        "--new-instance".into(),
        "https://gemini.google.com/".into(),
    ];
    let mut proc = spawn_browser(&argv, browser_pid).map_err(|err| BrowserError::Launch(err.to_string()))?;
    eprintln!("Sign in to Gemini in the browser window. It closes itself once the session is stored.");
    let db_path = profile.join("cookies.sqlite");
    let started = Instant::now();
    let result = production_wait(
        || {
            if !db_path.exists() {
                return Vec::new();
            }
            read_firefox_cookies(&db_path).unwrap_or_default()
        },
        || {
            if proc.try_wait().ok().flatten().is_none() {
                return Ok(true);
            }
            if started.elapsed() < Duration::from_secs(3) {
                return Err(BrowserError::Launch("browser exited immediately".into()));
            }
            Ok(false)
        },
        timeout_secs,
    );
    terminate_child(&mut proc);
    browser_pid.store(0, Ordering::SeqCst);
    result
}

fn safari_cookie_file() -> Option<PathBuf> {
    let candidates = [
        home_dir().join("Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies"),
        home_dir().join("Library/Cookies/Cookies.binarycookies"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

fn cstr(buf: &[u8], offset: i32) -> Result<String, ()> {
    if offset < 0 || offset as usize >= buf.len() {
        return Err(());
    }
    let start = offset as usize;
    let end = buf[start..].iter().position(|byte| *byte == 0).ok_or(())? + start;
    Ok(String::from_utf8_lossy(&buf[start..end]).into_owned())
}

pub fn read_safari_cookies(path: &Path) -> Result<Vec<Cookie>, String> {
    let data = fs::read(path).map_err(|err| err.to_string())?;
    if data.len() < 8 || &data[..4] != b"cook" {
        return Err("not a Safari cookie file".into());
    }
    let num_pages = i32::from_be_bytes(data[4..8].try_into().unwrap());
    let header_end = 8 + 4 * num_pages as usize;
    if num_pages < 0 || header_end > data.len() {
        return Err("truncated Safari cookie file".into());
    }
    let mut cookies = Vec::new();
    let mut offset = header_end;
    for index in 0..num_pages as usize {
        let size = i32::from_be_bytes(data[8 + 4 * index..12 + 4 * index].try_into().unwrap());
        if size < 8 || offset + size as usize > data.len() {
            break;
        }
        let page = &data[offset..offset + size as usize];
        offset += size as usize;
        if page.len() < 8 || &page[..4] != b"\x00\x00\x01\x00" {
            continue;
        }
        let num_cookies = i32::from_le_bytes(page[4..8].try_into().unwrap());
        if num_cookies < 0 || 8 + 4 * num_cookies as usize > page.len() {
            continue;
        }
        for cookie_index in 0..num_cookies as usize {
            let start = i32::from_le_bytes(page[8 + 4 * cookie_index..12 + 4 * cookie_index].try_into().unwrap());
            if start < 0 || start as usize + 32 > page.len() {
                continue;
            }
            let cookie_size = i32::from_le_bytes(page[start as usize..start as usize + 4].try_into().unwrap());
            if cookie_size < 32 || start as usize + cookie_size as usize > page.len() {
                continue;
            }
            let cookie = &page[start as usize..start as usize + cookie_size as usize];
            let url_off = i32::from_le_bytes(cookie[16..20].try_into().unwrap());
            let name_off = i32::from_le_bytes(cookie[20..24].try_into().unwrap());
            let value_off = i32::from_le_bytes(cookie[28..32].try_into().unwrap());
            let (Ok(host), Ok(name), Ok(value)) = (cstr(cookie, url_off), cstr(cookie, name_off), cstr(cookie, value_off)) else {
                continue;
            };
            cookies.push(Cookie { domain: host, name, value });
        }
    }
    Ok(cookies)
}

fn safari_is_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Safari"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn readable_safari_cookies() -> Option<(String, String)> {
    let path = safari_cookie_file()?;
    let cookies = read_safari_cookies(&path).ok()?;
    let (sid, sidts) = extract_sid_and_sidts(&cookies);
    Some((sid?, sidts?))
}

pub fn webdriver_call(port: u16, method: &str, path: &str, payload: Option<&str>, timeout: Duration) -> Result<serde_json::Value, BrowserError> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| BrowserError::Launch(err.to_string()))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let body = payload.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).map_err(|err| BrowserError::Launch(err.to_string()))?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|err| BrowserError::Launch(err.to_string()))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let status_line = head.lines().next().unwrap_or("");
    let code = status_line.split_whitespace().nth(1).unwrap_or("0");
    if !code.starts_with('2') {
        let mut message = rest.to_string();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(rest) {
            if let Some(value) = parsed.get("value") {
                if let Some(text) = value.get("message").and_then(|v| v.as_str()).or_else(|| value.get("error").and_then(|v| v.as_str())) {
                    message = text.to_string();
                }
            }
        }
        let message = message.chars().take(500).collect::<String>();
        return Err(BrowserError::Launch(message));
    }
    if rest.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(rest).map_err(|err| BrowserError::Launch(err.to_string()))
}

fn wait_for_local_port(port: u16, proc: &mut std::process::Child, timeout: Duration) -> Result<(), BrowserError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if proc.try_wait().ok().flatten().is_some() {
            return Err(BrowserError::Launch("safaridriver exited before it was ready".into()));
        }
        if TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_millis(500)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(BrowserError::Launch("timed out waiting for safaridriver".into()))
}

fn login_safari_driver(timeout_secs: u64, browser_pid: &AtomicU32) -> Result<(String, String), BrowserError> {
    let driver = PathBuf::from("/usr/bin/safaridriver");
    if !executable(&driver) {
        return Err(BrowserError::Launch(
            "Safari is installed, but safaridriver is missing. Set GEMINI_PSID and GEMINI_PSIDTS to sign in.".into(),
        ));
    }
    let port = free_port().map_err(|err| BrowserError::Launch(err.to_string()))?;
    let mut proc = spawn_browser(&[driver.to_string_lossy().to_string(), "-p".into(), port.to_string()], browser_pid)
        .map_err(|err| BrowserError::Launch(err.to_string()))?;
    let mut session_id = None;
    let result = (|| {
        wait_for_local_port(port, &mut proc, Duration::from_secs(20))?;
        let created = webdriver_call(
            port,
            "POST",
            "/session",
            Some(r#"{"capabilities":{"alwaysMatch":{"browserName":"safari"}}}"#),
            Duration::from_secs(30),
        )?;
        let value = created.get("value").cloned().unwrap_or(created.clone());
        let id = value.get("sessionId").and_then(|v| v.as_str()).map(str::to_string);
        let Some(id) = id else {
            return Err(BrowserError::Launch(
                "Safari did not start a sign-in window. In Safari, open Settings > Advanced, \
                 turn on 'Show features for web developers', then Develop > Allow Remote Automation."
                    .into(),
            ));
        };
        session_id = Some(id.clone());
        webdriver_call(
            port,
            "POST",
            &format!("/session/{id}/url"),
            Some(r#"{"url":"https://gemini.google.com/"}"#),
            Duration::from_secs(30),
        )?;
        eprintln!("Sign in to Gemini in the Safari window. It closes itself once the session is stored.");
        production_wait(
            || {
                webdriver_call(port, "GET", &format!("/session/{id}/cookie"), None, Duration::from_secs(5))
                    .ok()
                    .and_then(|result| result.get("value").cloned())
                    .and_then(|value| serde_json::from_value::<Vec<serde_json::Value>>(value).ok())
                    .map(cookies_from_json)
                    .unwrap_or_default()
            },
            || Ok(proc.try_wait().ok().flatten().is_none()),
            timeout_secs,
        )
    })();
    if let Some(id) = session_id {
        let _ = webdriver_call(port, "DELETE", &format!("/session/{id}"), None, Duration::from_secs(5));
    }
    terminate_child(&mut proc);
    browser_pid.store(0, Ordering::SeqCst);
    result
}

fn cookies_from_json(values: Vec<serde_json::Value>) -> Vec<Cookie> {
    values
        .into_iter()
        .filter_map(|value| {
            Some(Cookie {
                name: value.get("name")?.as_str()?.to_string(),
                value: value.get("value")?.as_str()?.to_string(),
                domain: value.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn login_safari(timeout_secs: u64, browser_pid: &AtomicU32) -> Result<(String, String), BrowserError> {
    if let Some(saved) = readable_safari_cookies() {
        return Ok(saved);
    }
    let path = safari_cookie_file();
    let file_readable = path.as_ref().and_then(|path| read_safari_cookies(path).ok()).is_some();
    match login_safari_driver(timeout_secs, browser_pid) {
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
    let _ = Command::new("open")
        .args(["-a", "Safari", "https://gemini.google.com/"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    production_wait(
        || read_safari_cookies(&path).unwrap_or_default(),
        || Ok(safari_is_running()),
        timeout_secs,
    )
}

pub fn read_firefox_cookies(path: &Path) -> Result<Vec<Cookie>, String> {
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let uri = format!("file://{}?immutable=1", absolute.display());
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let conn = rusqlite::Connection::open_with_flags(uri, flags).map_err(|err| err.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT host, name, value FROM moz_cookies \
             WHERE name IN ('__Secure-1PSID', '__Secure-1PSIDTS')",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(Cookie {
                domain: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut cookies = Vec::new();
    for row in rows {
        cookies.push(row.map_err(|err| err.to_string())?);
    }
    Ok(cookies)
}

fn chromium_cookies(port: u16) -> Result<Vec<Cookie>, String> {
    let pages = devtools_pages(port)?;
    let ws_url = pages.iter().find_map(|page| {
        if page.get("type").and_then(|v| v.as_str()) == Some("page") {
            page.get("webSocketDebuggerUrl").and_then(|v| v.as_str()).map(str::to_string)
        } else {
            None
        }
    });
    let Some(ws_url) = ws_url else {
        return Ok(Vec::new());
    };
    let mut client = CdpClient::connect(&ws_url, Duration::from_secs(5))?;
    let result: Result<serde_json::Value, String> = (|| {
        client.call("Network.enable", None, Duration::from_secs(5))?;
        let listed = client.call(
            "Network.getCookies",
            Some(serde_json::json!({ "urls": COOKIE_URLS })),
            Duration::from_secs(5),
        )?;
        let cookies = listed.get("cookies").cloned().unwrap_or(serde_json::json!([]));
        if cookies.as_array().map(|items| !items.is_empty()).unwrap_or(false) {
            return Ok(cookies);
        }
        let all = client.call("Network.getAllCookies", None, Duration::from_secs(5))?;
        Ok(all.get("cookies").cloned().unwrap_or(serde_json::json!([])))
    })();
    client.close();
    let value = result?;
    let values = serde_json::from_value::<Vec<serde_json::Value>>(value).unwrap_or_default();
    Ok(cookies_from_json(values))
}

fn devtools_pages(port: u16) -> Result<Vec<serde_json::Value>, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| err.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let request = format!("GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).map_err(|err| err.to_string())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let body = text.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    Ok(parsed.as_array().cloned().unwrap_or_default())
}

fn wait_for_devtools_port(profile: &Path, proc: &mut std::process::Child) -> Result<u16, BrowserError> {
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if proc.try_wait().ok().flatten().is_some() {
            return Err(BrowserError::Launch("browser exited before the debugging port was ready".into()));
        }
        if let Ok(text) = fs::read_to_string(&port_file) {
            if let Some(line) = text.lines().next() {
                if let Ok(port) = line.trim().parse::<u16>() {
                    return Ok(port);
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    if proc.try_wait().ok().flatten().is_some() {
        return Err(BrowserError::Launch("browser exited before the debugging port was ready".into()));
    }
    Err(BrowserError::Launch("timed out waiting for the debugging port".into()))
}

fn close_chromium(proc: &mut std::process::Child, port: Option<u16>) {
    if let Some(port) = port {
        if proc.try_wait().ok().flatten().is_none() {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let request = format!("GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(request.as_bytes());
                let mut raw = Vec::new();
                let _ = stream.read_to_end(&mut raw);
                let text = String::from_utf8_lossy(&raw);
                if let Some(body) = text.split_once("\r\n\r\n").map(|(_, body)| body) {
                    if let Ok(info) = serde_json::from_str::<serde_json::Value>(body) {
                        if let Some(ws_url) = info.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                            if let Ok(mut client) = CdpClient::connect(ws_url, Duration::from_secs(2)) {
                                let _ = client.call("Browser.close", None, Duration::from_secs(2));
                                client.close();
                            }
                        }
                    }
                }
            }
        }
    }
    terminate_child(proc);
}

pub fn pop_ws_frame(buf: &[u8]) -> Option<(bool, u8, Vec<u8>, Vec<u8>)> {
    if buf.len() < 2 {
        return None;
    }
    let masked = buf[1] & 0x80 != 0;
    let mut length = (buf[1] & 0x7f) as usize;
    let mut offset = 2;
    if length == 126 {
        if buf.len() < offset + 2 {
            return None;
        }
        length = u16::from_be_bytes(buf[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
    } else if length == 127 {
        if buf.len() < offset + 8 {
            return None;
        }
        length = u64::from_be_bytes(buf[offset..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
    }
    let mask = if masked {
        if buf.len() < offset + 4 {
            return None;
        }
        let mask = buf[offset..offset + 4].to_vec();
        offset += 4;
        mask
    } else {
        Vec::new()
    };
    let end = offset + length;
    if buf.len() < end {
        return None;
    }
    let mut payload = buf[offset..end].to_vec();
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index & 3];
        }
    }
    Some((buf[0] & 0x80 != 0, buf[0] & 0x0f, payload, buf[end..].to_vec()))
}

pub fn encode_client_frame(payload: &[u8], opcode: u8) -> Vec<u8> {
    let mask = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.subsec_nanos())
            .unwrap_or(1);
        nanos.to_le_bytes()
    };
    let masked: Vec<u8> = payload.iter().enumerate().map(|(index, byte)| byte ^ mask[index & 3]).collect();
    let first = 0x80 | (opcode & 0x0f);
    let mut header = Vec::new();
    let n = payload.len();
    if n < 126 {
        header.extend_from_slice(&[first, 0x80 | n as u8]);
    } else if n < 65536 {
        header.extend_from_slice(&[first, 0x80 | 126]);
        header.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        header.extend_from_slice(&[first, 0x80 | 127]);
        header.extend_from_slice(&(n as u64).to_be_bytes());
    }
    header.extend_from_slice(&mask);
    header.extend_from_slice(&masked);
    header
}

pub struct CdpClient {
    stream: TcpStream,
    buf: Vec<u8>,
    next_id: u64,
}

impl CdpClient {
    pub fn connect(ws_url: &str, timeout: Duration) -> Result<Self, String> {
        let (host, port, path) = split_ws_url(ws_url)?;
        let mut stream = TcpStream::connect((host.as_str(), port)).map_err(|err| err.to_string())?;
        stream.set_read_timeout(Some(timeout)).ok();
        stream.set_write_timeout(Some(timeout)).ok();
        let key_bytes = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos().to_le_bytes())
            .unwrap_or([1; 16]);
        let key = STANDARD.encode(key_bytes);
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: http://{host}:{port}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).map_err(|err| err.to_string())?;
        let mut client = Self { stream, buf: Vec::new(), next_id: 0 };
        let header = client.read_until(b"\r\n\r\n")?;
        let status = header.split(|byte| *byte == b'\n').next().unwrap_or_default();
        if !String::from_utf8_lossy(status).contains(" 101 ") {
            return Err(String::from_utf8_lossy(status).into_owned());
        }
        let mut hasher = Sha1::new();
        hasher.update(key.as_bytes());
        hasher.update(WS_GUID.as_bytes());
        let accept = STANDARD.encode(hasher.finalize());
        if !header.windows(accept.len()).any(|window| window == accept.as_bytes()) {
            return Err("websocket accept mismatch".into());
        }
        Ok(client)
    }

    pub fn call(&mut self, method: &str, params: Option<serde_json::Value>, timeout: Duration) -> Result<serde_json::Value, String> {
        self.next_id += 1;
        let message_id = self.next_id;
        let mut message = serde_json::json!({ "id": message_id, "method": method });
        if let Some(params) = params {
            message["params"] = params;
        }
        self.stream
            .write_all(&encode_client_frame(message.to_string().as_bytes(), 0x1))
            .map_err(|err| err.to_string())?;
        let deadline = Instant::now() + timeout;
        let mut fragments = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(method.into());
            }
            self.stream.set_read_timeout(Some(remaining)).ok();
            let (fin, opcode, payload) = self.read_frame()?;
            if opcode == 0x9 {
                self.stream
                    .write_all(&encode_client_frame(&payload, 0xA))
                    .map_err(|err| err.to_string())?;
                continue;
            }
            if opcode == 0xA {
                continue;
            }
            if opcode == 0x8 {
                return Err("websocket closed".into());
            }
            if matches!(opcode, 0x0 | 0x1 | 0x2) {
                fragments.extend(payload);
                if !fin {
                    continue;
                }
                let data: serde_json::Value = serde_json::from_slice(&fragments).map_err(|err| err.to_string())?;
                fragments.clear();
                if data.get("id").and_then(|v| v.as_u64()) != Some(message_id) {
                    continue;
                }
                if let Some(error) = data.get("error") {
                    return Err(error.to_string());
                }
                return Ok(data.get("result").cloned().unwrap_or(serde_json::json!({})));
            }
        }
    }

    pub fn close(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    fn read_until(&mut self, marker: &[u8]) -> Result<Vec<u8>, String> {
        while !self.buf.windows(marker.len()).any(|window| window == marker) {
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).map_err(|err| err.to_string())?;
            if read == 0 {
                return Err("socket closed".into());
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
        let index = self.buf.windows(marker.len()).position(|window| window == marker).unwrap() + marker.len();
        let out = self.buf[..index].to_vec();
        self.buf.drain(..index);
        Ok(out)
    }

    fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), String> {
        loop {
            if let Some((fin, opcode, payload, rest)) = pop_ws_frame(&self.buf) {
                self.buf = rest;
                return Ok((fin, opcode, payload));
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).map_err(|err| err.to_string())?;
            if read == 0 {
                return Err("socket closed".into());
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
    }
}

fn split_ws_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("ws://").ok_or_else(|| url.to_string())?;
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{path}");
    if let Some((host, port)) = hostport.rsplit_once(':') {
        let port = port.parse::<u16>().map_err(|_| url.to_string())?;
        Ok((host.to_string(), port, path))
    } else {
        Ok((hostport.to_string(), 80, path))
    }
}

pub fn load_saved_cookies() -> Option<(String, String)> {
    let text = fs::read_to_string(cookie_file()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sid = value.get("sid").and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty())?;
    let sidts = value.get("sidts").and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty())?;
    Some((sid.to_string(), sidts.to_string()))
}

pub fn cookies_from_env() -> Option<(String, String)> {
    let sid = std::env::var("GEMINI_PSID").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())?;
    let sidts = std::env::var("GEMINI_PSIDTS").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())?;
    Some((sid, sidts))
}

pub fn save_cookies(sid: &str, sidts: &str) -> std::io::Result<()> {
    fs::create_dir_all(cache_dir())?;
    let body = serde_json::json!({ "sid": sid, "sidts": sidts });
    fs::write(cookie_file(), body.to_string())
}

pub fn clear_stale_session() {
    let _ = fs::remove_file(cookie_file());
    let webapi = cache_dir().join("gemini_webapi_cache");
    let _ = fs::remove_dir_all(&webapi);
    let _ = fs::create_dir_all(&webapi);
    let _ = fs::remove_dir_all(profile_dir());
}

pub fn acquire_cookies(force_refresh: bool, browser_pid: &AtomicU32) -> Result<(String, String), BrowserError> {
    if !force_refresh {
        if let Some(saved) = load_saved_cookies() {
            return Ok(saved);
        }
        if let Some(from_env) = cookies_from_env() {
            return Ok(from_env);
        }
    } else {
        clear_stale_session();
    }
    eprintln!("Opening a browser to sign in to Gemini...");
    let pair = collect_session_cookies(&profile_dir(), 300, None, browser_pid)?;
    save_cookies(&pair.0, &pair.1).map_err(|err| BrowserError::Message(err.to_string()))?;
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, value: &str, domain: &str) -> Cookie {
        Cookie { name: name.into(), value: value.into(), domain: domain.into() }
    }

    #[test]
    fn closed_browser_is_reported() {
        let err = wait_for_google_cookies(|| Vec::new(), || Ok(false), 5.0, 1.0, |_| {}, || 0.0).unwrap_err();
        assert!(err.to_string().contains("Browser window was closed"));
    }

    #[test]
    fn timeout_uses_injected_clock() {
        let clock = std::cell::Cell::new(0.0);
        let err = wait_for_google_cookies(
            || Vec::new(),
            || Ok(true),
            5.0,
            1.0,
            |seconds| clock.set(clock.get() + seconds),
            || clock.get(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("5 minutes"));
        assert!(clock.get() > 5.0);
    }

    #[test]
    fn waits_for_both_cookies_then_rereads() {
        let calls = std::cell::Cell::new(0);
        let (sid, sidts) = wait_for_google_cookies(
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    vec![cookie("__Secure-1PSID", "sid1", ".google.com")]
                } else {
                    vec![
                        cookie("__Secure-1PSID", "sid1", ".google.com"),
                        cookie("__Secure-1PSIDTS", "sidts1", ".google.com"),
                    ]
                }
            },
            || Ok(true),
            30.0,
            1.0,
            |_| {},
            || 0.0,
        )
        .unwrap();
        assert_eq!((sid, sidts), ("sid1".into(), "sidts1".into()));
        assert!(calls.get() >= 3);
    }

    #[test]
    fn immediate_browser_exit_can_fall_through() {
        let err = wait_for_google_cookies(
            || Vec::new(),
            || Err(BrowserError::Launch("browser exited immediately".into())),
            5.0,
            1.0,
            |_| {},
            || 0.0,
        )
        .unwrap_err();
        assert!(matches!(err, BrowserError::Launch(_)));
    }

    fn safari_cookie(host: &str, name: &str, value: &str) -> Vec<u8> {
        let strings = [
            format!("{host}\0").into_bytes(),
            format!("{name}\0").into_bytes(),
            b"/\0".to_vec(),
            format!("{value}\0").into_bytes(),
        ];
        let mut offsets = Vec::new();
        let mut blob = Vec::new();
        let mut cursor = 32i32;
        for item in strings {
            offsets.push(cursor);
            cursor += item.len() as i32;
            blob.extend(item);
        }
        let size = 32 + blob.len() as i32;
        let mut header = Vec::new();
        for number in [size, 0, 0, 0, offsets[0], offsets[1], offsets[2], offsets[3]] {
            header.extend_from_slice(&number.to_le_bytes());
        }
        header.extend(blob);
        header
    }

    fn safari_file(cookies: &[Vec<u8>]) -> Vec<u8> {
        let mut offsets: Vec<i32> = Vec::new();
        let mut body: Vec<u8> = Vec::new();
        let mut start = 4 + 4 + 4 * cookies.len() as i32;
        for cookie in cookies {
            offsets.push(start);
            body.extend(cookie);
            start += cookie.len() as i32;
        }
        let mut page = b"\x00\x00\x01\x00".to_vec();
        page.extend_from_slice(&(cookies.len() as i32).to_le_bytes());
        for offset in offsets {
            page.extend_from_slice(&offset.to_le_bytes());
        }
        page.extend(body);
        let mut file = b"cook".to_vec();
        file.extend_from_slice(&1i32.to_be_bytes());
        file.extend_from_slice(&(page.len() as i32).to_be_bytes());
        file.extend(page);
        file
    }

    #[test]
    fn safari_file_keeps_google_cookies() {
        let raw = safari_file(&[
            safari_cookie(".google.com", "__Secure-1PSID", "sid"),
            safari_cookie(".google.com", "__Secure-1PSIDTS", "sidts"),
            safari_cookie(".youtube.com", "__Secure-1PSIDTS", "ignore"),
        ]);
        let dir = std::env::temp_dir().join(format!("gemini-safari-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("Cookies.binarycookies");
        fs::write(&path, raw).unwrap();
        let cookies = read_safari_cookies(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);
        let (sid, sidts) = extract_sid_and_sidts(&cookies);
        assert_eq!(sid.as_deref(), Some("sid"));
        assert_eq!(sidts.as_deref(), Some("sidts"));
    }

    #[test]
    fn firefox_database_keeps_google_cookies() {
        let dir = std::env::temp_dir().join(format!("gemini-firefox-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("cookies.sqlite");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT)", []).unwrap();
        conn.execute("INSERT INTO moz_cookies VALUES (?1, ?2, ?3)", [".google.com", "__Secure-1PSID", "sid"]).unwrap();
        conn.execute("INSERT INTO moz_cookies VALUES (?1, ?2, ?3)", [".google.com", "__Secure-1PSIDTS", "sidts"]).unwrap();
        conn.execute("INSERT INTO moz_cookies VALUES (?1, ?2, ?3)", [".youtube.com", "__Secure-1PSIDTS", "ignore"]).unwrap();
        drop(conn);
        let cookies = read_firefox_cookies(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);
        let (sid, sidts) = extract_sid_and_sidts(&cookies);
        assert_eq!(sid.as_deref(), Some("sid"));
        assert_eq!(sidts.as_deref(), Some("sidts"));
    }

    #[test]
    fn missing_browser_explains_the_next_step() {
        let err = collect_session_cookies(Path::new("/tmp/does-not-matter"), 5, Some(vec![]), &AtomicU32::new(0)).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("No Chrome"));
        assert!(text.contains("GEMINI_PSID"));
    }

    #[test]
    fn client_frame_roundtrip() {
        let encoded = encode_client_frame(br#"{"id":1}"#, 0x1);
        let (fin, opcode, payload, rest) = pop_ws_frame(&encoded).unwrap();
        assert!(fin);
        assert_eq!(opcode, 0x1);
        assert_eq!(payload, br#"{"id":1}"#);
        assert!(rest.is_empty());
    }

    #[test]
    fn webdriver_reads_cookies_and_surfaces_automation_error() {
        fn serve_once(status: &str, body: &str) -> u16 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let status = status.to_string();
            let body = body.to_string();
            thread::spawn(move || {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = conn.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..read]);
                }
                let payload = body.into_bytes();
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&payload);
            });
            port
        }

        let body = serde_json::json!({"value": [
            {"name": "__Secure-1PSID", "value": "sid", "domain": ".google.com"},
            {"name": "__Secure-1PSIDTS", "value": "sidts", "domain": ".google.com"}
        ]}).to_string();
        let port = serve_once("200 OK", &body);
        thread::sleep(Duration::from_millis(50));
        let result = webdriver_call(port, "GET", "/session/abc/cookie", None, Duration::from_secs(5)).unwrap();
        let cookies = cookies_from_json(result["value"].as_array().cloned().unwrap_or_default());
        let (sid, sidts) = extract_sid_and_sidts(&cookies);
        assert_eq!(sid.as_deref(), Some("sid"));
        assert_eq!(sidts.as_deref(), Some("sidts"));

        let body = serde_json::json!({"value": {"error": "session not created", "message": "Allow Remote Automation"}}).to_string();
        let port = serve_once("500 Internal", &body);
        thread::sleep(Duration::from_millis(50));
        let err = webdriver_call(port, "POST", "/session", Some("{}"), Duration::from_secs(5)).unwrap_err();
        assert!(err.to_string().contains("Allow Remote Automation"));
    }
}
