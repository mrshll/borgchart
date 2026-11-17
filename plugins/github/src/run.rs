//! One guarded polling session, health reporting and the processing seam.
use crate::{
    config::{self, Guard, Repositories},
    discover::{self, Changed},
    gh::{Api, Error, Gh},
    state::Snapshot,
};
use borgchart::{bus::time::format_utc, plugin};
use std::time::{Duration, SystemTime};

pub struct Poll {
    pub processed: usize,
    pub errors: Vec<String>,
}
pub struct Session<A: Api> {
    api: A,
    owner: String,
    guard: Option<Guard>,
    repositories: Repositories,
    pub snapshot: Option<Snapshot>,
    previous: Option<Snapshot>,
    epoch: SystemTime,
    paused_until: Option<SystemTime>,
    first_health: bool,
}
impl<A: Api> Session<A> {
    pub fn new(api: A, store: &plugin::State, epoch: SystemTime) -> Result<Self, String> {
        // Check state before any API request, and never write over unreadable state.
        let previous = Snapshot::read(store)?;
        Ok(Self::configured(
            api,
            previous,
            epoch,
            config::owner().map_err(|e| e.to_string())?,
            Repositories::new(config::path()),
        ))
    }
    fn configured(
        api: A,
        previous: Option<Snapshot>,
        epoch: SystemTime,
        owner: String,
        repositories: Repositories,
    ) -> Self {
        Self {
            api,
            owner,
            guard: None,
            repositories,
            snapshot: None,
            previous,
            epoch,
            paused_until: None,
            first_health: true,
        }
    }
    pub fn poll(&mut self, now: SystemTime) -> Result<Poll, Error> {
        if let Some(reset) = self.paused_until {
            if now < reset {
                return Err(Error::RateLimited(reset));
            }
            self.paused_until = None;
        }
        let result = self.collect(now);
        if let Err(Error::RateLimited(reset)) = &result {
            self.paused_until = Some(*reset);
        }
        result
    }
    fn collect(&mut self, now: SystemTime) -> Result<Poll, Error> {
        let first = self.guard.is_none();
        if self.guard.is_none() {
            let guard = Guard::start(&self.api, &self.owner, now)?;
            self.snapshot = Some(Snapshot::start(
                self.previous.take(),
                self.epoch,
                guard.machine_id,
                guard.owner_id,
            ));
            self.guard = Some(guard);
        }
        self.guard.as_mut().unwrap().check(&self.api, now)?;
        let entries = self.repositories.load().map_err(Error::Other)?;
        let snapshot = self.snapshot.as_mut().unwrap();
        snapshot.entries(entries, if first { self.epoch } else { now });
        let mut poll = Poll {
            processed: 0,
            errors: Vec::new(),
        };
        for entry in entries {
            if poll.processed == discover::POLL_CAP {
                break;
            }
            let cursor = snapshot.cursors.get_mut(&entry.key()).unwrap();
            match discover::fetch(
                &self.api,
                entry,
                cursor,
                discover::POLL_CAP - poll.processed,
            ) {
                Ok(batch) => {
                    for item in &batch.items {
                        process_item(item)?;
                        cursor.mark(item);
                        poll.processed += 1;
                    }
                    if batch.exhausted {
                        cursor.at = cursor.at.max(now);
                        // A drained pass starts the overlap again. The timeline
                        // stage must see repeats even if updated_at stayed equal.
                        cursor.processed.clear();
                    }
                }
                Err(Error::RateLimited(reset)) => return Err(Error::RateLimited(reset)),
                Err(Error::NotFound) if entry.is_org() => poll.errors.push(format!(
                    "{} is not an organisation; list its repositories one by one",
                    entry.key()
                )),
                Err(error) => poll.errors.push(format!("{}: {error}", entry.key())),
            }
        }
        snapshot.prune(now);
        Ok(poll)
    }
    pub fn tick(&mut self, store: &plugin::State, now: SystemTime) -> Result<(), String> {
        let result = self.poll(now);
        if let Some(snapshot) = &self.snapshot {
            snapshot.save(store)?;
        }
        let (ok, mut detail) = match result {
            Ok(poll) if poll.errors.is_empty() => (
                true,
                format!("{} changed item(s) processed", poll.processed),
            ),
            Ok(poll) => (false, poll.errors.join("; ")),
            Err(error) => (false, error.to_string()),
        };
        if self.first_health {
            detail.push_str(&format!(
                "; activity before {} is not delivered",
                format_utc(self.epoch)
            ));
            self.first_health = false;
        }
        store.health(ok, &detail).map_err(|_| {
            "cannot write GitHub health; check state directory permissions".to_string()
        })
    }
}
/// Replace this function with timeline admission in the next stage. Discovery
/// advances the cursor only after this returns successfully for the item.
fn process_item(_item: &Changed) -> Result<(), Error> {
    Ok(())
}

