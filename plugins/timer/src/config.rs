//! Strict YAML definitions and executable and prompt file containment checks.
use regex::bytes::Regex;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};

const RUN_PROMPT_BYTES: usize = 16_000;

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
pub struct Config {
    pub checks: Vec<Check>,
    pub schedules: Vec<crate::schedule::Schedule>,
}
pub fn parse(text: &str) -> Result<Config, String> {
    let raw: Vec<Raw> = borgchart::yaml::parse(text).map_err(|e| format!("checks file: {e}"))?;
    let mut names = BTreeSet::new();
    let mut schedules = Vec::new();
    let checks = raw
        .into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let label = if let Raw::Map(m) = &raw {
                m.get("name").and_then(Raw::text).unwrap_or("?")
            } else {
                "?"
            };
            let label = format!("check {} ({label})", i + 1);
            let validate = || -> Result<Option<Check>, String> {
                let Raw::Map(m) = raw else {
                    return Err("use check mappings".into());
                };
                let name = m
                    .get("name")
                    .and_then(Raw::text)
                    .filter(|n| valid_name(n) && !names.contains(*n))
                    .ok_or("use a unique name matching [a-z][a-z0-9-]{0,31}")?
                    .to_string();
                names.insert(name.clone());
                if m.contains_key("cron") {
                    if m.keys()
                        .any(|k| !["name", "cron", "missed", "prompt", "prompt_file", "run", "timeout_minutes"].contains(&k.as_str()))
                    {
                        return Err("use only name, cron, missed, prompt, prompt_file, run and timeout_minutes for a schedule".into());
                    }
                    let run = match m.get("run") {
                        Some(role) => {
                            if m.contains_key("missed") {
                                return Err("remove missed from the run schedule".into());
                            }
                            let role = role.text()
                                .filter(|r| borgchart::names::is_valid_id(r))
                                .ok_or("write run as a role name")?;
                            let minutes = match m.get("timeout_minutes") {
                                Some(Raw::Number(n)) if n.fract() == 0.0 && (1.0..=1440.0).contains(n) => *n as u64,
                                _ => return Err("set timeout_minutes to a whole number from 1 to 1440".into()),
                            };
                            Some(crate::schedule::Run {
                                role: role.into(),
                                timeout: std::time::Duration::from_secs(minutes * 60),
                                prompt_file: m.get("prompt_file").map(|v| {
                                    if m.contains_key("prompt") {
                                        return Err("use either prompt or prompt_file on a run schedule");
                                    }
                                    v.text().filter(|s| !s.is_empty()).map(str::to_owned)
                                        .ok_or("write prompt_file as a relative file path")
                                }).transpose()?,
                            })
                        }
                        None if m.contains_key("prompt_file") => return Err("use prompt_file only with run".into()),
                        None if m.contains_key("timeout_minutes") => return Err("use timeout_minutes only with run".into()),
                        None => None,
                    };
                    let cron = crate::cron::Cron::parse(
                        m.get("cron")
                            .and_then(Raw::text)
                            .ok_or("write cron as a five-field string")?,
                    )?;
                    let catch_up = match m.get("missed").map(Raw::text) {
                        None | Some(Some("skip")) => false,
                        Some(Some("catch-up")) => true,
                        _ => return Err("write missed: skip or missed: catch-up".into()),
                    };
                    let prompt = m.get("prompt").map(|v| {
                        let (limit, multiline, error) = if run.is_some() {
                            (RUN_PROMPT_BYTES, true, "write prompt as non-empty text, at most 16000 UTF-8 bytes after trimming, with no control characters except newlines")
                        } else {
                            (300, false, "write prompt as one non-empty string, at most 300 UTF-8 bytes after trimming, without newlines or control characters; point to a file for longer jobs")
                        };
                        v.text().filter(|s| !s.chars().any(|c| c.is_control() && !(multiline && c == '\n')))
                            .map(str::trim).filter(|s| !s.is_empty() && s.len() <= limit)
                            .map(str::to_owned).ok_or(error)
                    }).transpose()?;
                    schedules.push(crate::schedule::Schedule {
                        name,
                        cron,
                        catch_up,
                        prompt,
                        run,
                    });
                    return Ok(None);
                }
                if m.keys()
                    .any(|k| !["name", "run", "every_minutes", "ignore"].contains(&k.as_str()))
                {
                    return Err("use only name, run, every_minutes and ignore for a check".into());
                }
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
                Ok(Some(Check {
                    name,
                    run,
                    every_minutes,
                    ignore,
                }))
            };
            let check = validate().map_err(|e| format!("{label}: {e}"))?;
            Ok(check)
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(Config { checks, schedules })
}
pub fn path() -> std::path::PathBuf {
    std::env::var_os("CHECKS_FILE")
        .unwrap_or("checks.yaml".into())
        .into()
}
pub fn load(path: &Path) -> Result<Config, String> {
    let config = parse_bytes(
        &fs::read(path).map_err(|_| "checks file: cannot read YAML; fix CHECKS_FILE and retry")?,
    )?;
    prompt_files(&config, folder(path))?;
    Ok(config)
}
pub fn parse_bytes(bytes: &[u8]) -> Result<Config, String> {
    parse(std::str::from_utf8(bytes).map_err(|_| "checks file: write UTF-8 YAML")?)
}
pub(crate) fn folder(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
pub(crate) fn read_prompt_file(folder: &Path, value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err("use a relative prompt_file path without ..".into());
    }
    let folder = folder
        .canonicalize()
        .map_err(|_| "cannot resolve the checks folder; check CHECKS_FILE")?;
    let path = folder
        .join(path)
        .canonicalize()
        .map_err(|_| "cannot resolve prompt_file; point it to an existing file")?;
    if !path.starts_with(&folder) || !path.is_file() {
        return Err("use a regular prompt_file inside the checks folder".into());
    }
    let text = fs::read_to_string(path).map_err(|_| "use a readable UTF-8 prompt_file")?;
    let text = text.trim();
    if text.is_empty() || text.len() > RUN_PROMPT_BYTES {
        return Err(
            "write non-empty prompt_file text of at most 16000 UTF-8 bytes after trimming".into(),
        );
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(
            "remove control characters other than newlines and tabs from prompt_file".into(),
        );
    }
    Ok(text.to_owned())
}
pub(crate) fn prompt_files(config: &Config, folder: &Path) -> Result<(), String> {
    for schedule in &config.schedules {
        if let Some(path) = schedule
            .run
            .as_ref()
            .and_then(|run| run.prompt_file.as_deref())
        {
            read_prompt_file(folder, path)
                .map_err(|e| format!("schedule {}: {e}", schedule.name))?;
        }
    }
    Ok(())
}
pub fn grants(config: &Config) -> Result<(), String> {
    let grants = std::env::var("BORGCHART_RUNS").unwrap_or_default();
    for schedule in &config.schedules {
        if let Some(run) = &schedule.run
            && !grants.split(',').any(|role| role == run.role)
        {
            return Err(format!(
                "schedule {}: add role `{}` to the timer's `runs` in template.yaml and restart the timer",
                schedule.name, run.role
            ));
        }
    }
    if config.schedules.iter().any(|s| s.run.is_none())
        && !matches(
            "[a-z][a-z0-9_-]{0,79}(?:/[a-z][a-z0-9_-]{0,79})*",
            &std::env::var("BORGCHART_DELIVER_TO").unwrap_or_default(),
        )
    {
        return Err("set delivers_to to a valid member ID for schedules".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[rustfmt::skip]
    fn schedule_entries_reject_mixed_unknown_and_duplicate_fields() {
        for bad in [
            "- {name: review, cron: '0 0 * * *', run: [printf]}",
            "- {name: review, cron: '0 0 * * *', every_minutes: 1}",
            "- {name: review, cron: '0 0 * * *', ignore: x}",
            "- {name: review, cron: '0 0 * * *', at: '00:00'}",
            "- {name: review, cron: '0 0 * * *', missed: all}",
            "- {name: review, cron: '0 0 * * *', missed: true}",
            "- {name: review, cron: 0}",
            "- {name: review, cron: '0 0 * * *', cron: '0 1 * * *'}",
            "- {name: review, cron: '0 0 * * *'}\n- {name: review, run: [printf], every_minutes: 1}",
            "- {name: review, cron: '0 0 * * *'}\n- {name: review, cron: '0 1 * * *'}",
            "- {name: review, missed: skip, run: [printf], every_minutes: 1}",
        ] { assert!(parse(bad).is_err(), "{bad}"); }
        assert!(parse("- {name: probe, run: [printf], every_minutes: 1}\n- {name: review, cron: '0 0 * * *'}").is_ok());
    }
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
