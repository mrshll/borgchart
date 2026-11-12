//! The registry of teams, the suffixes that make team and member
//! IDs, and each team's own record.
//!
//! - `<state home>/registry.json` lists every team: its ID, its template's
//!   name and folder, its working folder, and when it was created. It also
//!   keeps every suffix ever issued for a team's name, so an ID is never
//!   issued twice, even after `delete`.
//! - `<state home>/registry.lock` is the one lock over the registry, suffix
//!   issue, checkout reservation, and the lifecycle steps that need them.
//! - `<state home>/<team>/team.json` is the team's record: its
//!   template folder, working folder, issued members, and the members that were
//!   running at the last stop.
//!
//! Both files are written whole (a temporary file, then a rename, 0600) while
//! the lock is held, so a reader without the lock sees the old file or the
//! new one.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::{self, StateDir};

pub const SCHEMA: u32 = 2;

/// The characters a suffix is made of: no `0 1 i l o`.
pub const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
pub const SUFFIX_LEN: usize = 4;

/// Whether `s` is a well-formed suffix.
pub fn is_suffix(s: &str) -> bool {
    s.len() == SUFFIX_LEN && s.bytes().all(|b| ALPHABET.contains(&b))
}

/// The longest name `create --name` takes.
pub const NAME_MAX: usize = 24;

/// Whether `name` can follow a role in an ID: lowercase letters and digits,
/// in words joined by single hyphens, at most [`NAME_MAX`] characters.
pub fn check_name(name: &str) -> Result<(), String> {
    let words = name.split('-').all(|w| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if words && name.len() <= NAME_MAX {
        return Ok(());
    }
    Err(format!(
        "`--name {name}` doesn't fit: use lowercase letters and digits in words joined by single hyphens, at most {NAME_MAX} characters, like `rounding-fix`"
    ))
}

/// Whether `id` is one brg issues for `role`: `<role>-<suffix>`, or
/// `<role>-<name>` from `create --name`.
pub fn is_id_of(id: &str, role: &str) -> bool {
    crate::names::is_valid_member_id(id)
        && id
            .strip_prefix(role)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|name| check_name(name).is_ok())
}

/// `<base>-<suffix>` split from the right, so a base with `-` in it works:
/// `dev-team-k3f9` is (`dev-team`, `k3f9`).
pub fn split_id(id: &str) -> Option<(&str, &str)> {
    let (base, suffix) = id.rsplit_once('-')?;
    (!base.is_empty() && is_suffix(suffix)).then_some((base, suffix))
}

/// Test builds: `BORGCHART_SUFFIX_SEED=<number>` makes the suffixes a fixed
/// sequence, the same in every process, so a test can force collisions.
#[cfg(feature = "test-support")]
fn seeded(n: usize) -> Option<Vec<u8>> {
    use std::sync::Mutex;
    static STATE: Mutex<Option<u64>> = Mutex::new(None);
    let mut state = STATE.lock().ok()?;
    if state.is_none() {
        let seed: u64 = std::env::var("BORGCHART_SUFFIX_SEED").ok()?.parse().ok()?;
        *state = Some(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    }
    let x = state.as_mut()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // xorshift64
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        out.push((*x >> 24) as u8);
    }
    Some(out)
}

#[cfg(not(feature = "test-support"))]
fn seeded(_n: usize) -> Option<Vec<u8>> {
    None
}

fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    if let Some(bytes) = seeded(n) {
        return Ok(bytes);
    }
    let mut buf = vec![0u8; n];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| format!("cannot read /dev/urandom: {e}"))?;
    Ok(buf)
}

