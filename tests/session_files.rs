use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gemini_native_client::browser::{read_cookie_file, write_cookie_file};

fn temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("gemini-session-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn session_file_roundtrip_rejects_a_tampered_value() {
    let dir = temp_dir();
    let rejected = write_cookie_file(&dir, "bad;cookie", "sidts-value").unwrap_err();
    assert_eq!(rejected.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!rejected.to_string().contains("bad;cookie"));

    write_cookie_file(&dir, "sid-value", "sidts-value").unwrap();
    let path = dir.join("cookies.json");
    let (sid, sidts) = read_cookie_file(&path).unwrap();
    assert_eq!((sid, sidts), ("sid-value".into(), "sidts-value".into()));

    fs::write(&path, r#"{"sid":"sid;evil","sidts":"sidts-value"}"#).unwrap();
    assert!(read_cookie_file(&path).is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn session_file_is_owner_readable_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir();
    write_cookie_file(&dir, "sid-value", "sidts-value").unwrap();
    let file_mode = fs::metadata(dir.join("cookies.json")).unwrap().permissions().mode() & 0o777;
    let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600);
    assert_eq!(dir_mode, 0o700);
    let _ = fs::remove_dir_all(&dir);
}
