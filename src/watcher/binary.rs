//! Noticing that the installed brg changed, so the watcher can restart on it.
//!
//! A brg's identity is the SHA-256 of its bytes. Hashing every pass would
//! be wasteful, so each pass compares the installed file's device, inode,
//! mtime and size with the running brg's. A difference must hold steady
//! for a quiet period (an install in progress keeps changing them) before
//! the file is hashed, through one opened descriptor whose metadata, and
//! the file the path names, must be unchanged after the read. Only
//! different bytes, read steadily, are a change. The caller
//! restarts at a pass boundary, through [`super::restart::mark_planned`].

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// The cheap part of a file's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    size: u64,
}

impl Stat {
    fn of(path: &Path) -> std::io::Result<Stat> {
        Ok(Stat::from(&std::fs::metadata(path)?))
    }

    fn from(m: &std::fs::Metadata) -> Stat {
        Stat {
            dev: m.dev(),
            ino: m.ino(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            size: m.size(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryId {
    pub path: PathBuf,
    stat: Stat,
    /// The identity proper, as lowercase hex.
    pub sha256: String,
}

/// Hash the file at `path` through one opened descriptor, and check that
/// nothing changed meanwhile: the descriptor's metadata after the read
/// matches before it, and `path` still names that same file. `None` if
/// anything moved. `between` runs after the read, before the checks (a
/// test seam).
fn read_steady(path: &Path, between: &mut dyn FnMut()) -> std::io::Result<Option<BinaryId>> {
    let mut file = std::fs::File::open(path)?;
    let before = Stat::from(&file.metadata()?);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    between();
    let after = Stat::from(&file.metadata()?);
    let named = Stat::of(path)?;
    if after != before || named != before {
        return Ok(None);
    }
    Ok(Some(BinaryId {
        path: path.to_path_buf(),
        stat: before,
        sha256: crate::config_snapshot::hex(&hasher.finalize()),
    }))
}

impl BinaryId {
    /// The file at `path` now (following symlinks, as running it would).
    /// An error if it changed while being read.
    pub fn read(path: &Path) -> std::io::Result<Self> {
        read_steady(path, &mut || {})?.ok_or_else(|| {
            std::io::Error::other(format!("{} changed while being read", path.display()))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Same,
    /// The file differs and is waiting out the quiet period.
    Settling,
    /// Different bytes, steady for the quiet period: plan a restart.
    Changed(BinaryId),
}

#[derive(Debug, Clone)]
struct Settling {
    stat: Stat,
    since: SystemTime,
    /// Hashed once the quiet period passed.
    id: Option<BinaryId>,
}

/// Watches the installed brg against the running one.
#[derive(Debug, Clone)]
pub struct BinaryWatch {
    running: BinaryId,
    settling: Option<Settling>,
}

impl BinaryWatch {
    pub fn new(running: BinaryId) -> Self {
        BinaryWatch {
            running,
            settling: None,
        }
    }

    pub fn running(&self) -> &BinaryId {
        &self.running
    }

    /// Check `installed` once. An error (the file missing mid-install, say)
    /// means "don't know": keep running.
    pub fn poll(
        &mut self,
        installed: &Path,
        now: SystemTime,
        quiet: Duration,
    ) -> std::io::Result<Poll> {
        self.poll_with(installed, now, quiet, &mut || {})
    }

    fn poll_with(
        &mut self,
        installed: &Path,
        now: SystemTime,
        quiet: Duration,
        while_hashing: &mut dyn FnMut(),
    ) -> std::io::Result<Poll> {
        let stat = Stat::of(installed)?;
        if installed == self.running.path && stat == self.running.stat {
            self.settling = None;
            return Ok(Poll::Same);
        }
        let s = match &mut self.settling {
            Some(s) if s.stat == stat => s,
            _ => {
                self.settling = Some(Settling {
                    stat,
                    since: now,
                    id: None,
                });
                return Ok(Poll::Settling);
            }
        };
        if now.duration_since(s.since).unwrap_or_default() < quiet {
            return Ok(Poll::Settling);
        }
        let id = match &s.id {
            Some(id) => id.clone(),
            None => match read_steady(installed, while_hashing)? {
                Some(id) if id.stat == stat => {
                    s.id = Some(id.clone());
                    id
                }
                _ => {
                    // Changed since the quiet period, or while hashing:
                    // start the quiet period again.
                    self.settling = None;
                    return Ok(Poll::Settling);
                }
            },
        };
        if id.sha256 == self.running.sha256 {
            // Same bytes, new file: adopt its stat so later passes are cheap.
            self.running = id;
            self.settling = None;
            return Ok(Poll::Same);
        }
        Ok(Poll::Changed(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    // Catches: a restart onto bytes read while the install was still
    // writing or replacing the file, so the hash is of a partial file or
    // of the old one.
    #[test]
    fn a_change_while_hashing_restarts_the_quiet_period() {
        let tmp = tempfile::tempdir().unwrap();
        let brg = tmp.path().join("brg");
        fs::write(&brg, b"version one").unwrap();
        let quiet = Duration::from_secs(10);
        let replace = |bytes: &[u8]| {
            let next = tmp.path().join("brg.next");
            fs::write(&next, bytes).unwrap();
            fs::rename(&next, &brg).unwrap();
        };
        let changes: [&dyn Fn(); 2] = [
            // Rewritten in place, a different length.
            &|| fs::write(&brg, b"version two, still writing").unwrap(),
            // Replaced by another file.
            &|| replace(b"version two, other file"),
        ];
        for change in changes {
            fs::write(&brg, b"version one").unwrap();
            let mut watch = BinaryWatch::new(BinaryId::read(&brg).unwrap());
            replace(b"version two");
            assert_eq!(watch.poll(&brg, t(0), quiet).unwrap(), Poll::Settling);
            let mut hook = || change();
            assert_eq!(
                watch.poll_with(&brg, t(10), quiet, &mut hook).unwrap(),
                Poll::Settling
            );
            // The quiet period starts again from the next look.
            assert_eq!(watch.poll(&brg, t(11), quiet).unwrap(), Poll::Settling);
            assert_eq!(watch.poll(&brg, t(20), quiet).unwrap(), Poll::Settling);
            assert!(matches!(
                watch.poll(&brg, t(21), quiet).unwrap(),
                Poll::Changed(_)
            ));
        }
    }
}
