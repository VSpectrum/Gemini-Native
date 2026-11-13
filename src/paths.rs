//! Files the window is allowed to open: only inside the media directory.

use std::path::{Component, Path, PathBuf};

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}

fn get_canonical_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    if path.exists() {
        return std::fs::canonicalize(path);
    }

    let mut current = path.to_path_buf();
    let mut missing = Vec::new();

    while !current.exists() {
        let parent = match current.parent() {
            Some(parent) => parent,
            None => break,
        };
        if let Some(component) = current.components().last() {
            match component {
                Component::Normal(name) => missing.push(name.to_os_string()),
                Component::ParentDir => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "parent traversal on a missing path is not supported",
                    ));
                }
                _ => {}
            }
        }
        current = parent.to_path_buf();
    }

    if current.exists() {
        let mut canonical = std::fs::canonicalize(current)?;
        for component in missing.iter().rev() {
            canonical.push(component);
        }
        return Ok(normalize_path(&canonical));
    }

    Ok(normalize_path(path))
}

/// `path` is inside `root` after resolving `..` and symlinks.
pub fn is_path_inside(path: &Path, root: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if path.as_os_str().as_bytes().contains(&0) || root.as_os_str().as_bytes().contains(&0) {
            return false;
        }
    }

    let canonical_root = match std::fs::canonicalize(root) {
        Ok(root) => root,
        Err(_) => return false,
    };
    let canonical_path = match get_canonical_path(path) {
        Ok(path) => path,
        Err(_) => return false,
    };
    canonical_path.starts_with(canonical_root)
}

pub fn is_safe_media_path(path_str: &str) -> bool {
    if path_str.is_empty() || path_str.contains('\0') {
        return false;
    }
    is_path_inside(Path::new(path_str), &crate::browser::media_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gemini-paths-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("media")).unwrap();
        dir
    }

    #[test]
    fn files_must_stay_inside_the_root() {
        let dir = temp_tree();
        let root = dir.join("media");
        let inside = root.join("picture.png");
        std::fs::write(&inside, b"png").unwrap();
        let outside = dir.join("secret.txt");
        std::fs::write(&outside, b"no").unwrap();
        let sibling = dir.join("media-backup");
        std::fs::create_dir_all(&sibling).unwrap();
        let sibling_file = sibling.join("picture.png");
        std::fs::write(&sibling_file, b"no").unwrap();

        assert!(is_path_inside(&inside, &root));
        assert!(!is_path_inside(&outside, &root));
        assert!(!is_path_inside(&sibling_file, &root));
        assert!(!is_path_inside(&root.join("..").join("secret.txt"), &root));
        assert!(!is_path_inside(Path::new(""), &root));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_out_of_the_root_is_rejected() {
        let dir = temp_tree();
        let root = dir.join("media");
        let outside = dir.join("secret.txt");
        std::fs::write(&outside, b"no").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(!is_path_inside(&root.join("link"), &root));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_root_rejects_every_path() {
        let missing = std::env::temp_dir().join(format!("gemini-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(!is_path_inside(&missing.join("file"), &missing));
    }
}
