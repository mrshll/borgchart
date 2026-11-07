//! The watcher's own state, against real files in a temporary folder: the
//! lock, heartbeat health, crash accounting and parking, planned restarts,
//! noticing a new installed brg, and the alert book. Starts no process.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use borgchart::watcher::alerts::{self, Book, Kind, Subject};
use borgchart::watcher::attempts;
use borgchart::watcher::binary::{BinaryId, BinaryWatch, Poll};
use borgchart::watcher::decide::{
    Action, Attempt, Defer, NeedsPerson, Outcome, Quiet, ReadProgress, Screen, Start,
};
use borgchart::watcher::heartbeat::{self, Health, Heartbeat, Limits, State};
use borgchart::watcher::lock::{Lock, StateLock};
use borgchart::watcher::methods::Method;
use borgchart::watcher::restart::{self, Begin, CrashLimits};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
}

fn folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("watcher");
    (tmp, dir)
}

fn lock(dir: &Path) -> Lock {
    Lock::try_acquire(dir).unwrap().expect("the lock is free")
}

const LIMITS: CrashLimits = CrashLimits {
    max_starts: 2,
    window: Duration::from_secs(100),
};

// Catches: two watchers for one folder, and a lock that outlives its holder.
#[test]
fn one_watcher_holds_the_lock() {
    let (_tmp, dir) = folder();
    let held = lock(&dir);
    assert!(Lock::try_acquire(&dir).unwrap().is_none());
    drop(held);
    assert!(Lock::try_acquire(&dir).unwrap().is_some());
}

// Catches: a stuck or dead watcher reading as healthy, a parked one reading
// as stuck, and a malformed heartbeat read as missing or crashing the reader.
#[test]
fn heartbeat_health() {
    let (_tmp, dir) = folder();
    let limits = Limits {
        stale_after: Duration::from_secs(30),
        scan_stale_after: Duration::from_secs(300),
    };
    let health = |now| heartbeat::health(now, heartbeat::read(&dir), &limits);
    assert_eq!(health(t(0)), Health::Missing);

    let mut beat = Heartbeat {
        at: t(1000),
        version: "0.0.0".to_string(),
        state: State::Running,
        last_scan: t(990),
    };
    let held = lock(&dir);
    heartbeat::write(&held, &dir, &beat).unwrap();
    assert_eq!(health(t(1010)), Health::Ok);
    assert_eq!(health(t(1040)), Health::Stale(Duration::from_secs(40)));

    beat.last_scan = t(600); // beating, but stuck mid-scan
    heartbeat::write(&held, &dir, &beat).unwrap();
    assert_eq!(health(t(1010)), Health::ScanStale(Duration::from_secs(410)));

    beat.state = State::Parked;
    heartbeat::write(&held, &dir, &beat).unwrap();
    assert_eq!(health(t(1010)), Health::Parked);

    for bad in [
        "{not json",
        r#"{"at":"yesterday","version":"0.0.0","state":"running","last_scan":"2027-01-15T08:00:00.000Z"}"#,
    ] {
        fs::write(dir.join("heartbeat.json"), bad).unwrap();
        assert!(matches!(health(t(1010)), Health::Malformed(_)), "{bad}");
    }
}

// Catches: a crash loop that never parks, parking that never clears, and
// parking that the same brg clears just by restarting.
#[test]
fn a_crash_loop_parks_until_the_instance_starts_or_brg_changes() {
    let (_tmp, dir) = folder();
    let held = lock(&dir);
    let start = |now, binary| restart::count_start(&held, &dir, &LIMITS, now, binary).unwrap();
    assert_eq!(start(t(0), "a"), Begin::Run { planned: false });
    assert_eq!(start(t(1), "a"), Begin::Run { planned: false });
    assert!(matches!(start(t(2), "a"), Begin::Park(_)));
    // Still parked after the window: parking isn't a count that ages out.
    assert!(matches!(start(t(500), "a"), Begin::Park(_)));
    assert!(restart::parked(&dir).unwrap().is_some());

    restart::instance_started(&dir).unwrap();
    assert!(restart::parked(&dir).unwrap().is_none());
    assert_eq!(start(t(501), "a"), Begin::Run { planned: false });

    assert_eq!(start(t(502), "a"), Begin::Run { planned: false });
    assert!(matches!(start(t(503), "a"), Begin::Park(_)));
    assert_eq!(start(t(504), "b"), Begin::Run { planned: false });
}

