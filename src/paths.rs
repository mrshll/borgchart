//! Resolving file references so they cannot escape their root directory.

use std::path::{Component, Path, PathBuf};

/// Resolve `rel` under `root`, refusing absolute paths, `..` components and
/// symlinks that lead outside `root`. The target must exist.
pub fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.is_empty() {
        return Err("path is empty".into());
    }
    if rel_path.is_absolute() {
        return Err(format!("`{rel}` must be a relative path"));
    }
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("`{rel}` must not contain `..`"));
    }
    let joined = root.join(rel_path);
    let real = joined
        .canonicalize()
        .map_err(|_| format!("`{rel}` does not exist"))?;
    let real_root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
    if !real.starts_with(&real_root) {
        return Err(format!(
            "`{rel}` resolves outside {} (through a symlink)",
            root.display()
        ));
    }
    Ok(real)
}

/// `path` with its longest existing prefix canonicalized (following
/// symlinks) and the missing rest appended, so a folder that doesn't exist
/// yet is still placed correctly. `..` in the missing rest is applied
/// lexically.
pub fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while existing.canonicalize().is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for name in rest.into_iter().rev() {
        if name == ".." {
            out.pop();
        } else if name != "." {
            out.push(name);
        }
    }
    out
}

/// Whether two folders are the same, or one is inside the other, once
/// symlinks are resolved.
pub fn overlaps(a: &Path, b: &Path) -> bool {
    let a = resolve_existing_prefix(a);
    let b = resolve_existing_prefix(b);
    a.starts_with(&b) || b.starts_with(&a)
}

/// Whether `path` is a regular file with an executable bit set.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Every executable named `name` on `PATH`, in order.
pub fn find_on_path(name: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .filter(|candidate| is_executable(candidate))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_same_inside_or_around_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(a.join("b")).unwrap();
        std::os::unix::fs::symlink(&a, dir.path().join("link")).unwrap();
        assert!(overlaps(&a, &a));
        assert!(overlaps(&a, &a.join("b")));
        assert!(overlaps(&a.join("b"), &a));
        assert!(overlaps(&dir.path().join("link/b"), &a));
        assert!(overlaps(&a.join("not-yet"), &a));
        assert!(!overlaps(&a.join("b"), &dir.path().join("c")));
        assert!(!overlaps(&dir.path().join("ab"), &a));
    }
}
