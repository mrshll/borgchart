//! The team format: `team.yaml` plus one Markdown file per role.
//!
//! See `docs/format.md` for the format itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Report;
use crate::frontmatter;
use crate::names::{is_plain_word, is_valid_env_name, is_valid_id};
use crate::paths::{is_executable, resolve_within};
use crate::yaml;

pub const TEAM_FILE: &str = "team.yaml";
pub const LEGACY_TEAM_FILE: &str = "topology.yaml";
pub const FORMAT_DOC: &str = "docs/format.md";

/// Reasoning-effort words accepted after a model name.
pub const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Claude,
    Codex,
}

impl Vendor {
    pub fn as_str(self) -> &'static str {
        match self {
            Vendor::Claude => "claude",
            Vendor::Codex => "codex",
        }
    }

    /// The vendor's name in messages.
    pub fn display_name(self) -> &'static str {
        match self {
            Vendor::Claude => "Claude",
            Vendor::Codex => "Codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Posture {
    Vendor,
    Locked,
}

/// Parse a posture name. `sandboxed` is reserved for hosting.
pub fn parse_posture(value: &str) -> Result<Posture, String> {
    match value {
        "vendor" => Ok(Posture::Vendor),
        "locked" => Ok(Posture::Locked),
        "sandboxed" => {
            Err("posture `sandboxed` is reserved for hosting and not available in v0".into())
        }
        other => Err(format!(
            "unknown posture `{other}` (expected `vendor` or `locked`)"
        )),
    }
}

// ---- raw file shapes -------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamFileRaw {
    default: Option<Vendor>,
    includes: Option<String>,
    bus: Option<BusRaw>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginRaw>,
    roles: BTreeMap<String, RoleRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BusRaw {
    alert: Option<AlertRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertRaw {
    plugin: String,
    action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginRaw {
    path: Option<String>,
    command: Option<String>,
    process: Option<Vec<String>>,
    delivers_to: Option<String>,
    #[serde(default)]
    env: EnvRaw,
    #[serde(default)]
    actions: BTreeMap<String, String>,
}

/// A plugin's `env`: a list of names (each a secret, or a one-entry map
/// `NAME: value` for a plain value), or a map of names to plain values
/// (empty for a secret).
#[derive(Debug, Deserialize)]
#[serde(
    untagged,
    expecting = "`env` must be a list of names (or `NAME: value` entries), or a map of names to values"
)]
enum EnvRaw {
    List(Vec<EnvItemRaw>),
    Map(BTreeMap<String, Option<String>>),
}

