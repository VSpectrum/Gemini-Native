//! Checks that sit in front of cookies, local sockets, and media downloads.
//!
//! Each function fails closed. Callers should show the error and stop, not
//! fall through to the request.

/// A process id we are willing to signal. Rejects 0 and 1, and values that
/// do not fit in the signed id `kill` takes, so a bad id cannot become
/// "signal my own group" or overflow the negation.
pub fn pid_allowed(pid: u32) -> bool {
    pid > 1 && pid <= i32::MAX as u32
}

/// Host is `google.com` or a subdomain of it. `evilgoogle.com` is not.
pub fn is_google_domain(domain: &str) -> bool {
    let host = domain
        .trim()
        .trim_start_matches('.')
        .split(['/', '?', '#', '%'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    host == "google.com" || host.strip_suffix("google.com").is_some_and(|prefix| prefix.ends_with('.'))
}

/// Cookie values that are safe to place in a `Set-Cookie` line.
/// Spaces, quotes, commas, semicolons, and backslashes would add attributes.
pub fn cookie_value_ok(value: &str) -> bool {
    let value = value.trim();
    (1..=4096).contains(&value.len())
        && value
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && !matches!(byte, b'"' | b',' | b';' | b'\\'))
}

pub fn loopback_host(host: &str) -> bool {
    matches!(
        host.trim().trim_matches(|ch| ch == '[' || ch == ']'),
        "127.0.0.1" | "localhost" | "::1"
    )
}

pub fn header_token_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// An HTTP path or query we will write into a request line.
pub fn request_target_ok(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 2048
        && value
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && byte != b' ')
}

pub fn session_id_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn host_is(host: &str, suffix: &str) -> bool {
    host == suffix || host.strip_suffix(suffix).is_some_and(|prefix| prefix.ends_with('.'))
}

/// Image and video hosts Gemini actually uses. Not all of `google.com`,
/// so a reply cannot point the signed-in client at Gmail or Accounts.
pub fn media_host_allowed(host: &str) -> bool {
    let host = host.trim().trim_matches(|ch| ch == '[' || ch == ']').to_ascii_lowercase();
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    ["googleusercontent.com", "ggpht.com", "gstatic.com", "usercontent.google.com"]
        .into_iter()
        .any(|suffix| host_is(&host, suffix))
}

pub fn media_url_allowed(raw: &str) -> bool {
    if raw.len() > 4096 || raw.bytes().any(|byte| byte.is_ascii_whitespace() || byte < 0x20) {
        return false;
    }
    let Some(rest) = raw.strip_prefix("https://") else {
        return false;
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    if rest.contains('@') || rest.contains('\\') {
        return false;
    }
    let hostport = rest.split('/').next().unwrap_or("");
    if hostport.is_empty() || hostport.contains('?') {
        return false;
    }
    let (host, port) = if let Some(host) = hostport.strip_prefix('[') {
        let Some((host, port)) = host.split_once("]:") else {
            return false;
        };
        (host, Some(port))
    } else if let Some((host, port)) = hostport.rsplit_once(':') {
        if host.contains(':') {
            return false;
        }
        (host, Some(port))
    } else {
        (hostport, None)
    };
    if port.is_some_and(|port| port != "443") {
        return false;
    }
    media_host_allowed(host)
}

/// Whether a redirect hop for a media download may be followed.
pub fn allow_media_hop(scheme: Option<&str>, host: Option<&str>, hops: usize) -> bool {
    hops < 4 && scheme == Some("https") && host.is_some_and(media_host_allowed)
}

/// Text that will be placed inside a markdown code fence in the chat.
pub fn plain_error(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().take(240) {
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out.replace("```", "'''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_domain_rejects_lookalikes() {
        assert!(is_google_domain("google.com"));
        assert!(is_google_domain(".google.com"));
        assert!(is_google_domain("accounts.google.com"));
        assert!(!is_google_domain("evilgoogle.com"));
        assert!(!is_google_domain("google.com.evil.com"));
        assert!(!is_google_domain("notgoogle.com"));
        assert!(!is_google_domain("youtube.com"));
        assert!(!is_google_domain(""));
    }

    #[test]
    fn cookie_values_cannot_add_attributes() {
        assert!(cookie_value_ok("g.a000-example_token"));
        assert!(!cookie_value_ok("sid; Domain=evil.com"));
        assert!(!cookie_value_ok("sid\r\nSet-Cookie: x=y"));
        assert!(!cookie_value_ok("has space"));
        assert!(!cookie_value_ok(""));
        assert!(!cookie_value_ok("quote\"d"));
    }

    #[test]
    fn media_urls_stay_on_the_cdn() {
        assert!(media_url_allowed("https://lh3.googleusercontent.com/a/image=s512"));
        assert!(media_url_allowed("https://lh3.googleusercontent.com:443/a"));
        assert!(media_url_allowed("https://work.fife.usercontent.google.com/file"));
        assert!(!media_url_allowed("https://accounts.google.com/secret"));
        assert!(!media_url_allowed("https://evilgoogle.com/a"));
        assert!(!media_url_allowed("http://lh3.googleusercontent.com/a"));
        assert!(!media_url_allowed("https://user:pass@lh3.googleusercontent.com/a"));
        assert!(!media_url_allowed("https://127.0.0.1/a"));
        assert!(!media_url_allowed("https://lh3.googleusercontent.com:444/a"));
    }

    #[test]
    fn redirects_stop_off_the_cdn_and_after_four_hops() {
        assert!(allow_media_hop(Some("https"), Some("lh3.ggpht.com"), 0));
        assert!(!allow_media_hop(Some("https"), Some("accounts.google.com"), 0));
        assert!(!allow_media_hop(Some("http"), Some("lh3.googleusercontent.com"), 0));
        assert!(!allow_media_hop(Some("https"), Some("lh3.googleusercontent.com"), 4));
    }

    #[test]
    fn local_request_tokens_reject_header_injection() {
        assert!(header_token_ok("GET"));
        assert!(!header_token_ok("GET\r\nHost: evil"));
        assert!(request_target_ok("/session/abc/cookie"));
        assert!(!request_target_ok("/session/abc\r\nX: 1"));
        assert!(session_id_ok("abc_DEF-1"));
        assert!(!session_id_ok("abc/../other"));
        assert!(loopback_host("127.0.0.1"));
        assert!(loopback_host("localhost"));
        assert!(loopback_host("[::1]"));
        assert!(!loopback_host("127.0.0.1.evil.com"));
        assert!(!pid_allowed(0));
        assert!(!pid_allowed(1));
        assert!(pid_allowed(1000));
    }

    #[test]
    fn error_text_cannot_break_out_of_a_code_fence() {
        let text = plain_error("nope```\n**bold**");
        assert!(!text.contains("```"));
        assert!(!text.contains('\n'));
    }
}
