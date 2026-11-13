//! Local Chrome DevTools client.
//!
//! The socket is accepted only when the host is loopback. Frame sizes are
//! capped so a debugging port cannot make the process allocate without bound.

use base64::{engine::general_purpose::STANDARD, Engine};
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use super::cookies::{cookies_from_json, Cookie};
use crate::safety;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const COOKIE_URLS: &[&str] = &[
    "https://gemini.google.com/",
    "https://accounts.google.com/",
    "https://www.google.com/",
];
const MAX_LOCAL_RESPONSE: usize = 1024 * 1024;
const MAX_WS_PAYLOAD: usize = 2 * 1024 * 1024;

/// Chrome 157 answers `/json/list` with `Content-Length` and then leaves the
/// socket open. Waiting for the server to close that socket times out, and
/// the sign-in window is then shut down as if the debugging port never started.
pub(crate) fn http_response_complete(raw: &[u8]) -> bool {
    let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&raw[..split]);
    let Some(len) = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    }) else {
        return false;
    };
    raw.len() >= split.saturating_add(4).saturating_add(len)
}

pub(crate) fn read_limited(stream: &mut TcpStream, max: usize) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if http_response_complete(&raw) {
            break;
        }
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(_) if http_response_complete(&raw) => break,
            Err(err) => return Err(err.to_string()),
        };
        if read == 0 {
            break;
        }
        if raw.len().saturating_add(read) > max {
            return Err("local response is too large".into());
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    Ok(raw)
}

pub fn pop_ws_frame(buf: &[u8]) -> Result<Option<(bool, u8, Vec<u8>, Vec<u8>)>, String> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let masked = buf[1] & 0x80 != 0;
    let mut length = (buf[1] & 0x7f) as usize;
    let mut offset = 2;
    if length == 126 {
        if buf.len() < offset + 2 {
            return Ok(None);
        }
        length = u16::from_be_bytes(buf[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
    } else if length == 127 {
        if buf.len() < offset + 8 {
            return Ok(None);
        }
        let wide = u64::from_be_bytes(buf[offset..offset + 8].try_into().unwrap());
        length = usize::try_from(wide).map_err(|_| "websocket frame is too large".to_string())?;
        offset += 8;
    }
    if length > MAX_WS_PAYLOAD {
        return Err("websocket frame is too large".into());
    }
    let mask = if masked {
        if buf.len() < offset + 4 {
            return Ok(None);
        }
        let mask = buf[offset..offset + 4].to_vec();
        offset += 4;
        mask
    } else {
        Vec::new()
    };
    let end = offset
        .checked_add(length)
        .ok_or_else(|| "websocket frame is too large".to_string())?;
    if buf.len() < end {
        return Ok(None);
    }
    let mut payload = buf[offset..end].to_vec();
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index & 3];
        }
    }
    Ok(Some((
        buf[0] & 0x80 != 0,
        buf[0] & 0x0f,
        payload,
        buf[end..].to_vec(),
    )))
}

pub fn encode_client_frame(payload: &[u8], opcode: u8) -> Vec<u8> {
    let mask = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.subsec_nanos())
            .unwrap_or(1);
        nanos.to_le_bytes()
    };
    let masked: Vec<u8> = payload
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ mask[index & 3])
        .collect();
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

pub(crate) struct CdpClient {
    stream: TcpStream,
    buf: Vec<u8>,
    next_id: u64,
}

impl CdpClient {
    pub(crate) fn connect(ws_url: &str, timeout: Duration) -> Result<Self, String> {
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
        stream
            .write_all(request.as_bytes())
            .map_err(|err| err.to_string())?;
        let mut client = Self {
            stream,
            buf: Vec::new(),
            next_id: 0,
        };
        let header = client.read_until(b"\r\n\r\n")?;
        let status = header.split(|byte| *byte == b'\n').next().unwrap_or_default();
        if !String::from_utf8_lossy(status).contains(" 101 ") {
            return Err(String::from_utf8_lossy(status).into_owned());
        }
        let mut hasher = Sha1::new();
        hasher.update(key.as_bytes());
        hasher.update(WS_GUID.as_bytes());
        let accept = STANDARD.encode(hasher.finalize());
        if !header
            .windows(accept.len())
            .any(|window| window == accept.as_bytes())
        {
            return Err("websocket accept mismatch".into());
        }
        Ok(client)
    }

