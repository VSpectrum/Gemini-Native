"""Collect __Secure-1PSID and __Secure-1PSIDTS without Playwright.

Sign-in uses a browser that is already installed (Chrome, Edge, Brave, Chromium,
or Firefox) and a profile directory we own. Chromium cookies are read through
the DevTools protocol, which returns them decrypted, so this module stays on
the Python standard library. The user's everyday browser profile is left alone:
rotating these cookies from a shared profile can sign that browser out.
"""

import base64
import hashlib
import json
import os
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_COOKIE_URLS = [
    "https://gemini.google.com/",
    "https://accounts.google.com/",
    "https://www.google.com/",
]


class BrowserLaunchError(RuntimeError):
    """The chosen browser never opened a window we could read."""


def extract_sid_and_sidts(cookies):
    """Extract valid __Secure-1PSID and __Secure-1PSIDTS for .google.com."""
    secure_1psid = None
    secure_1psidts = None
    for cookie in cookies:
        name = cookie.get("name")
        domain = cookie.get("domain", "")
        value = cookie.get("value", "")
        if not value or not isinstance(value, str) or not value.strip():
            continue
        # youtube.com also sets these names; Gemini only accepts the google.com pair.
        if domain.endswith("google.com"):
            if name == "__Secure-1PSID":
                secure_1psid = value.strip()
            elif name == "__Secure-1PSIDTS":
                secure_1psidts = value.strip()
    return secure_1psid, secure_1psidts


def wait_for_google_cookies(fetch, still_open, timeout=300, interval=1.0, sleep=time.sleep, clock=time.monotonic):
    """Poll until both session cookies exist, then re-read once so a redirect can settle.

    `fetch` returns a cookie list. `still_open` is false when the user dismissed the window.
    `sleep` and `clock` are injectable so tests do not wait on a real timer.
    """
    start = clock()
    while True:
        if not still_open():
            raise RuntimeError("Browser window was closed before sign-in completed.")
        if clock() - start > timeout:
            raise TimeoutError("Timed out waiting for sign-in cookies (5 minutes exceeded).")
        sid, sidts = extract_sid_and_sidts(fetch())
        if sid and sidts:
            sleep(1.0)
            if not still_open():
                raise RuntimeError("Browser window was closed before sign-in completed.")
            sid2, sidts2 = extract_sid_and_sidts(fetch())
            return sid2 or sid, sidts2 or sidts
        sleep(interval)


