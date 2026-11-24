//! Persistent recurring wakes, coalescing and a rolling send-attempt cap.
use crate::{config::Config, cron::Cron, state::Snapshot};
use borgchart::bus::time;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

pub struct Schedule {
    pub name: String,
    pub cron: Cron,
    pub catch_up: bool,
    pub prompt: Option<String>,
    pub run: Option<Run>,
}
pub struct Run {
    pub role: String,
    pub timeout: Duration,
}
#[derive(Deserialize, Serialize)]
pub struct Record {
    pub cron: String,
    pub next: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub run: bool,
}
impl Record {
    pub fn valid(&self) -> bool {
        Cron::parse(&self.cron).is_ok_and(|cron| {
            let occurrence = |s: &str| time::parse_utc(s).filter(|t| cron.previous(*t) == Some(*t));
            let Some(next) = occurrence(&self.next) else {
                return false;
            };
            self.pending
                .as_ref()
                .is_none_or(|p| occurrence(p).is_some_and(|p| p < next))
        })
    }
}
fn prune_attempts(state: &mut Snapshot, now: SystemTime) {
    state.schedule_sends.retain(|s| {
        time::parse_utc(s).is_some_and(|t| {
            now.duration_since(t)
                .is_ok_and(|d| d < Duration::from_secs(3600))
        })
    });
}
pub fn prepare(
    state: &mut Snapshot,
    config: &Config,
    now: SystemTime,
    started: SystemTime,
) -> Result<(), String> {
    state
        .schedules
        .retain(|name, _| config.schedules.iter().any(|s| &s.name == name));
    prune_attempts(state, now);
    for schedule in &config.schedules {
        let next = || {
            schedule
                .cron
                .next(now)
                .map(time::format_utc)
                .ok_or("schedule has no future date; edit cron".to_owned())
        };
        let record = state.schedules.entry(schedule.name.clone());
        let run = schedule.run.is_some();
        let record = match record {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Record {
                    cron: schedule.cron.text.clone(),
                    next: next()?,
                    pending: None,
                    run,
                });
                continue;
            }
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        };
        if record.cron != schedule.cron.text || record.run != run {
            *record = Record {
                cron: schedule.cron.text.clone(),
                next: next()?,
                pending: None,
                run,
            };
        } else if time::parse_utc(&record.next).is_some_and(|t| t <= now) {
            if let Some(latest) = schedule.cron.previous(now)
                && (latest > started || schedule.catch_up)
            {
                record.pending = Some(time::format_utc(latest));
            }
            record.next = next()?;
        }
    }
    Ok(())
}
pub fn send(
    state: &mut Snapshot,
    config: &Config,
    mut clock: impl FnMut() -> SystemTime,
    mut save: impl FnMut(&Snapshot) -> Result<(), String>,
    mut deliver: impl FnMut(&str) -> Result<(), String>,
    mut run: impl FnMut(
        &mut Snapshot,
        &Schedule,
        SystemTime,
        SystemTime,
        &str,
    ) -> Result<String, String>,
) -> Result<String, String> {
    save(state)?;
    let mut pending: Vec<_> = state
        .schedules
        .iter()
        .filter_map(|(n, r)| {
            r.pending
                .as_ref()
                .and_then(|p| time::parse_utc(p))
                .map(|p| (p, n.clone()))
        })
        .collect();
    pending.sort();
    let mut failures = Vec::new();
    for (occurrence, name) in pending {
        let schedule = config.schedules.iter().find(|s| s.name == name).unwrap();
        let now = clock();
        prune_attempts(state, now);
        let mut text = format!("{name} {}Z", &time::format_utc(occurrence)[..16]);
        if let Some(prompt) = &schedule.prompt {
            text.push('\n');
            text.push_str(prompt);
        }
        if schedule.run.is_some() {
            let detail = run(state, schedule, now, occurrence, &text)?;
            if !detail.is_empty() {
                failures.push(detail);
            }
            continue;
        }
        if state.schedule_sends.len() >= 6 {
            failures.push(format!(
                "schedule cap: 6 sends in the last hour; waiting: {name}"
            ));
            continue;
        }
        state.schedule_sends.push(time::format_utc(now));
        save(state)?;
        match deliver(&text) {
            Ok(()) => {
                state.schedules.get_mut(&name).unwrap().pending = None;
                save(state)?;
            }
            Err(e) => failures.push(format!(
                "schedule {name} delivery failed: {e}; kept for the next tick"
            )),
        }
    }
    Ok(failures.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use std::cell::RefCell;
    fn at(hour: u32, minute: u32) -> SystemTime {
        time::parse_utc(&format!("2024-01-01T{hour:02}:{minute:02}:00Z")).unwrap()
    }
    struct Timer {
        state: Snapshot,
        config: Config,
        started: SystemTime,
    }
    impl Timer {
        fn new(missed: &str) -> Self {
            Self {
                state: Snapshot::default(),
                config: config::parse(&format!(
                    "- {{name: review, cron: '0 * * * *', missed: {missed}}}"
                ))
                .unwrap(),
                started: at(0, 0),
            }
        }
        fn tick(&mut self, now: SystemTime, success: bool) -> (Vec<String>, String) {
            prepare(&mut self.state, &self.config, now, self.started).unwrap();
            let saved = RefCell::new(Vec::new());
            let mut messages = Vec::new();
            let detail = send(
                &mut self.state,
                &self.config,
                || now,
                |s| {
                    saved.borrow_mut().push(serde_json::to_value(s).unwrap());
                    Ok(())
                },
                |body| {
                    let snapshots = saved.borrow();
                    let snapshot = snapshots.last().unwrap();
                    let name = body.split_once(' ').unwrap().0;
                    assert!(snapshot["schedules"][name]["pending"].is_string());
                    assert!(!snapshot["schedule_sends"].as_array().unwrap().is_empty());
                    messages.push(body.to_owned());
                    if success {
                        Ok(())
                    } else {
                        Err("bus unavailable".into())
                    }
                },
                |_, _, _, _, _| unreachable!(),
            )
            .unwrap();
            (messages, detail)
        }
    }
    #[test]
    fn prompt_only_edit_preserves_next_occurrence_without_refiring() {
        let mut t = Timer::new("skip");
        t.tick(at(0, 0), true);
        t.tick(at(1, 0), true);
        t.config =
            config::parse("- {name: review, cron: '0 * * * *', prompt: Review releases}").unwrap();
        assert!(t.tick(at(1, 30), true).0.is_empty());
        t.config.schedules[0].prompt = Some("Review releases now".into());
        assert_eq!(
            t.tick(at(2, 0), true).0,
            ["review 2024-01-01T02:00Z\nReview releases now"]
        );
    }
    #[test]
    fn new_or_edited_schedule_never_fires_retroactively() {
        let mut t = Timer::new("catch-up");
        assert!(t.tick(at(12, 15), true).0.is_empty());
        assert_eq!(t.tick(at(13, 0), true).0, ["review 2024-01-01T13:00Z"]);
        t.config = config::parse("- {name: review, cron: '30 * * * *'}").unwrap();
        assert!(t.tick(at(14, 15), true).0.is_empty());
        assert_eq!(t.tick(at(14, 30), true).0, ["review 2024-01-01T14:30Z"]);
        t.config = config::parse("- {name: review, cron: '30  *\t* * *'}").unwrap();
        assert_eq!(t.tick(at(15, 30), true).0, ["review 2024-01-01T15:30Z"]);
    }
    #[test]
    fn running_process_sends_passed_occurrences_once_as_the_latest() {
        let mut t = Timer::new("skip");
        t.tick(at(0, 0), true);
        assert_eq!(t.tick(at(12, 15), true).0, ["review 2024-01-01T12:00Z"]);
        assert!(t.tick(at(12, 15), true).0.is_empty());
    }
    #[test]
    fn downtime_skip_sends_nothing_and_catch_up_sends_the_latest_once() {
        for missed in ["skip", "catch-up"] {
            let mut t = Timer::new(missed);
            t.tick(at(0, 0), true);
            t.started = at(12, 0);
            let sent = t.tick(at(12, 15), true).0;
            assert_eq!(
                sent,
                if missed == "skip" {
                    vec![]
                } else {
                    vec!["review 2024-01-01T12:00Z"]
                }
            );
            assert!(t.tick(at(12, 15), true).0.is_empty());
            assert_eq!(t.tick(at(13, 0), true).0, ["review 2024-01-01T13:00Z"]);
        }
    }
    #[test]
    fn clock_set_back_never_repeats_an_occurrence() {
        let mut t = Timer::new("catch-up");
        t.tick(at(0, 0), true);
        assert_eq!(t.tick(at(12, 0), true).0.len(), 1);
        assert!(t.tick(at(10, 0), true).0.is_empty());
        assert!(t.state.schedule_sends.is_empty());
        assert!(t.tick(at(12, 0), true).0.is_empty());
        assert_eq!(t.tick(at(13, 0), true).0, ["review 2024-01-01T13:00Z"]);
    }
    #[test]
    fn pending_survives_restart_and_is_sent_whatever_missed_says() {
        let mut t = Timer::new("skip");
        t.tick(at(0, 0), true);
        assert_eq!(t.tick(at(1, 0), false).0.len(), 1);
        let persisted = serde_json::to_value(&t.state).unwrap();
        t.state.schedules = serde_json::from_value(persisted["schedules"].clone()).unwrap();
        t.started = at(1, 30);
        assert_eq!(t.tick(at(1, 30), true).0, ["review 2024-01-01T01:00Z"]);
        assert!(t.tick(at(1, 30), true).0.is_empty());
    }
    #[test]
    fn failed_send_keeps_pending_and_coalesces_to_the_latest() {
        let mut t = Timer::new("skip");
        t.tick(at(0, 0), true);
        let (sent, error) = t.tick(at(1, 0), false);
        assert_eq!(sent, ["review 2024-01-01T01:00Z"]);
        assert!(error.contains("bus unavailable"));
        assert_eq!(t.tick(at(3, 30), true).0, ["review 2024-01-01T03:00Z"]);
        assert!(t.tick(at(3, 30), true).0.is_empty());
    }
    #[test]
    fn slow_sends_use_attempt_times_when_capacity_expires_between_deliveries() {
        let mut t = Timer::new("skip");
        t.config =
            config::parse("- {name: alpha, cron: '0 * * * *'}\n- {name: beta, cron: '0 * * * *'}")
                .unwrap();
        t.tick(at(0, 0), true);
        t.tick(at(1, 0), false);
        t.state.schedule_sends = vec![time::format_utc(at(1, 0)); 5];
        let clock = std::cell::Cell::new(at(1, 59));
        let mut sent = 0;
        send(
            &mut t.state,
            &t.config,
            || clock.get(),
            |_| Ok(()),
            |_| {
                sent += 1;
                clock.set(at(2, 0));
                Ok(())
            },
            |_, _, _, _, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(sent, 2);
        assert_eq!(
            t.state.schedule_sends,
            [time::format_utc(at(1, 59)), time::format_utc(at(2, 0))]
        );
    }
    #[test]
    fn largest_single_schedule_never_reaches_the_cap_with_unaligned_ticks() {
        let mut t = Timer::new("skip");
        t.config = config::parse("- {name: review, cron: '0,12,24,36,48 * * * *'}").unwrap();
        let start = at(0, 0);
        let tick = Duration::from_millis(30_700);
        let end = start + Duration::from_secs(48 * 3600);
        let mut now = start + Duration::from_secs(17);
        let mut expected = start + Duration::from_secs(12 * 60);
        while now <= end + tick {
            let (sent, detail) = t.tick(now, true);
            assert!(!detail.contains("schedule cap"), "{detail}");
            for body in sent {
                assert_eq!(
                    body,
                    format!("review {}Z", &time::format_utc(expected)[..16])
                );
                assert!(now >= expected && now <= expected + tick);
                expected += Duration::from_secs(12 * 60);
            }
            now += tick;
        }
        assert_eq!(expected, end + Duration::from_secs(12 * 60));
    }
    #[test]
    fn cap_counts_attempts_releases_in_order_and_expires_after_an_hour() {
        let mut t = Timer::new("skip");
        t.config = config::parse(
            &(0..8)
                .rev()
                .map(|i| {
                    format!(
                        "- {{name: review-{i}, cron: '{} * * * *'}}\n",
                        if i == 7 { 30 } else { 0 }
                    )
                })
                .collect::<String>(),
        )
        .unwrap();
        t.tick(at(0, 0), true);
        let (failed, error) = t.tick(at(1, 0), false);
        assert_eq!(failed.len(), 6);
        assert!(failed[0].starts_with("review-7 2024-01-01T00:30"));
        assert!(failed[1].starts_with("review-0 2024-01-01T01:00"));
        assert!(error.contains("schedule cap"));
        assert!(t.tick(at(1, 59), true).0.is_empty());
        t.started = at(1, 59);
        let (sent, detail) = t.tick(at(2, 0), true);
        assert_eq!(sent.len(), 6);
        assert!(sent[0].starts_with("review-7 2024-01-01T01:30"));
        assert!(sent[1].starts_with("review-0 2024-01-01T02:00"));
        assert!(detail.contains("schedule cap"));
        assert!(t.tick(at(2, 1), true).0.is_empty());
    }
}