    pub(crate) fn call(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
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
                if fragments.len().saturating_add(payload.len()) > MAX_WS_PAYLOAD {
                    return Err("websocket frame is too large".into());
                }
                fragments.extend(payload);
                if !fin {
                    continue;
                }
                let data: serde_json::Value =
                    serde_json::from_slice(&fragments).map_err(|err| err.to_string())?;
                fragments.clear();
                if data.get("id").and_then(|value| value.as_u64()) != Some(message_id) {
                    continue;
                }
                if let Some(error) = data.get("error") {
                    return Err(error.to_string());
                }
                return Ok(data.get("result").cloned().unwrap_or(serde_json::json!({})));
            }
        }
    }

    pub(crate) fn close(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    fn read_until(&mut self, marker: &[u8]) -> Result<Vec<u8>, String> {
        while !self.buf.windows(marker.len()).any(|window| window == marker) {
            if self.buf.len() > 64 * 1024 {
                return Err("local response is too large".into());
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).map_err(|err| err.to_string())?;
            if read == 0 {
                return Err("socket closed".into());
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
        let index = self
            .buf
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap()
            + marker.len();
        let out = self.buf[..index].to_vec();
        self.buf.drain(..index);
        Ok(out)
    }

    fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), String> {
        loop {
            if self.buf.len() > MAX_WS_PAYLOAD + 14 {
                return Err("websocket frame is too large".into());
            }
            match pop_ws_frame(&self.buf)? {
                Some((fin, opcode, payload, rest)) => {
                    self.buf = rest;
                    return Ok((fin, opcode, payload));
                }
                None => {}
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
    if !safety::request_target_ok(&path) {
        return Err("devtools path was rejected".into());
    }
    let (host, port) = if let Some((host, port)) = hostport.rsplit_once(':') {
        let port = port.parse::<u16>().map_err(|_| url.to_string())?;
        (host.to_string(), port)
    } else {
        (hostport.to_string(), 80)
    };
    if !safety::loopback_host(&host) {
        return Err("devtools endpoint is not local".into());
    }
    Ok((host, port, path))
}

pub(crate) fn devtools_pages(port: u16) -> Result<Vec<serde_json::Value>, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| err.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let request = format!("GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|err| err.to_string())?;
    let raw = read_limited(&mut stream, MAX_LOCAL_RESPONSE)?;
    let text = String::from_utf8_lossy(&raw);
    let body = text.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    Ok(parsed.as_array().cloned().unwrap_or_default())
}

pub(crate) fn chromium_cookies(port: u16) -> Result<Vec<Cookie>, String> {
    let pages = devtools_pages(port)?;
    let ws_url = pages.iter().find_map(|page| {
        if page.get("type").and_then(|value| value.as_str()) == Some("page") {
            page.get("webSocketDebuggerUrl")
                .and_then(|value| value.as_str())
                .map(str::to_string)
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
        let cookies = listed
            .get("cookies")
            .cloned()
            .unwrap_or(serde_json::json!([]));
        if cookies
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        {
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

/// Asks Chromium to close its windows. The caller still reaps the process.
pub(crate) fn close_devtools_browser(port: u16) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return;
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let request =
        format!("GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return;
    }
    let Ok(raw) = read_limited(&mut stream, MAX_LOCAL_RESPONSE) else {
        return;
    };
    let text = String::from_utf8_lossy(&raw);
    let Some(body) = text.split_once("\r\n\r\n").map(|(_, body)| body) else {
        return;
    };
    let Ok(info) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(ws_url) = info.get("webSocketDebuggerUrl").and_then(|value| value.as_str()) else {
        return;
    };
    if let Ok(mut client) = CdpClient::connect(ws_url, Duration::from_secs(2)) {
        let _ = client.call("Browser.close", None, Duration::from_secs(2));
        client.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_frame_roundtrip() {
        let encoded = encode_client_frame(br#"{"id":1}"#, 0x1);
        let (fin, opcode, payload, rest) = pop_ws_frame(&encoded).unwrap().unwrap();
        assert!(fin);
        assert_eq!(opcode, 0x1);
        assert_eq!(payload, br#"{"id":1}"#);
        assert!(rest.is_empty());
    }

    #[test]
    fn oversized_frames_are_rejected_before_allocation() {
        let mut buf = vec![0x82, 127];
        buf.extend_from_slice(&(3 * 1024 * 1024u64).to_be_bytes());
        let err = pop_ws_frame(&buf).unwrap_err();
        assert!(err.contains("too large"));
    }

    #[test]
    fn a_content_length_body_is_complete_while_the_socket_stays_open() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let body = br#"[{"type":"page"}]"#;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
            sock.write_all(head.as_bytes()).unwrap();
            sock.write_all(body).unwrap();
            let _ = release_rx.recv();
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let started = Instant::now();
        let raw = read_limited(&mut client, 1024).unwrap();
        assert!(started.elapsed() < Duration::from_millis(500), "waited for the server to close");
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains(r#"[{"type":"page"}]"#));
        let _ = release_tx.send(());
        server.join().unwrap();
    }

    #[test]
    fn devtools_urls_must_stay_on_loopback() {
        let err = split_ws_url("ws://evil.example/devtools/page/1").unwrap_err();
        assert!(err.contains("not local"));
        let err = split_ws_url("ws://127.0.0.1:9222/bad path").unwrap_err();
        assert!(err.contains("path"));
        let (host, port, path) = split_ws_url("ws://127.0.0.1:9222/devtools/page/1").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 9222);
        assert_eq!(path, "/devtools/page/1");
    }
}
