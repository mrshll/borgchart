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
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    owner: String,
    #[serde(default)]
    gh_config_dir: String,
    #[serde(default)]
    repositories: Vec<String>,
}
pub struct Config {
    pub owner: String,
    pub gh_config_dir: PathBuf,
    pub entries: Vec<Entry>,
}
pub fn parse(text: &str) -> Result<Config, String> {
    let raw: File = borgchart::yaml::parse(text)
        .map_err(|e| format!("GitHub configuration: {e}; fix GITHUB_CONFIG"))?;
    if !valid_login(&raw.owner) {
        return Err("set owner in GITHUB_CONFIG to the owner's GitHub login".into());
    }
    if raw.gh_config_dir.trim().is_empty() {
        return Err(
            "set gh_config_dir in GITHUB_CONFIG to the machine account's gh configuration".into(),
        );
    }
    Ok(Config {
        owner: raw.owner,
        gh_config_dir: borgchart::config::expand_home(Path::new(""), &raw.gh_config_dir),
        entries: entries(raw.repositories)?,
    })
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
pub fn template(path: &Path) -> Result<Option<Vec<Entry>>, String> {
    let load = || -> Result<_, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("cannot read repository list".into()),
        };
        borgchart::yaml::parse::<Option<Vec<String>>>(&text)?
            .filter(|values| !values.is_empty())
            .map(entries)
            .transpose()
    };
    load().map_err(|e| format!("{}: {e}; fix the file and retry", path.display()))
}
pub fn path() -> PathBuf {
    borgchart::config::path()
        .parent()
        .unwrap()
        .join(std::env::var_os("GITHUB_CONFIG").unwrap_or("github.yaml".into()))
}
/// Read every poll, so an edit applies at the next poll; an unreadable or
/// invalid file stops discovery rather than keeping an older list.
pub fn load(path: &Path) -> Result<Option<Config>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err("cannot read GitHub configuration; fix GITHUB_CONFIG and retry".into());
        }
    };
    parse(&text).map(Some)
}
const GUARD_ERROR: &str = "GitHub identity is unsafe or changed; set gh_config_dir in GITHUB_CONFIG to the machine account's login and restart the plugin";
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
                "unknown owner; set owner in GITHUB_CONFIG to the owner's GitHub login".into(),
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
        let entries =
            parse("owner: example-owner\ngh_config_dir: machine-profile\nrepositories: [example-org/widgets, example-org/*, example-org/widgets]").unwrap().entries;
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
        for yaml in ["true", "[42]", "[{}]", "{}"] {
            assert!(parse(&format!("owner: o\ngh_config_dir: p\nrepositories: {yaml}")).is_err());
        }
    }
}
