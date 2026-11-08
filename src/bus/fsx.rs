//! Durable, private, no-replace file publication.
//!
//! Every immutable bus file is written the same way: a uniquely named
//! temporary sibling (so concurrent writers never share one), flushed to
//! disk, then hard-linked to its final name, which fails if the name already
//! exists. The directory is synced afterwards so the new name survives a
//! crash. A crash can leave a stray `.tmp-*` file, never a partial final file.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Prefix of temporary files; readers skip names starting with it.
pub const TMP_PREFIX: &str = ".tmp-";

fn temp_sibling(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!(
        "{TMP_PREFIX}{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    path.with_file_name(name)
}

/// What `write_new` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// The file is new and holds `contents`.
    Created,
    /// A file already had that name; it was left untouched.
    Exists,
}

/// Create `dir` and any missing ancestors, each with mode 0700, syncing the
/// directory that holds each new entry so the new names survive a crash.
/// Existing directories are left as they are.
pub fn create_private_dirs(dir: &Path) -> io::Result<()> {
    if fs::symlink_metadata(dir).is_ok() {
        return Ok(());
    }
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other(format!("{} has no parent", dir.display())))?;
    create_private_dirs(parent)?;
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => sync_dir(parent),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write `contents` to `path` (mode 0600) only if `path` does not exist yet.
/// Missing parent directories are created as by [`create_private_dirs`].
pub fn write_new(path: &Path, contents: &[u8]) -> io::Result<Written> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    create_private_dirs(parent)?;
    let tmp = temp_sibling(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        match fs::hard_link(&tmp, path) {
            Ok(()) => Ok(Written::Created),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Written::Exists),
            Err(e) => Err(e),
        }
    })();
    let _ = fs::remove_file(&tmp);
    let written = result?;
    if written == Written::Created {
        sync_dir(parent)?;
    }
    Ok(written)
}

/// Replace `path` atomically with `contents` (mode 0600), using a unique
/// temporary sibling. For small mutable state files, not immutable records.
pub fn replace(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    create_private_dirs(parent)?;
    let tmp = temp_sibling(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    sync_dir(parent)
}

pub fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// Make sure `base` and `dir` (which must be inside `base`) exist and are
/// private: each of `base` and every directory from it down to `dir` must be
/// a real directory (not a symlink), owned by you and not writable by group
/// or others. Existing components are checked before anything is created
/// beneath them; missing ones are created with mode 0700 (see
/// [`create_private_dirs`]) and then checked. Writers call this before
/// writing into a store, so a root under a shared directory such as `/tmp`
/// can't have been planted or redirected by someone else.
pub fn ensure_private_tree(base: &Path, dir: &Path) -> io::Result<()> {
    let rel = dir.strip_prefix(base).map_err(|_| {
        io::Error::other(format!(
            "{} is not inside {}",
            dir.display(),
            base.display()
        ))
    })?;
    if rel
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(io::Error::other(format!(
            "{} must not contain `.` or `..`",
            dir.display()
        )));
    }
    create_private_dirs(base)?;
    check_private_dir(base)?;
    let mut current = base.to_path_buf();
    for part in rel.components() {
        current.push(part);
        create_private_dirs(&current)?;
        check_private_dir(&current)?;
    }
    Ok(())
}

/// A real directory, owned by the current user, not group/other-writable.
pub fn check_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path)?;
    let shown = path.display();
    if meta.file_type().is_symlink() {
        return Err(io::Error::other(format!("{shown} is a symlink")));
    }
    if !meta.is_dir() {
        return Err(io::Error::other(format!("{shown} is not a directory")));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::other(format!(
            "{shown} is owned by someone else"
        )));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(io::Error::other(format!(
            "{shown} is writable by group or others"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn no_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/rec.json");
        assert_eq!(write_new(&path, b"one").unwrap(), Written::Created);
        assert_eq!(write_new(&path, b"two").unwrap(), Written::Exists);
        assert_eq!(fs::read(&path).unwrap(), b"one");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let strays: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(TMP_PREFIX))
            .collect();
        assert!(strays.is_empty());
    }

    #[test]
    fn concurrent_writers_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || write_new(&path, format!("w{i}").as_bytes()).unwrap())
            })
            .collect();
        let created = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|w| *w == Written::Created)
            .count();
        assert_eq!(created, 1);
    }

    #[test]
    fn private_tree_refuses_symlinks_and_open_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("instance");
        let store = base.join("messages");
        ensure_private_tree(&base, &store).unwrap();
        let mode = fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        ensure_private_tree(&base, &store).unwrap();

        fs::set_permissions(&store, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_private_tree(&base, &store).is_err());
        fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();

        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
        let err = ensure_private_tree(&linked, &linked.join("messages")).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(
            !elsewhere.join("messages").exists(),
            "nothing is created under a refused symlink"
        );
        let hop = base.join("hop");
        std::os::unix::fs::symlink(&elsewhere, &hop).unwrap();
        assert!(ensure_private_tree(&base, &hop.join("records")).is_err());
        assert!(!elsewhere.join("records").exists());
        assert!(ensure_private_tree(&base, &elsewhere).is_err());
        assert!(ensure_private_tree(&base, &base.join("../x")).is_err());
    }

    #[test]
    fn new_directories_are_private_at_every_level() {
        use std::os::unix::fs::PermissionsExt;
        let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o002));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/rec.json");
        let result = write_new(&path, b"x");
        rustix::process::umask(old);
        result.unwrap();
        for rel in ["a", "a/b", "a/b/c"] {
            let mode = fs::metadata(dir.path().join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{rel}");
        }
    }

    #[test]
    fn replace_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        replace(&path, b"one").unwrap();
        replace(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
    }
}
