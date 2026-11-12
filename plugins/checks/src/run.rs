//! Sequential due checks and bounded status-only delivery.
use crate::{
    config::{self, Check},
    state::{Snapshot, one_line, update},
};
use borgchart::{
    bounded_process::{self, Error, Limits, Stream},
    plugin::{self, Outcome},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::process::ExitStatusExt,
    process::Command,
    time::{Duration, Instant},
};

pub fn notice(batch: &[String]) -> String {
    format!(
        "Checks: {} signal(s). Look with brg-checks last <name>.\n{}",
        batch.len(),
        batch.join("\n")
    )
}
fn deliver(state: &mut Snapshot, store: &plugin::State) -> Result<String, String> {
    if state.pending.is_empty() {
        return Ok(String::new());
    }
    let target = std::env::var("BORGCHART_DELIVER_TO").unwrap_or_default();
    if !config::matches("[a-z][a-z0-9_-]{0,79}(?:/[a-z][a-z0-9_-]{0,79})*", &target) {
        return Ok("set BORGCHART_DELIVER_TO to the intake member's ID".into());
    }
    let batch: Vec<_> = state.pending.drain(..state.pending.len().min(6)).collect();
    state
        .save(store)
        .map_err(|_| "cannot save plugin state; check permissions")?;
    Ok(
        match plugin::send("checks", &target, &notice(&batch), None) {
            Outcome::Committed { .. } => String::new(),
            Outcome::Uncertain { .. } => {
                state.uncertain_sends = state.uncertain_sends.saturating_add(1);
                "delivery uncertain; check brg before resending".into()
            }
            Outcome::Refused { message, .. } => {
                state.pending.splice(..0, batch);
                format!(
                    "delivery failed: {}; batch kept for the next tick",
                    one_line(&message)
                )
            }
        },
    )
}
fn collect(check: &Check, state: &mut Snapshot, timeout: Duration) {
    let last_run = borgchart::bus::time::utc_now();
    let execution = config::command_path(&check.run[0])
        .map_err(|_| Error::Io(std::io::ErrorKind::InvalidInput))
        .and_then(|program| {
            bounded_process::capture(
                Command::new(program).args(&check.run[1..]),
                Limits {
                    deadline: Instant::now() + timeout,
                    stdout_bytes: 16 << 20,
                    stderr_bytes: 1 << 20,
                },
            )
        });
    let (output, errors, code) = match execution {
        Ok(result) => (
            result.stdout,
            result.stderr,
            match result.status {
                Some(status) => json!(
                    status
                        .code()
                        .unwrap_or_else(|| -status.signal().unwrap_or(1))
                ),
                None => json!("timeout"),
            },
        ),
        Err(Error::OutputLimit(stream)) => {
            let (stream, cap) = match stream {
                Stream::Stdout => ("stdout", 16),
                Stream::Stderr => ("stderr", 1),
            };
            (
                Vec::new(),
                format!("{stream} exceeded {cap} MiB; reduce the check's output").into_bytes(),
                json!("output-limit"),
            )
        }
        Err(_) => (
            Vec::new(),
            b"cannot run command; check executable and permissions".to_vec(),
            json!(127),
        ),
    };
    let record = state.checks.entry(check.name.clone()).or_default();
    record.last_run = last_run;
    if code == json!(0) {
        record.last_success = borgchart::bus::time::utc_now();
    }
    state
        .pending
        .extend(update(record, check, &output, &errors, code));
}
pub fn run(store: &plugin::State, once: bool) -> Result<(), String> {
    let setup = || {
        let checks = config::load()?;
        let seconds: f64 = std::env::var("CHECKS_TIMEOUT_SECONDS")
            .unwrap_or("60".into())
            .parse()
            .unwrap_or(f64::NAN);
        let timeout = Duration::try_from_secs_f64(seconds)
            .ok()
            .filter(|d| !d.is_zero() && Instant::now().checked_add(*d).is_some());
        let timeout = timeout
            .ok_or("set CHECKS_TIMEOUT_SECONDS to a positive finite number (test hook only)")?;
        Ok::<_, String>((checks, timeout))
    };
    let (checks, timeout) = setup().inspect_err(|e| {
        let _ = Snapshot::read(store).health(store, &one_line(e));
    })?;
    let names: BTreeSet<_> = checks.iter().map(|c| c.name.clone()).collect();
    let mut due = BTreeMap::<String, (Instant, f64)>::new();
    loop {
        {
            let _lock = store
                .lock("checks.lock")
                .map_err(|_| "cannot lock plugin state; check permissions")?;
            let mut state = Snapshot::read(store);
            state.prune(&names);
            for check in &checks {
                if due
                    .get(&check.name)
                    .is_none_or(|(last, interval)| last.elapsed().as_secs_f64() >= interval * 60.0)
                {
                    collect(check, &mut state, timeout);
                    due.insert(check.name.clone(), (Instant::now(), check.every_minutes));
                    state
                        .save(store)
                        .and_then(|_| state.health(store, ""))
                        .map_err(|_| "cannot save plugin state; check permissions")?;
                }
            }
            state
                .save(store)
                .map_err(|_| "cannot save plugin state; check permissions")?;
            let detail = deliver(&mut state, store)?;
            state
                .save(store)
                .and_then(|_| state.health(store, &detail))
                .map_err(|_| "cannot save plugin state; check permissions")?;
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maximum_signal_batch_fits_the_locked_delivery_budget() {
        let batch: Vec<_> = (0..6).map(|i| format!("check-{i:026} recovered")).collect();
        let notice = notice(&batch);
        assert!(notice.len() <= 900);
        assert!(notice.lines().next().unwrap().len() <= 76);
        assert!(
            notice
                .lines()
                .map(|l| l.chars().count().div_ceil(78).max(1))
                .sum::<usize>()
                <= 8
        );
    }
}
