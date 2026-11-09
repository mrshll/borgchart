//! Disposable normalized cache. Every file operation is anchored to an owned
//! directory descriptor; credentials are never opened here.
use super::{
    policy::{self, Provider, Snapshot},
    protocol::MAX_OUTPUT,
};
use rustix::fs::{self as fd, Mode, OFlags};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

fn unsafe_path() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn checked(file: &File, directory: bool) -> io::Result<()> {
    let m = file.metadata()?;
    if m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
        || if directory {
            !m.is_dir()
        } else {
            !m.is_file() || m.nlink() != 1
        }
    {
        #[cfg(test)]
        eprintln!(
            "cache metadata rejected: directory={directory} owned={} mode={:o} regular={} dir={} links={}",
            m.uid() == rustix::process::geteuid().as_raw(),
            m.mode() & 0o7777,
            m.is_file(),
            m.is_dir(),
            m.nlink()
        );
        return Err(unsafe_path());
    }
    Ok(())
}
struct Directory(File);
impl Directory {
    fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(unsafe_path());
        }
        let leaf = path.file_name().ok_or_else(unsafe_path)?;
        let mut base = path.parent().ok_or_else(unsafe_path)?;
        let mut missing = Vec::new();
        while !base.try_exists()? {
            missing.push(base.file_name().ok_or_else(unsafe_path)?.to_owned());
            base = base.parent().ok_or_else(unsafe_path)?;
        }
        // Canonicalize the existing base so macOS /var and /tmp aliases work.
        // The cache leaf is deliberately excluded and must never be a symlink.
        let base = fs::canonicalize(base)
            .inspect_err(|error| diagnostic("directory canonicalize", error))?;
        let mut parent: File = fd::open(
            &base,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        missing.reverse();
        missing.push(leaf.to_owned());
        for component in missing {
            match fd::mkdirat(&parent, &component, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(e) => {
                    let error = io::Error::from(e);
                    diagnostic("directory mkdirat", &error);
                    return Err(error);
                }
            }
            let next: File = fd::openat(
                &parent,
                &component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
            checked(&next, true)?;
            parent = next;
        }
        Ok(Self(parent))
    }
    fn file(&self, name: &str, flags: OFlags) -> io::Result<File> {
        if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
            return Err(unsafe_path());
        }
        let file: File = fd::openat(
            &self.0,
            name,
            flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)
        .inspect_err(|error| {
            diagnostic(
                if flags.contains(OFlags::EXCL) {
                    "exclusive file openat"
                } else if flags.contains(OFlags::CREATE) {
                    "creating file openat"
                } else {
                    "existing file openat"
                },
                error,
            )
        })?
        .into();
        checked(&file, false)?;
        Ok(file)
    }
    fn lock(&self, key: &str, deadline: Instant) -> io::Result<File> {
        let file = self
            .file(&format!("{key}.lock"), OFlags::CREATE | OFlags::RDWR)
            .inspect_err(|error| diagnostic("lock create-open", error))?;
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match fd::flock(&file, fd::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(file),
                Err(rustix::io::Errno::WOULDBLOCK) => std::thread::sleep(
                    Duration::from_millis(50)
                        .min(deadline.saturating_duration_since(Instant::now())),
                ),
                Err(e) => return Err(e.into()),
            }
        }
    }
    fn read(&self, key: &str, provider: Provider, now: SystemTime) -> io::Result<Option<Snapshot>> {
        let file = match self.file(&format!("{key}.json"), OFlags::RDONLY) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_OUTPUT {
            return Ok(None);
        }
        Ok(serde_json::from_slice::<Snapshot>(&bytes)
            .ok()
            .filter(|s| s.valid(provider, now)))
    }
    fn write(&self, key: &str, value: &Snapshot) -> io::Result<()> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| unsafe_path())?
            .as_nanos();
        let temp = format!(
            "usage-{}-{nonce}-{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut file = self
            .file(&temp, OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY)
            .inspect_err(|error| diagnostic("write temp create", error))?;
        let result = (|| {
            let bytes = serde_json::to_vec(value)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
            file.write_all(&bytes)?;
            fd::renameat(&self.0, &temp, &self.0, format!("{key}.json"))
                .map_err(io::Error::from)
                .inspect_err(|error| diagnostic("write renameat", error))?;
            Ok(())
        })();
        let _ = fd::unlinkat(&self.0, &temp, fd::AtFlags::empty());
        result
    }
}