pub fn run(store: &plugin::State, once: bool) -> Result<(), String> {
    let result: Result<(), String> = (|| {
        let _lock = store
            .lock()
            .map_err(|_| "cannot lock GitHub state; check directory permissions".to_string())?;
        let mut session = Session::new(Gh, store, SystemTime::now())?;
        loop {
            session.tick(store, SystemTime::now())?;
            if once {
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    })();
    if let Err(error) = &result {
        let _ = store.health(false, error);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Entry, gh::Rest, state::Cursor};
    use borgchart::bus::time::{format_utc, parse_utc};
    use serde_json::{Value, json};
    use std::{cell::RefCell, collections::BTreeMap, fs, rc::Rc};

    struct Fake {
        machine: u64,
        owner: u64,
        calls: Vec<String>,
        pages: BTreeMap<String, Vec<Value>>,
        failure: Option<(String, Error)>,
    }
    #[derive(Clone)]
    struct ApiFixture(Rc<RefCell<Fake>>);
    impl ApiFixture {
        fn new() -> Self {
            Self(Rc::new(RefCell::new(Fake {
                machine: 101,
                owner: 202,
                calls: Vec::new(),
                pages: BTreeMap::new(),
                failure: None,
            })))
        }
        fn pages(&self, entry: &str, values: Vec<Value>) {
            self.0.borrow_mut().pages.insert(entry.into(), values);
        }
        fn fail(&self, path: &str, error: Error) {
            self.0.borrow_mut().failure = Some((path.into(), error));
        }
        fn calls(&self) -> Vec<String> {
            self.0.borrow().calls.clone()
        }
    }
    impl Api for ApiFixture {
        fn get(&self, request: Rest<'_>) -> Result<Value, Error> {
            let path = request.path()?;
            let mut fake = self.0.borrow_mut();
            fake.calls.push(path.clone());
            if let Some((prefix, error)) = &fake.failure
                && path.starts_with(prefix)
            {
                return Err(error.clone());
            }
            Ok(match request {
                Rest::Machine => json!({"id":fake.machine,"login":"same-name"}),
                Rest::Owner(_) => json!({"id":fake.owner,"login":"same-name"}),
                Rest::Issues { entry, page, .. } => fake
                    .pages
                    .get(&entry.key())
                    .and_then(|pages| pages.get(page as usize - 1))
                    .cloned()
                    .unwrap_or(json!([])),
            })
        }
    }
    fn now() -> SystemTime {
        parse_utc("2026-01-02T12:00:00Z").unwrap()
    }
    fn item(number: u64, updated_at: SystemTime) -> Value {
        json!({"node_id":format!("I_{number}"),"number":number,"updated_at":format_utc(updated_at),"repository":{"full_name":"example-org/widgets"}})
    }
    fn setup(entries: &str) -> (tempfile::TempDir, ApiFixture, Session<ApiFixture>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github.yaml");
        fs::write(&path, entries).unwrap();
        let api = ApiFixture::new();
        let session = Session::configured(
            api.clone(),
            None,
            now(),
            "example-owner".into(),
            Repositories::new(path),
        );
        (dir, api, session)
    }
    fn cursor(session: &Session<ApiFixture>, entry: &str) -> SystemTime {
        session.snapshot.as_ref().unwrap().cursors[entry].at
    }

    #[test]
    fn numeric_identity_guard_refuses_owner_and_latches_a_machine_change() {
        let api = ApiFixture::new();
        api.0.borrow_mut().machine = 202;
        assert!(
            Guard::start(&api, "example-owner", now())
                .err()
                .unwrap()
                .to_string()
                .contains("GH_CONFIG_DIR")
        );
        api.0.borrow_mut().machine = 101;
        // Equal login strings must not affect the numeric comparison.
        let mut guard = Guard::start(&api, "example-owner", now()).unwrap();
        api.0.borrow_mut().machine = 303;
        guard.check(&api, now() + Duration::from_secs(599)).unwrap();
        let count = api.calls().len();
        assert!(guard.check(&api, now() + Duration::from_secs(600)).is_err());
        assert_eq!(api.calls().len(), count + 1);
        api.0.borrow_mut().machine = 101;
        assert!(
            guard
                .check(&api, now() + Duration::from_secs(1200))
                .is_err()
        );
        assert_eq!(api.calls().len(), count + 1);
    }
    #[test]
    fn changed_identity_stops_discovery_after_ten_minutes() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        session.poll(now()).unwrap();
        api.0.borrow_mut().machine = 202;
        let calls = api.calls().len();
        assert!(
            session
                .poll(now() + Duration::from_secs(600))
                .err()
                .unwrap()
                .to_string()
                .contains("GH_CONFIG_DIR")
        );
        assert_eq!(api.calls().len(), calls + 1);
        assert_eq!(cursor(&session, "example-org/widgets"), now());
    }
    #[test]
    fn reload_adds_entries_at_now_and_invalid_yaml_stops_all_discovery() {
        let (dir, api, mut session) = setup("- example-org/widgets\n");
        session.poll(now()).unwrap();
        let later = now() + Duration::from_secs(60);
        let path = dir.path().join("github.yaml");
        fs::write(&path, "- example-org/widgets\n- example-org/gadgets\n").unwrap();
        api.pages(
            "example-org/gadgets",
            vec![json!([item(1, now()), item(2, later)])],
        );
        assert_eq!(session.poll(later).unwrap().processed, 1);
        assert_eq!(
            session.snapshot.as_ref().unwrap().cursors["example-org/gadgets"].floor,
            later
        );
        fs::write(&path, "- example-org/bad?query\n").unwrap();
        let calls = api.calls().len();
        assert!(
            session
                .poll(later)
                .err()
                .unwrap()
                .to_string()
                .contains("example-org/bad?query")
        );
        assert_eq!(api.calls().len(), calls);
        // The cached invalid version must also pause the next poll.
        assert!(session.poll(later).is_err());
        assert_eq!(api.calls().len(), calls);
        fs::write(&path, "- example-org/gadgets\n").unwrap();
        assert_eq!(session.poll(later).unwrap().processed, 1);
        assert!(
            !session
                .snapshot
                .as_ref()
                .unwrap()
                .cursors
                .contains_key("example-org/widgets")
        );
        fs::remove_file(&path).unwrap();
        assert!(session.poll(later).is_err());
        assert_eq!(api.calls().len(), calls + 1);
    }
    #[test]
    fn inaccessible_entries_report_the_reason_and_other_entries_continue() {
        for (bad, message) in [
            (
                "example-user/*",
                "is not an organisation; list its repositories one by one",
            ),
            ("example-org/hidden", "not visible to the machine account"),
        ] {
            let (_dir, api, mut session) = setup(&format!("- {bad}\n- example-org/widgets\n"));
            let entry = Entry::parse(bad).unwrap();
            api.fail(&entry.api_path(), Error::NotFound);
            api.pages("example-org/widgets", vec![json!([item(1, now())])]);
            let poll = session.poll(now()).unwrap();
            assert_eq!(poll.processed, 1);
            assert!(
                poll.errors[0].contains(bad) && poll.errors[0].contains(message),
                "{:?}",
                poll.errors
            );
            assert_eq!(cursor(&session, bad), now());
        }
    }
    #[test]
    fn first_start_and_restart_skip_old_activity_but_discovery_overlaps_by_five_minutes() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        let mut pr = item(2, now());
        pr["pull_request"] = json!({});
        api.pages(
            "example-org/widgets",
            vec![json!([item(1, now() - Duration::from_secs(1)), pr])],
        );
        assert_eq!(session.poll(now()).unwrap().processed, 1);
        assert!(
            api.calls()
                .last()
                .unwrap()
                .contains("since=2026-01-02T11:55:00.000Z")
        );
        // A newly visible earlier update inside the overlap is still processed.
        let later = now() + Duration::from_secs(60);
        session.poll(later).unwrap();
        api.pages(
            "example-org/widgets",
            vec![json!([item(3, now() + Duration::from_secs(30))])],
        );
        assert_eq!(session.poll(later).unwrap().processed, 1);
        let previous = session.snapshot.take();
        let restarted = Snapshot::start(previous, later, 101, 202);
        assert_eq!(restarted.epoch, later);
        assert!(restarted.cursors.is_empty());
    }
    #[test]
    fn drained_scan_revisits_equal_update_versions_for_timeline_seen_checks() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        api.pages("example-org/widgets", vec![json!([item(1, now())])]);
        assert_eq!(session.poll(now()).unwrap().processed, 1);
        assert_eq!(
            session
                .poll(now() + Duration::from_secs(60))
                .unwrap()
                .processed,
            1
        );
    }
    #[test]
    fn burst_over_twenty_carries_across_pages_equal_times_and_entries_without_loss() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n- example-org/gadgets\n");
        let values: Vec<_> = (1..=105).map(|number| item(number, now())).collect();
        api.pages(
            "example-org/widgets",
            vec![json!(values[..100]), json!(values[100..])],
        );
        api.pages("example-org/gadgets", vec![json!([item(106, now())])]);
        let mut counts = Vec::new();
        for _ in 0..6 {
            counts.push(session.poll(now()).unwrap().processed);
        }
        assert_eq!(counts, [20, 20, 20, 20, 20, 6]);
        assert_eq!(
            session.snapshot.as_ref().unwrap().cursors["example-org/widgets"]
                .processed
                .len(),
            0
        );
        assert_eq!(session.poll(now()).unwrap().processed, 20);
        assert!(api.calls().iter().any(|s| s.ends_with("page=2")));
        // The same node updated again must reach the later timeline stage.
        api.pages(
            "example-org/widgets",
            vec![json!([item(1, now() + Duration::from_secs(1))])],
        );
        assert_eq!(
            session
                .poll(now() + Duration::from_secs(1))
                .unwrap()
                .processed,
            2
        );
    }
    #[test]
    fn failed_later_page_keeps_the_cursor_and_does_not_commit_a_partial_batch() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        session.poll(now()).unwrap();
        let values: Vec<_> = (1..=100).map(|number| item(number, now())).collect();
        let entry = Entry::parse("example-org/widgets").unwrap();
        // The first page is already processed except one item; page two fails.
        let cursor = session
            .snapshot
            .as_mut()
            .unwrap()
            .cursors
            .get_mut(&entry.key())
            .unwrap();
        for number in 1..=99 {
            cursor.processed.insert(format!("I_{number}"), now());
        }
        api.pages(
            &entry.key(),
            vec![json!(values), json!({"message":"private API text"})],
        );
        let poll = session.poll(now() + Duration::from_secs(60)).unwrap();
        assert_eq!(poll.processed, 0);
        assert_eq!(poll.errors.len(), 1);
        assert_eq!(self::cursor(&session, &entry.key()), now());
        assert!(
            !session.snapshot.as_ref().unwrap().cursors[&entry.key()]
                .processed
                .contains_key("I_100")
        );
        api.pages(&entry.key(), vec![json!(values), json!([item(101, now())])]);
        assert_eq!(
            session
                .poll(now() + Duration::from_secs(60))
                .unwrap()
                .processed,
            2
        );
    }
    #[test]
    fn rate_limit_pauses_every_entry_until_reset_then_resumes() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n- example-org/gadgets\n");
        let reset = now() + Duration::from_secs(120);
        api.fail("repos/example-org/widgets", Error::RateLimited(reset));
        assert_eq!(session.poll(now()).err(), Some(Error::RateLimited(reset)));
        let count = api.calls().len();
        assert_eq!(
            session.poll(now() + Duration::from_secs(119)).err(),
            Some(Error::RateLimited(reset))
        );
        assert_eq!(api.calls().len(), count);
        api.0.borrow_mut().failure = None;
        session.poll(reset).unwrap();
        assert_eq!(api.calls().len(), count + 2);
    }
    #[test]
    fn organisation_pages_return_validated_repository_names_and_pull_requests() {
        let api = ApiFixture::new();
        let entry = Entry::parse("example-org/*").unwrap();
        let mut pr = item(1, now());
        pr["pull_request"] = json!({});
        api.pages(&entry.key(), vec![json!([pr])]);
        let batch = discover::fetch(&api, &entry, &Cursor::new(now()), 20).unwrap();
        assert_eq!(batch.items[0].repository, "example-org/widgets");
        assert_eq!(batch.items[0].number, 1);
        assert!(batch.items[0].is_pull_request);
        assert!(api.calls()[0].starts_with(
            "orgs/example-org/issues?filter=all&state=all&sort=updated&direction=asc"
        ));
        api.pages(&entry.key(), vec![json!([{"node_id":"I_1","number":1,"updated_at":format_utc(now()),"repository":{"full_name":"other-org/widgets"}}])]);
        assert!(discover::fetch(&api, &entry, &Cursor::new(now()), 20).is_err());
    }
    #[test]
    fn only_marked_items_advance_a_cursor_and_seen_records_expire_after_a_day() {
        let api = ApiFixture::new();
        let entry = Entry::parse("example-org/widgets").unwrap();
        api.pages(
            &entry.key(),
            vec![json!([
                item(1, now() + Duration::from_secs(1)),
                item(2, now() + Duration::from_secs(2))
            ])],
        );
        let mut cursor = Cursor::new(now());
        let batch = discover::fetch(&api, &entry, &cursor, 20).unwrap();
        assert_eq!(cursor.at, now());
        cursor.mark(&batch.items[0]);
        assert_eq!(cursor.at, now() + Duration::from_secs(1));
        let retry = discover::fetch(&api, &entry, &cursor, 20).unwrap();
        assert_eq!(retry.items.len(), 1);
        assert_eq!(retry.items[0].number, 2);
        let (_dir, _api, mut session) = setup("[]");
        session.poll(now()).unwrap();
        session.snapshot.as_mut().unwrap().seen.extend([
            ("old".into(), now() - Duration::from_secs(86401)),
            ("recent".into(), now()),
        ]);
        session.poll(now()).unwrap();
        assert_eq!(session.snapshot.as_ref().unwrap().seen.len(), 1);
        assert!(
            session
                .snapshot
                .as_ref()
                .unwrap()
                .seen
                .contains_key("recent")
        );
    }
}
