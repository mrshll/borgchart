//! Record store tests: `brg msg`, `brg inbox` and `brg bus` against a team's
//! message storage built from fixtures (a hand-written `current`, `run.json`
//! and incarnations).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The team's internal current run. Users never see it: every command's
/// output is checked for it (see [`output`]).
const RUN: &str = "r41";
/// The instance, as messages name it.
const TEAM: &str = "oncall-2a3b";

struct Fixture {
    dir: tempfile::TempDir,
}

/// What a `brg` call returned.
struct Out {
    code: i32,
    json: Value,
    stderr: String,
}

#[derive(Clone)]
enum As {
    Operator,
    Member(&'static str),
    /// A member with explicit (internal) run and incarnation values.
    MemberWith(&'static str, &'static str, &'static str),
}

impl Fixture {
    /// A deployment with a bus and a started team: `lead` (the root) with
    /// children `helper` and `helper2`, and `sub` under `helper`.
    fn new() -> Self {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        f.register();
        f.write_manifest(TEAM);
        fs::write(f.store_dir().join("current"), format!("{RUN}\n")).unwrap();
        for m in ["lead", "helper", "helper2", "sub"] {
            f.write_incarnation(m, &format!("{m}-1"), false);
        }
        f
    }

    /// brg's state folder.
    fn root(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    /// The instance's message storage: `<state>/<instance>/messages/`.
    fn store_dir(&self) -> PathBuf {
        self.root().join(TEAM).join("messages")
    }

    /// The instance in the registry, with its members by bare names.
    fn register(&self) {
        let members: BTreeMap<&str, Value> = ["lead", "helper", "helper2", "sub"]
            .iter()
            .map(|m| (*m, json!({"role": m})))
            .collect();
        self.write_private(
            &self.root().join("registry.json"),
            &json!({
                "schema": 1,
                "instances": {TEAM: {
                    "team": "oncall", "team_dir": self.dir.path().join("oncall"),
                    "folder": self.dir.path().join("work"), "created_at": "2026-11-05T19:00:00.000Z",
                }},
                "issued": {"oncall": ["2a3b"]},
            }),
        );
        self.write_private(
            &self.root().join(TEAM).join("instance.json"),
            &json!({
                "schema": 1, "id": TEAM, "team": "oncall",
                "team_dir": self.dir.path().join("oncall"),
                "folder": self.dir.path().join("work"),
                "created_at": "2026-11-05T19:00:00.000Z",
                "members": members,
            }),
        );
    }
    fn run_dir(&self) -> PathBuf {
        self.store_dir().join(RUN)
    }

    fn write_manifest(&self, deployment: &str) {
        self.write_manifest_for(RUN, deployment);
    }

    /// `run.json` for the internal run `run`.
    fn write_manifest_for(&self, run: &str, deployment: &str) {
        let parents = [
            ("lead", None),
            ("helper", Some("lead")),
            ("helper2", Some("lead")),
            ("sub", Some("helper")),
        ];
        let members: BTreeMap<&str, Value> = parents
            .iter()
            .map(|(m, p)| (*m, json!({"vendor": "claude", "parent": p})))
            .collect();
        let manifest = json!({
            "schema": 1,
            "run": run,
            "created_at": borgchart::bus::time::utc_now(),
            "instance": deployment,
            "source": self.dir.path().join("oncall"),
            "config": {"version": 1, "digest": "00", "inputs": {}},
            "roles": members,
        });
        self.write_private(&self.store_dir().join(run).join("run.json"), &manifest);
    }

    /// A new current storage `run` for the same team (same members), with a
    /// record `m1` (`(to, body)`) if one is given,
    /// switched to while holding the old storage's `bus/lock`, the way
    /// rollover does it.
    fn roll_over(&self, run: &str, record: Option<(&str, &str)>) {
        self.write_manifest_for(run, TEAM);
        if let Some((to, body)) = record {
            self.write_private(
                &self
                    .store_dir()
                    .join(run)
                    .join("bus/records/0000000001.json"),
                &json!({
                    "schema": 1, "run": run, "id": "m1", "seq": 1,
                    "at": "2026-11-05T19:00:00.000Z",
                    "actor": {"type": "member", "member": "lead", "incarnation": "lead-1"},
                    "key": "new-1", "to": to, "kind": "note", "body": body,
                }),
            );
        }
        let lock = hold(&self.run_dir().join("bus/lock"));
        fs::write(self.run_dir().join("retired"), "").unwrap();
        fs::write(self.store_dir().join("current"), format!("{run}\n")).unwrap();
        drop(lock);
    }

    /// How brg says to start one member.
    fn start_member(&self, member: &str) -> String {
        format!("brg start {TEAM}/{member}")
    }

    fn write_private(&self, path: &Path, v: &Value) {
        use std::os::unix::fs::PermissionsExt;
        for dir in [
            self.root(),
            self.store_dir(),
            path.parent().unwrap().to_path_buf(),
        ] {
            fs::create_dir_all(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    fn write_incarnation(&self, member: &str, incarnation: &str, ended: bool) {
        let mut v = json!({
            "member": member,
            "incarnation": incarnation,
            "started_at": "2026-11-05T19:00:00.000Z",
        });
        if ended {
            v["ended_at"] = json!("2026-11-05T20:00:00.000Z");
            v["ended_reason"] = json!("down");
        }
        self.write_private(
            &self
                .run_dir()
                .join(format!("runtime/incarnations/{member}.json")),
            &v,
        );
    }

    fn command(&self, who: &As, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("BORGCHART_STATE_HOME", self.root())
            .env("BORGCHART_CONFIG", self.dir.path().join("config.yaml"))
            .args(args)
            .args(["--instance", TEAM]);
        let (m, run, inc) = match who {
            As::Operator => return cmd,
            As::Member(m) => (m.to_string(), RUN.to_string(), format!("{m}-1")),
            As::MemberWith(m, r, i) => (m.to_string(), r.to_string(), i.to_string()),
        };
        cmd.env("BORGCHART_INSTANCE", TEAM)
            .env("BORGCHART_MEMBER", m)
            .env("BORGCHART_RUN", run)
            .env("BORGCHART_INCARNATION", inc);
        cmd
    }

    fn brg(&self, who: &As, args: &[&str], stdin: Option<&[u8]>) -> Out {
        let mut cmd = self.command(who, args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        {
            let mut input = child.stdin.take().unwrap();
            if let Some(bytes) = stdin {
                feed(&mut input, bytes);
            }
        }
        output(child.wait_with_output().unwrap())
    }

    fn publish(&self, who: &As, key: &str, draft: &Value) -> Out {
        let mut args = vec!["msg", "publish", "--key", key, "--json"];
        if let As::Operator = who {
            // Outside callers name themselves.
            args.extend(["--external", "ops"]);
        }
        self.brg(who, &args, Some(&serde_json::to_vec(draft).unwrap()))
    }

    fn records(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.run_dir().join("bus/records"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn inbox(&self, who: &As, extra: &[&str]) -> Value {
        let mut args = vec!["inbox", "--json"];
        args.extend(extra);
        let out = self.brg(who, &args, None);
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        out.json
    }

    /// Every path under the temp dir: files with their size, directories
    /// by existence only (a directory's size is platform-specific: on macOS
    /// it grows as entries are added).
    fn tree(&self) -> BTreeSet<String> {
        fn walk(dir: &Path, out: &mut BTreeSet<String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let meta = fs::symlink_metadata(e.path()).unwrap();
                if meta.is_dir() {
                    out.insert(format!("{} dir", e.path().display()));
                } else {
                    out.insert(format!("{} {}", e.path().display(), meta.len()));
                }
                if meta.is_dir() {
                    walk(&e.path(), out);
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(self.dir.path(), &mut out);
        out
    }
}

/// What a finished `brg` printed, after checking it speaks only of teams
/// and members.
fn output(out: std::process::Output) -> Out {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_team_words(&stdout);
    assert_team_words(&stderr);
    Out {
        code: out.status.code().unwrap_or(-1),
        json: serde_json::from_str(stdout.trim()).unwrap_or(Value::String(stdout)),
        stderr,
    }
}

/// Users see teams and members only: no run IDs, no "run" wording and no
/// paths inside the team's storage (`<root>/<deployment-id>/…`).
fn assert_team_words(text: &str) {
    assert!(
        !borgchart::bus::mentions_runs(text),
        "run wording in output: {text}"
    );
    assert!(
        !text.contains(&format!("/{TEAM}/messages")),
        "storage path in output: {text}"
    );
}

/// Write a draft to brg's stdin. brg may refuse and exit before reading it
/// all (e.g. an oversize draft), so a broken pipe is fine: the exit code and
/// outcome are what tests check.
fn feed(input: &mut impl Write, bytes: &[u8]) {
    match input.write_all(bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("cannot write brg's stdin: {e}"),
    }
}

fn note(to: &str, body: &str) -> Value {
    json!({"to": to, "kind": "note", "body": body})
}

fn ids(v: &Value) -> Vec<String> {
    v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn publish_retry_and_conflict() {
    let f = Fixture::new();
    let out = f.publish(&As::Operator, "demo-1", &note("lead", "hello"));
    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
    assert_eq!(out.json["outcome"], "committed");
    assert_eq!(out.json["id"], "m1");
    assert_eq!(out.json["attention"], "none");

    let again = f.publish(&As::Operator, "demo-1", &note("lead", "hello"));
    assert_eq!(again.code, 0);
    assert_eq!(again.json["outcome"], "duplicate");
    assert_eq!(again.json["id"], "m1");

    let changed = f.publish(&As::Operator, "demo-1", &note("lead", "hello!"));
    assert_eq!(changed.code, 6, "{:?}", changed.json);
    assert_eq!(changed.json["outcome"], "conflict");

    // Keys are per slot: the same key from another caller is a new record.
    let other = f.publish(&As::Member("helper"), "demo-1", &note("lead", "hello"));
    assert_eq!(other.json["id"], "m2", "{:?}", other.json);
    assert_eq!(f.records().len(), 2);

    let rec = f.brg(&As::Operator, &["msg", "read", "m1", "--json"], None);
    assert_eq!(rec.code, 0);
    let r = &rec.json["record"];
    assert_eq!(r["actor"], json!({"type": "external", "name": "ops"}));
    assert!(r.get("run").is_none(), "{r}");
    assert_eq!(r["schema"], 1);
    assert_eq!(r["key"], "demo-1");
    assert_eq!(r["body"], "hello");
    assert_eq!(r["acked"], false);
    assert_eq!(
        f.brg(&As::Operator, &["msg", "read", "m9", "--json"], None)
            .code,
        10
    );
}

#[test]
fn records_on_disk_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k", &note("lead", "x")).code, 0);
    let rec = f.run_dir().join("bus/records/0000000001.json");
    assert_eq!(
        fs::metadata(&rec).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(rec.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn changed_artifact_bytes_conflict() {
    let f = Fixture::new();
    let art = f.dir.path().join("report.md");
    fs::write(&art, "first").unwrap();
    let draft = json!({"to": "lead", "kind": "note",
        "artifacts": [{"path": art, "label": "report"}]});
    let out = f.publish(&As::Member("helper"), "r-1", &draft);
    assert_eq!(out.code, 0, "{:?}", out.json);
    let rec = f.brg(&As::Operator, &["msg", "read", "m1", "--json"], None);
    let a = &rec.json["record"]["artifacts"][0];
    assert_eq!(a["bytes"], 5);
    assert_eq!(a["sha256"], format!("{:x}", Sha256::digest(b"first")));
    assert_eq!(a["label"], "report");

    assert_eq!(
        f.publish(&As::Member("helper"), "r-1", &draft).json["outcome"],
        "duplicate"
    );
    fs::write(&art, "second").unwrap();
    assert_eq!(f.publish(&As::Member("helper"), "r-1", &draft).code, 6);

    let missing = json!({"to": "lead", "kind": "note",
        "artifacts": [{"path": f.dir.path().join("nope")}]});
    assert_eq!(f.publish(&As::Member("helper"), "r-2", &missing).code, 5);
    let dir_artifact = json!({"to": "lead", "kind": "note",
        "artifacts": [{"path": f.dir.path()}]});
    assert_eq!(
        f.publish(&As::Member("helper"), "r-3", &dir_artifact).code,
        5
    );
    assert_eq!(f.records().len(), 1);
}

#[test]
fn invalid_drafts_write_nothing() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "seed", &note("lead", "x")).code, 0);
    let cases: Vec<(&str, Value)> = vec![
        (
            "unknown field",
            json!({"to": "lead", "kind": "note", "extra": 1}),
        ),
        ("unknown kind", json!({"to": "lead", "kind": "shout"})),
        ("bad recipient", json!({"to": "Lead", "kind": "note"})),
        (
            "too many refs",
            json!({"to": "lead", "kind": "note", "reply_to": (1..=33).map(|n| format!("m{n}")).collect::<Vec<_>>()}),
        ),
        (
            "bad ref",
            json!({"to": "lead", "kind": "note", "reply_to": ["x1"]}),
        ),
        (
            "unknown ref",
            json!({"to": "lead", "kind": "note", "reply_to": ["m7"]}),
        ),
        ("body too big", note("lead", &"x".repeat(64 * 1024 + 1))),
        ("control char", note("lead", "bell\u{7}")),
        (
            "relative artifact",
            json!({"to": "lead", "kind": "note", "artifacts": [{"path": "rel/x"}]}),
        ),
        (
            "too many artifacts",
            json!({"to": "lead", "kind": "note", "artifacts": (0..33).map(|_| json!({"path": "/x"})).collect::<Vec<_>>()}),
        ),
        (
            "facts too big",
            json!({"to": "lead", "kind": "note", "facts": {"x": "y".repeat(16 * 1024)}}),
        ),
        ("not a member", note("nobody", "x")),
    ];
    for (name, draft) in &cases {
        let out = f.publish(&As::Operator, "k-invalid", draft);
        assert_eq!(out.code, 5, "{name}: {:?} {}", out.json, out.stderr);
        assert_eq!(out.json["outcome"], "invalid", "{name}");
    }
    // Topology: members publish only along parent edges.
    for (from, to) in [
        ("helper", "helper2"),
        ("sub", "lead"),
        ("helper", "external:ops"),
    ] {
        let out = f.publish(&As::Member(from), "k-route", &note(to, "x"));
        assert_eq!(out.code, 5, "{from} -> {to}: {:?}", out.json);
    }
    let out = f.publish(&As::Operator, "k-ext", &note("external:other", "x"));
    assert_eq!(out.code, 5, "external -> external");
    let bad_key = f.publish(&As::Operator, "has space", &note("lead", "x"));
    assert_eq!(bad_key.code, 5);
    let garbage = f.brg(
        &As::Operator,
        &[
            "msg",
            "publish",
            "--key",
            "g",
            "--external",
            "ops",
            "--json",
        ],
        Some(b"{not json"),
    );
    assert_eq!(garbage.code, 5);
    let huge = f.brg(
        &As::Operator,
        &[
            "msg",
            "publish",
            "--key",
            "g",
            "--external",
            "ops",
            "--json",
        ],
        Some(&vec![b' '; 200 * 1024]),
    );
    assert_eq!(huge.code, 5);
    assert_eq!(f.records(), vec!["0000000001.json"]);
}

#[test]
fn concurrent_publishers_get_contiguous_numbers() {
    let f = Fixture::new();
    let per = 5;
    let writers = 8;
    std::thread::scope(|s| {
        for w in 0..writers {
            let f = &f;
            s.spawn(move || {
                for i in 0..per {
                    let who = if w % 2 == 0 {
                        As::Member("helper")
                    } else {
                        As::Operator
                    };
                    let out = f.publish(
                        &who,
                        &format!("w{w}-{i}"),
                        &note("lead", &format!("{w}/{i}")),
                    );
                    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
                }
            });
        }
    });
    let expected: Vec<String> = (1..=writers * per)
        .map(|n| format!("{n:010}.json"))
        .collect();
    assert_eq!(f.records(), expected);
    let all = f.inbox(&As::Operator, &["--member", "lead", "--limit", "100"]);
    let bodies: BTreeSet<String> = all["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(bodies.len(), writers * per, "no lost or duplicate records");
    let check = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 0, "{:?}", check.json);
}

#[test]
fn crash_leftovers_are_tolerated_and_repaired() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "one")).code, 0);
    // A crash before linking leaves a stray temporary file.
    fs::write(f.run_dir().join("bus/records/.tmp-999-0-1"), "partial").unwrap();
    // A crash after the record but before the key index: the record stands.
    let keys = f.run_dir().join("bus/keys/external.ops");
    for e in fs::read_dir(&keys).unwrap().flatten() {
        fs::remove_file(e.path()).unwrap();
    }
    let retry = f.publish(&As::Operator, "k1", &note("lead", "one"));
    assert_eq!(retry.json["outcome"], "duplicate", "{:?}", retry.json);
    assert_eq!(retry.json["id"], "m1");
    assert_eq!(fs::read_dir(&keys).unwrap().count(), 1, "index repaired");
    let next = f.publish(&As::Operator, "k2", &note("lead", "two"));
    assert_eq!(next.json["id"], "m2");

    let check = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 0, "{:?}", check.json);
    let warnings = check.json["warnings"].to_string();
    assert!(warnings.contains(".tmp-999-0-1"), "{warnings}");
}

#[test]
fn corrupt_store_stops_mutations_but_not_reads() {
    for damage in ["garbage", "gap", "bad-ack"] {
        let f = Fixture::new();
        for k in ["k1", "k2", "k3"] {
            assert_eq!(f.publish(&As::Operator, k, &note("lead", k)).code, 0);
        }
        let records = f.run_dir().join("bus/records");
        match damage {
            "garbage" => fs::write(records.join("0000000002.json"), "{oops").unwrap(),
            "gap" => fs::remove_file(records.join("0000000002.json")).unwrap(),
            _ => {
                fs::create_dir_all(f.run_dir().join("bus/acks")).unwrap();
                fs::write(f.run_dir().join("bus/acks/m1.json"), "[]").unwrap()
            }
        }
        let out = f.publish(&As::Operator, "k4", &note("lead", "x"));
        assert_eq!(out.code, 7, "{damage}: {:?}", out.json);
        assert_eq!(out.json["outcome"], "corrupt");
        let ack = f.brg(&As::Member("lead"), &["msg", "ack", "m3", "--json"], None);
        assert_eq!(ack.code, 7, "{damage}");

        let inbox = f.inbox(&As::Operator, &["--member", "lead"]);
        assert_eq!(inbox["store"], "incomplete", "{damage}");
        assert!(ids(&inbox).contains(&"m3".to_string()));
        let inspect = f.brg(&As::Operator, &["bus", "inspect", "--json"], None);
        assert_eq!(inspect.code, 0);
        assert_eq!(inspect.json["store"]["healthy"], false);
        let check = f.brg(&As::Operator, &["bus", "check", "--json"], None);
        assert_eq!(check.code, 7, "{damage}: {:?}", check.json);
        assert!(!check.json["errors"].as_array().unwrap().is_empty());
    }
}

#[test]
fn pagination_survives_concurrent_arrivals() {
    let f = Fixture::new();
    for i in 1..=5 {
        f.publish(&As::Operator, &format!("p{i}"), &note("lead", "x"));
    }
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    let mut extra = 6;
    loop {
        let mut args = vec!["--member", "lead", "--limit", "2"];
        if let Some(a) = &after {
            args.extend(["--after", a.as_str()]);
        }
        let page = f.inbox(&As::Operator, &args);
        seen.extend(ids(&page));
        // New records arrive between pages.
        if extra <= 7 {
            f.publish(&As::Operator, &format!("p{extra}"), &note("lead", "x"));
            extra += 1;
        }
        match page["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => break,
        }
    }
    let expected: Vec<String> = (1..=7).map(|n| format!("m{n}")).collect();
    assert_eq!(seen, expected);
    assert_eq!(
        f.brg(
            &As::Operator,
            &["inbox", "--member", "lead", "--limit", "101"],
            None
        )
        .code,
        2
    );
    let default_page = f.inbox(&As::Operator, &["--member", "lead"]);
    assert_eq!(default_page["unacked"], 7);
    assert!(default_page["records"][0].get("body").is_none());
    let full = f.inbox(&As::Operator, &["--member", "lead", "--full"]);
    assert_eq!(full["records"][0]["body"], "x");
}

#[test]
fn acks_are_idempotent_and_recipient_only() {
    let f = Fixture::new();
    f.publish(&As::Operator, "k1", &note("lead", "a"));
    f.publish(&As::Operator, "k2", &note("lead", "b"));
    f.publish(&As::Operator, "k3", &note("helper", "c"));

    let own = f.inbox(&As::Member("lead"), &[]);
    assert_eq!(ids(&own), ["m1", "m2"]);
    // Reading never acknowledges.
    assert_eq!(f.inbox(&As::Member("lead"), &[])["unacked"], 2);

    let ack = f.brg(&As::Member("lead"), &["msg", "ack", "m1", "--json"], None);
    assert_eq!(ack.code, 0, "{:?}", ack.json);
    assert_eq!(ack.json["acked"][0]["outcome"], "acked");
    let again = f.brg(
        &As::Member("lead"),
        &["msg", "ack", "m1", "m2", "--json"],
        None,
    );
    assert_eq!(again.json["acked"][0]["outcome"], "already");
    assert_eq!(again.json["acked"][1]["outcome"], "acked");
    assert_eq!(
        ids(&f.inbox(&As::Member("lead"), &[])),
        Vec::<String>::new()
    );
    assert_eq!(ids(&f.inbox(&As::Member("lead"), &["--all"])), ["m1", "m2"]);

    // Not the recipient: refused, nothing written.
    let wrong = f.brg(&As::Member("lead"), &["msg", "ack", "m3", "--json"], None);
    assert_eq!(wrong.code, 4, "{:?}", wrong.json);
    let ext = f.brg(
        &As::Operator,
        &["msg", "ack", "m3", "--external", "ops", "--json"],
        None,
    );
    assert_eq!(ext.code, 4);
    let operator = f.brg(&As::Operator, &["msg", "ack", "m3", "--json"], None);
    assert_eq!(operator.code, 4);
    let missing = f.brg(&As::Member("helper"), &["msg", "ack", "m9", "--json"], None);
    assert_eq!(missing.code, 10);
    // All-or-nothing: one bad ID stops the others.
    let mixed = f.brg(
        &As::Member("helper"),
        &["msg", "ack", "m3", "m1", "--json"],
        None,
    );
    assert_eq!(mixed.code, 4);
    assert!(!f.run_dir().join("bus/acks/m3.json").exists());
    let read = f.brg(&As::Operator, &["msg", "read", "m1", "--json"], None);
    assert_eq!(read.json["record"]["acked"], true);
}

#[test]
fn inbox_wait_returns_early_or_times_out() {
    let f = Fixture::new();
    let start = Instant::now();
    let out = f.brg(
        &As::Member("lead"),
        &["inbox", "wait", "--timeout", "1", "--json"],
        None,
    );
    assert_eq!(out.code, 9, "{:?} {}", out.json, out.stderr);
    assert_eq!(out.json["outcome"], "timeout");
    assert!(start.elapsed() >= Duration::from_millis(900));

    let start = Instant::now();
    let waiter = f
        .command(
            &As::Member("lead"),
            &["inbox", "wait", "--timeout", "20", "--json"],
        )
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        f.publish(&As::Operator, "w1", &note("lead", "wake")).code,
        0
    );
    let out = waiter.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ids(&v), ["m1"]);
    assert!(start.elapsed() < Duration::from_secs(5));

    // After the cursor, nothing new: times out again.
    let out = f.brg(
        &As::Member("lead"),
        &["inbox", "wait", "--after", "m1", "--timeout", "1", "--json"],
        None,
    );
    assert_eq!(out.code, 9);
    for bad in ["0", "601"] {
        let out = f.brg(
            &As::Member("lead"),
            &["inbox", "wait", "--timeout", bad],
            None,
        );
        assert_eq!(out.code, 2, "--timeout {bad}");
    }
    let out = f.brg(&As::Member("lead"), &["inbox", "wait"], None);
    assert_eq!(out.code, 2, "--timeout is required");
}

#[cfg(feature = "test-support")]
mod background_wait {
    use super::*;

    fn command(f: &Fixture, who: &As, extra: &[&str]) -> Command {
        let mut args = vec!["inbox", "wait", "--background", "--json"];
        args.extend(extra);
        let mut cmd = f.command(who, &args);
        cmd.env("BORGCHART_WAIT_MIN_MS", "400")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn observation(f: &Fixture) -> PathBuf {
        f.run_dir().join("attention/observed/lead/lead-1.json")
    }

    fn bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    files.insert(path.clone(), Vec::new());
                    files.extend(bytes(&path));
                } else {
                    files.insert(path.clone(), fs::read(path).unwrap());
                }
            }
        }
        files
    }

    #[test]
    fn background_wait_ignores_shown_records_and_never_marks_arrivals() {
        let f = Fixture::new();
        assert_eq!(
            f.publish(&As::Operator, "old", &note("lead", "old body"))
                .code,
            0
        );
        assert_eq!(ids(&f.inbox(&As::Member("lead"), &[])), ["m1"]);
        let seen = fs::read(observation(&f)).unwrap();
        let mut child = command(&f, &As::Member("lead"), &[]).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(650));
        assert!(
            child.try_wait().unwrap().is_none(),
            "an unacked, shown record woke the wait"
        );
        assert_eq!(
            f.publish(&As::Operator, "new", &note("lead", "new body"))
                .code,
            0
        );
        let before = bytes(&f.run_dir());
        let out = finish_within(child, Duration::from_secs(3));
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        assert_eq!(
            out.json,
            json!({"ok": true, "code": 0, "outcome": "ok", "ids": ["m2"], "count": 1})
        );
        assert_eq!(bytes(&f.run_dir()), before);
        assert_eq!(fs::read(observation(&f)).unwrap(), seen);
        assert_eq!(ids(&f.inbox(&As::Member("lead"), &[])), ["m1", "m2"]);
        let observed: Value = serde_json::from_slice(&fs::read(observation(&f)).unwrap()).unwrap();
        assert_eq!(observed["ids"], json!(["m1", "m2"]));
    }