pub fn snapshot(
    path: &Path,
    provider: Provider,
    refresh: bool,
    lock_deadline: Instant,
    mut identity: impl FnMut() -> io::Result<String>,
    mut probe: impl FnMut() -> io::Result<(serde_json::Value, SystemTime)>,
) -> Snapshot {
    let result = (|| -> io::Result<Snapshot> {
        let dir = Directory::open(path).inspect_err(|error| diagnostic("directory open", error))?;
        let key = identity()?;
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(unsafe_path());
        }
        let _lock = dir
            .lock(&key, lock_deadline)
            .inspect_err(|error| diagnostic("lock", error))?;
        if identity()? != key {
            return Err(unsafe_path());
        }
        let now = SystemTime::now();
        let previous = dir
            .read(&key, provider, now)
            .inspect_err(|error| diagnostic("read", error))?;
        if identity()? != key {
            return Err(unsafe_path());
        }
        let last_success = previous.as_ref().and_then(|p| {
            p.last_success_at
                .clone()
                .or_else(|| (p.status == "ok").then(|| p.observed_at.clone()))
        });
        if let Some(p) = &previous
            && policy::time(&serde_json::Value::String(p.expires_at.clone()))
                .is_some_and(|expiry| expiry > now)
            && (!refresh || p.status == "unknown")
        {
            return Ok(p.clone());
        }
        let success = (|| -> io::Result<Snapshot> {
            let (data, observed) = probe()?;
            if last_success
                .as_ref()
                .and_then(|s| policy::time(&serde_json::Value::String(s.clone())))
                .is_some_and(|prior| observed < prior)
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let normalized = policy::normalize(provider, &data, observed, previous.as_ref());
            if identity()? != key
                || normalized.status != "ok"
                || !normalized.valid(provider, SystemTime::now())
            {
                return Err(unsafe_path());
            }
            Ok(normalized)
        })();
        let mut result = success.unwrap_or_else(|error| {
            diagnostic("probe", &error);
            policy::unknown(
                provider,
                SystemTime::now(),
                previous
                    .as_ref()
                    .map(|p| p.held_keys.clone())
                    .unwrap_or_default(),
            )
        });
        result.last_success_at = if result.status == "ok" {
            Some(result.observed_at.clone())
        } else {
            last_success
        };
        dir.write(&key, &result)
            .inspect_err(|error| diagnostic("write", error))?;
        Ok(result)
    })();
    result.unwrap_or_else(|error| {
        diagnostic("cache I/O", &error);
        policy::unknown(provider, SystemTime::now(), Vec::new())
    })
}
fn diagnostic(_stage: &str, _error: &io::Error) {
    #[cfg(test)]
    eprintln!(
        "{_stage}: kind={:?} os={:?}",
        _error.kind(),
        _error.raw_os_error()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{Arc, atomic::AtomicUsize};
    fn key() -> io::Result<String> {
        Ok("a".repeat(64))
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
    fn probe() -> io::Result<(serde_json::Value, SystemTime)> {
        let now = SystemTime::now();
        Ok((
            json!({"ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","allowed":true,"primary":{"usedPercent":94,"resetsAt":(now + Duration::from_secs(3600)).duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(),"windowDurationMins":300}}}),
            now,
        ))
    }
    #[test]
    fn private_cache_reuses_success_and_refresh_obeys_failure_backoff() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache");
        let first = snapshot(&path, Provider::Codex, false, deadline(), key, probe);
        assert_eq!(first.status, "ok");
        let cached = snapshot(&path, Provider::Codex, false, deadline(), key, || {
            panic!("unnecessary probe")
        });
        assert_eq!(cached.observed_at, first.observed_at);
        let failed = snapshot(&path, Provider::Codex, true, deadline(), key, || {
            Err(io::Error::other("DO_NOT_LOG"))
        });
        assert_eq!(failed.status, "unknown");
        assert_eq!(failed.held_keys, first.held_keys);
        assert_eq!(failed.last_success_at, Some(first.observed_at));
        let again = snapshot(&path, Provider::Codex, true, deadline(), key, || {
            panic!("refresh bypassed backoff")
        });
        assert_eq!(again.observed_at, failed.observed_at);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o700);
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(entry.metadata().unwrap().mode() & 0o7777, 0o600);
            assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
            assert!(
                !fs::read_to_string(entry.path())
                    .unwrap()
                    .contains("DO_NOT_LOG")
            );
        }
    }
    #[test]
    fn unsafe_paths_refuse_before_probe_but_canonical_parent_alias_works() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = temp.path().join("alias");
        symlink(&target, &alias).unwrap();
        assert_eq!(
            snapshot(&alias, Provider::Codex, false, deadline(), key, || panic!(
                "symlink leaf probed"
            ))
            .status,
            "unknown"
        );
        assert_eq!(
            snapshot(
                &alias.join("cache"),
                Provider::Codex,
                false,
                deadline(),
                key,
                probe
            )
            .status,
            "ok"
        );
        for kind in ["symlink", "hardlink", "mode"] {
            let path = temp.path().join(kind);
            let dir = Directory::open(&path).unwrap();
            let name = format!("{}.json", key().unwrap());
            let target = temp.path().join(format!("{kind}-file"));
            fs::write(&target, "{}").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            match kind {
                "symlink" => symlink(&target, path.join(&name)).unwrap(),
                "hardlink" => fs::hard_link(&target, path.join(&name)).unwrap(),
                _ => fs::write(path.join(&name), "{}").unwrap(),
            }
            if kind == "mode" {
                fs::set_permissions(path.join(&name), fs::Permissions::from_mode(0o644)).unwrap();
            }
            assert!(
                dir.read(&key().unwrap(), Provider::Codex, SystemTime::now())
                    .is_err()
            );
            assert_eq!(
                snapshot(&path, Provider::Codex, false, deadline(), key, || panic!(
                    "unsafe file probed"
                ))
                .status,
                "unknown"
            );
        }
    }
    #[test]
    fn concurrent_reads_serialize_and_lock_wait_is_bounded() {
        for _ in 0..24 {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("cache");
            let calls = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(std::sync::Barrier::new(3));
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for _ in 0..3 {
                    let calls = Arc::clone(&calls);
                    let barrier = Arc::clone(&barrier);
                    let path = &path;
                    handles.push(scope.spawn(move || {
                        barrier.wait();
                        snapshot(path, Provider::Codex, false, deadline(), key, || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(30));
                            probe()
                        })
                    }));
                }
                for handle in handles {
                    assert_eq!(handle.join().unwrap().status, "ok");
                }
            });
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let dir = Directory::open(&path).unwrap();
            let _lock = dir.lock(&key().unwrap(), deadline()).unwrap();
            let start = Instant::now();
            assert_eq!(
                snapshot(
                    &path,
                    Provider::Codex,
                    true,
                    start + Duration::from_millis(30),
                    key,
                    || panic!("locked probe")
                )
                .status,
                "unknown"
            );
            assert!(start.elapsed() < Duration::from_secs(1));
        }
    }
    #[test]
    fn identity_change_and_regressed_observation_never_authorize() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache");
        let first = snapshot(&path, Provider::Codex, false, deadline(), key, probe);
        assert_eq!(first.status, "ok");
        let mut calls = 0;
        let changed = snapshot(
            &path,
            Provider::Codex,
            false,
            deadline(),
            || {
                calls += 1;
                Ok(if calls < 3 { "a" } else { "b" }.repeat(64))
            },
            || panic!("identity changed before probe"),
        );
        assert_eq!(changed.status, "unknown");
        let regressed = snapshot(&path, Provider::Codex, true, deadline(), key, || {
            let (data, now) = probe()?;
            Ok((data, now - Duration::from_secs(20)))
        });
        assert_eq!(regressed.status, "unknown");
        assert_eq!(regressed.held_keys, first.held_keys);
    }
    #[test]
    fn malformed_or_foreign_cache_is_replaced_by_a_fresh_normalized_reading() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache");
        let first = snapshot(&path, Provider::Codex, false, deadline(), key, probe);
        assert_eq!(first.status, "ok");
        let file = path.join(format!("{}.json", key().unwrap()));
        for kind in ["schema", "extra", "future", "json"] {
            let mut bad = serde_json::to_value(&first).unwrap();
            match kind {
                "schema" => bad["schema_version"] = json!(1),
                "extra" => bad["DO_NOT_LOG"] = json!("foreign payload"),
                "future" => {
                    bad["observed_at"] = json!(policy::stamp(
                        SystemTime::now() + Duration::from_secs(86400)
                    ))
                }
                _ => {}
            }
            fs::write(
                &file,
                if kind == "json" {
                    b"DO_NOT_LOG_BAD_JSON".to_vec()
                } else {
                    serde_json::to_vec(&bad).unwrap()
                },
            )
            .unwrap();
            let mut calls = 0;
            assert_eq!(
                snapshot(&path, Provider::Codex, false, deadline(), key, || {
                    calls += 1;
                    probe()
                })
                .status,
                "ok"
            );
            assert_eq!(calls, 1);
            assert!(!fs::read_to_string(&file).unwrap().contains("DO_NOT_LOG"));
        }
    }
    #[test]
    fn explicit_refreshes_serialize_their_probes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache");
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let calls = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..3)
                .map(|_| {
                    scope.spawn(|| {
                        snapshot(&path, Provider::Codex, true, deadline(), key, || {
                            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(count, Ordering::SeqCst);
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(20));
                            active.fetch_sub(1, Ordering::SeqCst);
                            probe()
                        })
                    })
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().unwrap().status, "ok");
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn existing_lock_must_still_be_private_regular_with_one_link() {
        for kind in ["symlink", "hardlink", "mode", "directory"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("cache");
            let _dir = Directory::open(&path).unwrap();
            let lock = path.join(format!("{}.lock", key().unwrap()));
            let target = temp.path().join("untouched");
            fs::write(&target, "unchanged").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            match kind {
                "symlink" => symlink(&target, &lock).unwrap(),
                "hardlink" => fs::hard_link(&target, &lock).unwrap(),
                "mode" => {
                    fs::write(&lock, "unchanged").unwrap();
                    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
                }
                _ => fs::create_dir(&lock).unwrap(),
            }
            assert_eq!(
                snapshot(&path, Provider::Codex, false, deadline(), key, || panic!(
                    "unsafe lock authorized a probe"
                ))
                .status,
                "unknown"
            );
            assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
        }
    }

    #[test]
    fn cold_directory_and_lock_open_succeed_for_every_contender() {
        for _ in 0..16 {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("cache");
            let barrier = std::sync::Barrier::new(8);
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            let dir = Directory::open(&path)?;
                            let _lock = dir.lock(&key()?, deadline())?;
                            Ok::<(), io::Error>(())
                        })
                    })
                    .collect();
                for handle in handles {
                    let result = handle.join().unwrap();
                    assert!(
                        result.is_ok(),
                        "cold lock failure: {:?}",
                        result.err().map(|e| (e.kind(), e.raw_os_error()))
                    );
                }
            });
        }
    }
}
