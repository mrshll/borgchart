//! Local policy and private, explicitly initialized intake state.
use crate::bus::{fsx, time};
use rustix::fs::{FlockOperation, OFlags, flock};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
pub type Result<T> = std::result::Result<T, String>;
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn utc() -> String {
    time::utc_now()
}
pub fn stamp(s: &str) -> Result<u64> {
    let normalized = s
        .strip_suffix("+00:00")
        .map(|v| format!("{v}Z"))
        .unwrap_or_else(|| s.into());
    let t = time::parse_utc(&normalized).ok_or("timestamp")?;
    let base = t
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "timestamp")?
        .as_micros() as u64;
    let tail = normalized
        .get(20..normalized.len().saturating_sub(1))
        .unwrap_or("");
    let extra = if normalized.as_bytes().get(19) == Some(&b'.') {
        format!("{tail:0<6}")
            .get(3..6)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    } else {
        0
    };
    Ok(base + extra)
}
pub fn full_name(s: &str) -> bool {
    regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9._-]{1,100}$")
        .unwrap()
        .is_match(s)
        && !matches!(s.rsplit('/').next(), Some("." | ".."))
}
fn login(s: &str) -> bool {
    regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9-]{0,38}$")
        .unwrap()
        .is_match(s)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: u64,
    pub login: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub id: u64,
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerPolicy {
    pub owner: Owner,
    pub repository_policy: String,
    #[serde(default)]
    pub exclude_repository_ids: Vec<u64>,
    #[serde(default)]
    pub exclude_repositories: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub app_id: u64,
    pub bot_user_id: u64,
    pub repository_ids: Vec<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialRepo {
    pub full_name: String,
    pub baseline: String,
}
fn poll() -> u64 {
    300
}
fn seconds() -> u64 {
    120
}
fn pages() -> usize {
    10
}
fn records() -> usize {
    100000
}
fn bytes() -> usize {
    32 * 1024 * 1024
}
fn mode() -> String {
    "wake".into()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub gh: Vec<String>,
    pub agent: Identity,
    pub trusted_actor_ids: Vec<u64>,
    pub owner_policy: OwnerPolicy,
    #[serde(default)]
    pub app_initiators: Vec<App>,
    #[serde(default)]
    pub trusted_reviewer_ids: Vec<u64>,
    #[serde(default)]
    pub initial_repositories: BTreeMap<String, InitialRepo>,
    #[serde(default)]
    pub issue_mentions_after: Option<String>,
    #[serde(default)]
    pub merge_wakes_after: Option<String>,
    #[serde(default = "poll")]
    pub poll_seconds: u64,
    #[serde(default = "seconds")]
    pub max_seconds: u64,
    #[serde(default = "pages")]
    pub max_pages: usize,
    #[serde(default = "records")]
    pub max_records: usize,
    #[serde(default = "bytes")]
    pub max_state_bytes: usize,
    #[serde(default)]
    pub reactions: bool,
    #[serde(default = "mode")]
    pub mode: String,
}
#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    pub path: PathBuf,
    pub dir: PathBuf,
    pub home: PathBuf,
}
impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let raw = fs::read(path).map_err(|_| "config")?;
        if raw.len() > 1024 * 1024 {
            return Err("config_capacity".into());
        }
        Self::parse(
            serde_json::from_slice(&raw).map_err(|_| "config")?,
            path.to_owned(),
            crate::state::state_home(),
        )
    }
    pub fn parse(value: Value, path: PathBuf, home: PathBuf) -> Result<Self> {
        let mut s: Settings = serde_json::from_value(value).map_err(|_| "config")?;
        if !home.is_absolute()
            || (!cfg!(feature = "test-support") && crate::state::age_cleaned(&home))
        {
            return Err("state_directory".into());
        }
        if s.gh.is_empty()
            || !Path::new(&s.gh[0]).is_absolute()
            || s.gh.iter().any(|v| v.is_empty() || v.contains('\0'))
            || s.agent.id == 0
            || !login(&s.agent.login)
            || s.owner_policy.owner.id == 0
            || !login(&s.owner_policy.owner.login)
            || !matches!(s.owner_policy.owner.kind.as_str(), "User" | "Organization")
            || !matches!(
                s.owner_policy.repository_policy.as_str(),
                "snapshot" | "include_new"
            )
            || !matches!(s.mode.as_str(), "off" | "wake")
        {
            return Err("config".into());
        }
        if !(60..=86400).contains(&s.poll_seconds)
            || !(20..=300).contains(&s.max_seconds)
            || !(1..=10).contains(&s.max_pages)
            || !(100..=1000000).contains(&s.max_records)
            || !(8192..=128 * 1024 * 1024).contains(&s.max_state_bytes)
        {
            return Err("config_limits".into());
        }
        let mut identities = BTreeSet::from([s.agent.id]);
        if s.trusted_actor_ids.is_empty() {
            return Err("config_trust".into());
        }
        for id in s.trusted_actor_ids.iter().chain(&s.trusted_reviewer_ids) {
            if *id == 0 || !identities.insert(*id) {
                return Err("config_class_overlap".into());
            }
        }
        let mut apps = BTreeSet::new();
        for app in &mut s.app_initiators {
            if app.app_id == 0
                || !apps.insert(app.app_id)
                || app.bot_user_id == 0
                || !identities.insert(app.bot_user_id)
                || app.repository_ids.is_empty()
                || app.repository_ids.contains(&0)
                || app.repository_ids.iter().collect::<BTreeSet<_>>().len()
                    != app.repository_ids.len()
            {
                return Err("config_app_initiators".into());
            }
            app.repository_ids.sort();
        }
        s.trusted_actor_ids.sort();
        s.trusted_reviewer_ids.sort();
        s.app_initiators.sort_by_key(|a| a.app_id);
        if s.owner_policy.exclude_repository_ids.contains(&0)
            || s.owner_policy
                .exclude_repositories
                .iter()
                .any(|n| !full_name(n))
        {
            return Err("config_exclusions".into());
        }
        for cut in [&s.issue_mentions_after, &s.merge_wakes_after]
            .into_iter()
            .flatten()
        {
            stamp(cut)?;
        }
        for (id, r) in &s.initial_repositories {
            if id
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| n.to_string())
                .as_deref()
                != Some(id)
                || !full_name(&r.full_name)
            {
                return Err("config_repositories".into());
            }
            stamp(&r.baseline)?;
        }
        let dir = home
            .join("github")
            .join(format!("{}-{}", s.agent.id, s.owner_policy.owner.id));
        Ok(Self {
            settings: s,
            path,
            dir,
            home,
        })
    }
    pub fn value(&self) -> Value {
        serde_json::to_value(&self.settings).expect("settings serialize")
    }
    pub fn file(&self) -> PathBuf {
        self.dir.join("state.json")
    }
    pub fn scope(&self) -> String {
        let s = &self.settings;
        let v = json!([
            s.agent,
            s.owner_policy.owner,
            s.trusted_actor_ids,
            s.app_initiators,
            s.trusted_reviewer_ids,
            s.owner_policy.repository_policy,
            s.initial_repositories,
            s.issue_mentions_after,
            s.merge_wakes_after
        ]);
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&v).expect("scope serialize"))
        )
    }
    pub fn trusted_keys(&self) -> BTreeSet<String> {
        self.settings
            .trusted_actor_ids
            .iter()
            .map(ToString::to_string)
            .chain(
                self.settings
                    .app_initiators
                    .iter()
                    .map(|a| format!("app:{}", a.app_id)),
            )
            .chain(
                self.settings
                    .trusted_reviewer_ids
                    .iter()
                    .map(|id| format!("reviewer:{id}")),
            )
            .collect()
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub validator: Option<String>,
    pub last_complete: Option<String>,
    pub due_ms: u64,
    pub holds: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub tries: u8,
    pub next_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reaction {
    pub outcome: String,
    pub reason: Option<String>,
    pub at: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reactions {
    pub targets: BTreeMap<String, Reaction>,
    pub end_ms: u64,
    pub write_retry_ms: u64,
    pub read_retry_ms: u64,
    pub streak: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema: u32,
    pub scope: String,
    pub created_at: String,
    pub feature_start: String,
    pub feed: Feed,
    pub row: Value,
    pub backoff_ms: u64,
    pub last_tick: Option<String>,
    pub fault: Option<String>,
    pub attempts: BTreeMap<String, Attempt>,
    pub reactions: Reactions,
}
const MAPS: [&str; 8] = [
    "repos", "pins", "seen", "objects", "pending", "dirty", "hints", "hold",
];
fn validate_row(row: &Value) -> Result<()> {
    let Some(obj) = row.as_object() else {
        return Err("state_corrupt".into());
    };
    let expected: BTreeSet<_> = MAPS
        .into_iter()
        .chain([
            "new_repo_cut",
            "mention_since",
            "trusted_since",
            "scope_skipped",
        ])
        .collect();
    if obj.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected
        || MAPS.iter().any(|k| !row[k].is_object())
        || !row["scope_skipped"].is_u64()
    {
        return Err("state_corrupt".into());
    }
    for k in ["new_repo_cut", "mention_since"] {
        stamp(row[k].as_str().ok_or("state_corrupt")?)?;
    }
    for v in row["trusted_since"]
        .as_object()
        .ok_or("state_corrupt")?
        .values()
    {
        stamp(v.as_str().ok_or("state_corrupt")?)?;
    }
    for (key, value) in row["repos"].as_object().unwrap() {
        if key.parse::<u64>().ok().is_none_or(|n| n == 0)
            || !value.is_object()
            || value["full_name"].as_str().is_none_or(|n| !full_name(n))
            || !matches!(
                value["status"].as_str(),
                Some("admitted" | "excluded" | "held")
            )
            || !value["listed"].is_boolean()
        {
            return Err("state_corrupt".into());
        }
        stamp(value["baseline"].as_str().ok_or("state_corrupt")?)?;
    }
    for (key, value) in row["pins"].as_object().unwrap() {
        if !full_name(key) || value.as_u64().is_none_or(|n| n == 0) {
            return Err("state_corrupt".into());
        }
    }
    for value in row["seen"].as_object().unwrap().values() {
        stamp(value.as_str().ok_or("state_corrupt")?)?;
    }
    for value in row["objects"].as_object().unwrap().values() {
        if !matches!(
            value["kind"].as_str(),
            Some("assignment" | "feedback" | "merge")
        ) {
            return Err("state_corrupt".into());
        }
        stamp(value["seen_at"].as_str().ok_or("state_corrupt")?)?;
    }
    if row["hints"]
        .as_object()
        .unwrap()
        .values()
        .any(|v| !v.is_string())
    {
        return Err("state_corrupt".into());
    }
    for value in row["hold"].as_object().unwrap().values() {
        if !value["reason"].is_string() {
            return Err("state_corrupt".into());
        }
        stamp(value["since"].as_str().ok_or("state_corrupt")?)?;
    }
    let initiator = regex::Regex::new(r"^(human|app:[1-9][0-9]*|reviewer:[1-9][0-9]*)$").unwrap();
    for p in row["pending"].as_object().ok_or("state_corrupt")?.values() {
        if !p.is_object()
            || p["repo_id"].as_u64().is_none_or(|n| n == 0)
            || p["number"].as_u64().is_none_or(|n| n == 0)
            || p["actor_id"].as_u64().is_none_or(|n| n == 0)
            || p["full_name"].as_str().is_none_or(|s| !full_name(s))
            || !p["proof"].is_object()
        {
            return Err("state_corrupt".into());
        }
        for key in [
            "key",
            "kind",
            "object_id",
            "version",
            "initiator",
            "occurred_at",
        ] {
            if p[key].as_str().is_none() {
                return Err("state_corrupt".into());
            }
        }
        if !matches!(
            p["kind"].as_str(),
            Some("assignment" | "feedback" | "merge")
        ) || p.get("signal").is_some_and(|v| {
            !v.is_null()
                && !matches!(
                    v.as_str(),
                    Some("assignment" | "handoff" | "mention" | "merge")
                )
        }) || !initiator.is_match(p["initiator"].as_str().unwrap_or(""))
        {
            return Err("state_corrupt".into());
        }
        stamp(p["occurred_at"].as_str().unwrap())?;
    }
    for p in row["dirty"].as_object().unwrap().values() {
        if !p["held"].is_boolean()
            || !p["repo_id"].is_u64()
            || !p["number"].is_u64()
            || !p["full_name"].is_string()
            || !p["version"].is_string()
            || !p["at"].is_string()
        {
            return Err("state_corrupt".into());
        }
    }
    Ok(())
}
fn private(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path).map_err(|_| "state_missing")?;
    if m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.file_type().is_symlink()
        || if directory { !m.is_dir() } else { !m.is_file() }
    {
        return Err("state_private".into());
    }
    Ok(())
}
pub fn load(cfg: &Config) -> Result<State> {
    private(&cfg.dir, true)?;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(cfg.file())
        .map_err(|_| "state_missing")?;
    private(&cfg.file(), false)?;
    let mut raw = Vec::new();
    std::io::Read::by_ref(&mut f)
        .take(cfg.settings.max_state_bytes as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "state_unreadable")?;
    if raw.len() > cfg.settings.max_state_bytes {
        return Err("state_capacity".into());
    }
    let s: State = serde_json::from_slice(&raw).map_err(|_| "state_corrupt")?;
    if s.schema != 1 {
        return Err("state_corrupt".into());
    }
    if s.scope != cfg.scope() {
        return Err("scope_changed".into());
    }
    validate_row(&s.row)?;
    if s.row["trusted_since"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != cfg.trusted_keys()
    {
        return Err("state_corrupt".into());
    }
    stamp(&s.created_at)?;
    stamp(&s.feature_start)?;
    if s.attempts.values().any(|a| a.tries > 3) || s.reactions.targets.len() > 10000 {
        return Err("state_corrupt".into());
    }
    Ok(s)
}
pub fn save(cfg: &Config, s: &State) -> Result<()> {
    if s.scope != cfg.scope() {
        return Err("scope_changed".into());
    }
    let count: usize = MAPS
        .iter()
        .map(|k| s.row[k].as_object().map_or(0, |m| m.len()))
        .sum::<usize>()
        + s.attempts.len()
        + s.reactions.targets.len();
    let raw = serde_json::to_vec(s).map_err(|_| "state_encode")?;
    let held = s.feed.holds.contains("state_capacity");
    // Reserve space for a durable hold and one delivery attempt. A held state
    // may use that headroom to retire already-pending events.
    if count > cfg.settings.max_records
        || raw.len() > cfg.settings.max_state_bytes
        || (!held
            && (raw.len() > cfg.settings.max_state_bytes.saturating_sub(4096)
                || count >= cfg.settings.max_records))
    {
        return Err("state_capacity".into());
    }
    private(&cfg.dir, true)?;
    fsx::replace(&cfg.file(), &raw).map_err(|_| "state_write".into())
}
pub fn lock(cfg: &Config) -> Result<File> {
    private(&cfg.dir, true)?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(cfg.dir.join("lock"))
        .map_err(|_| "lock")?;
    let m = f.metadata().map_err(|_| "lock")?;
    if !m.is_file() || m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o077 != 0 {
        return Err("lock".into());
    }
    flock(f.as_fd(), FlockOperation::NonBlockingLockExclusive).map_err(|_| "busy")?;
    Ok(f)
}
pub fn initialize(cfg: &Config, since: &str) -> Result<()> {
    let cutoff = stamp(since)?;
    if cutoff > now_ms() * 1000 {
        return Err("future_cutoff".into());
    }
    fsx::ensure_private_tree(&cfg.home, &cfg.dir).map_err(|_| "state_directory")?;
    let _lock = lock(cfg)?;
    if cfg.file().symlink_metadata().is_ok() {
        return Err("state_exists".into());
    }
    let mut row =
        json!({"new_repo_cut":since,"mention_since":since,"trusted_since":{},"scope_skipped":0});
    for key in MAPS {
        row[key] = json!({});
    }
    for key in cfg.trusted_keys() {
        row["trusted_since"][key] = json!(since);
    }
    for (id, r) in &cfg.settings.initial_repositories {
        let baseline = if stamp(&r.baseline)? > cutoff {
            r.baseline.as_str()
        } else {
            since
        };
        row["repos"][id] =
            json!({"full_name":r.full_name,"baseline":baseline,"status":"admitted","listed":true});
    }
    save(
        cfg,
        &State {
            schema: 1,
            scope: cfg.scope(),
            created_at: utc(),
            feature_start: since.into(),
            feed: Feed::default(),
            row,
            backoff_ms: 0,
            last_tick: None,
            fault: None,
            attempts: BTreeMap::new(),
            reactions: Reactions::default(),
        },
    )
}
pub fn hold(row: &mut Value, key: &str, reason: &str) {
    if row["hold"].get(key).is_none() {
        row["hold"][key] = json!({"reason":reason,"since":utc()});
    }
}
pub fn retry(cfg: &Config, item: Option<&str>) -> Result<()> {
    let _lock = lock(cfg)?;
    let mut s = load(cfg)?;
    if let Some(item) = item {
        if !regex::Regex::new(r"^[1-9][0-9]*:[1-9][0-9]*$")
            .unwrap()
            .is_match(item)
        {
            return Err("item_key".into());
        }
        if s.row["dirty"][item]["held"] != true {
            return Err("item_not_held".into());
        }
        s.row["dirty"][item]["held"] = json!(false);
        s.row["hold"]
            .as_object_mut()
            .unwrap()
            .remove(&format!("item:{item}"));
    } else {
        s.feed.holds.clear();
        s.feed.due_ms = 0;
    }
    s.fault = None;
    save(cfg, &s)
}
