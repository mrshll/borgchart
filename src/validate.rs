//! Whole-template validation: the template, brg's global configuration, and the
//! rules between them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{self, Auth, Config};
use crate::diag::Report;
use crate::paths::{find_on_path, is_executable, resolve_within};
use crate::team::{self, Grant, Plugin, PluginLocation, Posture, Team, Vendor};

#[derive(Debug, Serialize)]
pub struct Validated {
    pub team: Team,
    pub config: Config,
}

impl Validated {
    /// The posture a role runs with.
    pub fn posture(&self, role: &str) -> Posture {
        self.team
            .roles
            .get(role)
            .map(|m| m.posture)
            .unwrap_or(Posture::Vendor)
    }

    pub fn auth(&self) -> Auth {
        self.config.auth
    }

    /// Where a plugin's commands live: its folder in the template, or under
    /// `plugins_dir`; `None` for commands on `PATH`.
    pub fn plugin_dir(&self, name: &str) -> Option<PathBuf> {
        let plugin = self.team.plugins.get(name)?;
        let dir = match plugin.location.as_ref()? {
            PluginLocation::Team(rel) => self.team.dir.join(rel),
            PluginLocation::Shared(shared) => self.config.plugins_dir.as_ref()?.join(shared),
            PluginLocation::Catalog(path) => return Some(path.clone()),
        };
        Some(dir.canonicalize().unwrap_or(dir))
    }
}

/// Validate a template folder and the global configuration before anything
/// starts. Returns the report and, when there were no errors, the resolved
/// template and configuration.
pub fn validate(team_dir: &Path) -> (Report, Option<Validated>) {
    validate_against(team_dir, &[])
}

/// Validate a copy's current template files against its actual, recorded
/// ancestor folders, including after an explicit template-folder move.
pub fn validate_against(team_dir: &Path, ancestors: &[PathBuf]) -> (Report, Option<Validated>) {
    let mut report = Report::default();
    let Some(config) = config::load(&mut report) else {
        return (report, None);
    };
    validate_into(team_dir, config, report, ancestors, None)
}

/// Validate only the declaring instance's files. Referenced templates are
/// checked separately when a copy is created or started.
pub(crate) fn validate_own(
    team_dir: &Path,
    frozen: &std::collections::BTreeMap<String, PathBuf>,
) -> (Report, Option<Validated>) {
    let mut report = Report::default();
    let Some(config) = config::load(&mut report) else {
        return (report, None);
    };
    validate_into(team_dir, config, report, &[], Some(frozen))
}

/// [`validate`] with a configuration already loaded (tests, and callers
/// that must not read the caller's configuration file).
pub fn validate_with(team_dir: &Path, config: Config) -> (Report, Option<Validated>) {
    validate_into(team_dir, config, Report::default(), &[], None)
}

fn validate_into(
    team_dir: &Path,
    config: Config,
    mut report: Report,
    ancestors: &[PathBuf],
    frozen: Option<&std::collections::BTreeMap<String, PathBuf>>,
) -> (Report, Option<Validated>) {
    let recursive = frozen.is_none();
    let loaded = team::load_with(team_dir, &config.teams, frozen, &mut report);
    let Some(team) = loaded else {
        return (report, None);
    };
    let validated = Validated { team, config };
    // Configuration warnings once, first: the template tree is checked per template.
    if let Some(user) = &validated.config.tmux_user_config {
        check_user_tmux_config(user, validated.config.file.as_deref(), &mut report);
    }
    check_team_tree(&validated, ancestors, recursive, &mut report);
    if report.has_errors() {
        return (report, None);
    }
    (report, Some(validated))
}

fn check_team_tree(v: &Validated, ancestors: &[PathBuf], recursive: bool, report: &mut Report) {
    let mut pending = vec![(
        v.team.clone(),
        ancestors.to_vec(),
        v.team.dir.join(team::TEAM_FILE),
    )];
    let mut checked = BTreeSet::new();
    while let Some((team, mut parents, declaring_file)) = pending.pop() {
        let file = team.dir.join(team::TEAM_FILE);
        if parents.contains(&team.dir) {
            report.error(
                Some(&declaring_file),
                format!("template cycle: {} is already an ancestor; change the `template` reference so the template does not contain itself", team.dir.display()),
            );
            continue;
        }
        if !checked.insert(team.dir.clone()) {
            continue;
        }
        parents.push(team.dir.clone());
        if recursive {
            for role in team.team_roles.values() {
                if let Some(child) = team::load_with(&role.dir, &v.config.teams, None, report) {
                    pending.push((child, parents.clone(), file.clone()));
                }
            }
        }
        cross_check(
            &Validated {
                team,
                config: v.config.clone(),
            },
            report,
        );
    }
}