    #[test]
    fn background_wait_returns_old_ids_at_minimum_in_text_or_json() {
        let f = Fixture::new();
        for key in ["one", "two"] {
            assert_eq!(
                f.publish(&As::Operator, key, &note("lead", "private body"))
                    .code,
                0
            );
        }
        let start = Instant::now();
        let mut cmd = command(&f, &As::Member("lead"), &["--timeout", "86400"]);
        let out = finish_within(cmd.spawn().unwrap(), Duration::from_secs(3));
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        assert_eq!(out.json["ids"], json!(["m1", "m2"]));
        assert!(start.elapsed() >= Duration::from_millis(400));
        let start = Instant::now();
        let child = f
            .command(&As::Member("lead"), &["inbox", "wait", "--background"])
            .env("BORGCHART_WAIT_MIN_MS", "400")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = finish_within(child, Duration::from_secs(3));
        assert_eq!(out.code, 0);
        assert_eq!(out.json, "m1 m2\n");
        assert!(start.elapsed() >= Duration::from_millis(400));
        assert!(!observation(&f).exists());
    }

    #[test]
    fn background_wait_leading_file_error_backs_off() {
        let f = Fixture::new();
        let mut cmd = f.command(
            &As::Member("lead"),
            &["--file", "removed", "inbox", "wait", "--background"],
        );
        cmd.env("BORGCHART_WAIT_MIN_MS", "400")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let start = Instant::now();
        let out = finish_within(cmd.spawn().unwrap(), Duration::from_secs(3));
        assert_eq!(out.code, 2);
        assert!(start.elapsed() >= Duration::from_millis(400));
    }

