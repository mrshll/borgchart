//! Whole-team validation: the team, its deployment, and the rules between them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::deploy::{self, Auth, Deployment};
use crate::diag::Report;
use crate::paths::{find_on_path, is_executable, resolve_within};
use crate::team::{self, Grant, Plugin, PluginLocation, Posture, Team, Vendor};

/// What to validate.
pub enum Input {
    /// A `borgchart.yaml` and the team it names.
    Deployment(PathBuf),
    /// A team directory on its own, with no deployment values.
    Team(PathBuf),
}

#[derive(Debug, Serialize)]
pub struct Validated {
    pub team: Team,
    pub deployment: Option<Deployment>,
}

impl Validated {
    /// The posture a member runs with: the deployment's override, else the team's.
    pub fn posture(&self, member: &str) -> Posture {
        self.deployment
            .as_ref()
            .and_then(|d| d.members.get(member))
            .and_then(|m| m.posture)
            .or_else(|| self.team.members.get(member).map(|m| m.posture))
            .unwrap_or(Posture::Vendor)
    }

    pub fn auth(&self) -> Auth {
        self.deployment.as_ref().map(|d| d.auth).unwrap_or_default()
    }
}

/// Validate everything before anything starts. Returns the report and, when
/// there were no errors, the resolved team and deployment.
pub fn validate(input: &Input) -> (Report, Option<Validated>) {
    let mut report = Report::default();
    let (team_dir, deployment) = match input {
        Input::Deployment(file) => match deploy::load(file, &mut report) {
            Some(d) => (d.team_dir.clone(), Some(d)),
            None => return (report, None),
        },
        Input::Team(dir) => (dir.clone(), None),
    };
    let Some(team) = team::load(&team_dir, &mut report) else {
        return (report, None);
    };
    let validated = Validated { team, deployment };
    cross_check(&validated, &mut report);
    if report.has_errors() {
        return (report, None);
    }
    (report, Some(validated))
}

fn cross_check(v: &Validated, report: &mut Report) {
    let team_file = v.team.dir.join(team::TEAM_FILE);
    let team_file = Some(team_file.as_path());

    if v.team.members.is_empty() {
        report.warning(team_file, "the team has no members");
    }
    for name in v.team.plugins.keys() {
        if v.team.members.contains_key(name) {
            report.error(
                team_file,
                format!("`{name}` is both a member and a plugin; give them different names"),
            );
        }
    }
    let deploy_file = v.deployment.as_ref().map(|d| d.file.clone());
    let deploy_file = deploy_file.as_deref();

    // Deployment entries must refer to the team.
    if let Some(d) = &v.deployment {
        for id in d.members.keys() {
            if !v.team.members.contains_key(id) {
                report.error(deploy_file, format!("member `{id}` is not in the team"));
            }
        }
        for (name, p) in &d.plugins {
            let Some(plugin) = v.team.plugins.get(name) else {
                report.error(deploy_file, format!("plugin `{name}` is not in the team"));
                continue;
            };
            for key in p.env.keys() {
                if !plugin.env.contains(key) {
                    report.error(
                        deploy_file,
                        format!(
                            "plugin `{name}`: env `{key}` is not declared in the team's `env` list"
                        ),
                    );
                }
            }
        }
        for (id, m) in &d.members {
            if let Some(member) = v.team.members.get(id)
                && member.vendor == Vendor::Codex
                && v.posture(id) == Posture::Locked
                && let Some(cwd) = &m.cwd
            {
                let cwd_real = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
                let inside = |p: &Path| {
                    let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
                    p.starts_with(&cwd_real)
                };
                if inside(&crate::state::state_home()) || inside(&v.team.dir) {
                    report.warning(
                        deploy_file,
                        format!(
                            "member `{id}`: a `locked` Codex member can write its cwd, and {} contains the team or brg's state; pick a narrower folder",
                            cwd.display()
                        ),
                    );
                }
            }
            let vendor_claude = v
                .team
                .members
                .get(id)
                .is_some_and(|t| t.vendor == Vendor::Claude);
            if d.auto_trust == Some(true)
                && vendor_claude
                && v.posture(id) == Posture::Vendor
                && let Some(cwd) = &m.cwd
                && (cwd.join(".claude").exists() || cwd.join(".mcp.json").exists())
            {
                report.warning(
                    deploy_file,
                    format!(
                        "member `{id}`: `auto_trust: true` will trust {}, which has project settings (`.claude/` or `.mcp.json`) that will load in `vendor` posture",
                        cwd.display()
                    ),
                );
            }
            if let Some(cwd) = &m.cwd
                && !cwd.is_dir()
            {
                report.warning(
                    deploy_file,
                    format!("member `{id}`: cwd {} does not exist", cwd.display()),
                );
            }
        }
    }

    // Shared plugins need the deployment's plugins_dir.
    for plugin in v.team.plugins.values() {
        if let Some(PluginLocation::Shared(shared)) = &plugin.location {
            check_shared_plugin(v, plugin, shared, team_file, deploy_file, report);
        }
    }

    let targets: BTreeSet<&str> = v
        .team
        .plugins
        .values()
        .filter_map(|p| p.delivers_to.as_deref())
        .collect();

    for (id, member) in &v.team.members {
        let posture = v.posture(id);
        let at = |msg: &str| format!("member `{id}`: {msg}");

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

        if v.auth() == Auth::ApiKey
            && member.vendor == Vendor::Claude
            && !member.connectors.is_empty()
        {
            report.error(
                deploy_file.or(team_file),
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
                .members
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
            .members
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
                .members
                .values()
                .any(|m| m.grants.contains_key(&plugin.name));
        if !used {
            continue;
        }
        for name in &plugin.env {
            let in_deploy = v
                .deployment
                .as_ref()
                .and_then(|d| d.plugins.get(&plugin.name))
                .is_some_and(|p| p.env.contains_key(name));
            if !in_deploy && std::env::var_os(name).is_none() {
                report.warning(
                    deploy_file.or(team_file),
                    format!(
                        "plugin `{}`: `{name}` is not set in borgchart.yaml or your environment; `brg up` will need it",
                        plugin.name
                    ),
                );
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
    deploy_file: Option<&Path>,
    report: &mut Report,
) {
    let Some(root) = v.deployment.as_ref().and_then(|d| d.plugins_dir.as_ref()) else {
        report.error(
            team_file,
            format!(
                "plugin `{}`: `shared:{shared}` needs `plugins_dir` in borgchart.yaml",
                plugin.name
            ),
        );
        return;
    };
    let dir = match resolve_within(root, shared) {
        Ok(dir) if dir.is_dir() => dir,
        Ok(_) | Err(_) => {
            report.error(
                deploy_file,
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
                deploy_file,
                format!("plugin `{}`: `{program}` is not executable", plugin.name),
            ),
            Err(e) => report.error(deploy_file, format!("plugin `{}`: {e}", plugin.name)),
        }
    }
}