fn cross_check(v: &Validated, report: &mut Report) {
    let team_file = v.team.dir.join(team::TEAM_FILE);
    let team_file = Some(team_file.as_path());
    let config_file = v.config.file.clone();
    let config_file = config_file.as_deref();

    if v.team.roles.is_empty() {
        report.warning(team_file, "the template has no roles");
    }
    for name in v.team.plugins.keys() {
        if v.team.roles.contains_key(name) {
            report.error(
                team_file,
                format!("`{name}` is both a role and a plugin; give them different names"),
            );
        }
    }
    for (id, member) in &v.team.roles {
        if member.vendor == Vendor::Codex
            && v.posture(id) == Posture::Locked
            && v.config.auto_trust == Some(true)
        {
            report.warning(
                config_file,
                format!(
                    "member `{id}`: `auto_trust: true` doesn't trust the folder of a `locked` Codex member, which can write that folder; a trusted folder's `.codex/` loads outside its gate. Its trust screen is left for you"
                ),
            );
        }
    }

    for (id, role) in &v.team.roles {
        if role.interactive && v.posture(id) == Posture::Locked {
            report.error(
                team_file,
                format!("member `{id}`: `interactive` cannot be used with `locked`"),
            );
        }
    }
    if let Some(bus) = &v.team.bus {
        check_bus(v, bus, team_file, report);
    }
    // Shared plugins need the configuration's plugins_dir.
    for plugin in v.team.plugins.values() {
        if let Some(PluginLocation::Shared(shared)) = &plugin.location {
            check_shared_plugin(v, plugin, shared, team_file, config_file, report);
        }
    }

    let targets: BTreeSet<&str> = v
        .team
        .plugins
        .values()
        .filter_map(|p| p.delivers_to.as_deref())
        .collect();

    for (id, member) in &v.team.roles {
        let posture = v.posture(id);
        let at = |msg: &str| format!("role `{id}`: {msg}");

        if member.untrusted_input && posture != Posture::Locked && !member.accept_risk {
            report.error(
                team_file,
                at("`untrusted_input` members must be `locked`, or say `accept_risk: true`"),
            );
        }

        if posture == Posture::Locked
            && !member.connectors.is_empty()
            && !member.connectors.with_tools
        {
            report.error(
                team_file,
                at("`locked` members must list connector tools: `connectors: {<server>: [<tool>, …]}`"),
            );
        }
        if posture == Posture::Vendor && member.connectors.with_tools {
            report.warning(
                team_file,
                at("connector tool lists only apply in `locked` posture; in `vendor` posture every tool of the server is available"),
            );
        }

        if member.remote_control && member.vendor != Vendor::Claude {
            report.error(
                team_file,
                at("Remote Control is Claude-only; remove `remote_control`"),
            );
        }
        if member.remote_control && v.auth() == Auth::ApiKey {
            report.error(
                config_file.or(team_file),
                at("Remote Control needs your claude.ai login; remove `remote_control` or use `auth: login`"),
            );
        }

        if v.auth() == Auth::ApiKey
            && member.vendor == Vendor::Claude
            && !member.connectors.is_empty()
        {
            report.error(
                config_file.or(team_file),
                at("Claude's own connectors need your login, so `auth: api_key` can't be used with members that declare connectors"),
            );
        }

        if posture == Posture::Locked {
            for (plugin_name, grant) in &member.grants {
                if let (Grant::All, Some(plugin)) = (grant, v.team.plugins.get(plugin_name))
                    && plugin.actions.is_empty()
                {
                    report.warning(
                        team_file,
                        at(&format!(
                            "`locked` with an all-actions grant of `{plugin_name}`, which has no action lines, allows every `{}` subcommand",
                            plugin.command.as_deref().unwrap_or(plugin_name)
                        )),
                    );
                }
            }
        }

        if posture == Posture::Vendor {
            if targets.contains(id.as_str()) {
                report.warning(
                    team_file,
                    at("receives work from a plugin process but runs in `vendor` posture; text it reads can steer it"),
                );
            }
            let parent_of_target = v
                .team
                .roles
                .values()
                .any(|m| targets.contains(m.id.as_str()) && m.parent.as_deref() == Some(id));
            if parent_of_target {
                report.warning(
                    team_file,
                    at("is the parent of a member that receives plugin work, but runs in `vendor` posture"),
                );
            }
            for plugin_name in member.grants.keys() {
                if v.team
                    .plugins
                    .get(plugin_name)
                    .is_some_and(|p| p.process.is_some())
                {
                    report.warning(
                        team_file,
                        at(&format!(
                            "is granted `{plugin_name}`, which runs a background process, but runs in `vendor` posture"
                        )),
                    );
                }
            }
        }
    }

    // Granted commands that also exist elsewhere on PATH (once per plugin).
    for plugin in v.team.plugins.values() {
        let granted = v
            .team
            .roles
            .values()
            .any(|m| m.grants.contains_key(&plugin.name));
        if !granted || plugin.location.is_none() {
            continue;
        }
        for command in plugin_commands(plugin) {
            if let Some(other) = find_on_path(&command).first() {
                report.warning(
                    team_file,
                    format!(
                        "plugin `{}`: `{command}` also exists on PATH at {}; prompts use the shim's absolute path",
                        plugin.name,
                        other.display()
                    ),
                );
            }
        }
    }

    // Plugin env names that nothing supplies yet.
    for plugin in v.team.plugins.values() {
        let used = plugin.process.is_some()
            || v.team
                .roles
                .values()
                .any(|m| m.grants.contains_key(&plugin.name));
        if !used {
            continue;
        }
        for name in plugin.secrets() {
            if std::env::var_os(name).is_none() {
                report.warning(
                    team_file,
                    format!(
                        "plugin `{}`: the secret `{name}` is not set in your environment; `brg create` and `brg start` will need it",
                        plugin.name
                    ),
                );
            }
        }
    }
}

