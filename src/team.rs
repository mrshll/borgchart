//! The team format: `team.yaml` plus one Markdown file per member.
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
    #[serde(default)]
    plugins: BTreeMap<String, PluginRaw>,
    members: BTreeMap<String, MemberRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginRaw {
    path: Option<String>,
    command: Option<String>,
    process: Option<Vec<String>>,
    delivers_to: Option<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    actions: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberRaw {
    prompt: String,
    vendor: Option<Vendor>,
    parent: Option<String>,
    start: Option<bool>,
    posture: Option<String>,
    #[serde(default)]
    untrusted_input: bool,
    #[serde(default)]
    accept_risk: bool,
    #[serde(default)]
    plugins: BTreeMap<String, Vec<String>>,
    connectors: Option<ConnectorsRaw>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontmatterRaw {
    role: String,
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
    pub dir: PathBuf,
    /// SHA-256 of `team.yaml` as it was read and parsed.
    pub file_sha256: String,
    pub default_vendor: Vendor,
    pub members: BTreeMap<String, Member>,
    pub plugins: BTreeMap<String, Plugin>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub id: String,
    pub role: String,
    pub vendor: Vendor,
    pub model: Option<Model>,
    pub parent: Option<String>,
    pub start: bool,
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
    pub env: Vec<String>,
    pub actions: BTreeMap<String, Action>,
}

#[derive(Debug, Clone, Serialize)]
pub enum PluginLocation {
    /// A directory inside the team, relative to the team dir.
    Team(String),
    /// `shared:<name>`, resolved under the deployment's `plugins_dir`.
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
        if let Some(plugin) = load_plugin(dir, &team_file, name, p, &raw.members, report) {
            plugins.insert(name.clone(), plugin);
        }
    }

    let mut members = BTreeMap::new();
    for (id, m) in &raw.members {
        if let Some(member) = load_member(
            dir,
            &team_file,
            &includes_dir,
            default_vendor,
            id,
            m,
            &raw,
            report,
        ) {
            members.insert(id.clone(), member);
        }
    }

    check_parents(&team_file, &raw.members, report);

    if report.errors().count() > errors_before {
        return None;
    }
    Some(Team {
        dir: dir.to_path_buf(),
        file_sha256: crate::bus::manifest::sha256_hex(text.as_bytes()),
        default_vendor,
        members,
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
    members: &BTreeMap<String, MemberRaw>,
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

    for env in &p.env {
        if !is_valid_env_name(env) {
            report.error(
                Some(team_file),
                at(format!("env name `{env}` must match `[A-Z_][A-Z0-9_]*`")),
            );
        }
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
        (Some(target), Some(_)) if !members.contains_key(target) => report.error(
            Some(team_file),
            at(format!("`delivers_to: {target}` is not a member")),
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
        env: p.env.clone(),
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

#[allow(clippy::too_many_arguments)]
fn load_member(
    dir: &Path,
    team_file: &Path,
    includes_dir: &str,
    default_vendor: Vendor,
    id: &str,
    m: &MemberRaw,
    raw: &TeamFileRaw,
    report: &mut Report,
) -> Option<Member> {
    let before = report.errors().count();
    let at = |msg: String| format!("member `{id}`: {msg}");
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

    // Member file.
    let prompt_path = match resolve_within(dir, &m.prompt) {
        Ok(p) => Some(p),
        Err(e) => {
            report.error(Some(team_file), at(format!("prompt {e}")));
            None
        }
    };
    let mut role = String::new();
    let mut model = None;
    let mut instruction = String::new();
    let mut prompt_sha256 = String::new();
    if let Some(prompt_path) = &prompt_path {
        let shown = dir.join(&m.prompt);
        if let Some((front, body, sha)) =
            read_member_file(dir, includes_dir, &shown, prompt_path, report)
        {
            prompt_sha256 = sha;
            role = front.role;
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
    Some(Member {
        id: id.to_string(),
        role,
        vendor,
        model,
        parent: m.parent.clone(),
        start: m.start.unwrap_or(true),
        posture,
        untrusted_input: m.untrusted_input,
        accept_risk: m.accept_risk,
        grants,
        connectors,
        prompt_file: dir.join(&m.prompt),
        prompt_sha256,
        instruction,
    })
}

struct Front {
    role: String,
    models: ModelsRaw,
}

fn read_member_file(
    dir: &Path,
    includes_dir: &str,
    shown: &Path,
    real: &Path,
    report: &mut Report,
) -> Option<(Front, String, String)> {
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
                "this member uses the older frontmatter (`name`, `claude_model`/`codex_model`); use `role` and `models` as described in {FORMAT_DOC}"
            ),
        );
        return None;
    }
    let front: FrontmatterRaw = match yaml::parse(&split.frontmatter) {
        Ok(f) => f,
        Err(e) => {
            report.error(Some(shown), e);
            return None;
        }
    };
    if front.role.trim().is_empty() {
        report.error(Some(shown), "`role` must not be empty");
    }
    let body = resolve_includes(dir, includes_dir, shown, &split.body, report)?;
    Some((
        Front {
            role: front.role,
            models: front.models,
        },
        body,
        crate::bus::manifest::sha256_hex(text.as_bytes()),
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

fn check_parents(team_file: &Path, members: &BTreeMap<String, MemberRaw>, report: &mut Report) {
    for (id, m) in members {
        let Some(parent) = &m.parent else { continue };
        if parent == id {
            report.error(Some(team_file), format!("member `{id}`: is its own parent"));
            continue;
        }
        if !members.contains_key(parent) {
            report.error(
                Some(team_file),
                format!("member `{id}`: parent `{parent}` is not a member"),
            );
        }
    }
    // Cycle detection: walk up from each member.
    for start in members.keys() {
        let mut seen = vec![start.as_str()];
        let mut current = start.as_str();
        while let Some(parent) = members.get(current).and_then(|m| m.parent.as_deref()) {
            if !members.contains_key(parent) || parent == current {
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