def find_browsers():
    """Installed browsers, Chromium-family first, as (kind, executable) pairs."""
    found = []
    seen = set()

    def add(kind, path):
        if not path:
            return
        resolved = str(path)
        if resolved in seen:
            return
        candidate = Path(resolved)
        if candidate.is_file() and os.access(candidate, os.X_OK):
            seen.add(resolved)
            found.append((kind, resolved))

    if sys.platform == "darwin":
        mac = [
            ("chromium", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            ("chromium", "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            ("chromium", "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
            ("chromium", "/Applications/Chromium.app/Contents/MacOS/Chromium"),
            ("firefox", "/Applications/Firefox.app/Contents/MacOS/firefox"),
        ]
        for kind, path in mac:
            add(kind, path)
    elif sys.platform == "win32":
        local = os.environ.get("LOCALAPPDATA", "")
        program = os.environ.get("PROGRAMFILES", r"C:\Program Files")
        program_x86 = os.environ.get("PROGRAMFILES(X86)", r"C:\Program Files (x86)")
        win = [
            ("chromium", os.path.join(program, r"Google\Chrome\Application\chrome.exe")),
            ("chromium", os.path.join(program_x86, r"Google\Chrome\Application\chrome.exe")),
            ("chromium", os.path.join(local, r"Google\Chrome\Application\chrome.exe")),
            ("chromium", os.path.join(program, r"Microsoft\Edge\Application\msedge.exe")),
            ("chromium", os.path.join(program_x86, r"Microsoft\Edge\Application\msedge.exe")),
            ("chromium", os.path.join(local, r"BraveSoftware\Brave-Browser\Application\brave.exe")),
            ("firefox", os.path.join(program, r"Mozilla Firefox\firefox.exe")),
            ("firefox", os.path.join(program_x86, r"Mozilla Firefox\firefox.exe")),
        ]
        for kind, path in win:
            add(kind, path)
    else:
        for name in (
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "brave-browser",
            "chromium",
            "chromium-browser",
        ):
            add("chromium", shutil.which(name))
        add("firefox", shutil.which("firefox"))

    if sys.platform != "win32":
        for name in (
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "brave-browser",
            "chromium",
            "chromium-browser",
        ):
            add("chromium", shutil.which(name))
        add("firefox", shutil.which("firefox"))
    return found


def collect_session_cookies(profile_dir, timeout=300, browsers=None):
    """Open a private browser window and return (sid, sidts)."""
    browsers = list(find_browsers() if browsers is None else browsers)
    if not browsers:
        raise RuntimeError(
            "No Chrome, Edge, Brave, Chromium, or Firefox is installed. "
            "Install one of them and send the message again to sign in. "
            "Or set GEMINI_PSID and GEMINI_PSIDTS."
        )
    failures = []
    for kind, exe in browsers:
        try:
            if kind == "firefox":
                return _login_firefox(exe, Path(profile_dir), timeout)
            return _login_chromium(exe, Path(profile_dir), timeout)
        except BrowserLaunchError as exc:
            failures.append(f"{Path(exe).name}: {exc}")
            continue
    raise RuntimeError(
        "Could not open a browser for sign-in.\n" + "\n".join(failures)
    )


def _login_chromium(exe, profile_dir, timeout):
    profile = Path(profile_dir) / "chromium"
    profile.mkdir(parents=True, exist_ok=True)
    port = _free_port()
    proc = _spawn([
        exe,
        f"--user-data-dir={profile}",
        f"--remote-debugging-port={port}",
        "--remote-allow-origins=*",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-blink-features=AutomationControlled",
        "https://gemini.google.com/",
    ])
    print(
        "Sign in to Gemini in the browser window. It closes itself once the session is stored.",
        file=sys.stderr,
    )
    devtools_port = None
    try:
        devtools_port = _wait_for_devtools_port(profile, proc)
        if devtools_port is None:
            raise BrowserLaunchError("browser exited before the debugging port was ready")
        opened = time.monotonic()

        def fetch():
            try:
                return _chromium_cookies(devtools_port)
            except (OSError, ConnectionError, TimeoutError, RuntimeError, json.JSONDecodeError):
                return []

        def still_open():
            if proc.poll() is not None:
                return False
            # The window can take a moment to appear. After that, no pages means it was closed.
            # macOS Chrome stays running with zero windows, so the process alone is not enough.
            if time.monotonic() - opened < 5:
                return True
            try:
                return len(_devtools_pages(devtools_port)) > 0
            except (OSError, ConnectionError, TimeoutError, json.JSONDecodeError):
                return proc.poll() is None

        return wait_for_google_cookies(fetch, still_open, timeout=timeout)
    finally:
        _close_chromium(proc, devtools_port)


def _login_firefox(exe, profile_dir, timeout):
    profile = profile_dir / "firefox"
    profile.mkdir(parents=True, exist_ok=True)
    user_js = profile / "user.js"
    if not user_js.exists():
        user_js.write_text(
            'user_pref("browser.shell.checkDefaultBrowser", false);\n'
            'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
        )
    proc = _spawn([
        exe,
        "--profile",
        str(profile),
        "--no-remote",
        "--new-instance",
        "https://gemini.google.com/",
    ])
    print(
        "Sign in to Gemini in the browser window. It closes itself once the session is stored.",
        file=sys.stderr,
    )
    db_path = profile / "cookies.sqlite"
    started = time.monotonic()
    try:
        def fetch():
            if not db_path.exists():
                return []
            try:
                return read_firefox_cookies(db_path)
            except sqlite3.Error:
                return []

        def still_open():
            if proc.poll() is None:
                return True
            # An instant exit means this Firefox cannot run. Try the next browser.
            # A later exit is the user closing the window.
            if time.monotonic() - started < 3:
                raise BrowserLaunchError("browser exited immediately")
            return False

        return wait_for_google_cookies(fetch, still_open, timeout=timeout)
    finally:
        _terminate_process(proc)


def read_firefox_cookies(db_path):
    """Read cookie name/value/domain rows. `immutable=1` tolerates Firefox holding the file."""
    uri = Path(db_path).resolve().as_uri() + "?immutable=1"
    conn = sqlite3.connect(uri, uri=True)
    try:
        rows = conn.execute(
            "SELECT host, name, value FROM moz_cookies "
            "WHERE name IN ('__Secure-1PSID', '__Secure-1PSIDTS')"
        ).fetchall()
    finally:
        conn.close()
    return [{"domain": host, "name": name, "value": value} for host, name, value in rows]


def _chromium_cookies(port):
    pages = _devtools_pages(port)
    ws_url = None
    for page in pages:
        if page.get("type") == "page" and page.get("webSocketDebuggerUrl"):
            ws_url = page["webSocketDebuggerUrl"]
            break
    if not ws_url:
        return []
    client = CdpClient.connect(ws_url, timeout=5)
    try:
        client.call("Network.enable")
        result = client.call("Network.getCookies", {"urls": _COOKIE_URLS})
        cookies = result.get("cookies") or []
        if cookies:
            return cookies
        try:
            result = client.call("Network.getAllCookies")
        except RuntimeError:
            return []
        return result.get("cookies") or []
    finally:
        client.close()


def _devtools_pages(port):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=2) as resp:
        data = json.load(resp)
    return data if isinstance(data, list) else []


def _wait_for_devtools_port(profile_dir, proc, timeout=20):
    port_file = Path(profile_dir) / "DevToolsActivePort"
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return None
        if port_file.exists():
            lines = port_file.read_text(errors="replace").splitlines()
            if lines and lines[0].strip().isdigit():
                return int(lines[0].strip())
        time.sleep(0.1)
    if proc.poll() is not None:
        return None
    raise BrowserLaunchError("timed out waiting for the debugging port")


def _close_chromium(proc, port):
    if port and proc.poll() is None:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=2) as resp:
                info = json.load(resp)
            ws_url = info.get("webSocketDebuggerUrl")
            if ws_url:
                client = CdpClient.connect(ws_url, timeout=2)
                try:
                    client.call("Browser.close", timeout=2)
                finally:
                    client.close()
        except (OSError, ConnectionError, TimeoutError, RuntimeError, json.JSONDecodeError):
            pass
    _terminate_process(proc)