fn check_bus(v: &Validated, bus: &team::BusConfig, team_file: Option<&Path>, report: &mut Report) {
    if let Some(alert) = &bus.alert {
        match v.team.plugins.get(&alert.plugin) {
            None => report.error(
                team_file,
                format!(
                    "bus: alert plugin `{}` is not in the template",
                    alert.plugin
                ),
            ),
            Some(plugin) => {
                if !plugin.actions.contains_key(&alert.action) {
                    report.error(
                        team_file,
                        format!(
                            "bus: plugin `{}` has no action `{}`",
                            alert.plugin, alert.action
                        ),
                    );
                }
                if plugin.location.is_none() {
                    report.error(
                        team_file,
                        format!(
                            "bus: alert plugin `{}` needs a `path`, so brg runs a known program",
                            alert.plugin
                        ),
                    );
                }
            }
        }
    }
}

fn plugin_commands(plugin: &Plugin) -> Vec<String> {
    let mut commands: Vec<String> = plugin
        .actions
        .values()
        .map(|a| a.command().to_string())
        .collect();
    if let Some(command) = &plugin.command {
        commands.push(command.clone());
    }
    commands.sort();
    commands.dedup();
    commands
}

fn check_shared_plugin(
    v: &Validated,
    plugin: &Plugin,
    shared: &str,
    team_file: Option<&Path>,
    config_file: Option<&Path>,
    report: &mut Report,
) {
    let Some(root) = v.config.plugins_dir.as_ref() else {
        report.error(
            team_file,
            format!(
                "plugin `{}`: `shared:{shared}` needs `plugins_dir` in brg's configuration ({})",
                plugin.name,
                config::path().display()
            ),
        );
        return;
    };
    let dir = match resolve_within(root, shared) {
        Ok(dir) if dir.is_dir() => dir,
        Ok(_) | Err(_) => {
            report.error(
                config_file,
                format!(
                    "plugin `{}`: shared plugin `{shared}` not found under {}",
                    plugin.name,
                    root.display()
                ),
            );
            return;
        }
    };
    let mut programs = plugin_commands(plugin);
    if let Some(program) = plugin.process.as_ref().and_then(|p| p.first()) {
        programs.push(program.strip_prefix("./").unwrap_or(program).to_string());
    }
    for program in programs {
        match resolve_within(&dir, &program) {
            Ok(real) if is_executable(&real) => {}
            Ok(_) => report.error(
                config_file,
                format!("plugin `{}`: `{program}` is not executable", plugin.name),
            ),
            Err(e) => report.error(config_file, format!("plugin `{}`: {e}", plugin.name)),
        }
    }
}

