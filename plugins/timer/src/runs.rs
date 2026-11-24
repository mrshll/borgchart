//! Scheduled member starts, completion and recovery.
use crate::{
    config,
    schedule::Schedule,
    state::{Snapshot, one_line},
};
use borgchart::{
    bounded_process::{self, Limits},
    bus::time,
    plugin,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

#[derive(Deserialize, Serialize)]
pub struct Record {
    pub member: String,
    role: String,
    occurrence: Option<String>,
    start_id: Option<String>,
    prompted: bool,
    started: String,
    pub deadline: String,
    ending: Option<String>,
}

fn call(command: &mut Command, seconds: u64) -> Result<Vec<u8>, String> {
    let result = bounded_process::run(
        command,
        None,
        Limits {
            deadline: Instant::now() + Duration::from_secs(seconds),
            stdout_bytes: 4 << 20,
            stderr_bytes: 64 << 10,
        },
    )
    .map_err(|e| e.to_string())?;
    if result.status.success() {
        return Ok(result.stdout);
    }
    let reason = [&result.stderr, &result.stdout]
        .into_iter()
        .find_map(|bytes| {
            let text = String::from_utf8_lossy(bytes);
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(one_line)
        });
    Err(reason.unwrap_or_else(|| result.status.to_string()))
}

fn brg(args: &[&str], seconds: u64) -> Result<Vec<u8>, String> {
    call(
        Command::new(std::env::var_os("BORGCHART_BIN").unwrap_or("brg".into())).args(args),
        seconds,
    )
}
fn status(team: &str) -> Result<Vec<Value>, String> {
    let mut rows: Vec<Value> = serde_json::from_slice(&brg(&["status", team, "--json"], 30)?)
        .map_err(|_| "cannot read brg status JSON")?;
    rows.retain(|row| {
        row["team"] == team
            || row["kind"] == "team"
                && row["team"]
                    .as_str()
                    .and_then(|id| id.rsplit_once('/'))
                    .is_some_and(|(parent, _)| parent == team)
    });
    Ok(rows)
}
fn member<'a>(rows: &'a [Value], id: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| row["kind"] == "member" && row["name"] == id)
}
fn role_status<'a>(rows: &'a [Value], state: &Snapshot, role: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| row["kind"] == "role" && row["name"] == role)
        .or_else(|| {
            state
                .runs
                .values()
                .filter(|run| run.role == role)
                .find_map(|run| {
                    member(rows, &run.member).filter(|row| {
                        run.start_id
                            .as_deref()
                            .is_some_and(|id| row["start_id"] == id)
                    })
                })
        })
}
fn seat(choice: Option<&Value>) -> Result<(), String> {
    let Some(program) = std::env::var_os("SEAT").filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let (vendor, model) = choice
        .and_then(|row| Some((row["vendor"].as_str()?, row["model"].as_str()?)))
        .ok_or("cannot read the role's vendor and model")?;
    call(Command::new(program).args(["route", vendor, model]), 30).map(|_| ())
}

fn name(schedule: &str, role: &str, occurrence: SystemTime) -> String {
    let mut minute = occurrence
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60;
    let mut encoded = Vec::new();
    loop {
        encoded.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(minute % 36) as usize]);
        minute /= 36;
        if minute == 0 {
            break;
        }
    }
    encoded.reverse();
    let stamp = std::str::from_utf8(&encoded).unwrap();
    let limit = borgchart::registry::NAME_MAX.min(39 - role.len());
    let hash = format!("{:x}", Sha256::digest(schedule.as_bytes()));
    let suffix = format!(
        "{}{}",
        &hash[..(limit.saturating_sub(stamp.len())).min(4)],
        stamp
    );
    let prefix = schedule[..schedule.len().min(limit.saturating_sub(suffix.len() + 1))]
        .split('-')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if prefix.is_empty() {
        suffix
    } else {
        format!("{prefix}-{suffix}")
    }
}

fn outcome(kind: &str, reason: &str) -> String {
    let at = time::utc_now();
    if reason.is_empty() {
        format!("{kind} {at}")
    } else {
        format!("{kind} {at}: {reason}")
    }
}
fn save(state: &Snapshot, store: &plugin::State) -> Result<(), String> {
    state
        .save(store)
        .map_err(|_| "cannot save scheduled run state; check permissions".into())
}

