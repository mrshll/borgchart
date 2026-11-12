//! Strict YAML definitions and executable containment checks.
use regex::bytes::Regex;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};

#[derive(Deserialize)]
#[serde(untagged)]
enum Raw {
    Number(f64),
    Text(String),
    List(Vec<Raw>),
    Map(BTreeMap<String, Raw>),
    Other(serde::de::IgnoredAny),
}
impl Raw {
    fn text(&self) -> Option<&str> {
        if let Self::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
}
pub struct Check {
    pub name: String,
    pub run: Vec<String>,
    pub every_minutes: f64,
    pub ignore: Option<Regex>,
}
pub fn matches(pattern: &str, text: &str) -> bool {
    regex::Regex::new(&format!("\\A(?:{pattern})\\z"))
        .unwrap()
        .is_match(text)
}
pub fn valid_name(text: &str) -> bool {
    matches("[a-z][a-z0-9-]{0,31}", text)
}
pub fn command_path(value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|c| c == Component::ParentDir)
    {
        return Err(
            "use a PATH command or a relative executable inside the working directory".into(),
        );
    }
    if value.contains('/') {
        let cwd = std::env::current_dir().map_err(|_| "cannot read working directory")?;
        // Missing local executables fail at execution, just as missing PATH commands do.
        let mut ancestor = cwd.join(path);
        let mut tail = Vec::new();
        while fs::symlink_metadata(&ancestor).is_err() {
            tail.push(
                ancestor
                    .file_name()
                    .ok_or("cannot resolve executable")?
                    .to_owned(),
            );
            ancestor.pop();
        }
        let mut resolved = ancestor
            .canonicalize()
            .map_err(|_| "cannot resolve executable; check symlinks")?;
        if !resolved.starts_with(
            cwd.canonicalize()
                .map_err(|_| "cannot resolve working directory")?,
        ) {
            return Err("executable escapes the working directory; use a local executable".into());
        }
        for part in tail.iter().rev() {
            resolved.push(part);
        }
        return Ok(resolved.to_string_lossy().into_owned());
    }
    Ok(value.to_string())
}
pub fn parse(text: &str) -> Result<Vec<Check>, String> {
    let raw: Vec<Raw> = borgchart::yaml::parse(text).map_err(|e| format!("checks file: {e}"))?;
    let mut names = BTreeSet::new();
    raw.into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let label = if let Raw::Map(m) = &raw {
                m.get("name").and_then(Raw::text).unwrap_or("?")
            } else {
                "?"
            };
            let label = format!("check {} ({label})", i + 1);
            let validate = || -> Result<Check, String> {
                let Raw::Map(m) = raw else {
                    return Err("use check mappings".into());
                };
                if m.keys()
                    .any(|k| !["name", "run", "every_minutes", "ignore"].contains(&k.as_str()))
                {
                    return Err("use only name, run, every_minutes and ignore".into());
                }
                let name = m
                    .get("name")
                    .and_then(Raw::text)
                    .filter(|n| valid_name(n) && !names.contains(*n))
                    .ok_or("use a unique name matching [a-z][a-z0-9-]{0,31}")?
                    .to_string();
                let run = match m.get("run") {
                    Some(Raw::List(list)) if !list.is_empty() => list
                        .iter()
                        .map(|v| v.text().map(str::to_owned))
                        .collect::<Option<Vec<_>>>(),
                    _ => None,
                }
                .ok_or("run must be a non-empty list of strings")?;
                command_path(&run[0])?;
                let every_minutes = match m.get("every_minutes") {
                    Some(Raw::Number(n)) if n.is_finite() && *n > 0.0 => *n,
                    _ => return Err("every_minutes must be a finite positive number".into()),
                };
                let ignore = m
                    .get("ignore")
                    .map(|v| {
                        v.text()
                            .ok_or("ignore must be a regex string")
                            .and_then(|s| Regex::new(s).map_err(|_| "ignore must be a valid regex"))
                    })
                    .transpose()?;
                Ok(Check {
                    name,
                    run,
                    every_minutes,
                    ignore,
                })
            };
            let check = validate().map_err(|e| format!("{label}: {e}"))?;
            names.insert(check.name.clone());
            Ok(check)
        })
        .collect()
}
pub fn load() -> Result<Vec<Check>, String> {
    let path = std::env::var_os("CHECKS_FILE").unwrap_or("checks.yaml".into());
    let text = fs::read_to_string(path)
        .map_err(|_| "checks file: cannot read YAML; fix CHECKS_FILE and retry")?;
    parse(&text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intervals_cannot_accept_booleans_strings_or_nonfinite_values() {
        for value in ["true", "yes", "'1'", ".inf", ".nan", "0", "-1"] {
            assert!(
                parse(&format!(
                    "- {{name: probe, run: [printf], every_minutes: {value}}}"
                ))
                .is_err(),
                "{value}"
            );
        }
        assert!(parse("- {name: probe, run: [printf], every_minutes: 0.1}").is_ok());
    }
    #[test]
    fn duplicate_names_keys_and_unknown_fields_cannot_hide_commands() {
        let entry = "- {name: probe, run: [printf], every_minutes: 1}\n";
        assert!(parse(&(entry.to_owned() + entry)).is_err());
        assert!(parse("- {name: probe, name: hidden, run: [printf], every_minutes: 1}").is_err());
        assert!(parse("- {name: probe, run: [printf], every_minutes: 1, hidden: true}").is_err());
    }
}
