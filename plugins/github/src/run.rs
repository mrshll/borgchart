//! Guarded polling, journal admission before discovery advances, and delivery.
use crate::{
    config::{self, Guard, TEMPLATE},
    delivery, discover,
    gh::{Api, Error, Gh},
    state::{Kind, OVERLAP, PendingEvent, Snapshot},
    timeline,
};
use borgchart::{bus::time::format_utc, plugin};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

/// Received, not acted on: one attempt per committed comment or review.
const EYES: &str = "mutation($id: ID!) { addReaction(input: {subjectId: $id, content: EYES}) { reaction { content } } }";
#[derive(Default)]
pub struct Poll {
    pub processed: usize,
    pub admitted: usize,
    pub errors: Vec<String>,
}
pub struct Session<A: Api> {
    api: A,
    identity: Option<(String, PathBuf)>,
    guard: Option<Guard>,
    config_file: PathBuf,
    note: String,
    idle: bool,
    pub snapshot: Snapshot,
    epoch: SystemTime,
    paused_until: Option<SystemTime>,
}
impl<A: Api> Session<A> {
    /// Reads state before any API request, and never writes over unreadable state.
    pub fn new(api: A, store: &plugin::State, epoch: SystemTime) -> Result<Self, String> {
        let snapshot = Snapshot::start(store)?;
        Ok(Self::configured(
            api,
            snapshot,
            epoch,
            borgchart::config::path(),
        ))
    }
    fn configured(api: A, snapshot: Snapshot, epoch: SystemTime, config_file: PathBuf) -> Self {
        Self {
            api,
            identity: None,
            guard: None,
            config_file,
            note: String::new(),
            idle: true,
            snapshot,
            epoch,
            paused_until: None,
        }
    }
    pub fn poll(&mut self, now: SystemTime) -> Result<Poll, Error> {
        self.idle = true;
        self.note.clear();
        let Some(mut config) = config::load(&self.config_file).map_err(Error::Other)? else {
            self.note = format!("off: no github section in {}", self.config_file.display());
            self.guard = None;
            self.identity = None;
            self.epoch = now;
            return Ok(Poll::default());
        };
        let template = config::template().map_err(Error::Other)?;
        if !template.is_empty() {
            if !config.entries.is_empty() {
                self.note = format!(
                    "repositories from {TEMPLATE}; the list in {} is ignored",
                    self.config_file.display()
                );
            }
            config.entries = template;
        }
        if config.entries.is_empty() {
            self.note = "no repositories listed".into();
            if self.guard.is_none() {
                self.epoch = now;
            }
            return Ok(Poll::default());
        }
        self.idle = false;
        let identity = (config.owner, config.gh_config_dir);
        if self.identity.as_ref() != Some(&identity) {
            self.guard = None;
            self.api.configure(&identity.1);
            self.identity = Some(identity);
        }
        if let Some(retry) = self.paused_until {
            if now < retry {
                return Err(Error::RateLimited(retry));
            }
            self.paused_until = None;
        }
        let result = self.collect(now, &config.entries);
        if let Err(Error::RateLimited(retry)) = &result {
            self.paused_until = Some(*retry);
        }
        result
    }
    fn collect(&mut self, now: SystemTime, entries: &[config::Entry]) -> Result<Poll, Error> {
        // Entries listed before the guard first passes start at the epoch.
        let first = self.guard.is_none();
        if first {
            self.guard = Some(Guard::start(
                &self.api,
                &self.identity.as_ref().unwrap().0,
                now,
            )?);
        }
        self.guard.as_mut().unwrap().check(&self.api, now)?;
        let snapshot = &mut self.snapshot;
        snapshot.entries(entries, if first { self.epoch } else { now });
        if first && let Some((saved, edge)) = snapshot.replay(now) {
            snapshot.pending.push(PendingEvent {
                id: "cutoff".into(),
                repository: String::new(),
                number: 0,
                is_pull_request: false,
                kind: Kind::Cutoff,
                reactable: false,
                recipient: delivery::root_lead(),
                reason: format!("GitHub: intake was not running between {} and {}; owner actions before {} were not delivered; check GitHub for them.", format_utc(saved), format_utc(self.epoch), format_utc(edge)),
            });
        }
        let mut poll = Poll {
            processed: 0,
            admitted: 0,
            errors: Vec::new(),
        };
        let mut attempted = 0;
        let mut routing = None;
        for entry in entries {
            if attempted == discover::POLL_CAP {
                break;
            }
            let cursor = snapshot.cursors.get_mut(&entry.key()).unwrap();
            let original_at = cursor.at;
            let since = cursor
                .floor
                .max(cursor.drained.checked_sub(OVERLAP).unwrap_or(cursor.floor));
            match discover::fetch(&self.api, entry, cursor, discover::POLL_CAP - attempted) {
                Ok(batch) => {
                    let mut failed = false;
                    for item in &batch.items {
                        attempted += 1;
                        let guard = self.guard.as_ref().unwrap();
                        match timeline::fetch(&self.api, item, cursor.floor, since, guard) {
                            Ok((events, heads)) => {
                                for mut event in events {
                                    if !snapshot.seen.contains_key(&event.id) {
                                        snapshot.seen.insert(event.id.clone(), now);
                                        let routing =
                                            routing.get_or_insert_with(delivery::Routing::load);
                                        (event.recipient, event.reason) =
                                            delivery::route(routing.as_ref(), &heads);
                                        if let Some(copy) = delivery::approval_copy(&event) {
                                            snapshot.pending.push(copy);
                                        }
                                        snapshot.pending.push(event);
                                        poll.admitted += 1;
                                    }
                                }
                                cursor.mark(item, now);
                                poll.processed += 1;
                            }
                            Err(error @ Error::RateLimited(_)) => {
                                cursor.at = original_at;
                                return Err(error);
                            }
                            Err(error) => {
                                failed = true;
                                poll.errors
                                    .push(format!("{}#{}: {error}", item.repository, item.number));
                            }
                        }
                    }
                    if failed {
                        cursor.at = original_at;
                    } else if batch.exhausted {
                        cursor.at = cursor.at.max(now);
                        cursor.drained = now;
                    }
                }
                Err(Error::RateLimited(retry)) => return Err(Error::RateLimited(retry)),
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
        self.snapshot.save(store)?;
        let (sent, delivery_error) = delivery::send(&mut self.snapshot, store)?;
        if result.is_ok() && self.idle && self.snapshot.pending.is_empty() {
            return store.health(true, &self.note).map_err(|e| e.to_string());
        }
        // No GitHub call while off or with nothing listed, so no reaction either.
        let may_react = result.is_ok() && !self.idle;
        let (mut errors, counts) = match result {
            Ok(poll) => (
                poll.errors,
                format!(
                    "{} changed item(s) processed; {} admitted; {sent} sent",
                    poll.processed, poll.admitted
                ),
            ),
            Err(error) => (vec![error.to_string()], format!("{sent} sent")),
        };
        if may_react && let Some(id) = self.snapshot.reactions.pop_first() {
            self.snapshot.save(store)?;
            if let Err(error) = self.api.graphql(EYES, &[("id", &id)], &[]) {
                errors.push(format!("eyes reaction on {id} failed: {error}"));
                if let Error::RateLimited(retry) = error {
                    self.paused_until = Some(retry);
                    self.snapshot.reactions.insert(id);
                    self.snapshot.save(store)?;
                }
            }
        }
        if !self.snapshot.pending.is_empty() {
            errors.push(format!(
                "{} pending event(s): {}",
                self.snapshot.pending.len(),
                delivery_error.unwrap_or_default()
            ));
        }
        let ok = errors.is_empty();
        errors.insert(0, counts);
        if !self.note.is_empty() {
            errors.push(self.note.clone());
        }
        let floor = self.snapshot.cursors.values().map(|c| c.floor).min();
        errors.push(format!(
            "activity before {} is not delivered",
            format_utc(floor.unwrap_or(self.epoch))
        ));
        let detail = errors.join("; ");
        store.health(ok, &detail).map_err(|_| {
            "cannot write GitHub health; check state directory permissions".to_string()
        })
    }
}
pub fn run(store: &plugin::State, once: bool) -> Result<(), String> {
    let result: Result<(), String> = (|| {
        let _lock = store
            .lock()
            .map_err(|_| "cannot lock GitHub state; check directory permissions".to_string())?;
        let mut session = Session::new(Gh::default(), store, SystemTime::now())?;
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
        nodes: Vec<Value>,
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
                nodes: Vec::new(),
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
        fn graphql(
            &self,
            _: &str,
            _: &[(&str, &str)],
            integers: &[(&str, u64)],
        ) -> Result<Value, Error> {
            Ok(json!({"data":{"repository":{"issueOrPullRequest":{
                "__typename":"Issue","id":format!("I_{}", integers[0].1),
                "author":{"databaseId":202},"body":"","createdAt":format_utc(now()),"lastEditedAt":null,
                "assignees":{"nodes":[]},"closedByPullRequestsReferences":{"nodes":[]},
                "timelineItems":{"nodes":self.0.borrow().nodes,"pageInfo":{"hasNextPage":false}}
            }}}}))
        }
        fn get(&self, request: Rest<'_>) -> Result<Value, Error> {
            let path = request.path();
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
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }
    fn item(number: u64, updated_at: SystemTime) -> Value {
        json!({"node_id":format!("I_{number}"),"number":number,"updated_at":format_utc(updated_at),"repository":{"full_name":"example-org/widgets"}})
    }
    fn config(entries: &str) -> String {
        format!("schema: 1\ngithub:\n  owner: example-owner\n  gh_config_dir: machine-profile\n  repositories:\n{entries}").replace("\n-", "\n  -")
    }
    fn setup(entries: &str) -> (tempfile::TempDir, ApiFixture, Session<ApiFixture>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, config(entries)).unwrap();
        let api = ApiFixture::new();
        let session = Session::configured(api.clone(), Snapshot::default(), now(), path);
        (dir, api, session)
    }
    fn processed(session: &mut Session<ApiFixture>, at: SystemTime) -> usize {
        session.poll(at).unwrap().processed
    }

    #[test]
    fn changed_machine_identity_stops_discovery_until_restart_or_reconfiguration() {
        let (dir, api, mut session) = setup("- example-org/widgets\n");
        session.poll(now()).unwrap();
        api.0.borrow_mut().machine = 303;
        session.poll(now() + secs(599)).unwrap();
        let error = session.poll(now() + secs(600)).err().unwrap();
        assert!(error.to_string().contains("gh_config_dir"), "{error}");
        let calls = api.calls().len();
        // Switching back does not resume a session whose identity changed.
        api.0.borrow_mut().machine = 101;
        assert!(session.poll(now() + secs(1200)).is_err());
        assert_eq!(api.calls().len(), calls);
        let owner = config("- example-org/widgets\n").replace("example-owner", "example-next");
        for text in [owner.clone(), owner.replace("machine-profile", "p2")] {
            fs::write(dir.path().join("config.yaml"), text).unwrap();
            api.0.borrow_mut().calls.clear();
            session.poll(now() + secs(1201)).unwrap();
            assert_eq!(&api.calls()[..2], ["user", "users/example-next"]);
        }
    }
    #[test]
    fn a_guard_outage_at_start_still_bounds_replay_from_when_intake_resumes() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        let stopped = now() - secs(3600);
        let cursors = [("example-org/widgets".into(), Cursor::new(stopped))];
        session.snapshot.cursors.extend(cursors);
        session.snapshot.seen.insert("AE_before".into(), stopped);
        api.fail("user", Error::Other("unreachable".into()));
        assert!(session.poll(now()).is_err());
        api.0.borrow_mut().failure = None;
        let resumed = now() + secs(4 * 86400);
        // An undrained backlog relists the timeline after the old IDs are pruned.
        let backlog: Vec<_> = (1..=21).map(|n| item(n, resumed)).collect();
        api.pages("example-org/widgets", vec![json!(backlog)]);
        let assigned = |id: &str, at: SystemTime| json!({"__typename":"AssignedEvent","id":id,"createdAt":format_utc(at),"actor":{"databaseId":202},"assignee":{"databaseId":101}});
        let edge = resumed - secs(72 * 3600);
        api.0.borrow_mut().nodes = vec![assigned("AE_before", stopped), assigned("AE_edge", edge)];
        session.poll(resumed).unwrap();
        session.poll(resumed + secs(60)).unwrap();
        assert_eq!(session.snapshot.cursors["example-org/widgets"].floor, edge);
        let ids: Vec<_> = session.snapshot.pending.iter().map(|e| &e.id).collect();
        assert_eq!(ids, ["cutoff", "AE_edge"]);
    }
    #[test]
    fn reload_adds_entries_at_now_and_invalid_files_stop_discovery() {
        let (dir, api, mut session) = setup("- example-org/widgets\n");
        session.poll(now()).unwrap();
        let later = now() + secs(60);
        let path = dir.path().join("config.yaml");
        let entries = config("- example-org/widgets\n- example-org/gadgets\n");
        fs::write(&path, entries).unwrap();
        // The new entry starts when it was added: its older item is skipped.
        api.pages(
            "example-org/gadgets",
            vec![json!([item(1, now()), item(2, later)])],
        );
        assert_eq!(processed(&mut session, later), 1);
        for bad in ["- example-org/bad?query\n", "wrong-shape"] {
            fs::write(&path, config(bad)).unwrap();
            let calls = api.calls().len();
            assert!(session.poll(later).is_err());
            assert_eq!(api.calls().len(), calls);
        }
        fs::remove_file(&path).unwrap();
        let calls = api.calls().len();
        session.poll(later + secs(60)).unwrap();
        assert_eq!(api.calls().len(), calls);
        // A new entry added at turn-on skips activity from before the last poll that found no file.
        fs::write(&path, config("- example-org/tools\n")).unwrap();
        api.pages("example-org/tools", vec![json!([item(3, later)])]);
        assert_eq!(processed(&mut session, later + secs(60)), 0);
        assert!(!session.snapshot.cursors.contains_key("example-org/widgets"));
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
        }
    }
    #[test]
    fn discovery_overlaps_by_five_minutes_but_skips_activity_before_start() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        api.pages(
            "example-org/widgets",
            vec![json!([item(1, now() - secs(1)), item(2, now())])],
        );
        assert_eq!(processed(&mut session, now()), 1);
        assert!(
            api.calls()
                .last()
                .unwrap()
                .contains("since=2026-01-02T11:55:00.000Z")
        );
        // An earlier update that becomes visible after the cursor passed it.
        let later = now() + secs(120);
        api.pages("example-org/widgets", vec![json!([])]);
        session.poll(later).unwrap();
        api.pages(
            "example-org/widgets",
            vec![json!([item(3, now() + secs(30))])],
        );
        assert_eq!(processed(&mut session, later), 1);
    }
    #[test]
    fn relisted_versions_are_processed_until_settled_and_new_versions_always() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        let page = json!([item(1, now() + secs(50)), item(2, now() + secs(110))]);
        api.pages("example-org/widgets", vec![page]);
        assert_eq!(processed(&mut session, now() + secs(120)), 2);
        // The young version is queried once more for a lagging timeline; the settled one is not.
        assert_eq!(processed(&mut session, now() + secs(180)), 1);
        assert_eq!(processed(&mut session, now() + secs(240)), 0);
        let page = json!([item(1, now() + secs(150)), item(2, now() + secs(110))]);
        api.pages("example-org/widgets", vec![page]);
        assert_eq!(processed(&mut session, now() + secs(300)), 1);
        // Seen versions last as long as the overlap; admitted events four days.
        session.snapshot.seen.extend([
            ("old".into(), now() - secs(4 * 86400 + 1)),
            ("recent".into(), now()),
        ]);
        api.pages("example-org/widgets", vec![json!([])]);
        session.poll(now() + secs(600)).unwrap();
        assert!(
            session.snapshot.cursors["example-org/widgets"]
                .seen
                .is_empty()
        );
        assert_eq!(session.snapshot.seen.keys().collect::<Vec<_>>(), ["recent"]);
    }
    #[test]
    fn burst_over_twenty_carries_across_pages_equal_times_and_entries_without_loss() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n- example-org/gadgets\n");
        let at = now() + secs(1);
        let values: Vec<_> = (1..=105).map(|number| item(number, at)).collect();
        api.pages(
            "example-org/widgets",
            vec![json!(values[..100]), json!(values[100..])],
        );
        api.pages("example-org/gadgets", vec![json!([item(106, at)])]);
        let counts: Vec<_> = (0..7)
            .map(|_| processed(&mut session, now() + secs(120)))
            .collect();
        assert_eq!(counts, [20, 20, 20, 20, 20, 6, 0]);
        assert!(api.calls().iter().any(|s| s.ends_with("page=2")));
    }
    #[test]
    fn failed_later_page_keeps_the_cursor_and_does_not_commit_a_partial_batch() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n");
        session.poll(now()).unwrap();
        let key = "example-org/widgets";
        let values: Vec<_> = (1..=100).map(|number| item(number, now())).collect();
        // The first page is already processed except one item; page two fails.
        let cursor = session.snapshot.cursors.get_mut(key).unwrap();
        for number in 1..=99 {
            cursor.seen.insert(format!("I_{number}"), now());
        }
        api.pages(
            key,
            vec![json!(values), json!({"message":"private API text"})],
        );
        let poll = session.poll(now() + secs(60)).unwrap();
        assert_eq!((poll.processed, poll.errors.len()), (0, 1));
        api.pages(key, vec![json!(values), json!([item(101, now())])]);
        assert_eq!(processed(&mut session, now() + secs(60)), 2);
    }
    #[test]
    fn rate_limit_pauses_every_entry_until_the_retry_time_then_resumes() {
        let (_dir, api, mut session) = setup("- example-org/widgets\n- example-org/gadgets\n");
        let retry = now() + secs(120);
        api.fail("repos/example-org/widgets", Error::RateLimited(retry));
        assert_eq!(session.poll(now()).err(), Some(Error::RateLimited(retry)));
        let count = api.calls().len();
        assert_eq!(
            session.poll(now() + secs(119)).err(),
            Some(Error::RateLimited(retry))
        );
        assert_eq!(api.calls().len(), count);
        api.0.borrow_mut().failure = None;
        api.pages("example-org/widgets", vec![json!([item(1, now())])]);
        api.pages("example-org/gadgets", vec![json!([item(2, now())])]);
        assert_eq!(processed(&mut session, retry), 2);
    }
    #[test]
    fn organisation_pages_name_each_item_by_its_validated_repository() {
        let api = ApiFixture::new();
        let entry = Entry::parse("example-org/*").unwrap();
        api.pages(&entry.key(), vec![json!([item(1, now())])]);
        let batch = discover::fetch(&api, &entry, &Cursor::new(now()), 20).unwrap();
        assert_eq!(batch.items[0].repository, "example-org/widgets");
        assert!(api.calls()[0].starts_with("orgs/example-org/issues?filter=all&state=all"));
        let mut bad = item(1, now());
        bad["repository"]["full_name"] = json!("example-org/../widgets");
        api.pages(&entry.key(), vec![json!([bad])]);
        assert!(discover::fetch(&api, &entry, &Cursor::new(now()), 20).is_err());
    }
}