/// One random suffix. Bytes past the largest multiple of the alphabet's
/// size are skipped, so every character is equally likely.
fn random_suffix() -> Result<String, String> {
    let limit = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    let mut out = String::with_capacity(SUFFIX_LEN);
    while out.len() < SUFFIX_LEN {
        for b in random_bytes(SUFFIX_LEN * 2)? {
            if b < limit && out.len() < SUFFIX_LEN {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    Ok(out)
}

/// The one suffix generator: a random suffix that `taken` doesn't hold,
/// retried on collision.
pub fn new_suffix(taken: impl Fn(&str) -> bool) -> Result<String, String> {
    for _ in 0..10_000 {
        let s = random_suffix()?;
        if !taken(&s) {
            return Ok(s);
        }
    }
    Err("brg couldn't find a free suffix; too many have been issued".into())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema: u32,
    #[serde(rename = "teams")]
    pub instances: BTreeMap<String, Entry>,
    /// Every suffix issued for each team name, kept after `delete`.
    pub issued: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(rename = "template")]
    pub team: String,
    #[serde(rename = "template_dir")]
    pub team_dir: PathBuf,
    pub folder: PathBuf,
    pub created_at: String,
}

/// `team.json`: one team's record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceRecord {
    pub schema: u32,
    pub id: String,
    #[serde(rename = "template")]
    pub team: String,
    #[serde(rename = "template_dir")]
    pub team_dir: PathBuf,
    pub folder: PathBuf,
    pub created_at: String,
    /// Every member issued in this team, by ID, with its role. A
    /// member's ID is never issued again in the team.
    pub members: BTreeMap<String, IssuedMember>,
    /// Child teams, including deleted IDs so their addresses are never reused.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(rename = "child_teams")]
    pub copies: BTreeMap<String, IssuedMember>,
    /// The full address of this child team's parent member in its declaring team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The members running when the team was last fully stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_at_stop: Option<BTreeSet<String>>,
    /// A cascade is unfinished; starts and creates in this subtree are refused.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopping: bool,
    /// Descendant child teams running at the last full stop, by relative team path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "child_teams_running_at_stop")]
    pub copies_running_at_stop: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuedMember {
    pub role: String,
    /// Deleted members reserve their suffix but have no live state.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// The working folder its latest start resolved (symlinks resolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

impl InstanceRecord {
    /// Members still kept in this team, running or stopped.
    pub fn kept_members(&self) -> impl Iterator<Item = (&String, &IssuedMember)> {
        self.members.iter().filter(|(_, m)| !m.deleted)
    }

    /// The members of `role`, by ID.
    pub fn members_of(&self, role: &str) -> Vec<String> {
        self.kept_members()
            .filter(|(_, m)| m.role == role)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The role a member fills.
    pub fn role_of(&self, member: &str) -> Option<&str> {
        self.members
            .get(member)
            .filter(|m| !m.deleted)
            .map(|m| m.role.as_str())
    }

    /// Issue a child team of a team role, preserving its suffix after deletion:
    /// `<role>-<name>` when named, else a random suffix no member or child team has.
    pub fn issue_copy(
        &mut self,
        role: &str,
        name: Option<&str>,
        is_role: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let id = match name {
            Some(name) => self.named(role, name, is_role)?,
            None => {
                let suffix = new_suffix(|s| {
                    self.members
                        .keys()
                        .chain(self.copies.keys())
                        .any(|id| split_id(id).is_some_and(|(_, issued)| issued == s))
                })?;
                format!("{role}-{suffix}")
            }
        };
        self.copies.insert(
            id.clone(),
            IssuedMember {
                role: role.to_string(),
                deleted: false,
                cwd: None,
            },
        );
        Ok(id)
    }

    /// Issue a new member of `role`: `<role>-<name>` when named, else
    /// `<role>-<suffix>`, never one this team issued before.
    pub fn issue_member(
        &mut self,
        role: &str,
        name: Option<&str>,
        is_role: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let id = match name {
            Some(name) => self.named(role, name, is_role)?,
            None => {
                let suffix = new_suffix(|s| {
                    is_role(&format!("{role}-{s}"))
                        || self
                            .members
                            .keys()
                            .chain(self.copies.keys())
                            .any(|id| split_id(id).is_some_and(|(_, t)| t == s))
                })?;
                format!("{role}-{suffix}")
            }
        };
        self.members.insert(
            id.clone(),
            IssuedMember {
                role: role.to_string(),
                deleted: false,
                cwd: None,
            },
        );
        Ok(id)
    }

    /// `<role>-<name>`, if it is a valid ID that no role, member or child team of
    /// this team has or had, and `name` isn't another issued ID's suffix.
    pub fn named(
        &self,
        role: &str,
        name: &str,
        is_role: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        check_name(name)?;
        let id = format!("{role}-{name}");
        if !crate::names::is_valid_member_id(&id) {
            return Err(format!(
                "`{id}` is longer than 40 characters; choose a shorter name"
            ));
        }
        if is_role(&id) {
            return Err(format!(
                "`{id}` is the name of a role in this team; choose another name"
            ));
        }
        let mut issued = self.members.keys().chain(self.copies.keys());
        if issued.clone().any(|taken| *taken == id) {
            return Err(format!(
                "`{id}` was issued before in this team, and IDs are never reused, even after delete; choose another name"
            ));
        }
        if let Some(taken) =
            issued.find(|taken| split_id(taken).is_some_and(|(_, suffix)| suffix == name))
        {
            return Err(format!(
                "`{name}` is the suffix of `{taken}` in this team; choose a name that isn't another ID's suffix"
            ));
        }
        Ok(id)
    }
}

fn registry_file() -> PathBuf {
    state::state_home().join("registry.json")
}

fn lock_path() -> PathBuf {
    state::state_home().join("registry.lock")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} can't be read: {e}", path.display()))?;
            if let Some(schema) = value.get("schema").and_then(serde_json::Value::as_u64)
                && schema != u64::from(SCHEMA)
            {
                let dir = path.parent().unwrap_or(path);
                return Err(format!(
                    "this state uses schema {schema}; use a brg that reads it, or remove {}",
                    dir.display()
                ));
            }
            serde_json::from_value(value)
                .map(Some)
                .map_err(|e| format!("{} can't be read: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    state::write_private(path, &body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The registry as it is now, read without the lock. Creates nothing.
pub fn load() -> Result<Registry, String> {
    Ok(read_json(&registry_file())?.unwrap_or(Registry {
        schema: SCHEMA,
        ..Registry::default()
    }))
}

/// The registry lock, held while it lives. Everything that changes the
/// registry or a team's record goes through it.
pub struct Lock {
    _file: File,
}

/// Take the registry lock, waiting for it. Never take it twice in one
/// process: the second waits for the first for ever.
pub fn lock() -> Result<Lock, String> {
    let home = state::state_home();
    state::ensure_dir(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let path = lock_path();
    let file =
        state::open_lock(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    file.lock()
        .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
    Ok(Lock { _file: file })
}

impl Lock {
    pub fn load(&self) -> Result<Registry, String> {
        load()
    }

    pub fn save(&self, registry: &Registry) -> Result<(), String> {
        write_json(&registry_file(), registry)
    }

    pub fn save_instance(&self, state: &StateDir, record: &InstanceRecord) -> Result<(), String> {
        write_json(&state.instance_file(), record)
    }
}

/// A team's record, read without the lock.
pub fn read_instance(state: &StateDir) -> Result<InstanceRecord, String> {
    read_json(&state.instance_file())?.ok_or_else(|| format!("team `{}` has no record", state.id))
}

/// Register a new team from template `team` (its name and folder), working in
/// `folder`: a fresh suffix, the registry entry, the state folder and its
/// record. Called with the lock held.
pub fn create(
    lock: &Lock,
    team: &str,
    team_dir: &Path,
    folder: &Path,
    created_at: &str,
) -> Result<(StateDir, InstanceRecord), String> {
    let mut registry = lock.load()?;
    let issued = registry.issued.entry(team.to_string()).or_default();
    let taken: Vec<String> = registry.instances.keys().cloned().collect();
    let suffix =
        new_suffix(|s| issued.contains(s) || taken.iter().any(|id| id == &format!("{team}-{s}")))?;
    // An ID nobody else used, even outside the registry (a stray folder).
    let id = format!("{team}-{suffix}");
    issued.insert(suffix);
    let record = InstanceRecord {
        schema: SCHEMA,
        id,
        team: team.to_string(),
        team_dir: team_dir.to_path_buf(),
        folder: folder.to_path_buf(),
        created_at: created_at.to_string(),
        members: BTreeMap::new(),
        copies: BTreeMap::new(),
        parent: None,
        running_at_stop: None,
        stopping: false,
        copies_running_at_stop: None,
    };
    register(lock, registry, record)
}

/// Register a stopped nested team. Its ID was issued by the parent.
pub fn create_copy(
    lock: &Lock,
    id: &str,
    team_dir: &Path,
    folder: &Path,
    parent: Option<String>,
) -> Result<(StateDir, InstanceRecord), String> {
    register(
        lock,
        lock.load()?,
        InstanceRecord {
            schema: SCHEMA,
            id: id.to_string(),
            team: crate::team::team_name(team_dir),
            team_dir: team_dir.to_path_buf(),
            folder: folder.to_path_buf(),
            created_at: crate::bus::time::utc_now(),
            members: BTreeMap::new(),
            copies: BTreeMap::new(),
            parent,
            running_at_stop: None,
            stopping: false,
            copies_running_at_stop: None,
        },
    )
}

fn register(
    lock: &Lock,
    mut registry: Registry,
    record: InstanceRecord,
) -> Result<(StateDir, InstanceRecord), String> {
    let id = &record.id;
    let state = StateDir::new(id);
    if registry.instances.contains_key(id) || state.root.exists() {
        lock.save(&registry)?;
        return Err(format!(
            "{} already exists; try again with a new child team ID",
            state.root.display()
        ));
    }
    for dir in [&state.root, &state.messages_dir(), &state.notes_dir()] {
        state::ensure_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    lock.save_instance(&state, &record)?;
    registry.instances.insert(
        id.clone(),
        Entry {
            team: record.team.clone(),
            team_dir: record.team_dir.clone(),
            folder: record.folder.clone(),
            created_at: record.created_at.clone(),
        },
    );
    lock.save(&registry)?;
    Ok((state, record))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_are_well_formed() {
        for _ in 0..50 {
            let s = random_suffix().unwrap();
            assert!(is_suffix(&s), "{s}");
        }
        assert!(!is_suffix("k3f0"));
        assert!(!is_suffix("k3f"));
        assert!(!is_suffix("K3F9"));
    }

    #[test]
    fn ids_split_from_the_right() {
        assert_eq!(split_id("code-review-k3f9"), Some(("code-review", "k3f9")));
        assert_eq!(split_id("dev-team-k3f9"), Some(("dev-team", "k3f9")));
        assert_eq!(split_id("lead-q8m3"), Some(("lead", "q8m3")));
        assert_eq!(split_id("lead"), None);
        assert_eq!(split_id("dev-tea1"), None);
        assert_eq!(split_id("-k3f9"), None);
    }

    #[test]
    fn member_suffix_skips_role_names() {
        let mut record = InstanceRecord {
            schema: SCHEMA,
            id: "sample-2222".into(),
            team: "sample".into(),
            team_dir: "/team".into(),
            folder: "/work".into(),
            created_at: "2026-11-08T00:00:00Z".into(),
            members: BTreeMap::new(),
            copies: BTreeMap::new(),
            parent: None,
            running_at_stop: None,
            stopping: false,
            copies_running_at_stop: None,
        };
        // The first candidate is a declared role. It must be skipped even
        // though no issued member has that suffix yet.
        let role = std::cell::OnceCell::new();
        let member = record
            .issue_member("code-review", None, |candidate| {
                role.get_or_init(|| candidate.to_string()) == candidate
            })
            .unwrap();
        assert_ne!(&member, role.get().unwrap());
        assert_eq!(split_id(&member).unwrap().0, "code-review");
    }

    #[test]
    fn collisions_are_retried() {
        let first = new_suffix(|_| false).unwrap();
        let next = new_suffix(|s| s == first).unwrap();
        assert_ne!(first, next);
    }

    #[test]
    fn members_are_issued_once() {
        let mut r = InstanceRecord {
            schema: SCHEMA,
            id: "t-2222".into(),
            team: "t".into(),
            team_dir: "/x".into(),
            folder: "/y".into(),
            created_at: "now".into(),
            members: BTreeMap::new(),
            copies: BTreeMap::new(),
            parent: None,
            running_at_stop: None,
            stopping: false,
            copies_running_at_stop: None,
        };
        let a = r.issue_member("my-role", None, |_| false).unwrap();
        let (role, suffix) = split_id(&a).unwrap();
        assert_eq!(role, "my-role");
        assert!(is_suffix(suffix));
        assert_eq!(r.members_of("my-role"), vec![a.clone()]);
        assert_eq!(r.role_of(&a), Some("my-role"));
    }

    #[test]
    fn names_are_lowercase_words_up_to_the_cap() {
        for name in [
            "rounding-fix",
            "k3f9",
            "v2-docs",
            "a",
            &"a".repeat(NAME_MAX),
        ] {
            assert!(check_name(name).is_ok(), "{name}");
        }
        let long = "a".repeat(NAME_MAX + 1);
        for name in [
            "",
            "Rounding",
            "rounding_fix",
            "-fix",
            "fix-",
            "a--b",
            "a.b",
            &long,
        ] {
            let err = check_name(name).unwrap_err();
            assert!(err.contains("at most 24"), "{name}: {err}");
        }
    }

    #[test]
    fn named_ids_are_issued_once_in_the_team() {
        let mut r = InstanceRecord {
            schema: SCHEMA,
            id: "t-2222".into(),
            team: "t".into(),
            team_dir: "/x".into(),
            folder: "/y".into(),
            created_at: "now".into(),
            members: BTreeMap::new(),
            copies: BTreeMap::new(),
            parent: None,
            running_at_stop: None,
            stopping: false,
            copies_running_at_stop: None,
        };
        let no_roles = |_: &str| false;
        let id = r
            .issue_member("helper", Some("rounding-fix"), no_roles)
            .unwrap();
        assert_eq!(id, "helper-rounding-fix");
        assert_eq!(r.role_of(&id), Some("helper"));
        r.members.get_mut(&id).unwrap().deleted = true;
        for err in [
            r.issue_member("helper", Some("rounding-fix"), no_roles),
            r.issue_copy("helper", Some("rounding-fix"), no_roles),
        ] {
            assert!(err.unwrap_err().contains("issued before"));
        }
        let copy = r.issue_copy("dev", Some("make-install"), no_roles).unwrap();
        assert_eq!(copy, "dev-make-install");
        let err = r
            .issue_member("dev", Some("make-install"), no_roles)
            .unwrap_err();
        assert!(err.contains("issued before"), "{err}");
        // A random suffix stays unique in the team, so a name can't take one.
        let lead = r.issue_member("lead", None, no_roles).unwrap();
        let suffix = split_id(&lead).unwrap().1.to_string();
        let err = r
            .issue_member("helper", Some(&suffix), no_roles)
            .unwrap_err();
        assert!(err.contains("suffix of") && err.contains(&lead), "{err}");
        let err = r
            .issue_member("code", Some("review"), |id| id == "code-review")
            .unwrap_err();
        assert!(err.contains("role"), "{err}");
        let err = r
            .issue_member(&"r".repeat(32), Some("rounding-fix"), no_roles)
            .unwrap_err();
        assert!(err.contains("40"), "{err}");
    }
}
