//! The alert book: what needs a person, in `alerts.json`, for `status` to
//! show. Nothing here runs a command or contacts anyone.
//!
//! An alert is one cause: a [`Kind`] for a [`Subject`]. It opens once, keeps
//! the time it opened while its cause stays, and clears itself when its
//! cause clears. The watcher feeds the book each scan from the wake
//! decision ([`Book::note_member`]) and its own health
//! ([`Book::note_watcher`]), and saves it; the book survives a restart.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::decide::{Action, NeedsPerson};
use super::heartbeat::Health;
use super::lock::Lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// The member was woken repeatedly with nothing new read.
    Repeats,
    /// The member can't be reached: no usable method, or a screen that
    /// refuses the one owed.
    Unreachable,
    /// The watcher is parked after a crash loop.
    Parked,
    /// The watcher's last scan is stale.
    ScanStale,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    Member(String),
    Watcher,
}

/// An open alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub kind: Kind,
    pub subject: Subject,
    #[serde(with = "super::utc")]
    pub opened: SystemTime,
    /// What the cause was when it opened, for a person to read.
    pub detail: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    alerts: Vec<Alert>,
}

#[derive(Debug, Default)]
pub struct Book {
    file: File,
}

fn path(dir: &Path) -> PathBuf {
    dir.join("alerts.json")
}

/// The alert a decision calls for, if any: its kind and detail.
fn wanted_for(action: &Action) -> Option<(Kind, String)> {
    let Action::NeedsPerson(why) = action else {
        return None;
    };
    let kind = match why {
        NeedsPerson::Repeats { .. } => Kind::Repeats,
        NeedsPerson::UnknownVersion
        | NeedsPerson::RetriesExhausted { .. }
        | NeedsPerson::Unreachable { .. }
        | NeedsPerson::MethodWithdrawn(_)
        | NeedsPerson::Screen(_) => Kind::Unreachable,
    };
    Some((kind, format!("{why:?}")))
}

impl Book {
    pub fn load(dir: &Path) -> Result<Book, String> {
        Ok(Book {
            file: super::load(&path(dir))?.unwrap_or_default(),
        })
    }

    pub fn save(&self, _lock: &Lock, dir: &Path) -> Result<(), String> {
        super::save(&path(dir), &self.file)
    }

    /// The open alerts.
    pub fn open(&self) -> &[Alert] {
        &self.file.alerts
    }

    /// Make `subject`'s alerts among `kinds` exactly `wanted`: open the
    /// missing one, keep one already open, clear the rest.
    fn set(
        &mut self,
        subject: &Subject,
        kinds: &[Kind],
        wanted: Option<(Kind, String)>,
        now: SystemTime,
    ) {
        let want_kind = wanted.as_ref().map(|(k, _)| *k);
        self.file.alerts.retain(|a| {
            &a.subject != subject || !kinds.contains(&a.kind) || Some(a.kind) == want_kind
        });
        if let Some((kind, detail)) = wanted
            && !self
                .file
                .alerts
                .iter()
                .any(|a| &a.subject == subject && a.kind == kind)
        {
            self.file.alerts.push(Alert {
                kind,
                subject: subject.clone(),
                opened: now,
                detail,
            });
        }
    }

    /// Feed one member's wake decision. A person needed opens (or keeps)
    /// its alert; a wake or nothing owed clears the member's alerts; a
    /// deferral, being no evidence, leaves them as they are.
    pub fn note_member(&mut self, member: &str, action: &Action, now: SystemTime) {
        if matches!(action, Action::Defer(_)) {
            return;
        }
        self.set(
            &Subject::Member(member.to_string()),
            &[Kind::Repeats, Kind::Unreachable],
            wanted_for(action),
            now,
        );
    }

    /// Feed the watcher's own health. Parked and a stale scan open their
    /// alerts; `Ok` clears them. A missing, malformed or stale heartbeat
    /// says nothing about either, so it leaves them as they are.
    pub fn note_watcher(&mut self, health: &Health, now: SystemTime) {
        let wanted = match health {
            Health::Parked => Some((Kind::Parked, "the watcher is parked".to_string())),
            Health::ScanStale(age) => Some((
                Kind::ScanStale,
                format!("the last scan finished {}s ago", age.as_secs()),
            )),
            Health::Ok => None,
            Health::Missing | Health::Malformed(_) | Health::Stale(_) => return,
        };
        self.set(
            &Subject::Watcher,
            &[Kind::Parked, Kind::ScanStale],
            wanted,
            now,
        );
    }

    /// Clear the alerts of members no longer in the team.
    pub fn keep_members(&mut self, members: &[&str]) {
        self.file.alerts.retain(|a| match &a.subject {
            Subject::Member(m) => members.contains(&m.as_str()),
            Subject::Watcher => true,
        });
    }
}

/// Every open alert, for a reader such as `status`.
pub fn read(dir: &Path) -> Result<Vec<Alert>, String> {
    Ok(Book::load(dir)?.file.alerts)
}
