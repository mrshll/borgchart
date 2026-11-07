//! Noticing that the installed brg changed, so the watcher can restart on it.
//!
//! A brg's identity is the SHA-256 of its bytes. Hashing every pass would
//! be wasteful, so each pass compares the installed file's device, inode,
//! mtime and size with the running brg's. A difference must hold steady
//! for a quiet period (an install in progress keeps changing them) before
//! the file is hashed, and only different bytes are a change. The caller
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
        let m = std::fs::metadata(path)?;
        Ok(Stat {
            dev: m.dev(),
            ino: m.ino(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            size: m.size(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryId {
    pub path: PathBuf,
    stat: Stat,
    /// The identity proper, as lowercase hex.
    pub sha256: String,
}

fn hash(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::bus::manifest::hex(&hasher.finalize()))
}

impl BinaryId {
    /// The file at `path` now (following symlinks, as running it would).
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let stat = Stat::of(path)?;
        Ok(BinaryId {
            path: path.to_path_buf(),
            stat,
            sha256: hash(path)?,
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
            None => {
                let id = BinaryId::read(installed)?;
                if id.stat != stat {
                    // Changed while hashing: start the quiet period again.
                    self.settling = None;
                    return Ok(Poll::Settling);
                }
                s.id = Some(id.clone());
                id
            }
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
