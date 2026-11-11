import base64
import hashlib
import json
import socket
import sqlite3
import struct
import tempfile
import threading
import unittest
from pathlib import Path

import browser_session
from browser_session import (
    BrowserLaunchError,
    CdpClient,
    _WS_GUID,
    collect_session_cookies,
    encode_client_frame,
    extract_sid_and_sidts,
    pop_ws_frame,
    read_firefox_cookies,
    read_safari_cookies,
    wait_for_google_cookies,
    webdriver_call,
)


class TestWaitForCookies(unittest.TestCase):
    def test_closed_browser(self):
        with self.assertRaises(RuntimeError) as ctx:
            wait_for_google_cookies(lambda: [], lambda: False, timeout=5, sleep=lambda _s: None)
        self.assertIn("Browser window was closed", str(ctx.exception))

    def test_timeout_uses_injected_clock(self):
        clock = {"t": 0.0}

        def now():
            return clock["t"]

        def sleep(seconds):
            clock["t"] += seconds

        with self.assertRaises(TimeoutError):
            wait_for_google_cookies(
                lambda: [],
                lambda: True,
                timeout=5,
                interval=1.0,
                sleep=sleep,
                clock=now,
            )
        self.assertGreater(clock["t"], 5)

    def test_waits_for_both_cookies_then_rereads(self):
        calls = {"n": 0}

        def fetch():
            calls["n"] += 1
            if calls["n"] == 1:
                return [{"name": "__Secure-1PSID", "value": "sid1", "domain": ".google.com"}]
            return [
                {"name": "__Secure-1PSID", "value": "sid1", "domain": ".google.com"},
                {"name": "__Secure-1PSIDTS", "value": "sidts1", "domain": ".google.com"},
            ]

        sid, sidts = wait_for_google_cookies(
            fetch,
            lambda: True,
            timeout=30,
            sleep=lambda _s: None,
        )
        self.assertEqual((sid, sidts), ("sid1", "sidts1"))
        self.assertGreaterEqual(calls["n"], 3)

    def test_immediate_browser_exit_can_fall_through(self):
        def still_open():
            raise BrowserLaunchError("browser exited immediately")

        with self.assertRaises(BrowserLaunchError):
            wait_for_google_cookies(lambda: [], still_open, timeout=5, sleep=lambda _s: None)


class TestFirefoxCookies(unittest.TestCase):
    def test_reads_google_cookies(self):
        with tempfile.TemporaryDirectory() as tmp:
            db_path = Path(tmp) / "cookies.sqlite"
            conn = sqlite3.connect(db_path)
            conn.execute(
                "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT)"
            )
            conn.execute(
                "INSERT INTO moz_cookies VALUES (?, ?, ?)",
                (".google.com", "__Secure-1PSID", "sid"),
            )
            conn.execute(
                "INSERT INTO moz_cookies VALUES (?, ?, ?)",
                (".google.com", "__Secure-1PSIDTS", "sidts"),
            )
            conn.execute(
                "INSERT INTO moz_cookies VALUES (?, ?, ?)",
                (".youtube.com", "__Secure-1PSIDTS", "ignore"),
            )
            conn.commit()
            conn.close()

            cookies = read_firefox_cookies(db_path)
            sid, sidts = extract_sid_and_sidts(cookies)
            self.assertEqual(sid, "sid")
            self.assertEqual(sidts, "sidts")


def _safari_cookie(host, name, value):
    strings = [host.encode() + b"\x00", name.encode() + b"\x00", b"/\x00", value.encode() + b"\x00"]
    offsets = []
    cursor = 32
    blob = bytearray()
    for item in strings:
        offsets.append(cursor)
        blob += item
        cursor += len(item)
    size = 32 + len(blob)
    header = struct.pack("<iiiiiiii", size, 0, 0, 0, *offsets)
    return header + blob


def _safari_file(cookies):
    page_header = b"\x00\x00\x01\x00"
    offsets = []
    body = bytearray()
    start = 4 + 4 + 4 * len(cookies)
    for cookie in cookies:
        offsets.append(start)
        body += cookie
        start += len(cookie)
    page = page_header + struct.pack("<i", len(cookies)) + struct.pack(f"<{len(cookies)}i", *offsets) + body
    return b"cook" + struct.pack(">i", 1) + struct.pack(">i", len(page)) + page


class TestSafariCookies(unittest.TestCase):
    def test_reads_google_cookies_and_ignores_youtube(self):
        raw = _safari_file([
            _safari_cookie(".google.com", "__Secure-1PSID", "sid"),
            _safari_cookie(".google.com", "__Secure-1PSIDTS", "sidts"),
            _safari_cookie(".youtube.com", "__Secure-1PSIDTS", "ignore"),
        ])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "Cookies.binarycookies"
            path.write_bytes(raw)
            cookies = read_safari_cookies(path)
        sid, sidts = extract_sid_and_sidts(cookies)
        self.assertEqual(sid, "sid")
        self.assertEqual(sidts, "sidts")