    #[test]
    fn background_wait_boolean_value_error_backs_off() {
        let f = Fixture::new();
        let mut cmd = f.command(&As::Member("lead"), &["inbox", "wait", "--background=true"]);
        cmd.env("BORGCHART_WAIT_MIN_MS", "400")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let start = Instant::now();
        let out = finish_within(cmd.spawn().unwrap(), Duration::from_secs(3));
        assert_eq!(out.code, 2);
        assert!(start.elapsed() >= Duration::from_millis(400));
    }

    #[test]
    fn background_wait_explicit_help_is_immediate() {
        let f = Fixture::new();
        for flag in ["--help", "-h"] {
            for background in ["--background", "--background=true"] {
                let mut cmd = f.command(&As::Member("lead"), &["inbox", "wait", background, flag]);
                cmd.env("BORGCHART_WAIT_MIN_MS", "10000")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let start = Instant::now();
                let out = finish_within(cmd.spawn().unwrap(), Duration::from_secs(3));
                assert!(matches!(out.code, 0 | 2));
                assert!(start.elapsed() < Duration::from_secs(3));
            }
        }
    }

    #[test]
    fn background_wait_persistent_observation_failure_is_rate_limited() {
        let f = Fixture::new();
        assert_eq!(
            f.publish(&As::Operator, "old", &note("lead", "old body"))
                .code,
            0
        );
        borgchart::bus::fsx::create_private_dirs(&observation(&f)).unwrap();
        for _ in 0..3 {
            assert_eq!(ids(&f.inbox(&As::Member("lead"), &[])), ["m1"]);
            assert!(
                observation(&f).is_dir(),
                "observation writes must keep failing"
            );
            let before = bytes(&f.run_dir());
            let start = Instant::now();
            let out = finish_within(
                command(&f, &As::Member("lead"), &[]).spawn().unwrap(),
                Duration::from_secs(3),
            );
            assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
            assert_eq!(out.json["ids"], json!(["m1"]));
            assert!(start.elapsed() >= Duration::from_millis(400));
            assert_eq!(bytes(&f.run_dir()), before);
        }
    }

