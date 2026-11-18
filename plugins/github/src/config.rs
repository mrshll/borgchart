//! Private identity, template repository scope and numeric identity guard.
use crate::gh::{Api, Error, Rest};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    owner: String,
    name: String,
}
fn valid_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn valid_repo(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && ![".", ".."].contains(&value)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
impl Entry {
    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
    pub fn is_org(&self) -> bool {
        self.name == "*"
    }
    pub fn api_path(&self) -> String {
        if self.is_org() {
            format!("orgs/{}", self.owner)
        } else {
            format!("repos/{}/{}", self.owner, self.name)
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        let (owner, name) = value.split_once('/').filter(|(o, n)| valid_login(o) && (*n == "*" || valid_repo(n)))
            .ok_or_else(|| format!("invalid repository entry {}; use owner/name or org/* with GitHub identifier characters", borgchart::plugin::escape(value)))?;
        Ok(Self {
            owner: owner.into(),
            name: name.into(),
        })
    }
}
pub struct Config {
    pub owner: String,
    pub gh_config_dir: PathBuf,
    pub entries: Vec<Entry>,
}
fn entries(values: Vec<String>) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for value in values {
        let entry = Entry::parse(&value)?;
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    Ok(entries)
}
/// Read only this plugin's scope; the rest of the template is brg's to validate.
pub fn template() -> Result<Vec<Entry>, String> {
    let state = PathBuf::from(std::env::var_os("BORGCHART_STATE_DIR").unwrap_or_default());
    let name = state.file_name().unwrap_or_default().to_string_lossy();
    let fix = |error| {
        format!(
            "template.yaml: plugins.{name}.env.GITHUB_REPOSITORIES: {error}; fix the file and retry"
        )
    };
    let text = match fs::read_to_string("template.yaml") {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(fix("cannot read template.yaml".to_string())),
    };
    let template = borgchart::yaml::parse::<serde_json::Value>(&text).map_err(fix)?;
    let Some(value) = template.pointer(&format!("/plugins/{name}/env/GITHUB_REPOSITORIES")) else {
        return Ok(Vec::new());
    };
    let text = value
        .as_str()
        .ok_or_else(|| fix("use a whitespace-separated string".into()))?;
    entries(text.split_whitespace().map(str::to_string).collect()).map_err(fix)
}
/// Read the shared configuration every poll, retaining only the GitHub section.
pub fn load(path: &Path) -> Result<Option<Config>, String> {
    let mut report = borgchart::diag::Report::default();
    let config = borgchart::config::load_file(path, &mut report).ok_or_else(|| {
        report
            .errors()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let Some(raw) = config.github else {
        return Ok(None);
    };
    if !valid_login(&raw.owner) {
        return Err("set owner in config.yaml's github section to the owner's GitHub login".into());
    }
    Ok(Some(Config {
        owner: raw.owner,
        gh_config_dir: raw.gh_config_dir,
        entries: entries(raw.repositories)?,
    }))
}
const GUARD_ERROR: &str = "GitHub identity is unsafe or changed; set gh_config_dir in config.yaml's github section to the machine account's login and restart the plugin";
pub struct Guard {
    pub machine_id: u64,
    pub machine_login: String,
    pub owner_id: u64,
    checked: SystemTime,
    stopped: bool,
}
fn id(value: &serde_json::Value) -> Result<u64, Error> {
    value["id"].as_u64().filter(|n| *n > 0).ok_or_else(|| {
        Error::Other(
            "GitHub returned no numeric account ID; check the machine login and retry".into(),
        )
    })
}
impl Guard {
    pub fn start(api: &impl Api, owner: &str, now: SystemTime) -> Result<Self, Error> {
        let machine = api.get(Rest::Machine)?;
        let machine_id = id(&machine)?;
        let machine_login = machine["login"]
            .as_str()
            .filter(|s| valid_login(s))
            .ok_or_else(|| Error::Other(GUARD_ERROR.into()))?
            .to_string();
        let owner_id = id(&api.get(Rest::Owner(owner)).map_err(|e| match e {
            Error::NotFound => Error::Other(
                "unknown owner; set owner in config.yaml's github section to the owner's GitHub login".into(),
            ),
            error => error,
        })?)?;
        if machine_id == owner_id {
            return Err(Error::Other(GUARD_ERROR.into()));
        }
        Ok(Self {
            machine_id,
            machine_login,
            owner_id,
            checked: now,
            stopped: false,
        })
    }
    pub fn check(&mut self, api: &impl Api, now: SystemTime) -> Result<(), Error> {
        if self.stopped {
            return Err(Error::Other(GUARD_ERROR.into()));
        }
        if now.duration_since(self.checked).unwrap_or_default() >= Duration::from_secs(600) {
            let current = id(&api.get(Rest::Machine)?)?;
            if current != self.machine_id || current == self.owner_id {
                self.stopped = true;
                return Err(Error::Other(GUARD_ERROR.into()));
            }
            self.checked = now;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repository_entries_accept_only_safe_repository_and_org_identifiers() {
        let entries = entries(vec![
            "example-org/widgets".into(),
            "example-org/*".into(),
            "example-org/widgets".into(),
        ])
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].api_path(), "repos/example-org/widgets");
        assert_eq!(entries[1].api_path(), "orgs/example-org");
        for value in [
            "org",
            "org/",
            "/repo",
            "org/a/b",
            "org/repo?x",
            "../repo",
            "org/..",
            "org/a b",
            "org/*x",
            "-org/repo",
            "org/repo\nforge",
        ] {
            let error = Entry::parse(value).unwrap_err();
            assert!(error.contains(&borgchart::plugin::escape(value)), "{error}");
        }
    }
}