pub fn start(
    store: &plugin::State,
    state: &mut Snapshot,
    schedule: &Schedule,
    now: SystemTime,
    occurrence: SystemTime,
    mut body: String,
    folder: &Path,
) -> Result<String, String> {
    let key = &schedule.name;
    state.schedules.get_mut(key).unwrap().pending = None;
    if let Some(path) = schedule.run.as_ref().unwrap().prompt_file.as_deref() {
        match config::read_prompt_file(folder, path) {
            Ok(prompt) => {
                body.push('\n');
                body.push_str(&prompt);
            }
            Err(reason) => {
                state
                    .outcomes
                    .insert(key.clone(), outcome("failed-to-start", &reason));
                save(state, store)?;
                return Ok(String::new());
            }
        }
    }
    let skip = if state.runs.contains_key(key) {
        Some("previous run still going")
    } else if state.schedule_sends.len() >= 6 {
        Some("schedule cap")
    } else {
        None
    };
    if let Some(reason) = skip {
        state
            .outcomes
            .insert(key.clone(), outcome("skipped", reason));
        save(state, store)?;
        return Ok(String::new());
    }
    state.schedule_sends.push(time::format_utc(now));
    save(state, store)?;
    let team = std::env::var("BORGCHART_TEAM").unwrap_or_default();
    if let Err(reason) = launch(store, state, schedule, occurrence, &body, &team) {
        let line = outcome("failed-to-start", &reason);
        if let Some(run) = state.runs.get_mut(key) {
            run.ending = Some(line);
            save(state, store)?;
            let result = status(&team).and_then(|rows| finish(store, state, key, &team, &rows));
            return Ok(result
                .err()
                .map_or_else(String::new, |e| cleanup_error(key, &e)));
        } else {
            state.outcomes.insert(key.clone(), line);
        }
    }
    save(state, store)?;
    Ok(String::new())
}

fn launch(
    store: &plugin::State,
    state: &mut Snapshot,
    schedule: &Schedule,
    occurrence: SystemTime,
    body: &str,
    team: &str,
) -> Result<(), String> {
    let spec = schedule.run.as_ref().unwrap();
    let key = &schedule.name;
    let rows = status(team)?;
    let name = name(key, &spec.role, occurrence);
    let id = format!("{}-{name}", spec.role);
    if rows.iter().any(|row| row["name"] == id) {
        return Err(format!(
            "{id} already exists; choose a different schedule name"
        ));
    }
    let role = role_status(&rows, state, &spec.role);
    if role.is_some_and(|row| row["vendor"] == "claude" && row["restricted_access"] == true)
        && (body.lines().count() > 2 || body.len() > 700)
    {
        return Err(
            "prompt exceeds the restricted Claude limit; shorten the prompt or use an unrestricted role"
                .into(),
        );
    }
    if let Err(reason) = seat(role) {
        state
            .outcomes
            .insert(key.clone(), outcome("skipped", &format!("seat: {reason}")));
        return Ok(());
    }
    let now = SystemTime::now();
    state.runs.insert(
        key.clone(),
        Record {
            member: id.clone(),
            role: spec.role.clone(),
            occurrence: Some(time::format_utc(occurrence)),
            start_id: None,
            prompted: false,
            started: time::format_utc(now),
            deadline: time::format_utc(now + spec.timeout),
            ending: None,
        },
    );
    save(state, store)?;
    brg(
        &[
            "create",
            &format!("{team}/{}", spec.role),
            "--name",
            &name,
            "--run",
            key,
        ],
        120,
    )?;
    let rows = status(team)?;
    let start_id = member(&rows, &id)
        .and_then(|row| row["start_id"].as_str())
        .filter(|s| !s.is_empty())
        .ok_or("cannot read the member's start ID")?;
    state.runs.get_mut(key).unwrap().start_id = Some(start_id.into());
    save(state, store)?;
    plugin::send("timer", &format!("{team}/{id}"), body, None)?;
    state.runs.get_mut(key).unwrap().prompted = true;
    Ok(())
}

fn marker(member: &str) -> String {
    format!("done-{member}.json")
}