// Catches: the start command's reset lost to a count_start that read the
// history before it and saved after it, leaving the watcher parked. Both
// must wait for the state lock, which the test holds to stop each one
// between its read and its write.
#[test]
fn an_instance_start_never_interleaves_with_counting_a_start() {
    let (_tmp, dir) = folder();
    let held = lock(&dir);
    restart::count_start(&held, &dir, &LIMITS, t(0), "a").unwrap();
    restart::count_start(&held, &dir, &LIMITS, t(1), "a").unwrap();

    let state = StateLock::acquire(&dir).unwrap();
    let (done, finished) = mpsc::channel();
    let counting = {
        let (dir, done) = (dir.clone(), done.clone());
        thread::spawn(move || {
            // The crash too many: this would park.
            let begin = restart::count_start(&held, &dir, &LIMITS, t(2), "a").unwrap();
            done.send("count").unwrap();
            begin
        })
    };
    let starting = {
        let dir = dir.clone();
        thread::spawn(move || {
            restart::instance_started(&dir).unwrap();
            done.send("start").unwrap();
        })
    };
    assert_eq!(
        finished.recv_timeout(Duration::from_millis(300)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a writer went ahead while the state lock was held"
    );
    drop(state);
    let order = [finished.recv().unwrap(), finished.recv().unwrap()];
    let begin = counting.join().unwrap();
    starting.join().unwrap();
    // Whichever ran first, the instance start wins over the crash count:
    // either it cleared the parking after, or the count started afresh.
    assert!(
        restart::parked(&dir).unwrap().is_none(),
        "{order:?} {begin:?}"
    );
}

// Catches: a planned restart counted as a crash (an upgrade parking the
// watcher), and one allowed while a wake may be in flight.
#[test]
fn a_planned_restart_is_not_a_crash() {
    let (_tmp, dir) = folder();
    let held = lock(&dir);
    let start = |now| restart::count_start(&held, &dir, &LIMITS, now, "a").unwrap();
    let retry_after = Duration::from_secs(60);
    assert_eq!(start(t(0)), Begin::Run { planned: false });
    assert_eq!(start(t(1)), Begin::Run { planned: false });

    let intent = Attempt {
        start: Start {
            id: "s1".to_string(),
            pane: "p1".to_string(),
        },
        progress: ReadProgress { newest: 2, read: 1 },
        method: Method::Queue,
        at: t(2),
        tries: 1,
        wakes: 1,
        maybe_delivered: false,
        outcome: None,
    };
    attempts::record(&held, &dir, "coder", &intent).unwrap();
    assert!(!restart::mark_planned(&held, &dir, t(3), retry_after).unwrap());
    attempts::record(&held, &dir, "coder", &intent.resolved(Outcome::Dispatched)).unwrap();
    assert!(restart::mark_planned(&held, &dir, t(3), retry_after).unwrap());

    assert_eq!(start(t(4)), Begin::Run { planned: true });
    // The mark is used once: the next start counts, and is the crash too many.
    assert!(matches!(start(t(5)), Begin::Park(_)));
}

// Catches: restarting on a brg that is still being written, and on a file
// whose bytes didn't change.
#[test]
fn a_new_brg_is_noticed_once_it_holds_steady() {
    let tmp = tempfile::tempdir().unwrap();
    let brg = tmp.path().join("brg");
    fs::write(&brg, b"version one").unwrap();
    let mut watch = BinaryWatch::new(BinaryId::read(&brg).unwrap());
    let quiet = Duration::from_secs(10);
    let poll = |w: &mut BinaryWatch, now| w.poll(&brg, now, quiet).unwrap();
    assert_eq!(poll(&mut watch, t(0)), Poll::Same);

    fs::write(&brg, b"version two, part").unwrap();
    assert_eq!(poll(&mut watch, t(1)), Poll::Settling);
    fs::write(&brg, b"version two, complete").unwrap();
    assert_eq!(poll(&mut watch, t(9)), Poll::Settling);
    // Quiet for 10s since the last change, not since the first.
    assert_eq!(poll(&mut watch, t(12)), Poll::Settling);
    match poll(&mut watch, t(19)) {
        Poll::Changed(id) => assert_ne!(id.sha256, watch.running().sha256),
        other => panic!("expected a change, got {other:?}"),
    }

    // Same bytes, a new file: no change.
    let mut watch = BinaryWatch::new(BinaryId::read(&brg).unwrap());
    // The new file can even reuse the inode and mtime tick, so its stat may
    // match at once (seen on ext4 in CI); either way it is never a change.
    fs::remove_file(&brg).unwrap();
    fs::write(&brg, b"version two, complete").unwrap();
    assert!(matches!(
        poll(&mut watch, t(20)),
        Poll::Settling | Poll::Same
    ));
    assert_eq!(poll(&mut watch, t(31)), Poll::Same);
}

fn repeats() -> Action {
    Action::NeedsPerson(NeedsPerson::Repeats { wakes: 3 })
}

// Catches: an alert reopened (with a new time) on every scan or after a
// watcher restart, and an alert lost by a restart.
#[test]
fn an_alert_opens_once_and_survives_a_restart() {
    let (_tmp, dir) = folder();
    let held = lock(&dir);
    let mut book = Book::load(&dir).unwrap();
    book.note_member("coder", &repeats(), t(0));
    book.save(&held, &dir).unwrap();
    drop((book, held));

    let held = lock(&dir);
    let mut book = Book::load(&dir).unwrap();
    book.note_member("coder", &repeats(), t(50));
    book.save(&held, &dir).unwrap();
    let open = alerts::read(&dir).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].kind, &open[0].subject, open[0].opened),
        (Kind::Repeats, &Subject::Member("coder".to_string()), t(0))
    );
}

