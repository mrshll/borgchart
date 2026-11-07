//! Which wake methods each vendor and CLI version allows: the bundled
//! `rules/wake.yaml`, or your override in
//! `$XDG_CONFIG_HOME/borgchart/rules/wake.yaml` (as for the screen rules in
//! [`crate::rules`]).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::team::Vendor;
use crate::yaml;

const BUNDLED: &str = include_str!("../../rules/wake.yaml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// Queue a message to the member's live thread.
    Queue,
    /// The guarded typed reminder, on an idle, empty screen.
    Typed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRaw {
    wake: Vec<EntryRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryRaw {
    vendor: Vendor,
    version: String,
    #[serde(default)]
    self_wake: bool,
    methods: Vec<Method>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Version {
    Exact(String),
    /// Matches versions starting with this, which ends in `.`.
    Prefix(String),
}

impl Version {
    fn parse(s: &str) -> Result<Version, String> {
        let bad = || format!("version `{s}`: use an exact version or a prefix ending in `.*`");
        match s.strip_suffix('*') {
            Some(prefix) if prefix.ends_with('.') && !prefix.contains('*') && prefix.len() > 1 => {
                Ok(Version::Prefix(prefix.to_string()))
            }
            Some(_) => Err(bad()),
            None if !s.is_empty() && !s.contains('*') => Ok(Version::Exact(s.to_string())),
            None => Err(bad()),
        }
    }

    fn matches(&self, v: &str) -> bool {
        match self {
            Version::Exact(e) => v == e,
            Version::Prefix(p) => v.len() > p.len() && v.starts_with(p.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub vendor: Vendor,
    version: Version,
    pub self_wake: bool,
    pub methods: Vec<Method>,
}

#[derive(Debug, Clone)]
pub struct WakeRules {
    pub source: String,
    entries: Vec<Entry>,
}

pub fn parse(text: &str, source: &str) -> Result<WakeRules, String> {
    let raw: FileRaw = yaml::parse(text).map_err(|e| format!("{source}: {e}"))?;
    let mut entries = Vec::new();
    for e in raw.wake {
        let version = Version::parse(&e.version).map_err(|m| format!("{source}: {m}"))?;
        if e.methods.is_empty() {
            return Err(format!(
                "{source}: version `{}` lists no methods",
                e.version
            ));
        }
        for (i, m) in e.methods.iter().enumerate() {
            if e.methods[..i].contains(m) {
                return Err(format!(
                    "{source}: version `{}` lists a method twice",
                    e.version
                ));
            }
        }
        entries.push(Entry {
            vendor: e.vendor,
            version,
            self_wake: e.self_wake,
            methods: e.methods,
        });
    }
    Ok(WakeRules {
        source: source.to_string(),
        entries,
    })
}

/// The bundled rules.
pub fn bundled() -> WakeRules {
    parse(BUNDLED, "bundled wake rules").expect("the bundled wake rules parse")
}

fn override_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("borgchart/rules/wake.yaml"))
}

/// Your override file if present, else the bundled rules.
pub fn load() -> Result<WakeRules, String> {
    if let Some(path) = override_path()
        && path.is_file()
    {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        return parse(&text, &path.display().to_string());
    }
    Ok(bundled())
}

impl WakeRules {
    /// The first entry for this vendor matching `version`. An unknown
    /// version matches nothing.
    pub fn find(&self, vendor: Vendor, version: Option<&str>) -> Option<&Entry> {
        let version = version?;
        self.entries
            .iter()
            .find(|e| e.vendor == vendor && e.version.matches(version))
    }
}
