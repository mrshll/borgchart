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
