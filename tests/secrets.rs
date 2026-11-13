//! Fails if a session cookie, key, or token-shaped string is sitting in the tree.
//! Patterns are built at runtime so this file does not contain the needles itself.

use std::fs;
use std::path::Path;

fn needle(parts: &[&str]) -> String {
    parts.concat()
}

fn ignored(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("target") | Some(".git") | Some("assets")
        )
    })
}

fn suspicious_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name == "cookies.json" || name == ".env" || name == "credentials.json" || name == "id_rsa" {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".pem")
        || lower.ends_with(".key")
        || (lower.starts_with(".env.") && lower != ".env.example")
}

fn text_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("rs" | "md" | "toml" | "yml" | "yaml" | "sh" | "json" | "txt" | "html" | "js" | "css")
    )
}

#[test]
fn repository_does_not_contain_session_material() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let needles = [
        ("private key", needle(&["-----BEGIN ", "PRIVATE KEY-----"])),
        ("github token", needle(&["gh", "p_"])),
        ("github fine-grained token", needle(&["github", "_pat_"])),
        ("slack token", needle(&["xox", "b-"])),
        ("google api key", needle(&["AI", "za"])),
    ];
    let mut hits = Vec::new();
    walk(root, &needles, &mut hits);
    assert!(
        hits.is_empty(),
        "found session material that should not be committed:\n{}",
        hits.join("\n")
    );
}

fn walk(dir: &Path, needles: &[(&str, String)], hits: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if ignored(&path) {
            continue;
        }
        if path.is_dir() {
            walk(&path, needles, hits);
            continue;
        }
        if suspicious_name(&path) {
            hits.push(format!("{} matches a secret filename", path.display()));
            continue;
        }
        if !text_file(&path) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (name, needle) in needles {
            if text.contains(needle) {
                hits.push(format!("{} contains a {name}", path.display()));
            }
        }
    }
}