def _spawn(argv):
    kwargs = {
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
    }
    # Stay in the helper's process group so a group kill reaches the browser
    # when the browser does not detach. Chrome often calls setsid anyway.
    proc = subprocess.Popen(argv, **kwargs)
    if sys.platform != "win32":
        _tie_browser_to_parent(proc.pid)
    return proc


def _tie_browser_to_parent(browser_pid):
    """Kill the browser if this process dies, including via SIGKILL.

    A detached shell polls both pids. It exits as soon as the browser exits, so
    it does not linger for the rest of the chat or kill a recycled pid later.
    The shell is in its own session so a group kill of the helper cannot take
    the watcher out before it has noticed.
    """
    parent = os.getpid()
    script = (
        "while kill -0 {parent} 2>/dev/null && kill -0 {browser} 2>/dev/null; do sleep 0.5; done; "
        "if ! kill -0 {parent} 2>/dev/null && kill -0 {browser} 2>/dev/null; then "
        "kill -TERM {browser} 2>/dev/null; sleep 1; kill -KILL {browser} 2>/dev/null; "
        "fi"
    ).format(parent=parent, browser=browser_pid)
    subprocess.Popen(
        ["sh", "-c", script],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


def _terminate_process(proc):
    if proc.poll() is not None:
        return
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    else:
        proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass


def _free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def pop_ws_frame(buf):
    """Pop one WebSocket frame. Returns (fin, opcode, payload, rest) or None if incomplete."""
    if len(buf) < 2:
        return None
    b0, b1 = buf[0], buf[1]
    masked = b1 & 0x80
    length = b1 & 0x7F
    offset = 2
    if length == 126:
        if len(buf) < offset + 2:
            return None
        length = struct.unpack("!H", buf[offset:offset + 2])[0]
        offset += 2
    elif length == 127:
        if len(buf) < offset + 8:
            return None
        length = struct.unpack("!Q", buf[offset:offset + 8])[0]
        offset += 8
    mask = b""
    if masked:
        if len(buf) < offset + 4:
            return None
        mask = buf[offset:offset + 4]
        offset += 4
    end = offset + length
    if len(buf) < end:
        return None
    payload = buf[offset:end]
    if masked:
        payload = bytes(byte ^ mask[i & 3] for i, byte in enumerate(payload))
    return bool(b0 & 0x80), b0 & 0x0F, payload, buf[end:]


def encode_client_frame(payload, opcode=0x1):
    mask = os.urandom(4)
    masked = bytes(byte ^ mask[i & 3] for i, byte in enumerate(payload))
    first = 0x80 | (opcode & 0x0F)
    n = len(payload)
    if n < 126:
        header = bytes((first, 0x80 | n))
    elif n < 65536:
        header = bytes((first, 0x80 | 126)) + struct.pack("!H", n)
    else:
        header = bytes((first, 0x80 | 127)) + struct.pack("!Q", n)
    return header + mask + masked


class CdpClient:
    """Tiny Chrome DevTools client. Cookies are the only call site, so this is not a general driver."""

    def __init__(self, sock):
        self.sock = sock
        self.buf = b""
        self.next_id = 0

    @classmethod
    def connect(cls, ws_url, timeout=5.0):
        host, port, path = _split_ws_url(ws_url)
        sock = socket.create_connection((host, port), timeout=timeout)
        sock.settimeout(timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            f"Origin: http://{host}:{port}\r\n"
            "\r\n"
        )
        sock.sendall(request.encode())
        client = cls(sock)
        header = client._read_until(b"\r\n\r\n")
        status = header.split(b"\r\n", 1)[0]
        if b" 101 " not in status:
            sock.close()
            raise ConnectionError(status.decode("latin1", "replace"))
        accept = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode()).digest())
        if accept not in header:
            sock.close()
            raise ConnectionError("websocket accept mismatch")
        return client

    def call(self, method, params=None, timeout=5.0):
        self.next_id += 1
        message_id = self.next_id
        message = {"id": message_id, "method": method}
        if params is not None:
            message["params"] = params
        self.sock.sendall(encode_client_frame(json.dumps(message).encode()))
        deadline = time.monotonic() + timeout
        fragments = []
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(method)
            self.sock.settimeout(remaining)
            fin, opcode, payload = self._read_frame()
            if opcode == 0x9:
                self.sock.sendall(encode_client_frame(payload, opcode=0xA))
                continue
            if opcode == 0xA:
                continue
            if opcode == 0x8:
                raise ConnectionError("websocket closed")
            if opcode in (0x0, 0x1, 0x2):
                fragments.append(payload)
                if not fin:
                    continue
                data = json.loads(b"".join(fragments).decode())
                fragments = []
                if data.get("id") != message_id:
                    continue
                if "error" in data:
                    raise RuntimeError(str(data["error"]))
                return data.get("result") or {}

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def _read_until(self, marker):
        while marker not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("socket closed")
            self.buf += chunk
        index = self.buf.index(marker) + len(marker)
        out, self.buf = self.buf[:index], self.buf[index:]
        return out

    def _read_exact(self, size):
        while len(self.buf) < size:
            chunk = self.sock.recv(max(4096, size - len(self.buf)))
            if not chunk:
                raise ConnectionError("socket closed")
            self.buf += chunk
        out, self.buf = self.buf[:size], self.buf[size:]
        return out

    def _read_frame(self):
        while True:
            frame = pop_ws_frame(self.buf)
            if frame is not None:
                _fin, _opcode, _payload, self.buf = frame
                return frame[0], frame[1], frame[2]
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("socket closed")
            self.buf += chunk


def _split_ws_url(url):
    if not url.startswith("ws://"):
        raise ValueError(url)
    rest = url[len("ws://"):]
    hostport, _, path = rest.partition("/")
    path = "/" + path
    host, separator, port_text = hostport.rpartition(":")
    if not separator:
        return hostport, 80, path
    return host, int(port_text), path