class TestSafariWebDriver(unittest.TestCase):
    def test_reads_cookies_and_surfaces_automation_error(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.bind(("127.0.0.1", 0))
        server.listen(1)
        port = server.getsockname()[1]

        def serve_once(status, body):
            conn, _addr = server.accept()
            buf = b""
            while b"\r\n\r\n" not in buf:
                buf += conn.recv(4096)
            payload = body.encode()
            conn.sendall(
                f"HTTP/1.1 {status}\r\nContent-Length: {len(payload)}\r\n"
                "Content-Type: application/json\r\n\r\n".encode() + payload
            )
            conn.close()

        thread = threading.Thread(
            target=serve_once,
            args=(200, json.dumps({"value": [
                {"name": "__Secure-1PSID", "value": "sid", "domain": ".google.com"},
                {"name": "__Secure-1PSIDTS", "value": "sidts", "domain": ".google.com"},
            ]})),
        )
        thread.start()
        try:
            result = webdriver_call(port, "GET", "/session/abc/cookie", timeout=5)
        finally:
            thread.join(timeout=5)
            server.close()
        sid, sidts = extract_sid_and_sidts(result["value"])
        self.assertEqual((sid, sidts), ("sid", "sidts"))

        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.bind(("127.0.0.1", 0))
        server.listen(1)
        port = server.getsockname()[1]
        thread = threading.Thread(
            target=serve_once,
            args=(500, json.dumps({"value": {"error": "session not created", "message": "Allow Remote Automation"}})),
        )
        thread.start()
        try:
            with self.assertRaises(BrowserLaunchError) as ctx:
                webdriver_call(port, "POST", "/session", {"capabilities": {}}, timeout=5)
        finally:
            thread.join(timeout=5)
            server.close()
        self.assertIn("Allow Remote Automation", str(ctx.exception))


class TestNoBrowser(unittest.TestCase):
    def test_missing_browser_explains_next_step(self):
        with self.assertRaises(RuntimeError) as ctx:
            collect_session_cookies(Path("/tmp/does-not-matter"), browsers=[])
        self.assertIn("No Chrome", str(ctx.exception))
        self.assertIn("GEMINI_PSID", str(ctx.exception))


class TestWebSocket(unittest.TestCase):
    def test_client_frame_roundtrip(self):
        encoded = encode_client_frame(b'{"id":1}')
        frame = pop_ws_frame(encoded)
        self.assertIsNotNone(frame)
        fin, opcode, payload, rest = frame
        self.assertTrue(fin)
        self.assertEqual(opcode, 0x1)
        self.assertEqual(payload, b'{"id":1}')
        self.assertEqual(rest, b"")

    def test_cdp_call_against_local_server(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.bind(("127.0.0.1", 0))
        server.listen(1)
        port = server.getsockname()[1]
        result = {}

        def serve():
            conn, _addr = server.accept()
            buf = b""
            while b"\r\n\r\n" not in buf:
                buf += conn.recv(4096)
            key = None
            for line in buf.split(b"\r\n"):
                if line.lower().startswith(b"sec-websocket-key:"):
                    key = line.split(b":", 1)[1].strip().decode()
            accept = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode()).digest()).decode()
            conn.sendall(
                (
                    "HTTP/1.1 101 Switching Protocols\r\n"
                    "Upgrade: websocket\r\n"
                    "Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Accept: {accept}\r\n"
                    "\r\n"
                ).encode()
            )
            frame_buf = b""
            while True:
                parsed = pop_ws_frame(frame_buf)
                if parsed is not None:
                    _fin, _opcode, payload, _rest = parsed
                    request = json.loads(payload.decode())
                    result["method"] = request["method"]
                    reply = json.dumps({
                        "id": request["id"],
                        "result": {"cookies": [{"name": "__Secure-1PSID", "value": "s", "domain": ".google.com"}]},
                    }).encode()
                    header = bytes((0x81, len(reply)))
                    conn.sendall(header + reply)
                    break
                chunk = conn.recv(4096)
                if not chunk:
                    break
                frame_buf += chunk
            conn.close()

        thread = threading.Thread(target=serve)
        thread.start()
        try:
            client = CdpClient.connect(f"ws://127.0.0.1:{port}/devtools/page/test", timeout=5)
            payload = client.call("Network.getCookies", {"urls": ["https://gemini.google.com/"]})
            client.close()
        finally:
            thread.join(timeout=5)
            server.close()

        self.assertEqual(result["method"], "Network.getCookies")
        self.assertEqual(payload["cookies"][0]["name"], "__Secure-1PSID")


if __name__ == "__main__":
    unittest.main()