    #[test]
    fn background_wait_new_arrival_bypasses_minimum_and_includes_old_ids() {
        let f = Fixture::new();
        assert_eq!(
            f.publish(&As::Operator, "old", &note("lead", "old body"))
                .code,
            0
        );
        let mut cmd = command(&f, &As::Member("lead"), &[]);
        cmd.env("BORGCHART_WAIT_MIN_MS", "10000");
        let start = Instant::now();
        let mut child = cmd.spawn().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none());
        assert_eq!(
            f.publish(&As::Operator, "new", &note("lead", "new body"))
                .code,
            0
        );
        let out = finish_within(child, Duration::from_secs(3));
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        assert_eq!(out.json["ids"], json!(["m1", "m2"]));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(!observation(&f).exists());
    }

    #[test]
    fn background_wait_requires_draining_every_inbox_page() {
        let f = Fixture::new();
        for n in 1..=21 {
            assert_eq!(
                f.publish(&As::Operator, &format!("note-{n}"), &note("lead", "body"))
                    .code,
                0
            );
        }
        let first = f.inbox(&As::Member("lead"), &[]);
        assert_eq!(first["records"].as_array().unwrap().len(), 20);
        assert_eq!(first["next"], "m20");
        let start = Instant::now();
        let out = finish_within(
            command(&f, &As::Member("lead"), &[]).spawn().unwrap(),
            Duration::from_secs(3),
        );
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        assert_eq!(out.json["ids"], json!(["m21"]));
        assert!(start.elapsed() >= Duration::from_millis(400));
        let last = f.inbox(&As::Member("lead"), &["--after", "m20"]);
        assert_eq!(ids(&last), ["m21"]);
        assert!(last["next"].is_null());
        let mut cmd = command(&f, &As::Member("lead"), &["--timeout", "60"]);
        cmd.env("BORGCHART_WAIT_TIMEOUT_MS", "500");
        let out = finish_within(cmd.spawn().unwrap(), Duration::from_secs(3));
        assert_eq!(
            out.code, 9,
            "all 21 records are shown but still unacknowledged: {:?} {}",
            out.json, out.stderr
        );
    }

    #[test]
    fn background_wait_errors_back_off_including_parser_errors() {
        let f = Fixture::new();
        for (who, flags, code) in [
            (As::Operator, vec![], 2),
            (As::Member("lead"), vec!["--timeout", "30"], 2),
            (As::Member("lead"), vec!["--timeout", "86401"], 2),
            (As::Member("lead"), vec!["--after", "m1"], 2),
            (As::Member("lead"), vec!["--limit", "20"], 2),
            (As::Member("lead"), vec!["--full"], 2),
            (As::Member("lead"), vec!["--member", "helper"], 2),
            (As::Operator, vec!["--external", "ops"], 2),
            (As::Member("lead"), vec!["--timeout", "bad"], 2),
            (As::Member("lead"), vec!["--unknown"], 2),
            (As::MemberWith("lead", RUN, "old"), vec![], 8),
        ] {
            let start = Instant::now();
            let out = finish_within(
                command(&f, &who, &flags).spawn().unwrap(),
                Duration::from_secs(3),
            );
            assert_eq!(out.code, code, "{flags:?}: {:?} {}", out.json, out.stderr);
            assert!(
                start.elapsed() >= Duration::from_millis(400),
                "{flags:?}: no backoff"
            );
        }
        assert_eq!(
            f.publish(&As::Operator, "bad", &note("lead", "body")).code,
            0
        );
        fs::write(f.run_dir().join("bus/records/0000000001.json"), "broken").unwrap();
        let start = Instant::now();
        let out = finish_within(
            command(&f, &As::Member("lead"), &[]).spawn().unwrap(),
            Duration::from_secs(3),
        );
        assert_eq!(out.code, 7);
        assert!(start.elapsed() >= Duration::from_millis(400));
    }

    #[test]
    fn background_wait_timeout_obeys_the_minimum() {
        let f = Fixture::new();
        let start = Instant::now();
        let mut cmd = command(&f, &As::Member("lead"), &["--timeout", "60"]);
        cmd.env("BORGCHART_WAIT_TIMEOUT_MS", "1");
        let out = finish_within(cmd.spawn().unwrap(), Duration::from_secs(3));
        assert_eq!(out.code, 9, "{:?} {}", out.json, out.stderr);
        assert!(start.elapsed() >= Duration::from_millis(400));
    }

    #[test]
    fn background_wait_killed_mid_flight_changes_no_files() {
        let f = Fixture::new();
        assert_eq!(
            f.publish(&As::Operator, "seen", &note("lead", "body")).code,
            0
        );
        f.inbox(&As::Member("lead"), &[]);
        let before = bytes(&f.run_dir());
        let mut child = command(&f, &As::Member("lead"), &[]).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(650));
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap(); // SIGKILL on Unix.
        child.wait().unwrap();
        assert_eq!(bytes(&f.run_dir()), before);
    }

    #[test]
    fn background_wait_fences_restarts_and_replacements() {
        for rollover in [true, false] {
            let f = Fixture::new();
            let child = command(&f, &As::Member("lead"), &[]).spawn().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            if rollover {
                f.roll_over("r42", Some(("lead", "new storage")));
            } else {
                f.write_incarnation("lead", "lead-2", false);
            }
            let out = finish_within(child, Duration::from_secs(3));
            assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
        }
    }

    #[test]
    fn observations_union_own_pages_and_tolerate_damage() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        for n in 1..=12 {
            assert_eq!(
                f.publish(&As::Operator, &format!("note-{n}"), &note("lead", "body"))
                    .code,
                0
            );
        }
        f.inbox(&As::Operator, &["--member", "lead"]);
        f.inbox(&As::Member("lead"), &["--member", "lead"]);
        f.brg(&As::Member("lead"), &["msg", "read", "m1", "--json"], None);
        assert!(!observation(&f).exists());
        f.inbox(&As::Member("lead"), &["--after", "m9"]);
        f.inbox(&As::Member("lead"), &["--limit", "2"]);
        let seen: Value = serde_json::from_slice(&fs::read(observation(&f)).unwrap()).unwrap();
        assert_eq!(
            seen,
            json!({"schema": 1, "member": "lead", "incarnation": "lead-1", "ids": ["m1", "m2", "m10", "m11", "m12"]})
        );
        assert_eq!(
            fs::metadata(observation(&f)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for dir in ["attention", "attention/observed", "attention/observed/lead"] {
            assert_eq!(
                fs::metadata(f.run_dir().join(dir))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        fs::write(observation(&f), "broken").unwrap();
        let out = finish_within(
            command(&f, &As::Member("lead"), &[]).spawn().unwrap(),
            Duration::from_secs(3),
        );
        assert_eq!(out.code, 0, "bad observation is treated as empty");
        assert_eq!(out.json["count"], 12);
        f.inbox(&As::Member("lead"), &["--limit", "1"]);
        let seen: Value = serde_json::from_slice(&fs::read(observation(&f)).unwrap()).unwrap();
        assert_eq!(seen["ids"], json!(["m1"]));
    }
}

#[test]
fn caller_context_is_checked() {
    let f = Fixture::new();
    // `--external` from inside a member.
    let out = f.brg(
        &As::Member("lead"),
        &[
            "msg",
            "publish",
            "--key",
            "k",
            "--external",
            "ops",
            "--json",
        ],
        Some(&serde_json::to_vec(&note("helper", "x")).unwrap()),
    );
    assert_eq!(out.code, 4, "{:?}", out.json);
    let out = f.brg(
        &As::Member("lead"),
        &["inbox", "--external", "ops", "--json"],
        None,
    );
    assert_eq!(out.code, 4);
    // An operator can't publish.
    let out = f.brg(
        &As::Operator,
        &["msg", "publish", "--key", "k", "--json"],
        Some(&serde_json::to_vec(&note("lead", "x")).unwrap()),
    );
    assert_eq!(out.code, 4);
    // A partial member context.
    let mut cmd = f.command(&As::Operator, &["msg", "ack", "m1", "--json"]);
    let out = cmd.env("BORGCHART_MEMBER", "lead").output().unwrap();
    assert_eq!(out.status.code(), Some(4));
    // Operators need to name an inbox.
    assert_eq!(f.brg(&As::Operator, &["inbox", "--json"], None).code, 2);

    let fenced = [
        As::MemberWith("helper", "r40", "helper-1"),
        As::MemberWith("helper", RUN, "helper-0"),
        As::MemberWith("stranger", RUN, "stranger-1"),
    ];
    for who in &fenced {
        let out = f.publish(who, "k-f", &note("lead", "x"));
        assert_eq!(out.code, 8, "{:?}", out.json);
        assert_eq!(out.json["outcome"], "fenced");
        assert_eq!(f.brg(who, &["inbox", "--json"], None).code, 8);
        assert_eq!(f.brg(who, &["msg", "ack", "m1", "--json"], None).code, 8);
    }
    f.write_incarnation("helper2", "helper2-1", true);
    assert_eq!(
        f.publish(&As::Member("helper2"), "k-e", &note("lead", "x"))
            .code,
        8
    );
    fs::remove_file(f.run_dir().join("runtime/incarnations/sub.json")).unwrap();
    assert_eq!(
        f.publish(&As::Member("sub"), "k-s", &note("helper", "x"))
            .code,
        8
    );
    assert!(f.records().is_empty());
    // Reading someone's inbox by name needs no live identity.
    let out = f.brg(
        &As::MemberWith("helper", "r40", "x"),
        &["inbox", "--member", "lead", "--json"],
        None,
    );
    assert_eq!(out.code, 0);
}

#[test]
fn a_member_from_before_a_restart_is_fenced_in_team_terms() {
    let f = Fixture::new();
    // The team was restarted: `current` moved on, `helper` still has the old one.
    let stale = As::MemberWith("helper", "r40", "helper-1");
    let out = f.publish(&stale, "k", &note("lead", "x"));
    assert_eq!(out.code, 8, "{:?}", out.json);
    let msg = out.json["message"].as_str().unwrap();
    assert_eq!(
        msg,
        format!(
            "oncall-2a3b/helper is from before the instance was restarted; restart it: {}",
            f.start_member("helper")
        )
    );
    let text = f.brg(&stale, &["inbox"], None);
    assert_eq!(text.code, 8);
    assert!(
        text.stderr.contains("oncall-2a3b/helper is from before"),
        "{}",
        text.stderr
    );
    // A member whose session was replaced, and one that was stopped.
    let out = f.publish(
        &As::MemberWith("helper", RUN, "helper-0"),
        "k",
        &note("lead", "x"),
    );
    assert!(
        out.json["message"]
            .as_str()
            .unwrap()
            .starts_with("oncall-2a3b/helper "),
        "{:?}",
        out.json
    );
    f.write_incarnation("helper2", "helper2-1", true);
    let out = f.publish(&As::Member("helper2"), "k", &note("lead", "x"));
    assert_eq!(out.code, 8);
    assert!(
        out.json["message"]
            .as_str()
            .unwrap()
            .ends_with(&f.start_member("helper2")),
        "{:?}",
        out.json
    );
}

#[test]
fn another_message_format_is_refused_with_the_restart_command() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let manifest_path = f.run_dir().join("run.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    m["schema"] = json!(999);
    fs::write(&manifest_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let before = f.tree();
    let expected = "instance `oncall-2a3b`'s messages use format 999; this brg uses format 1.\nbrg can't read them or check them for open work, so the instance can't start again; create a new instance for fresh storage: brg create <team> <folder>\n(The old messages stay on disk; send their open work again.)".to_string();
    let commands: Vec<(As, Vec<&str>)> = vec![
        (
            As::Operator,
            vec!["msg", "publish", "--key", "k2", "--external", "ops"],
        ),
        (As::Member("lead"), vec!["msg", "ack", "m1"]),
        (As::Member("lead"), vec!["inbox"]),
        (As::Operator, vec!["inbox", "--member", "lead"]),
        (As::Member("lead"), vec!["inbox", "wait", "--timeout", "1"]),
        (As::Operator, vec!["msg", "read", "m1"]),
        (As::Operator, vec!["bus", "inspect"]),
        (As::Operator, vec!["bus", "check"]),
    ];
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();
    for (who, args) in &commands {
        let mut json_args = args.clone();
        json_args.push("--json");
        let out = f.brg(who, &json_args, Some(&draft));
        assert_eq!(out.code, 4, "{args:?}: {:?}", out.json);
        assert_eq!(out.json["outcome"], "refused", "{args:?}");
        assert_eq!(out.json["message"], expected, "{args:?}");
        let text = f.brg(who, args, Some(&draft));
        assert_eq!(text.code, 4, "{args:?}");
        assert_eq!(text.stderr, format!("brg: {expected}\n"), "{args:?}");
    }
    assert_eq!(f.tree(), before, "nothing written");
}

#[test]
fn another_format_points_to_a_new_instance() {
    let f = Fixture::new();
    let manifest_path = f.run_dir().join("run.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    m["schema"] = json!(999);
    fs::write(&manifest_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let out = output(
        f.command(&As::Operator, &["bus", "check"])
            .output()
            .unwrap(),
    );
    assert_eq!(out.code, 4);
    assert!(
        out.stderr
            .contains("create a new instance for fresh storage: brg create <team> <folder>\n"),
        "{}",
        out.stderr
    );
}

#[test]
fn writers_follow_the_team_to_new_storage() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    // A rollover happens while a publish waits for the old storage's lock.
    let lock = hold(&f.run_dir().join("bus/lock"));
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();
    let args = [
        "msg",
        "publish",
        "--key",
        "k2",
        "--external",
        "ops",
        "--json",
    ];
    let child = spawn(&f, &As::Operator, &args, &draft);
    std::thread::sleep(Duration::from_millis(500));
    f.write_manifest_for("r42", TEAM);
    fs::write(f.run_dir().join("retired"), "").unwrap();
    fs::write(f.store_dir().join("current"), "r42\n").unwrap();
    drop(lock);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
    assert_eq!(out.json["id"], "m1", "numbered in the new storage");
    assert_eq!(f.records(), ["0000000001.json"], "nothing more in the old");
    assert!(
        f.store_dir()
            .join("r42/bus/records/0000000001.json")
            .exists()
    );
    // An ack begun after the switch reads IDs in the new storage.
    let ack = f.brg(
        &As::Operator,
        &["msg", "ack", "m1", "--external", "ops", "--json"],
        None,
    );
    assert_eq!(
        ack.code, 4,
        "m1 is the new storage's, addressed to lead: {:?}",
        ack.json
    );
    // A retired current with nothing newer is damage, not a loop.
    fs::write(f.store_dir().join("r42/retired"), "").unwrap();
    let out = f.publish(&As::Operator, "k3", &note("lead", "c"));
    assert_eq!(out.code, 7, "{:?}", out.json);
}

#[test]
fn an_ack_queued_behind_a_rollover_is_refused() {
    let f = Fixture::new();
    // Old m1 for external:ops; the new storage has an unrelated m1 for it too.
    let q = json!({"to": "external:ops", "kind": "note", "body": "OLD MESSAGE"});
    assert_eq!(f.publish(&As::Member("lead"), "q-1", &q).code, 0);
    let lock = hold(&f.run_dir().join("bus/lock"));
    let mut child = spawn(
        &f,
        &As::Operator,
        &["msg", "ack", "m1", "--external", "ops", "--json"],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(child.try_wait().is_ok_and(|s| s.is_none()), "the ack waits");
    f.write_manifest_for("r42", TEAM);
    f.write_private(
        &f.store_dir().join("r42/bus/records/0000000001.json"),
        &json!({
            "schema": 1, "run": "r42", "id": "m1", "seq": 1,
            "at": "2026-11-05T19:00:00.000Z",
            "actor": {"type": "member", "member": "lead", "incarnation": "lead-1"},
            "key": "new-1", "to": "external:ops", "kind": "note", "body": "UNRELATED NEW MESSAGE",
        }),
    );
    fs::write(f.run_dir().join("retired"), "").unwrap();
    fs::write(f.store_dir().join("current"), "r42\n").unwrap();
    drop(lock);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert_eq!(
        out.json["message"],
        "instance `oncall-2a3b` was restarted, so `m1` may now be a different message; list the inbox again"
    );
    assert!(!f.store_dir().join("r42/bus/acks/m1.json").exists());
    assert!(!f.run_dir().join("bus/acks/m1.json").exists());
}

#[test]
fn a_reply_queued_behind_a_rollover_is_refused() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let id = TEAM;
    let draft = json!({"to": "lead", "kind": "note", "reply_to": ["m1"], "body": "re"});
    let lock = hold(&f.run_dir().join("bus/lock"));
    let child = spawn(
        &f,
        &As::Operator,
        &[
            "msg",
            "publish",
            "--key",
            "reply",
            "--external",
            "ops",
            "--json",
        ],
        &serde_json::to_vec(&draft).unwrap(),
    );
    std::thread::sleep(Duration::from_millis(500));
    // Roll over to storage that has its own m1, for lead.
    let next = "r42";
    f.write_manifest_for(next, id);
    for m in ["lead", "helper", "helper2", "sub"] {
        f.write_private(
            &f.store_dir()
                .join(next)
                .join(format!("runtime/incarnations/{m}.json")),
            &json!({"member": m, "incarnation": format!("{m}-1"),
                "started_at": "2026-11-05T19:00:00.000Z"}),
        );
    }
    f.write_private(
        &f.store_dir().join(next).join("bus/records/0000000001.json"),
        &json!({
            "schema": 1, "run": next, "id": "m1", "seq": 1,
            "at": "2026-11-05T19:00:00.000Z",
            "actor": {"type": "external", "name": "ops"},
            "key": "new-1", "to": "lead", "kind": "note",
            "facts": {"status": "complete"},
        }),
    );
    fs::write(f.run_dir().join("retired"), "").unwrap();
    fs::write(f.store_dir().join("current"), format!("{next}\n")).unwrap();
    drop(lock);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert!(
        out.json["message"]
            .as_str()
            .unwrap()
            .contains("list the inbox again"),
        "{:?}",
        out.json
    );
    let records = f.store_dir().join(next).join("bus/records");
    assert_eq!(
        fs::read_dir(&records).unwrap().count(),
        1,
        "nothing written"
    );
}

#[test]
fn inbox_wait_follows_the_team_to_new_storage() {
    let f = Fixture::new();
    let start = Instant::now();
    let child = spawn(
        &f,
        &As::Operator,
        &[
            "inbox",
            "wait",
            "--member",
            "lead",
            "--timeout",
            "10",
            "--json",
        ],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    f.roll_over("r42", Some(("lead", "new")));
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
    assert_eq!(ids(&out.json), ["m1"]);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn inbox_wait_after_a_cursor_refuses_new_storage() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let child = spawn(
        &f,
        &As::Operator,
        &[
            "inbox",
            "wait",
            "--member",
            "lead",
            "--after",
            "m1",
            "--timeout",
            "10",
            "--json",
        ],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    f.roll_over("r42", Some(("lead", "new")));
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert_eq!(
        out.json["message"],
        "instance `oncall-2a3b` was restarted while waiting, so `--after m1` no longer applies; list the inbox again"
    );
}

#[test]
fn inbox_wait_rechecks_the_member_and_format() {
    // A member from before the restart.
    let f = Fixture::new();
    let child = spawn(
        &f,
        &As::Member("lead"),
        &["inbox", "wait", "--timeout", "10", "--json"],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    f.roll_over("r42", Some(("lead", "new")));
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert!(
        out.json["message"]
            .as_str()
            .unwrap()
            .starts_with("oncall-2a3b/lead is from before the instance was restarted"),
        "{:?}",
        out.json
    );
    // New storage in a format this brg can't read.
    let g = Fixture::new();
    let child = spawn(
        &g,
        &As::Operator,
        &[
            "inbox",
            "wait",
            "--member",
            "lead",
            "--timeout",
            "10",
            "--json",
        ],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    g.write_manifest_for("r42", TEAM);
    let path = g.store_dir().join("r42/run.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    m["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
    let before = g.tree();
    let lock = hold(&g.run_dir().join("bus/lock"));
    fs::write(g.run_dir().join("retired"), "").unwrap();
    fs::write(g.store_dir().join("current"), "r42\n").unwrap();
    drop(lock);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 4, "{:?} {}", out.json, out.stderr);
    assert!(
        out.json["message"].as_str().unwrap().contains("format 999"),
        "{:?}",
        out.json
    );
    // Read-only throughout: only the rollover itself changed anything.
    let after = g.tree();
    let changed: Vec<_> = after.symmetric_difference(&before).collect();
    assert!(
        changed.iter().all(|p| p.contains("/current ")
            || p.contains("/retired ")
            || p.contains("/bus ")
            || p.contains("/bus/lock ")),
        "{changed:?}"
    );
}

#[test]
fn inbox_wait_is_bounded_while_a_writer_holds_the_lock() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let lock = hold(&f.run_dir().join("bus/lock"));
    let start = Instant::now();
    let mut child = spawn(
        &f,
        &As::Operator,
        &[
            "inbox",
            "wait",
            "--member",
            "lead",
            "--after",
            "m1",
            "--timeout",
            "1",
            "--json",
        ],
        b"",
    );
    while child.try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "wait still blocked on the lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(lock);
    let out = output(child.wait_with_output().unwrap());
    assert_eq!(out.code, 9, "{:?} {}", out.json, out.stderr);
}

#[test]
fn damaged_storage_errors_name_the_team_not_paths() {
    // A malformed member session record.
    let f = Fixture::new();
    fs::write(f.run_dir().join("runtime/incarnations/lead.json"), "{").unwrap();
    for args in [vec!["inbox", "--json"], vec!["inbox"]] {
        let out = f.brg(&As::Member("lead"), &args, None);
        assert_eq!(out.code, 1, "{args:?}: {:?} {}", out.json, out.stderr);
        let msg = if args.len() == 2 {
            out.json["message"].as_str().unwrap().to_string()
        } else {
            out.stderr.clone()
        };
        assert!(
            msg.contains("oncall-2a3b/lead's session record is unreadable (EOF"),
            "{msg}"
        );
    }
    // A record area that isn't a directory.
    let g = Fixture::new();
    assert_eq!(g.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let records = g.run_dir().join("bus/records");
    fs::remove_dir_all(&records).unwrap();
    fs::write(&records, "").unwrap();
    let out = g.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(out.code, 7, "{:?}", out.json);
    assert!(
        out.json["errors"]
            .to_string()
            .contains("cannot list bus/records: Not a directory"),
        "{:?}",
        out.json
    );
    let out = g.brg(&As::Operator, &["bus", "check"], None);
    assert!(
        out.stderr.contains("cannot list bus/records"),
        "{}",
        out.stderr
    );
    // Storage someone else could write to.
    let h = Fixture::new();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(h.store_dir(), fs::Permissions::from_mode(0o777)).unwrap();
    let out = h.publish(&As::Operator, "k1", &note("lead", "a"));
    assert_eq!(out.code, 1, "{:?}", out.json);
    assert_eq!(
        out.json["message"],
        "instance `oncall-2a3b`'s message storage can't be used: its folder is writable by group or others"
    );
    let text = h.brg(
        &As::Operator,
        &["msg", "publish", "--key", "k1", "--external", "ops"],
        Some(&serde_json::to_vec(&note("lead", "a")).unwrap()),
    );
    assert_eq!(text.code, 1);
    assert!(
        text.stderr.contains("its folder is writable by group"),
        "{}",
        text.stderr
    );
}

#[test]
fn storage_without_current_is_damaged_not_new() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    fs::remove_file(f.store_dir().join("current")).unwrap();
    let before = f.tree();
    for args in [
        vec!["bus", "check", "--json"],
        vec!["inbox", "--member", "lead", "--json"],
    ] {
        let out = f.brg(&As::Operator, &args, None);
        assert_eq!(out.code, 7, "{args:?}: {:?}", out.json);
        let msg = out.json["message"].as_str().unwrap();
        assert!(
            msg.starts_with("instance `oncall-2a3b`'s message storage is damaged"),
            "{msg}"
        );
    }
    let out = f.publish(&As::Operator, "k2", &note("lead", "b"));
    assert_eq!(out.code, 7, "{:?}", out.json);
    assert_eq!(f.tree(), before, "nothing created");
}

#[test]
fn a_record_in_another_format_is_still_corrupt() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let path = f.run_dir().join("bus/records/0000000001.json");
    let mut r: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    r["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    assert_eq!(f.publish(&As::Operator, "k2", &note("lead", "b")).code, 7);
    let check = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 7, "{:?}", check.json);
}

#[test]
fn a_team_not_started_yet_is_not_configured() {
    let f = Fixture::new();
    // The team's directory exists, but `up` never got as far as a first run.
    fs::remove_dir_all(f.run_dir()).unwrap();
    fs::remove_file(f.store_dir().join("current")).unwrap();
    let before = f.tree();
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();
    let commands: Vec<(As, Vec<&str>)> = vec![
        (
            As::Operator,
            vec![
                "msg",
                "publish",
                "--key",
                "k",
                "--external",
                "ops",
                "--json",
            ],
        ),
        (As::Member("lead"), vec!["msg", "ack", "m1", "--json"]),
        (As::Operator, vec!["inbox", "--member", "lead", "--json"]),
        (As::Operator, vec!["msg", "read", "m1", "--json"]),
        (As::Operator, vec!["bus", "inspect", "--json"]),
        (As::Operator, vec!["bus", "check", "--json"]),
    ];
    for (who, args) in &commands {
        let out = f.brg(who, args, Some(&draft));
        assert_eq!(out.code, 3, "{args:?}: {:?}", out.json);
        assert_eq!(out.json["outcome"], "not-configured");
        assert_eq!(
            out.json["message"],
            "instance `oncall-2a3b` has no messages: its team has no `bus:`, or it hasn't started with one"
        );
    }
    assert_eq!(f.tree(), before, "nothing created");
    // Nor when there is no message storage at all.
    fs::remove_dir_all(f.store_dir()).unwrap();
    let out = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(out.code, 3, "{:?}", out.json);
    assert!(!f.store_dir().exists());
}

#[test]
fn deployment_and_storage_must_match() {
    let f = Fixture::new();
    f.write_manifest("someone-else-0000");
    let out = f.publish(&As::Operator, "k", &note("lead", "x"));
    assert_eq!(out.code, 4, "{:?}", out.json);
    assert_eq!(
        f.brg(&As::Operator, &["bus", "inspect", "--json"], None)
            .code,
        4
    );

    let g = Fixture::new();
    fs::remove_file(g.run_dir().join("run.json")).unwrap();
    // `current` names storage without its manifest: damaged, never new.
    let out = g.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(out.code, 7, "{:?}", out.json);
    assert_eq!(out.json["outcome"], "corrupt");
}

#[test]
fn read_only_commands_create_nothing() {
    let f = Fixture::new();
    // A fresh team (no bus/ yet), then one with records.
    for populated in [false, true] {
        if populated {
            f.publish(&As::Operator, "k1", &note("lead", "x"));
            f.brg(&As::Member("lead"), &["msg", "ack", "m1"], None);
        }
        let before = f.tree();
        let reads: Vec<(As, Vec<&str>)> = vec![
            (As::Operator, vec!["inbox", "--member", "lead", "--json"]),
            (As::Member("lead"), vec!["inbox", "--json"]),
            (As::Operator, vec!["inbox", "--external", "ops", "--all"]),
            (As::Operator, vec!["msg", "read", "m1"]),
            (As::Operator, vec!["bus", "inspect", "--json"]),
            (As::Operator, vec!["bus", "inspect"]),
            (As::Operator, vec!["bus", "check"]),
            (
                As::Member("lead"),
                vec!["inbox", "wait", "--timeout", "1", "--json"],
            ),
        ];
        for (who, args) in &reads {
            f.brg(who, args, None);
        }
        assert_eq!(f.tree(), before, "populated: {populated}");
    }
}

#[test]
fn external_recipient_flow() {
    let f = Fixture::new();
    let q = json!({"to": "external:ops", "kind": "note", "body": "ship it?"});
    let out = f.publish(&As::Member("lead"), "q-1", &q);
    assert_eq!(out.code, 0, "{:?}", out.json);
    let inbox = f.inbox(&As::Operator, &["--external", "ops"]);
    assert_eq!(inbox["recipient"], "external:ops");
    assert_eq!(ids(&inbox), ["m1"]);
    // Another outside caller, or a member, can't ack it.
    let other = f.brg(
        &As::Operator,
        &["msg", "ack", "m1", "--external", "audit", "--json"],
        None,
    );
    assert_eq!(other.code, 4);
    let ack = f.brg(
        &As::Operator,
        &["msg", "ack", "m1", "--external", "ops", "--json"],
        None,
    );
    assert_eq!(ack.code, 0, "{:?}", ack.json);
    assert_eq!(
        ids(&f.inbox(&As::Operator, &["--external", "ops"])),
        Vec::<String>::new()
    );
    // The answer goes back to the member.
    let d = json!({"to": "lead", "kind": "note", "reply_to": ["m1"], "body": "yes"});
    assert_eq!(f.publish(&As::Operator, "d-1", &d).code, 0);
    assert_eq!(ids(&f.inbox(&As::Member("lead"), &[])), ["m2"]);
}

#[test]
fn inspect_summarizes_the_team() {
    let f = Fixture::new();
    f.publish(&As::Operator, "k1", &note("lead", "a"));
    f.publish(&As::Member("lead"), "k2", &note("helper", "b"));
    f.brg(&As::Member("helper"), &["msg", "ack", "m2"], None);
    let out = f.brg(
        &As::Operator,
        &["bus", "inspect", "--last", "1", "--json"],
        None,
    );
    assert_eq!(out.code, 0);
    let v = &out.json;
    assert_eq!(v["instance"], TEAM);
    assert!(v.get("run").is_none(), "{v}");
    assert_eq!(v["members"]["sub"]["parent"], "helper");
    assert_eq!(v["records"], 2);
    assert_eq!(v["recent"].as_array().unwrap().len(), 1);
    assert_eq!(v["recent"][0]["id"], "m2");
    let lead = v["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["to"] == "lead")
        .unwrap();
    assert_eq!(lead["unacked"], 1);
    assert_eq!(v["store"]["healthy"], true);
    let text = f.brg(&As::Operator, &["bus", "inspect"], None);
    assert!(
        text.json
            .as_str()
            .unwrap()
            .contains("helper: 0 unacknowledged of 1")
    );
}

#[test]
fn check_warns_and_fails_on_storage_age() {
    let f = Fixture::new();
    let manifest_path = f.run_dir().join("run.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let days_ago = |d: u64| {
        borgchart::bus::time::format_utc(
            std::time::SystemTime::now() - Duration::from_secs(d * 86_400 + 60),
        )
    };
    m["created_at"] = json!(days_ago(8));
    fs::write(&manifest_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let out = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(out.code, 0, "{:?}", out.json);
    assert!(out.json["warnings"].to_string().contains("days old"));
    assert_eq!(out.json["age_days"], 8.0);
    m["created_at"] = json!(days_ago(15));
    fs::write(&manifest_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let out = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(out.code, 1, "{:?}", out.json);
    assert!(out.json["errors"].to_string().contains("max_age_days"));
}

/// Hold `path` (created if missing) with an exclusive flock.
fn hold(path: &Path) -> fs::File {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    file
}

/// Start `brg` with a draft on stdin, without waiting for it.
fn spawn(f: &Fixture, who: &As, args: &[&str], stdin: &[u8]) -> std::process::Child {
    let mut child = f
        .command(who, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed(&mut child.stdin.take().unwrap(), stdin);
    child
}

/// Wait for a child at most `limit`, killing it (and failing) past that.
fn finish_within(mut child: std::process::Child, limit: Duration) -> Out {
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("brg did not finish within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    output(child.wait_with_output().unwrap())
}

#[test]
fn authority_is_checked_again_under_the_locks() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();

    // An incarnation ended while an ack waits for bus/lock.
    let bus = hold(&f.run_dir().join("bus/lock"));
    let child = spawn(
        &f,
        &As::Member("lead"),
        &["msg", "ack", "m1", "--json"],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    f.write_incarnation("lead", "lead-1", true);
    drop(bus);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert!(!f.run_dir().join("bus/acks/m1.json").exists());

    // A member replaced while its publish waits for bus/lock.
    let bus = hold(&f.run_dir().join("bus/lock"));
    let args = ["msg", "publish", "--key", "h1", "--json"];
    let child = spawn(&f, &As::Member("helper"), &args, &draft);
    std::thread::sleep(Duration::from_millis(500));
    f.write_incarnation("helper", "helper-2", false);
    drop(bus);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert_eq!(f.records().len(), 1);
}

#[test]
fn reads_wait_for_a_writer() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let bus = hold(&f.run_dir().join("bus/lock"));
    let mut child = spawn(&f, &As::Operator, &["bus", "check", "--json"], b"");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "check waits for bus/lock"
    );
    drop(bus);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 0, "{:?}", out.json);
}

#[test]
fn a_huge_record_number_is_a_bounded_gap() {
    let f = Fixture::new();
    for k in ["k1", "k2"] {
        assert_eq!(f.publish(&As::Operator, k, &note("lead", k)).code, 0);
    }
    fs::write(f.run_dir().join("bus/records/9999999999.json"), "{}").unwrap();
    let start = Instant::now();
    let check = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 7, "{:?} {}", check.json, check.stderr);
    let errors = check.json["errors"].to_string();
    assert!(errors.contains("gap") && errors.contains("m3"), "{errors}");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(
        f.inbox(&As::Operator, &["--member", "lead"])["store"],
        "incomplete"
    );
    assert_eq!(f.publish(&As::Operator, "k3", &note("lead", "x")).code, 7);
}

#[test]
fn a_fifo_artifact_is_invalid_not_a_hang() {
    let f = Fixture::new();
    let fifo = f.dir.path().join("pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let draft = json!({"to": "lead", "kind": "note", "artifacts": [{"path": fifo}]});
    let child = spawn(
        &f,
        &As::Operator,
        &[
            "msg",
            "publish",
            "--key",
            "fifo",
            "--external",
            "ops",
            "--json",
        ],
        &serde_json::to_vec(&draft).unwrap(),
    );
    let out = finish_within(child, Duration::from_secs(10));
    assert_eq!(out.code, 5, "{:?} {}", out.json, out.stderr);
    assert!(f.records().is_empty());
    // A symlink to a regular file is still measured as that file.
    let real = f.dir.path().join("real.md");
    fs::write(&real, "ok").unwrap();
    let link = f.dir.path().join("link.md");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let draft = json!({"to": "lead", "kind": "note", "artifacts": [{"path": link}]});
    assert_eq!(f.publish(&As::Operator, "link", &draft).code, 0);
}

#[test]
fn an_ack_with_another_schema_is_corrupt() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    assert_eq!(
        f.brg(&As::Member("lead"), &["msg", "ack", "m1"], None).code,
        0
    );
    let path = f.run_dir().join("bus/acks/m1.json");
    let mut ack: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ack["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&ack).unwrap()).unwrap();
    let check = f.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 7, "{:?}", check.json);
    assert!(check.json["errors"].to_string().contains("schema"));
    assert_eq!(f.publish(&As::Operator, "k2", &note("lead", "b")).code, 7);
    // Not counted as consumption.
    let inbox = f.inbox(&As::Operator, &["--member", "lead"]);
    assert_eq!(ids(&inbox), ["m1"]);
    assert_eq!(inbox["store"], "incomplete");
}

#[test]
fn inbox_wait_help_names_the_outcomes_wait_returns() {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .args(["inbox", "wait", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(out.stdout).unwrap();
    let f = Fixture::new();
    let wait = |extra: &[&str]| {
        let mut args = vec!["inbox", "wait", "--json"];
        args.extend(extra);
        f.brg(&As::Member("lead"), &args, None)
    };
    let usage = wait(&["--timeout", "0"]);
    let timeout = wait(&["--timeout", "1"]);
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let ok = wait(&["--timeout", "1"]);
    let child = spawn(
        &f,
        &As::Operator,
        &[
            "inbox",
            "wait",
            "--member",
            "lead",
            "--after",
            "m1",
            "--timeout",
            "10",
            "--json",
        ],
        b"",
    );
    std::thread::sleep(Duration::from_millis(500));
    f.roll_over("r42", Some(("lead", "new")));
    let fenced = finish_within(child, Duration::from_secs(15));
    for (expect, out) in [
        ("usage", &usage),
        ("timeout", &timeout),
        ("ok", &ok),
        ("fenced", &fenced),
    ] {
        assert_eq!(out.json["outcome"], expect, "{:?} {}", out.json, out.stderr);
        let named = format!("{expect} ({})", out.code);
        assert!(
            help.contains(&named),
            "`{named}` missing from inbox wait --help"
        );
    }
}

/// The example in `msg publish --help`: the shell block exactly as printed
/// (from the `<<'EOF'` line through its terminator, untrimmed), the JSON
/// draft inside it, its key, and the output it says publishing prints.
struct Example {
    block: String,
    draft: String,
    key: String,
    printed: String,
}

fn help_example() -> Example {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .args(["msg", "publish", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines().skip_while(|l| !l.contains("<<'EOF'"));
    let command = lines
        .next()
        .expect("a heredoc example in msg publish --help");
    let key = command
        .split_whitespace()
        .skip_while(|w| *w != "--key")
        .nth(1)
        .unwrap()
        .to_string();
    let mut block = format!("{command}\n");
    let mut draft = String::new();
    for l in lines.by_ref() {
        block.push_str(l);
        block.push('\n');
        if l == "EOF" {
            break;
        }
        draft.push_str(l);
        draft.push('\n');
    }
    let printed = lines
        .find_map(|l| l.strip_prefix("Prints: "))
        .expect("`Prints: …` after the example")
        .to_string();
    Example {
        block,
        draft,
        key,
        printed,
    }
}

fn help_text(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .args(args)
        .arg("--help")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_publish_help_example_publishes_and_its_outcomes_are_documented() {
    let f = Fixture::new();
    let help = help_text(&["msg", "publish"]);
    let ex = help_example();
    assert_eq!(
        f.publish(
            &As::Member("lead"),
            "n1",
            &note("helper", "Please check the parser.")
        )
        .code,
        0
    );
    assert_eq!(
        f.publish(&As::Member("helper"), "n2", &note("lead", "Checking."))
            .code,
        0
    );

    // Run the block as a member's shell would, pasted as printed.
    let out = Command::new("sh")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", f.dir.path())
        .env("BORGCHART_BIN", env!("CARGO_BIN_EXE_brg"))
        .env("BORGCHART_STATE_HOME", f.root())
        .env("BORGCHART_INSTANCE", TEAM)
        .env("BORGCHART_MEMBER", "helper")
        .env("BORGCHART_RUN", RUN)
        .env("BORGCHART_INCARNATION", "helper-1")
        .args(["-c", &ex.block])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout} {stderr}");
    assert!(stderr.is_empty(), "the shell complained: {stderr}");
    assert_eq!(stdout.trim(), ex.printed);
    let record = f.brg(&As::Operator, &["msg", "read", "m3", "--json"], None);
    assert_eq!(record.json["record"]["kind"], "note");

    let publish = |body: &[u8]| {
        f.brg(
            &As::Member("helper"),
            &["msg", "publish", "--key", ex.key.as_str(), "--json"],
            Some(body),
        )
    };
    // Retrying is safe; reusing the key for something else is not. Each
    // outcome publish returned is named in the help with its exit code.
    let retry = publish(ex.draft.as_bytes());
    let other = publish(ex.draft.replace("Parser done", "Other").as_bytes());
    for out in [&retry, &other] {
        let named = format!("{} ({})", out.json["outcome"].as_str().unwrap(), out.code);
        assert!(
            help.contains(&named),
            "`{named}` missing from msg publish --help"
        );
    }
    assert_eq!(retry.json["outcome"], "duplicate");
    assert_eq!(other.json["outcome"], "conflict");
}
