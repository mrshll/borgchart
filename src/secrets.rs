//! Resolve plugin secrets through a bounded wrapper; child data never reaches diagnostics.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::{bounded_process, state, validate::Validated};

pub type PluginValues = BTreeMap<String, BTreeMap<String, String>>;
const PREFIX: &str = "brg-secrets ";

struct EnvFile(PathBuf);
impl Drop for EnvFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Hidden helper: only the resolver's captured stdout pipe receives this line.
pub fn helper() {
    let values: BTreeMap<_, _> = std::env::vars()
        .filter(|(n, _)| {
            n.strip_prefix("BORGCHART_SECRET_")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    println!(
        "{PREFIX}{}",
        crate::config_snapshot::hex(&serde_json::to_vec(&values).unwrap())
    );
}

/// Collect values before taking the registry lock or registering a team.
pub fn values(v: &Validated, id: &str) -> Result<PluginValues, String> {
    let sources: Vec<_> = v
        .team
        .plugins
        .iter()
        .flat_map(|(p, plugin)| plugin.sources.iter().map(move |(n, r)| (p, n, r)))
        .collect();
    let mut all = BTreeMap::new();
    for (name, plugin) in &v.team.plugins {
        let mut values = plugin.values.clone();
        for n in plugin
            .secrets()
            .filter(|n| !plugin.sources.contains_key(*n))
        {
            let value = std::env::var(n).map_err(|_| format!("set {name}: {n} in your environment and run the command again; nothing was started"))?;
            values.insert(n.clone(), value);
        }
        all.insert(name.clone(), values);
    }
    if sources.is_empty() {
        return Ok(all);
    }
    if crate::runtime::has_member_context() {
        return Err(format!(
            "team {id} takes secrets from the resolver; ask the owner to run brg start {id}"
        ));
    }
    let resolver = &v.config.secrets.as_ref().ok_or("plugin secrets need a resolver: add secrets: {resolver: [...]} to brg's configuration and run the command again; nothing was started")?.resolver;
    let failure = "the secrets resolver failed; check your resolver and run the command again; nothing was started";
    state::ensure_dir(&state::state_home()).map_err(|_| failure)?;
    let file = EnvFile(state::state_home().join(format!("resolver-{}.env", std::process::id())));
    let body: String = sources
        .iter()
        .enumerate()
        .map(|(i, (_, _, r))| format!("BORGCHART_SECRET_{i}='{r}'\n"))
        .collect();
    state::write_private(&file.0, body.as_bytes()).map_err(|_| failure)?;
    let mut command = Command::new(&resolver[0]);
    command.args(
        resolver[1..]
            .iter()
            .map(|a| a.replace("{file}", &file.0.to_string_lossy())),
    );
    command
        .arg(std::env::current_exe().map_err(|_| failure)?)
        .arg("_secrets");
    for i in 0..sources.len() {
        command.env_remove(format!("BORGCHART_SECRET_{i}"));
    }
    let timeout = Duration::from_secs(120);
    #[cfg(feature = "test-support")]
    let timeout = std::env::var("BORGCHART_TEST_SECRETS_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(timeout);
    eprintln!("resolving plugin secrets for {id}; approve your secret manager if prompted");
    let output = bounded_process::run(
        &mut command,
        None,
        bounded_process::Limits {
            deadline: Instant::now() + timeout,
            stdout_bytes: 256 * 1024,
            stderr_bytes: 64 * 1024,
        },
    );
    drop(file);
    let output = output.map_err(|e| match e {
        bounded_process::Error::Deadline => "the secrets resolver timed out after 120 s; approve its prompt or sign in and run the command again; nothing was started",
        _ => failure,
    })?;
    if !output.status.success() {
        return Err(format!(
            "the secrets resolver failed with exit {}; check your resolver and run the command again; nothing was started",
            output
                .status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "signal".into())
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let hex = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(PREFIX))
        .ok_or(failure)?;
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .map(|c| {
            if c.len() != 2 {
                return None;
            }
            let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
            Some(digit(c[0])? * 16 + digit(c[1])?)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(failure)?;
    let resolved: BTreeMap<String, String> = serde_json::from_slice(&bytes).map_err(|_| failure)?;
    for (i, (plugin, name, reference)) in sources.iter().enumerate() {
        let value = resolved.get(&format!("BORGCHART_SECRET_{i}"))
            .filter(|value| !value.is_empty() && *value != *reference)
            .ok_or_else(|| format!("the secrets resolver left {plugin}: {name} unset; check its reference in template.yaml and run the command again; nothing was started"))?;
        all.get_mut(*plugin)
            .unwrap()
            .insert((*name).clone(), value.clone());
    }
    Ok(all)
}