pub fn done() -> Result<String, (i32, String)> {
    let refused = || {
        (
            2,
            "brg-timer done is for the current start of a scheduled run".into(),
        )
    };
    if std::env::var_os("BORGCHART_STATE_DIR").is_none_or(|path| !PathBuf::from(path).is_dir()) {
        return Err(refused());
    }
    let store = plugin::State::from_env().map_err(|e| (1, e.to_string()))?;
    let caller = std::env::var("BORGCHART_MEMBER").unwrap_or_default();
    let start = std::env::var("BORGCHART_INCARNATION").unwrap_or_default();
    let state = Snapshot::read(&store);
    let (name, _) = state
        .runs
        .iter()
        .find(|(_, run)| {
            !start.is_empty()
                && run.member == caller
                && run.start_id.as_deref() == Some(&start)
                && run.ending.is_none()
                && borgchart::names::is_valid_member_id(&caller)
        })
        .ok_or_else(refused)?;
    store
        .write_json(&marker(&caller), &json!({"start_id": start}))
        .map_err(|_| (1, "cannot mark the run done; check permissions".into()))?;
    Ok(format!(
        "run {name} finished; the timer will stop and remove you"
    ))
}

pub fn reap(store: &plugin::State, state: &mut Snapshot) -> String {
    if state.runs.is_empty() {
        return String::new();
    }
    let team = std::env::var("BORGCHART_TEAM").unwrap_or_default();
    let rows = match status(&team) {
        Ok(rows) => rows,
        Err(e) => return format!("cannot check scheduled runs: {e}"),
    };
    let mut failures = Vec::new();
    for key in state.runs.keys().cloned().collect::<Vec<_>>() {
        let result = finish(store, state, &key, &team, &rows);
        if let Err(e) = result {
            failures.push(cleanup_error(&key, &e));
        }
    }
    failures.join("; ")
}

fn cleanup_error(key: &str, reason: &str) -> String {
    format!("{key} cleanup failed: {reason}; retrying next tick")
}

fn finish(
    store: &plugin::State,
    state: &mut Snapshot,
    key: &str,
    team: &str,
    rows: &[Value],
) -> Result<(), String> {
    let run = &state.runs[key];
    let grants = std::env::var("BORGCHART_RUNS").unwrap_or_default();
    let occurrence = run.occurrence.as_deref().and_then(time::parse_utc);
    if !grants.split(',').any(|role| role == run.role)
        || !borgchart::names::is_valid_id(&run.role)
        || !crate::config::valid_name(key)
        || !occurrence
            .is_some_and(|at| run.member == format!("{}-{}", run.role, name(key, &run.role, at)))
    {
        return Err(
            "run record does not name a granted scheduled member; check timer state".into(),
        );
    }
    let row = member(rows, &run.member);
    if row.is_none()
        && run.start_id.is_none()
        && run.ending.is_none()
        && time::parse_utc(&run.started)
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(120))
    {
        return Err("waiting for an interrupted create to settle".into());
    }
    let file = marker(&run.member);
    let done: Value = store.read_json(&file);
    let start = run.start_id.as_deref();
    let marked_done = run.prompted && start.is_some_and(|id| done["start_id"] == id);
    let same_start = start.is_some_and(|id| row.is_some_and(|r| r["start_id"] == id));
    let liveness = row.and_then(|r| r["liveness"].as_str());
    if !marked_done && same_start && matches!(liveness, None | Some("unknown")) {
        return Err(format!("cannot tell whether {} is running", run.member));
    }
    let kind = if marked_done {
        "done"
    } else if !run.prompted
        || !same_start
        || liveness != Some("alive")
        || row.is_some_and(|r| matches!(r["state"].as_str(), Some("stopped" | "exited")))
    {
        "interrupted"
    } else if time::parse_utc(&run.deadline).is_some_and(|t| t <= SystemTime::now()) {
        "timed-out"
    } else {
        ""
    };
    if run.ending.is_none() && kind.is_empty() {
        return Ok(());
    }
    let id = run.member.clone();
    if run.ending.is_none() {
        state.runs.get_mut(key).unwrap().ending = Some(outcome(kind, ""));
        save(state, store)?;
    }
    if row.is_none() && rows.iter().any(|r| r["kind"] == "team" && r["name"] == id) {
        return Err("run ID belongs to a child team; check timer state".into());
    }
    if row.is_some() {
        let target = format!("{team}/{id}");
        brg(&["stop", &target], 30)?;
        brg(&["delete", &target, "--force"], 30)?;
    }
    let path = PathBuf::from(std::env::var_os("BORGCHART_STATE_DIR").unwrap()).join(file);
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove done marker: {e}")),
    }
    let run = state.runs.remove(key).unwrap();
    state.outcomes.insert(key.into(), run.ending.unwrap());
    save(state, store)
}
