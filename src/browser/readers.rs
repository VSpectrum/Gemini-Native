//! Firefox, Safari, and WebDriver cookie readers.
//!
//! These functions only parse stores the caller already opened. They do not
//! launch a browser.

use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use super::cookies::Cookie;
use super::devtools::read_limited;
use super::BrowserError;
use crate::safety;

const MAX_LOCAL_RESPONSE: usize = 1024 * 1024;

fn range_end(start: usize, len: usize, buf_len: usize) -> Option<usize> {
    let end = start.checked_add(len)?;
    (end <= buf_len).then_some(end)
}

fn cstr(buf: &[u8], offset: i32) -> Result<String, ()> {
    if offset < 0 || offset as usize >= buf.len() {
        return Err(());
    }
    let start = offset as usize;
    let end = buf[start..].iter().position(|byte| *byte == 0).ok_or(())? + start;
    Ok(String::from_utf8_lossy(&buf[start..end]).into_owned())
}

pub(crate) fn safari_cookie_file() -> Option<std::path::PathBuf> {
    let candidates = [
        super::home_dir().join(
            "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
        ),
        super::home_dir().join("Library/Cookies/Cookies.binarycookies"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

pub fn read_safari_cookies(path: &Path) -> Result<Vec<Cookie>, String> {
    let data = std::fs::read(path).map_err(|err| err.to_string())?;
    if data.len() < 8 || data.len() > 8 * 1024 * 1024 || &data[..4] != b"cook" {
        return Err("not a Safari cookie file".into());
    }
    let num_pages = i32::from_be_bytes(data[4..8].try_into().unwrap());
    if num_pages < 0 {
        return Err("truncated Safari cookie file".into());
    }
    let pages = num_pages as usize;
    let Some(header_end) = range_end(8, pages.saturating_mul(4), data.len()) else {
        return Err("truncated Safari cookie file".into());
    };
    if pages.saturating_mul(4) / 4 != pages {
        return Err("truncated Safari cookie file".into());
    }
    let mut cookies = Vec::new();
    let mut offset = header_end;
    for index in 0..pages {
        let size_at = 8 + 4 * index;
        let size = i32::from_be_bytes(data[size_at..size_at + 4].try_into().unwrap());
        if size < 8 {
            break;
        }
        let Some(page_end) = range_end(offset, size as usize, data.len()) else {
            break;
        };
        let page = &data[offset..page_end];
        offset = page_end;
        if page.len() < 8 || &page[..4] != b"\x00\x00\x01\x00" {
            continue;
        }
        let num_cookies = i32::from_le_bytes(page[4..8].try_into().unwrap());
        if num_cookies < 0 {
            continue;
        }
        let count = num_cookies as usize;
        if count.saturating_mul(4) / 4 != count {
            continue;
        }
        let Some(table_end) = range_end(8, count * 4, page.len()) else {
            continue;
        };
        for cookie_index in 0..count {
            let start = i32::from_le_bytes(
                page[8 + 4 * cookie_index..12 + 4 * cookie_index]
                    .try_into()
                    .unwrap(),
            );
            // The offset is from the start of the page. A short page's first
            // cookie sits just after the offset table, which can be below 32.
            if start < 0 || (start as usize) < table_end {
                continue;
            }
            let start = start as usize;
            let Some(size_end) = range_end(start, 4, page.len()) else {
                continue;
            };
            let cookie_size = i32::from_le_bytes(page[start..size_end].try_into().unwrap());
            if cookie_size < 32 {
                continue;
            }
            let Some(cookie_end) = range_end(start, cookie_size as usize, page.len()) else {
                continue;
            };
            let cookie = &page[start..cookie_end];
            if cookie.len() < 32 {
                continue;
            }
            let url_off = i32::from_le_bytes(cookie[16..20].try_into().unwrap());
            let name_off = i32::from_le_bytes(cookie[20..24].try_into().unwrap());
            let value_off = i32::from_le_bytes(cookie[28..32].try_into().unwrap());
            let (Ok(host), Ok(name), Ok(value)) = (
                cstr(cookie, url_off),
                cstr(cookie, name_off),
                cstr(cookie, value_off),
            ) else {
                continue;
            };
            cookies.push(Cookie {
                domain: host,
                name,
                value,
            });
        }
    }
    Ok(cookies)
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

pub fn webdriver_call(
    port: u16,
    method: &str,
    path: &str,
    payload: Option<&str>,
    timeout: Duration,
) -> Result<serde_json::Value, BrowserError> {
    if !safety::header_token_ok(method) || !safety::request_target_ok(path) {
        return Err(BrowserError::Launch("rejected local request".into()));
    }
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|err| BrowserError::Launch(err.to_string()))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let body = payload.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| BrowserError::Launch(err.to_string()))?;
    let raw = read_limited(&mut stream, MAX_LOCAL_RESPONSE)
        .map_err(|err| BrowserError::Launch(err))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let status_line = head.lines().next().unwrap_or("");
    let code = status_line.split_whitespace().nth(1).unwrap_or("0");
    if !code.starts_with('2') {
        let mut message = rest.to_string();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(rest) {
            if let Some(value) = parsed.get("value") {
                if let Some(text) = value
                    .get("message")
                    .and_then(|item| item.as_str())
                    .or_else(|| value.get("error").and_then(|item| item.as_str()))
                {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::cookies::cookies_from_json;
    use crate::browser::extract_sid_and_sidts;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

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
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Cookies.binarycookies");
        std::fs::write(&path, raw).unwrap();
        let cookies = read_safari_cookies(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let (sid, sidts) = extract_sid_and_sidts(&cookies);
        assert_eq!(sid.as_deref(), Some("sid"));
        assert_eq!(sidts.as_deref(), Some("sidts"));
    }

    #[test]
    fn firefox_database_keeps_google_cookies() {
        let dir = std::env::temp_dir().join(format!("gemini-firefox-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cookies.sqlite");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_cookies VALUES (?1, ?2, ?3)",
            [".google.com", "__Secure-1PSID", "sid"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_cookies VALUES (?1, ?2, ?3)",
            [".google.com", "__Secure-1PSIDTS", "sidts"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_cookies VALUES (?1, ?2, ?3)",
            [".youtube.com", "__Secure-1PSIDTS", "ignore"],
        )
        .unwrap();
        drop(conn);
        let cookies = read_firefox_cookies(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let (sid, sidts) = extract_sid_and_sidts(&cookies);
        assert_eq!(sid.as_deref(), Some("sid"));
        assert_eq!(sidts.as_deref(), Some("sidts"));
    }

    #[test]
    fn webdriver_rejects_header_injection_without_connecting() {
        let err = webdriver_call(
            1,
            "GET\r\nX",
            "/session/abc/cookie",
            None,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.to_string().contains("rejected"));
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
        ]})
        .to_string();
        let port = serve_once("200 OK", &body);
        thread::sleep(Duration::from_millis(50));
        let result =
            webdriver_call(port, "GET", "/session/abc/cookie", None, Duration::from_secs(5))
                .unwrap();
        let cookies = cookies_from_json(result["value"].as_array().cloned().unwrap_or_default());
        let (sid, sidts) = extract_sid_and_sidts(&cookies);
        assert_eq!(sid.as_deref(), Some("sid"));
        assert_eq!(sidts.as_deref(), Some("sidts"));

        let body = serde_json::json!({"value": {"error": "session not created", "message": "Allow Remote Automation"}}).to_string();
        let port = serve_once("500 Internal", &body);
        thread::sleep(Duration::from_millis(50));
        let err = webdriver_call(port, "POST", "/session", Some("{}"), Duration::from_secs(5))
            .unwrap_err();
        assert!(err.to_string().contains("Allow Remote Automation"));
    }
}
