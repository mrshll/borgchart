//! Validated repository entries, reloadable YAML and numeric identity guard.
use crate::gh::{Api, Error, Rest};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    owner: String,
    name: String,
}
pub fn valid_login(value: &str) -> bool {
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
    pub fn contains(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner)
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
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let raw: Vec<String> = borgchart::yaml::parse(text)
        .map_err(|e| format!("repositories file: {e}; fix GITHUB_REPOS_FILE"))?;
    let mut entries = Vec::new();
    for value in raw {
        let entry = Entry::parse(&value)?;
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    Ok(entries)
}
pub fn path() -> PathBuf {
    std::env::var_os("GITHUB_REPOS_FILE")
        .unwrap_or("github.yaml".into())
        .into()
}
pub fn owner() -> Result<String, Error> {
    std::env::var("GITHUB_OWNER")
        .ok()
        .filter(|s| valid_login(s))
        .ok_or_else(|| Error::Other("set GITHUB_OWNER to the owner's GitHub login".into()))
}
pub struct Repositories {
    path: PathBuf,
    version: Option<(SystemTime, u64)>,
    active: Result<Vec<Entry>, String>,
}
impl Repositories {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            version: None,
            active: Err("repositories file not loaded; retry".into()),
        }
    }
    pub fn load(&mut self) -> Result<&[Entry], String> {
        let version = fs::metadata(&self.path).and_then(|m| m.modified().map(|t| (t, m.len())));
        match version {
            Ok(version) if self.version == Some(version) => {}
            Ok(version) => {
                self.version = Some(version);
                self.active = fs::read_to_string(&self.path)
                    .map_err(|_| {
                        "cannot read repositories file; fix GITHUB_REPOS_FILE and retry".into()
                    })
                    .and_then(|text| parse(&text));
            }
            Err(_) => {
                self.version = None;
                self.active =
                    Err("cannot read repositories file; fix GITHUB_REPOS_FILE and retry".into());
            }
        }
        self.active.as_deref().map_err(Clone::clone)
    }
}
const GUARD_ERROR: &str = "GitHub identity is unsafe or changed; start brg with GH_CONFIG_DIR naming the machine account's login";
pub struct Guard {
    pub machine_id: u64,
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
        let machine_id = id(&api.get(Rest::Machine)?)?;
        let owner_id = id(&api.get(Rest::Owner(owner)).map_err(|e| match e {
            Error::NotFound => Error::Other(
                "unknown GITHUB_OWNER; set it to the owner's GitHub login and restart".into(),
            ),
            error => error,
        })?)?;
        if machine_id == owner_id {
            return Err(Error::Other(GUARD_ERROR.into()));
        }
        Ok(Self {
            machine_id,
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
            parse("- example-org/widgets\n- example-org/*\n- example-org/widgets\n").unwrap();
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
            assert!(parse(yaml).is_err());
        }
    }
}