/// `tmux.user_config`: the file must be usable, and anything in it (or in a
/// file it sources by a plain path) that can act after brg's required options
/// are applied is reported with its lines.
fn check_user_tmux_config(
    user: &config::UserTmuxConfig,
    config_file: Option<&Path>,
    report: &mut Report,
) {
    let Some(path) = user.resolve() else {
        report.warning(
            config_file,
            "`tmux.user_config: true`, but none of ~/.tmux.conf, $XDG_CONFIG_HOME/tmux/tmux.conf or ~/.config/tmux/tmux.conf exists; nothing will be loaded",
        );
        return;
    };
    let shown = path.display().to_string();
    // tmux reads the path quoted, then expands it as a glob pattern.
    if shown.contains(['\'', '\n', '*', '?', '[', '\\']) {
        report.error(
            config_file,
            format!(
                "`tmux.user_config`: brg can't load a path containing `'`, `*`, `?`, `[`, `\\` or a newline: {shown:?}"
            ),
        );
        return;
    }
    if !path.is_file() {
        report.error(
            config_file,
            format!("`tmux.user_config`: {shown} is not a file"),
        );
        return;
    }
    let mut queue = vec![path];
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(file) = queue.pop() {
        if seen.len() >= 16 || !seen.insert(file.clone()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let scan = scan_tmux_config(&text);
        let mut relative = Vec::new();
        for (line, arg) in &scan.sources {
            match sourced_path(arg) {
                Some(p) => queue.push(p),
                None => relative.push(*line),
            }
        }
        let later = "brg applies its own options after your config loads, but these can change them later or act on brg's sessions";
        for (what, lines, why) in [
            (
                "runs shell commands (`run`, `run-shell`, `if-shell`)",
                &scan.runs,
                later,
            ),
            ("sets hooks (`set-hook`)", &scan.hooks, later),
            ("uses a plugin manager", &scan.plugins, later),
            (
                "uses a session save/restore plugin",
                &scan.restore,
                "on brg's server it can save brg's sessions over the ones you saved from your own tmux, or restore yours into brg's server",
            ),
            (
                "creates or attaches sessions as it loads",
                &scan.sessions,
                "brg's server would then outlive `brg stop` and keep this config until it's stopped",
            ),
            (
                "sources a file by a relative path",
                &relative,
                "tmux finds it from the folder brg runs in, so brg didn't check it",
            ),
        ] {
            if lines.is_empty() {
                continue;
            }
            let at: Vec<String> = lines.iter().map(usize::to_string).collect();
            report.warning(
                config_file,
                format!(
                    "`tmux.user_config`: {} {what} at {} {}; {why}",
                    file.display(),
                    if at.len() == 1 { "line" } else { "lines" },
                    at.join(", ")
                ),
            );
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TmuxConfigScan {
    runs: Vec<usize>,
    hooks: Vec<usize>,
    plugins: Vec<usize>,
    restore: Vec<usize>,
    sessions: Vec<usize>,
    /// Literal paths given to `source`/`source-file`, with their lines.
    sources: Vec<(usize, String)>,
}

/// Lines (1-based) in a tmux config that brg can't vouch for. Best effort:
/// only a line's first command is read, and key bindings (which act only on a
/// key press) are skipped.
fn scan_tmux_config(text: &str) -> TmuxConfigScan {
    let mut scan = TmuxConfigScan::default();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or("");
        let n = i + 1;
        match first {
            "bind" | "bind-key" => continue,
            "run" | "run-shell" => scan.runs.push(n),
            // `-F` only expands a format; without it the condition is a shell command.
            "if" | "if-shell" if !line.split_whitespace().any(|w| w == "-F") => scan.runs.push(n),
            "set-hook" => scan.hooks.push(n),
            "new" | "new-session" | "neww" | "new-window" | "attach" | "attach-session" | "a" => {
                scan.sessions.push(n)
            }
            "source" | "source-file" => {
                if let Some(arg) = words.find(|w| !w.starts_with('-')) {
                    let arg = arg.trim_matches(|c| c == '\'' || c == '"');
                    if !arg.contains('$') && !arg.contains("#{") {
                        scan.sources.push((n, arg.to_string()));
                    }
                }
            }
            _ => {}
        }
        if ["resurrect", "continuum"].iter().any(|w| line.contains(w)) {
            scan.restore.push(n);
        } else if ["@plugin", "tpm"].iter().any(|w| line.contains(w)) {
            scan.plugins.push(n);
        }
    }
    scan
}

/// A sourced file brg can find: an absolute or `~/` path. tmux resolves a
/// relative one from its client's folder, which brg can't know here.
fn sourced_path(arg: &str) -> Option<PathBuf> {
    if let Some(rest) = arg.strip_prefix("~/") {
        return std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest));
    }
    let path = PathBuf::from(arg);
    path.is_absolute().then_some(path)
}
