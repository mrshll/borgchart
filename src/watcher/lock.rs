//! The watcher's lock: `lock` in its state folder, held (flock, exclusive)
//! for the life of the one watcher. Counting starts, writing the heartbeat
//! and recording attempts all take a [`Lock`], so a second watcher can't.

use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rustix::fs::{FlockOperation, OFlags, flock};

/// Proof that this process is the folder's watcher. Released when dropped.
#[derive(Debug)]
pub struct Lock {
    _file: std::fs::File,
}

impl Lock {
    /// Take the lock without waiting. `Ok(None)` if another watcher holds
    /// it: the caller exits, and that is not a crash (it never reaches
    /// [`super::restart::count_start`]).
    pub fn try_acquire(dir: &Path) -> Result<Option<Lock>, String> {
        super::ensure_dir(dir)?;
        let path = dir.join("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        match flock(file.as_fd(), FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Lock { _file: file })),
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => Ok(None),
            Err(e) => Err(format!("cannot lock {}: {e}", path.display())),
        }
    }
}