impl Default for EnvRaw {
    fn default() -> Self {
        EnvRaw::List(Vec::new())
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EnvItemRaw {
    Name(String),
    Value(BTreeMap<String, String>),
}

impl EnvRaw {
    /// Every name in order, each with its plain value if it has one.
    fn entries(&self) -> Vec<(String, Option<String>)> {
        match self {
            EnvRaw::Map(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            EnvRaw::List(items) => items
                .iter()
                .flat_map(|item| match item {
                    EnvItemRaw::Name(n) => vec![(n.clone(), None)],
                    EnvItemRaw::Value(map) => map
                        .iter()
                        .map(|(k, v)| (k.clone(), Some(v.clone())))
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleRaw {
    prompt: Option<String>,
    team: Option<String>,
    vendor: Option<Vendor>,
    parent: Option<String>,
    start: Option<bool>,
    max: Option<usize>,
    posture: Option<String>,
    #[serde(default)]
    untrusted_input: bool,
    #[serde(default)]
    accept_risk: bool,
    #[serde(default)]
    plugins: BTreeMap<String, Vec<String>>,
    connectors: Option<ConnectorsRaw>,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(
    untagged,
    expecting = "`connectors` must be a list of server names, or a map of server name to a list of tool names"
)]
enum ConnectorsRaw {
    Names(Vec<String>),
    Tools(BTreeMap<String, Vec<String>>),
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontmatterRaw {
    #[serde(default)]
    models: ModelsRaw,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelsRaw {
    claude: Option<String>,
    codex: Option<String>,
}

// ---- resolved team ---------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Team {
    /// The team folder, with symlinks resolved.
    pub dir: PathBuf,
    /// The team's name: its folder's name (see [`team_name`]).
    pub name: String,
    /// SHA-256 of `team.yaml` as it was read and parsed.
    pub file_sha256: String,
    pub default_vendor: Vendor,
    /// The coordination bus, when the team uses one.
    pub bus: Option<BusConfig>,
    pub roles: BTreeMap<String, Role>,
    pub team_roles: BTreeMap<String, TeamRole>,
    pub plugins: BTreeMap<String, Plugin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BusConfig {
    /// The plugin action brg invokes to alert a person (never a member's).
    pub alert: Option<AlertConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlertConfig {
    pub plugin: String,
    pub action: String,
}

/// Whether a command-line argument is a path: it starts with `/`, `./`,
/// `../` or `~/`. Anything else is a name.
pub fn is_path(arg: &str) -> bool {
    arg.starts_with('/') || arg.starts_with("./") || arg.starts_with("../") || arg.starts_with("~/")
}

/// The teams in the search paths: each subfolder holding a `team.yaml`,
/// with its name, in search-path order.
pub fn list(search: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for root in search {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(TEAM_FILE).is_file())
            .collect();
        found.sort();
        for dir in found {
            let dir = dir.canonicalize().unwrap_or(dir);
            if !out.iter().any(|(_, d)| d == &dir) {
                out.push((team_name(&dir), dir));
            }
        }
    }
    out
}

/// Find a team: a path (see [`is_path`]) to its folder, or a name looked up
/// in every search path. Two folders with the name are refused, listing
/// both; brg never picks the first.
pub fn find(arg: &str, search: &[PathBuf]) -> Result<PathBuf, String> {
    if is_path(arg) {
        let path = match arg.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => PathBuf::from(arg),
        };
        if !path.join(TEAM_FILE).is_file() {
            return Err(format!(
                "{} is not a team folder (no {TEAM_FILE} in it)",
                path.display()
            ));
        }
        return Ok(path.canonicalize().unwrap_or(path));
    }
    if arg.contains('/') {
        return Err(format!(
            "`{arg}` is not a team's name; for a folder, write `./{arg}`"
        ));
    }
    let matches: Vec<PathBuf> = list(search)
        .into_iter()
        .filter(|(name, _)| name == arg)
        .map(|(_, dir)| dir)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no team `{arg}` in {}; for a folder, give its path (`./{arg}`)",
            if search.is_empty() {
                "the search paths (none are set)".to_string()
            } else {
                search
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )),
        many => Err(format!(
            "more than one team is called `{arg}`; name one by its path: {}",
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The name a team goes by: its folder's name, lowercased, with anything
/// outside `[a-z0-9_-]` turned into `-` (at most 32 characters).
pub fn team_name(dir: &Path) -> String {
    let clean: String = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(32)
        .collect();
    if clean.is_empty() {
        "team".to_string()
    } else {
        clean
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Role {
    pub id: String,
    pub vendor: Vendor,
    pub model: Option<Model>,
    pub parent: Option<String>,
    pub start: bool,
    /// Kept members, running or stopped, allowed for this role.
    pub max: usize,
    pub posture: Posture,
    pub untrusted_input: bool,
    pub accept_risk: bool,
    pub grants: BTreeMap<String, Grant>,
    pub connectors: Connectors,
    pub prompt_file: PathBuf,
    /// SHA-256 of the member file as it was read and parsed.
    pub prompt_sha256: String,
    /// The member body with includes resolved: its standing instruction.
    pub instruction: String,
    /// Extra arguments for the vendor CLI.
    pub args: Vec<String>,
    /// Where its members start, relative to the instance's folder; may hold
    /// `{member_id}`.
    pub cwd: Option<String>,
}

/// A role whose copies are nested instances, prepared before their first start.
#[derive(Debug, Clone, Serialize)]
pub struct TeamRole {
    pub dir: PathBuf,
    pub parent: Option<String>,
    pub max: usize,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Model {
    pub name: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Grant {
    /// Every action of the plugin (`["*"]`), or its whole command when it has none.
    All,
    Actions(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Connectors {
    /// Server names as the vendor's `mcp list` prints them, each with the
    /// tools allowed in `locked` posture (empty when only names were given).
    pub servers: BTreeMap<String, Vec<String>>,
    /// Whether tool names were given (the map form).
    pub with_tools: bool,
}

impl Connectors {
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Plugin {
    pub name: String,
    /// `None` for a plugin whose commands run from `PATH`.
    pub location: Option<PluginLocation>,
    pub command: Option<String>,
    pub process: Option<Vec<String>>,
    pub delivers_to: Option<String>,
    /// Every name the plugin gets, in order.
    pub env: Vec<String>,
    /// The plain values the team gives; every other name is a secret.
    pub values: BTreeMap<String, String>,
    pub actions: BTreeMap<String, Action>,
}

impl Plugin {
    /// The names whose values come from the caller's environment.
    pub fn secrets(&self) -> impl Iterator<Item = &String> {
        self.env.iter().filter(|n| !self.values.contains_key(*n))
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum PluginLocation {
    /// A directory inside the team, relative to the team dir.
    Team(String),
    /// `shared:<name>`, resolved under the configuration's `plugins_dir`.
    Shared(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    pub line: String,
    /// The literal words before the first placeholder; the only part enforced.
    pub prefix: Vec<String>,
}

impl Action {
    pub fn command(&self) -> &str {
        &self.prefix[0]
    }
}

/// Parse an action line such as `slackbot escalate <ts> <severity> <reason>`.
pub fn parse_action(line: &str) -> Result<Action, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut prefix = Vec::new();
    let mut seen_placeholder = false;
    for word in &words {
        let is_placeholder = word.len() > 2 && word.starts_with('<') && word.ends_with('>');
        if is_placeholder {
            let inner = &word[1..word.len() - 1];
            if !inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!("placeholder `{word}` must be `<name>`"));
            }
            seen_placeholder = true;
        } else if seen_placeholder {
            // Literal words after a placeholder are documentation only.
            continue;
        } else if is_plain_word(word) {
            prefix.push((*word).to_string());
        } else {
            return Err(format!(
                "`{word}` is not a plain word; action lines may only use letters, digits and `._:/@%+=,-` before the first placeholder"
            ));
        }
    }
    if prefix.is_empty() {
        return Err("action line must start with a command".into());
    }
    if prefix[0].contains('/') {
        return Err(format!(
            "the command `{}` must be a bare name in the plugin's directory, not a path",
            prefix[0]
        ));
    }
    Ok(Action {
        line: words.join(" "),
        prefix,
    })
}

/// Parse `"opus high"` into a model name and optional effort.
pub fn parse_model(value: &str) -> Result<Model, String> {
    let words: Vec<&str> = value.split_whitespace().collect();
    match words.as_slice() {
        [name] => Ok(Model {
            name: (*name).to_string(),
            effort: None,
        }),
        [name, effort] if EFFORTS.contains(effort) => Ok(Model {
            name: (*name).to_string(),
            effort: Some((*effort).to_string()),
        }),
        [_, effort] => Err(format!(
            "unknown effort `{effort}` (expected one of {})",
            EFFORTS.join(", ")
        )),
        _ => Err(format!(
            "model `{value}` must be `<model>` or `<model> <effort>`"
        )),
    }
}

/// Load and check a team directory. Problems go into `report`; a team is
/// returned only when there were no errors.
pub fn load(dir: &Path, report: &mut Report) -> Option<Team> {
    let real = dir.canonicalize();
    let dir = real.as_deref().unwrap_or(dir);
    let team_file = dir.join(TEAM_FILE);
    if !team_file.exists() {
        if dir.join(LEGACY_TEAM_FILE).exists() {
            report.error(
                Some(&dir.join(LEGACY_TEAM_FILE)),
                format!(
                    "this team uses the older `{LEGACY_TEAM_FILE}` shape; rename it to `{TEAM_FILE}` and update member files as described in {FORMAT_DOC}"
                ),
            );
        } else {
            report.error(Some(&team_file), "team file not found");
        }
        return None;
    }
    let text = match read_text(&team_file) {
        Ok(text) => text,
        Err(e) => {
            report.error(Some(&team_file), e);
            return None;
        }
    };
    let raw: TeamFileRaw = match yaml::parse(&text) {
        Ok(raw) => raw,
        Err(e) => {
            report.error(Some(&team_file), e);
            return None;
        }
    };
    let errors_before = report.errors().count();
    let default_vendor = raw.default.unwrap_or(Vendor::Claude);

    let includes_dir = raw
        .includes
        .clone()
        .unwrap_or_else(|| "includes".to_string());

    // Plugins first, so member grants can be checked against them.
    let mut plugins = BTreeMap::new();
    for (name, p) in &raw.plugins {
        if let Some(plugin) = load_plugin(dir, &team_file, name, p, &raw.roles, report) {
            plugins.insert(name.clone(), plugin);
        }
    }

    let mut roles = BTreeMap::new();
    let mut team_roles = BTreeMap::new();
    for (id, m) in &raw.roles {
        if let Some(path) = &m.team {
            if let Some(role) = load_team_role(dir, &team_file, id, m, path, report) {
                team_roles.insert(id.clone(), role);
            }
            continue;
        }
        if let Some(member) = load_role(
            dir,
            &team_file,
            &includes_dir,
            default_vendor,
            id,
            m,
            &raw,
            report,
        ) {
            roles.insert(id.clone(), member);
        }
    }

    check_parents(&team_file, &raw.roles, report);
    for name in raw.roles.keys() {
        if crate::names::conflicts_with_member_id(name, |base| raw.roles.contains_key(base)) {
            report.error(
                Some(&team_file),
                format!(
                    "role `{name}` also names a member of another role; give it a different name"
                ),
            );
        }
    }

    if report.errors().count() > errors_before {
        return None;
    }
    Some(Team {
        dir: dir.to_path_buf(),
        name: team_name(dir),
        file_sha256: crate::config_snapshot::sha256_hex(text.as_bytes()),
        default_vendor,
        bus: raw.bus.map(|b| BusConfig {
            alert: b.alert.map(|a| AlertConfig {
                plugin: a.plugin,
                action: a.action,
            }),
        }),
        roles,
        team_roles,
        plugins,
    })
}

fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read: {e}"))?;
    if meta.len() as usize > yaml::MAX_FILE_BYTES {
        return Err(format!(
            "file is larger than {} bytes",
            yaml::MAX_FILE_BYTES
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("cannot read: {e}"))
}

fn load_plugin(
    dir: &Path,
    team_file: &Path,
    name: &str,
    p: &PluginRaw,
    roles: &BTreeMap<String, RoleRaw>,
    report: &mut Report,
) -> Option<Plugin> {
    let before = report.errors().count();
    let at = |msg: String| format!("plugin `{name}`: {msg}");
    if !is_valid_id(name) {
        report.error(
            Some(team_file),
            at("name must match `[a-z][a-z0-9_-]{0,31}`".into()),
        );
    }

    let location = match p.path.as_deref() {
        None => None,
        Some(path) => match path.strip_prefix("shared:") {
            Some(shared) if is_valid_id(shared) => Some(PluginLocation::Shared(shared.to_string())),
            Some(shared) => {
                report.error(
                    Some(team_file),
                    at(format!(
                        "`shared:{shared}` must name a plugin like `shared:slackbot`"
                    )),
                );
                None
            }
            None => match resolve_within(dir, path) {
                Ok(real) if real.is_dir() => Some(PluginLocation::Team(path.to_string())),
                Ok(_) => {
                    report.error(Some(team_file), at(format!("`{path}` is not a directory")));
                    None
                }
                Err(e) => {
                    report.error(Some(team_file), at(e));
                    None
                }
            },
        },
    };

    let mut env = Vec::new();
    let mut values = BTreeMap::new();
    for (name, value) in p.env.entries() {
        if !is_valid_env_name(&name) {
            report.error(
                Some(team_file),
                at(format!("env name `{name}` must match `[A-Z_][A-Z0-9_]*`")),
            );
        }
        if env.contains(&name) {
            report.error(Some(team_file), at(format!("env `{name}` is listed twice")));
            continue;
        }
        if let Some(value) = value {
            values.insert(name.clone(), value);
        }
        env.push(name);
    }

    let mut actions = BTreeMap::new();
    for (action_name, line) in &p.actions {
        if !is_valid_id(action_name) {
            report.error(
                Some(team_file),
                at(format!(
                    "action name `{action_name}` must match `[a-z][a-z0-9_-]{{0,31}}`"
                )),
            );
        }
        match parse_action(line) {
            Ok(action) => {
                actions.insert(action_name.clone(), action);
            }
            Err(e) => report.error(Some(team_file), at(format!("action `{action_name}`: {e}"))),
        }
    }

    if let Some(command) = &p.command
        && (!is_plain_word(command) || command.contains('/'))
    {
        report.error(
            Some(team_file),
            at(format!("command `{command}` must be a bare name")),
        );
    }

    if let Some(process) = &p.process
        && process.is_empty()
    {
        report.error(Some(team_file), at("`process` must not be empty".into()));
    }

    match (&p.delivers_to, &p.process) {
        (Some(_), None) => report.error(
            Some(team_file),
            at("`delivers_to` needs a `process`".into()),
        ),
        (Some(target), Some(_)) if !roles.contains_key(target) => report.error(
            Some(team_file),
            at(format!("`delivers_to: {target}` is not a role")),
        ),
        (Some(target), Some(_)) if roles[target].max.unwrap_or(1) > 1 => report.error(
            Some(team_file),
            at(format!(
                "`delivers_to: {target}` needs a role with `max: 1`"
            )),
        ),
        _ => {}
    }

    // Commands that live in a team-local plugin directory must exist and be executable.
    if let Some(PluginLocation::Team(rel)) = &location {
        let plugin_dir = dir.join(rel);
        let mut commands: Vec<&str> = actions.values().map(Action::command).collect();
        if let Some(command) = &p.command {
            commands.push(command);
        }
        commands.sort();
        commands.dedup();
        for command in commands {
            check_executable(&plugin_dir, command, team_file, &at, report);
        }
        if let Some(process) = &p.process
            && let Some(program) = process.first()
        {
            let program = program.strip_prefix("./").unwrap_or(program);
            check_executable(&plugin_dir, program, team_file, &at, report);
        }
    }

    if report.errors().count() > before {
        return None;
    }
    Some(Plugin {
        name: name.to_string(),
        location,
        command: p.command.clone(),
        process: p.process.clone(),
        delivers_to: p.delivers_to.clone(),
        env,
        values,
        actions,
    })
}

fn check_executable(
    plugin_dir: &Path,
    rel: &str,
    team_file: &Path,
    at: &dyn Fn(String) -> String,
    report: &mut Report,
) {
    match resolve_within(plugin_dir, rel) {
        Ok(real) if is_executable(&real) => {}
        Ok(_) => report.error(Some(team_file), at(format!("`{rel}` is not executable"))),
        Err(e) => report.error(Some(team_file), at(e)),
    }
}

fn load_team_role(
    dir: &Path,
    team_file: &Path,
    id: &str,
    role: &RoleRaw,
    path: &str,
    report: &mut Report,
) -> Option<TeamRole> {
    let load = || -> Result<TeamRole, String> {
        if !is_valid_id(id) {
            return Err("name must match `[a-z][a-z0-9_-]{0,31}`".into());
        }
        if role.prompt.is_some() {
            return Err("choose either `prompt` or `team`".into());
        }
        let dir = resolve_within(dir, path)?;
        if !dir.join(TEAM_FILE).is_file() {
            return Err(format!("`{path}` needs a `{TEAM_FILE}`"));
        }
        let cwd = role.cwd.as_deref().ok_or("a team role needs `cwd`")?;
        check_cwd(cwd)?;
        Ok(TeamRole {
            dir,
            parent: role.parent.clone(),
            max: role.max.unwrap_or(1),
            cwd: cwd.to_string(),
        })
    };
    match load() {
        Ok(role) => Some(role),
        Err(error) => {
            report.error(Some(team_file), format!("role `{id}`: {error}"));
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn load_role(
    dir: &Path,
    team_file: &Path,
    includes_dir: &str,
    default_vendor: Vendor,
    id: &str,
    m: &RoleRaw,
    raw: &TeamFileRaw,
    report: &mut Report,
) -> Option<Role> {
    let before = report.errors().count();
    let at = |msg: String| format!("role `{id}`: {msg}");
    if !is_valid_id(id) {
        report.error(
            Some(team_file),
            at("name must match `[a-z][a-z0-9_-]{0,31}`".into()),
        );
    }
    let vendor = m.vendor.unwrap_or(default_vendor);

    let posture = match m.posture.as_deref().map(parse_posture).transpose() {
        Ok(p) => p.unwrap_or(Posture::Vendor),
        Err(e) => {
            report.error(Some(team_file), at(e));
            Posture::Vendor
        }
    };

    // Grants.
    let mut grants = BTreeMap::new();
    for (plugin_name, granted) in &m.plugins {
        let Some(plugin) = raw.plugins.get(plugin_name) else {
            report.error(
                Some(team_file),
                at(format!("grants undeclared plugin `{plugin_name}`")),
            );
            continue;
        };
        if granted.is_empty() {
            report.error(
                Some(team_file),
                at(format!(
                    "grant for `{plugin_name}` is empty; list action names, or `\"*\"` for all"
                )),
            );
            continue;
        }
        if granted.iter().any(|a| a == "*") {
            if granted.len() > 1 {
                report.error(
                    Some(team_file),
                    at(format!(
                        "grant for `{plugin_name}`: `\"*\"` must be the only entry"
                    )),
                );
            }
            if plugin.actions.is_empty() && plugin.command.is_none() {
                report.error(
                    Some(team_file),
                    at(format!("plugin `{plugin_name}` has no actions, so it needs a `command` for `\"*\"` grants")),
                );
            }
            grants.insert(plugin_name.clone(), Grant::All);
            continue;
        }
        for action in granted {
            if !plugin.actions.contains_key(action) {
                report.error(
                    Some(team_file),
                    at(format!(
                        "grants undeclared action `{action}` of plugin `{plugin_name}`"
                    )),
                );
            }
        }
        grants.insert(plugin_name.clone(), Grant::Actions(granted.clone()));
    }

    // Connectors.
    let connectors = match &m.connectors {
        None => Connectors::default(),
        Some(ConnectorsRaw::Names(names)) => Connectors {
            servers: names.iter().map(|n| (n.clone(), Vec::new())).collect(),
            with_tools: false,
        },
        Some(ConnectorsRaw::Tools(map)) => Connectors {
            servers: map.clone(),
            with_tools: true,
        },
    };
    for (server, tools) in &connectors.servers {
        if server.trim().is_empty() {
            report.error(
                Some(team_file),
                at("connector names must not be empty".into()),
            );
        }
        if connectors.with_tools && tools.is_empty() {
            report.error(
                Some(team_file),
                at(format!("connector `{server}` lists no tools")),
            );
        }
    }

    let max = m.max.unwrap_or(1);
    if max == 0 {
        report.error(Some(team_file), at("`max` must be at least 1".into()));
    }
    if m.cwd.as_ref().is_some_and(|cwd| {
        cwd.contains("{member_id}")
            && (Path::new(cwd)
                .file_name()
                .is_none_or(|c| c != "{member_id}")
                || cwd.matches("{member_id}").count() != 1)
    }) {
        report.error(
            Some(team_file),
            at(
                "`{member_id}` must be cwd's last folder name, for example `copies/{member_id}`"
                    .into(),
            ),
        );
    }
    if let Some(cwd) = &m.cwd
        && let Err(e) = check_cwd(cwd)
    {
        report.error(Some(team_file), at(e));
    }

    // Role file.
    let Some(prompt) = m.prompt.as_deref() else {
        report.error(Some(team_file), at("needs a `prompt` or `team`".into()));
        return None;
    };
    let prompt_path = match resolve_within(dir, prompt) {
        Ok(p) => Some(p),
        Err(e) => {
            report.error(Some(team_file), at(format!("prompt {e}")));
            None
        }
    };
    let mut model = None;
    let mut instruction = String::new();
    let mut prompt_sha256 = String::new();
    if let Some(prompt_path) = &prompt_path {
        let shown = dir.join(prompt);
        if let Some((front, body, sha)) =
            read_role_file(dir, includes_dir, &shown, prompt_path, report)
        {
            prompt_sha256 = sha;
            let configured = match vendor {
                Vendor::Claude => front.models.claude,
                Vendor::Codex => front.models.codex,
            };
            match configured {
                Some(value) => match parse_model(&value) {
                    Ok(parsed) => model = Some(parsed),
                    Err(e) => report.error(Some(&shown), e),
                },
                None => report.warning(
                    Some(&shown),
                    format!(
                        "no `models.{}` entry; the vendor's default model will be used",
                        vendor.as_str()
                    ),
                ),
            }
            instruction = body;
        }
    }

    if report.errors().count() > before {
        return None;
    }
    Some(Role {
        id: id.to_string(),
        vendor,
        model,
        parent: m.parent.clone(),
        start: m.start.unwrap_or(true),
        max,
        posture,
        untrusted_input: m.untrusted_input,
        accept_risk: m.accept_risk,
        grants,
        connectors,
        prompt_file: dir.join(prompt),
        prompt_sha256,
        instruction,
        args: m.args.clone(),
        cwd: m.cwd.clone(),
    })
}

/// A role's `cwd`: relative, inside the instance's folder, with `{member_id}`
/// as its only placeholder.
fn check_cwd(cwd: &str) -> Result<(), String> {
    let path = Path::new(cwd);
    if cwd.is_empty() {
        return Err("`cwd` is empty".into());
    }
    if cwd.contains("{member}") {
        return Err("`{member}` is now `{member_id}` in `cwd`".into());
    }
    if path.is_absolute() || cwd.starts_with('~') {
        return Err(format!(
            "`cwd: {cwd}` must be relative: it is a folder inside the instance's folder"
        ));
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "`cwd: {cwd}` must stay inside the instance's folder (no `..`)"
        ));
    }
    if cwd.replace("{member_id}", "").contains(['{', '}']) {
        return Err(format!(
            "`cwd: {cwd}`: the only placeholder is `{{member_id}}`"
        ));
    }
    Ok(())
}

fn read_role_file(
    dir: &Path,
    includes_dir: &str,
    shown: &Path,
    real: &Path,
    report: &mut Report,
) -> Option<(FrontmatterRaw, String, String)> {
    let text = match read_text(real) {
        Ok(t) => t,
        Err(e) => {
            report.error(Some(shown), e);
            return None;
        }
    };
    let split = match frontmatter::split(&text) {
        Ok(s) => s,
        Err(e) => {
            report.error(Some(shown), e);
            return None;
        }
    };
    if looks_legacy(&split.frontmatter) {
        report.error(
            Some(shown),
            format!(
                "this role file uses the older frontmatter (`name`, `claude_model`/`codex_model`); remove `name` and use `models` as described in {FORMAT_DOC}"
            ),
        );
        return None;
    }
    let front: FrontmatterRaw = if split.frontmatter.trim().is_empty() {
        FrontmatterRaw::default()
    } else {
        match yaml::parse(&split.frontmatter) {
            Ok(f) => f,
            Err(e) => {
                report.error(Some(shown), e);
                return None;
            }
        }
    };
    let body = resolve_includes(dir, includes_dir, shown, &split.body, report)?;
    Some((
        front,
        body,
        crate::config_snapshot::sha256_hex(text.as_bytes()),
    ))
}

fn looks_legacy(frontmatter: &str) -> bool {
    let has = |key: &str| {
        frontmatter
            .lines()
            .any(|l| l.trim_start().starts_with(&format!("{key}:")) && !l.starts_with(' '))
    };
    has("name") && (has("claude_model") || has("codex_model"))
}

/// Own-line `{{includes/<stem>}}` directives, one level deep.
fn include_stem(line: &str) -> Option<&str> {
    line.trim().strip_prefix("{{includes/")?.strip_suffix("}}")
}

fn resolve_includes(
    dir: &Path,
    includes_dir: &str,
    shown: &Path,
    body: &str,
    report: &mut Report,
) -> Option<String> {
    let mut out = String::new();
    let mut ok = true;
    for line in body.split_inclusive('\n') {
        let Some(stem) = include_stem(line) else {
            out.push_str(line);
            continue;
        };
        if !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
            || stem.starts_with('.')
        {
            report.error(Some(shown), format!("bad include name `{stem}`"));
            ok = false;
            continue;
        }
        let rel = format!("{includes_dir}/{stem}.md");
        match resolve_within(dir, &rel) {
            Ok(real) => match read_text(&real) {
                Ok(text) => {
                    let text = text.replace("\r\n", "\n");
                    if text.lines().any(|l| include_stem(l).is_some()) {
                        report.error(
                            Some(&dir.join(&rel)),
                            "includes are one level deep; this include contains another include",
                        );
                        ok = false;
                        continue;
                    }
                    if out.len() + text.len() > yaml::MAX_FILE_BYTES {
                        report.error(
                            Some(shown),
                            format!(
                                "instruction is larger than {} bytes once includes are expanded",
                                yaml::MAX_FILE_BYTES
                            ),
                        );
                        return None;
                    }
                    out.push_str(&text);
                    if !text.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Err(e) => {
                    report.error(Some(&dir.join(&rel)), e);
                    ok = false;
                }
            },
            Err(e) => {
                report.error(Some(shown), format!("include {e}"));
                ok = false;
            }
        }
    }
    if out.len() > yaml::MAX_FILE_BYTES {
        report.error(
            Some(shown),
            format!(
                "instruction is larger than {} bytes once includes are expanded",
                yaml::MAX_FILE_BYTES
            ),
        );
        return None;
    }
    ok.then_some(out)
}

fn check_parents(team_file: &Path, roles: &BTreeMap<String, RoleRaw>, report: &mut Report) {
    for (id, m) in roles {
        let Some(parent) = &m.parent else { continue };
        if parent == id {
            report.error(Some(team_file), format!("role `{id}`: is its own parent"));
            continue;
        }
        if !roles.contains_key(parent) {
            report.error(
                Some(team_file),
                format!("role `{id}`: parent `{parent}` is not a role"),
            );
        } else if roles[parent].max.unwrap_or(1) > 1 {
            report.error(
                Some(team_file),
                format!("role `{id}`: parent `{parent}` needs `max: 1`"),
            );
        }
    }
    // Cycle detection: walk up from each member.
    for start in roles.keys() {
        let mut seen = vec![start.as_str()];
        let mut current = start.as_str();
        while let Some(parent) = roles.get(current).and_then(|m| m.parent.as_deref()) {
            if !roles.contains_key(parent) || parent == current {
                break;
            }
            if seen.contains(&parent) {
                if parent == start {
                    report.error(
                        Some(team_file),
                        format!("parent cycle: {} -> {parent}", seen.join(" -> ")),
                    );
                }
                break;
            }
            seen.push(parent);
            current = parent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_prefix() {
        let a = parse_action("slackbot escalate <ts> <severity> <reason>").unwrap();
        assert_eq!(a.prefix, vec!["slackbot", "escalate"]);
        assert_eq!(a.command(), "slackbot");
    }

    #[test]
    fn action_rejects_metacharacters_and_paths() {
        assert!(parse_action("slackbot read; rm").is_err());
        assert!(parse_action("./slackbot read <ts>").is_err());
        assert!(parse_action("<ts>").is_err());
        assert!(parse_action("slackbot <bad name>").is_err());
    }

    #[test]
    fn models() {
        assert_eq!(
            parse_model("opus high").unwrap(),
            Model {
                name: "opus".into(),
                effort: Some("high".into())
            }
        );
        assert_eq!(parse_model("haiku").unwrap().effort, None);
        assert!(parse_model("opus extreme").is_err());
        assert!(parse_model("a b c").is_err());
    }

    #[test]
    fn postures() {
        assert_eq!(parse_posture("locked").unwrap(), Posture::Locked);
        assert!(parse_posture("sandboxed").unwrap_err().contains("reserved"));
        assert!(parse_posture("open").is_err());
    }
}
