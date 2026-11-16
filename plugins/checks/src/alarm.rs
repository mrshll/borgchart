//! One persistent check-in alarm per member, delivered by the checks process.
use crate::config;
use borgchart::{bus::time, plugin};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::{Duration, SystemTime},
};

#[derive(Subcommand)]
pub enum Action {
    /// Replace your alarm: 1–10080 minutes or YYYY-MM-DDTHH:MMZ, and a short note
    Set { when: String, note: String },
    /// Show your next alarm
    Show,
    /// Remove your alarm
    Clear,
}
#[derive(Deserialize, Serialize)]
struct Alarm {
    team: String,
    member: String,
    due: String,
    note: String,
    set_at: String,
}
fn folder() -> Result<PathBuf, String> {
    let path =
        PathBuf::from(std::env::var_os("BORGCHART_STATE_DIR").ok_or("run this through brg")?)
            .join("alarms");
    fs::create_dir_all(&path).map_err(storage)?;
    Ok(path)
}
fn storage(error: io::Error) -> String {
    format!("cannot access alarms: {error}; check state permissions")
}
fn due(when: &str, now: SystemTime) -> Result<SystemTime, String> {
    let parsed = if !when.is_empty() && when.bytes().all(|b| b.is_ascii_digit()) {
        when.parse::<u64>()
            .ok()
            .filter(|m| (1..=10080).contains(m))
            .and_then(|m| now.checked_add(Duration::from_secs(m * 60)))
    } else {
        when.strip_suffix('Z')
            .filter(|s| s.len() == 16)
            .and_then(|s| time::parse_utc(&format!("{s}:00Z")))
    };
    parsed
        .filter(|t| {
            t.duration_since(now)
                .is_ok_and(|d| !d.is_zero() && d <= Duration::from_secs(604800))
        })
        .ok_or_else(|| {
            "use 1–10080 whole minutes or YYYY-MM-DDTHH:MMZ within the next 7 days".into()
        })
}
pub fn action(store: &plugin::State, action: Action) -> Result<(), String> {
    let team = std::env::var("BORGCHART_TEAM").unwrap_or_default();
    let member = std::env::var("BORGCHART_MEMBER").unwrap_or_default();
    if !config::matches("[a-z][a-z0-9_-]{0,79}(?:/[a-z][a-z0-9_-]{0,79})*", &team)
        || !config::matches("[a-z][a-z0-9_-]{0,79}", &member)
    {
        return Err(
            "run this through brg with valid BORGCHART_TEAM and BORGCHART_MEMBER IDs".into(),
        );
    }
    let _lock = store.lock_named("alarms.lock").map_err(storage)?;
    let path = folder()?.join(format!("{member}.json"));
    match action {
        Action::Set { when, note } => {
            let now = SystemTime::now();
            let due = time::format_utc(due(&when, now)?);
            if note.trim().is_empty() || note.len() > 200 || note.chars().any(char::is_control) {
                return Err(
                    "use one non-empty note of at most 200 bytes without control characters".into(),
                );
            }
            let health: serde_json::Value = store.read_json("health.json");
            let age = health["at"]
                .as_str()
                .and_then(time::parse_utc)
                .and_then(|t| now.duration_since(t).ok());
            match age {
                Some(age) if age <= Duration::from_secs(300) => (),
                Some(age) => return Err(format!("the checks process has not reported for {}s, so no alarm can fire; tell the owner", age.as_secs())),
                None => return Err("the checks process has no current heartbeat, so no alarm can fire; tell the owner".into()),
            }
            let alarm = Alarm {
                team,
                member,
                due: due.clone(),
                note,
                set_at: time::format_utc(now),
            };
            let tmp = path.with_extension("tmp");
            let file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(storage)?;
            serde_json::to_writer(&file, &alarm).map_err(|e| storage(e.into()))?;
            file.sync_all().map_err(storage)?;
            fs::rename(tmp, path).map_err(storage)?;
            println!("alarm set for {due}");
        }
        Action::Show => match fs::read(&path) {
            Ok(bytes) => {
                let alarm: Alarm = serde_json::from_slice(&bytes).map_err(|e| storage(e.into()))?;
                println!("alarm {}: {}", alarm.due, plugin::escape(&alarm.note));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => println!("no alarm"),
            Err(e) => return Err(storage(e)),
        },
        Action::Clear => match fs::remove_file(path) {
            Ok(()) => println!("alarm cleared"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => println!("no alarm"),
            Err(e) => return Err(storage(e)),
        },
    }
    Ok(())
}
pub fn fire(store: &plugin::State) -> Result<(), String> {
    let mut failures = Vec::new();
    let alarms = {
        let _lock = store.lock_named("alarms.lock").map_err(storage)?;
        fs::read_dir(folder()?)
            .map_err(storage)?
            .map(|entry| {
                let path = entry.map_err(storage)?.path();
                if path.extension().is_none_or(|e| e != "json") {
                    return Ok(None);
                }
                let read = fs::read(&path).map_err(storage).and_then(|bytes| {
                    serde_json::from_slice::<Alarm>(&bytes)
                        .map(|alarm| (bytes, alarm))
                        .map_err(|e| storage(e.into()))
                });
                let (bytes, alarm) = match read {
                    Ok(alarm) => alarm,
                    Err(e) => {
                        failures.push(format!("alarm {} skipped: {e}", path.display()));
                        return Ok(None);
                    }
                };
                Ok(time::parse_utc(&alarm.due)
                    .filter(|t| *t <= SystemTime::now())
                    .map(|_| (path, bytes, alarm)))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    for (path, bytes, alarm) in alarms.into_iter().flatten() {
        let text = format!(
            "Alarm due {}: {}. Check your open briefs and retries.",
            alarm.due,
            plugin::escape(&alarm.note)
        );
        match plugin::send(
            "alarm",
            &format!("{}/{}", alarm.team, alarm.member),
            &text,
            None,
        ) {
            Ok(_) => {
                let _lock = store.lock_named("alarms.lock").map_err(storage)?;
                if fs::read(&path).ok().as_ref() == Some(&bytes) {
                    fs::remove_file(path).map_err(storage)?;
                }
            }
            Err(e) => failures.push(format!(
                "alarm {}/{} delivery failed: {e}; kept for the next tick",
                alarm.team, alarm.member
            )),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