// Catches: alerts that stay open after their cause cleared, and ones
// cleared by a scan that only lacked evidence.
#[test]
fn an_alert_resolves_itself_when_its_cause_clears() {
    let mut book = Book::default();
    book.note_member("coder", &repeats(), t(0));
    book.note_member("coder", &Action::Defer(Defer::ProgressUnknown), t(1));
    assert_eq!(book.open().len(), 1);
    book.note_member("coder", &Action::Nothing(Quiet::NoUnread), t(2));
    assert!(book.open().is_empty());

    book.note_watcher(&Health::Parked, t(3));
    book.note_watcher(&Health::Stale(Duration::from_secs(90)), t(4));
    assert_eq!(book.open().len(), 1);
    book.note_watcher(&Health::Ok, t(5));
    assert!(book.open().is_empty());

    book.note_member("gone", &repeats(), t(6));
    book.keep_members(&["coder"]);
    assert!(book.open().is_empty());
}

// Catches: one cause opening two alerts as its details change (a draft,
// then a refused queue; more wakes), and one member's alert clearing
// another's.
#[test]
fn one_cause_never_opens_two_alerts() {
    let mut book = Book::default();
    let unreachable = [
        NeedsPerson::Screen(Screen::Draft),
        NeedsPerson::Unreachable {
            method: Method::Queue,
            outcome: Outcome::Rejected,
        },
        NeedsPerson::UnknownVersion,
    ];
    for (i, why) in unreachable.into_iter().enumerate() {
        book.note_member("coder", &Action::NeedsPerson(why), t(i as u64));
    }
    book.note_member("lead", &repeats(), t(10));
    book.note_member(
        "lead",
        &Action::NeedsPerson(NeedsPerson::Repeats { wakes: 4 }),
        t(11),
    );
    book.note_watcher(&Health::Parked, t(12));
    book.note_watcher(&Health::Parked, t(13));
    let mut seen: Vec<_> = book
        .open()
        .iter()
        .map(|a| (a.kind, a.subject.clone(), a.opened))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (Kind::Repeats, Subject::Member("lead".to_string()), t(10)),
            (
                Kind::Unreachable,
                Subject::Member("coder".to_string()),
                t(0)
            ),
            (Kind::Parked, Subject::Watcher, t(12)),
        ]
    );
}
