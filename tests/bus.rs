//! Record store tests: `brg msg`, `brg inbox` and `brg bus` against a run
//! directory built from fixtures (a hand-written `run.json` and incarnations).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const RUN: &str = "example-run-1";

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
    /// A member with explicit run and incarnation values.
    MemberWith(&'static str, &'static str, &'static str),
}

impl Fixture {
    /// A deployment with a bus and a started run: `lead` (the root) with
    /// children `helper` and `helper2`, and `sub` under `helper`.
    fn new() -> Self {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(f.dep_dir()).unwrap();
        fs::write(
            f.dep_file(),
            format!(
                "schema: 1\nbus:\n  root: {}\n  run: {RUN}\n",
                f.root().display()
            ),
        )
        .unwrap();
        f.write_manifest(&borgchart::state::deployment_id(&f.dep_file()));
        for m in ["lead", "helper", "helper2", "sub"] {
            f.write_incarnation(m, &format!("{m}-1"), false);
        }
        f
    }

    fn dep_dir(&self) -> PathBuf {
        self.dir.path().join("deploy")
    }
    fn dep_file(&self) -> PathBuf {
        self.dep_dir().join("borgchart.yaml")
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("runs")
    }
    fn run_dir(&self) -> PathBuf {
        self.root().join(RUN)
    }

    fn write_manifest(&self, deployment: &str) {
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
            "run": RUN,
            "created_at": borgchart::bus::time::utc_now(),
            "deployment": deployment,
            "source": self.dep_file(),
            "config": {"version": 1, "digest": "00", "inputs": {}},
            "members": members,
        });
        self.write_private(&self.run_dir().join("run.json"), &manifest);
    }

    fn write_private(&self, path: &Path, v: &Value) {
        use std::os::unix::fs::PermissionsExt;
        for dir in [self.root(), self.run_dir()] {
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
            "generation": 1,
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

    fn write_generation(&self, sha: &str) {
        self.write_private(
            &self.run_dir().join("runtime/generations/000001.json"),
            &json!({
                "n": 1,
                "at": "2026-11-05T19:00:00.000Z",
                "binary": env!("CARGO_BIN_EXE_brg"),
                "binary_sha256": sha,
                "pinned": self.run_dir().join(format!("runtime/bin/{sha}/brg")),
                "state_dir": self.dir.path().join("state"),
                "socket": "borgchart-test",
                "reason": "start",
            }),
        );
    }

    fn command(&self, who: &As, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", "/usr/bin:/bin")
            .args(args)
            .arg("-f")
            .arg(self.dep_file());
        let (m, run, inc) = match who {
            As::Operator => return cmd,
            As::Member(m) => (m.to_string(), RUN.to_string(), format!("{m}-1")),
            As::MemberWith(m, r, i) => (m.to_string(), r.to_string(), i.to_string()),
        };
        cmd.env("BORGCHART_MEMBER", m)
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
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        Out {
            code: out.status.code().unwrap_or(-1),
            json: serde_json::from_str(stdout.trim()).unwrap_or(Value::String(stdout)),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }
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

    /// Every path under the temp dir, with its kind and size.
    fn tree(&self) -> BTreeSet<String> {
        fn walk(dir: &Path, out: &mut BTreeSet<String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let meta = fs::symlink_metadata(e.path()).unwrap();
                out.insert(format!("{} {}", e.path().display(), meta.len()));
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
    assert_eq!(r["run"], RUN);
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
    let sha = "a".repeat(64);
    let cases: Vec<(&str, Value)> = vec![
        (
            "unknown field",
            json!({"to": "lead", "kind": "note", "extra": 1}),
        ),
        ("unknown kind", json!({"to": "lead", "kind": "shout"})),
        ("bad recipient", json!({"to": "Lead", "kind": "note"})),
        (
            "task kind without task",
            json!({"to": "helper", "kind": "accept"}),
        ),
        (
            "bad task id",
            json!({"to": "helper", "kind": "note", "task": "T 1"}),
        ),
        (
            "attempt 0",
            json!({"to": "helper", "kind": "note", "task": "t", "attempt": 0}),
        ),
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
        (
            "assign without facts",
            json!({"to": "helper", "kind": "assign", "task": "t", "attempt": 1}),
        ),
        (
            "assign bad digest",
            json!({"to": "helper", "kind": "assign", "task": "t", "attempt": 1,
                "facts": {"agreement": {"path": "/a.md", "sha256": "xyz"}, "scope": "s"}}),
        ),
        (
            "verify unknown result",
            json!({"to": "helper", "kind": "verify", "task": "t",
                "facts": {"result": "m5", "outcome": "accepted"}}),
        ),
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

    // A valid assign, and a verify that names a real result.
    let assign = json!({"to": "helper", "kind": "assign", "task": "t-1", "attempt": 1,
        "reply_to": ["m1"],
        "facts": {"agreement": {"path": "/a.md", "sha256": sha}, "scope": "the parser"}});
    assert_eq!(f.publish(&As::Member("lead"), "a-1", &assign).code, 0);
    let verify = json!({"to": "helper", "kind": "verify", "task": "t-1",
        "facts": {"result": "m2", "outcome": "rejected"}});
    assert_eq!(f.publish(&As::Member("lead"), "v-1", &verify).code, 0);
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
        As::MemberWith("helper", "example-run-0", "helper-1"),
        As::MemberWith("helper", RUN, "helper-0"),
        As::MemberWith("stranger", RUN, "stranger-1"),
    ];
    for who in &fenced {
        let out = f.publish(who, "k-f", &note("lead", "x"));
        assert_eq!(out.code, 8, "{:?}", out.json);
        assert_eq!(out.json["outcome"], "fenced");
        assert_eq!(f.brg(who, &["inbox", "--json"], None).code, 8);
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
        &As::MemberWith("helper", "example-run-0", "x"),
        &["inbox", "--member", "lead", "--json"],
        None,
    );
    assert_eq!(out.code, 0);
}

#[test]
fn mutations_need_the_generation_binary() {
    let f = Fixture::new();
    f.write_generation(&"0".repeat(64));
    let out = f.publish(&As::Operator, "k", &note("lead", "x"));
    assert_eq!(out.code, 4, "{:?}", out.json);
    assert_eq!(out.json["outcome"], "refused");
    // Reads work from any brg.
    assert_eq!(
        f.brg(&As::Operator, &["bus", "check", "--json"], None).code,
        0
    );

    let sha = format!(
        "{:x}",
        Sha256::digest(fs::read(env!("CARGO_BIN_EXE_brg")).unwrap())
    );
    f.write_generation(&sha);
    let out = f.publish(&As::Operator, "k", &note("lead", "x"));
    assert_eq!(out.code, 0, "{:?}", out.json);
}

#[test]
fn deployment_and_run_must_match() {
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
    let out = g.brg(&As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(out.code, 3);
    assert_eq!(out.json["outcome"], "not-configured");

    let h = Fixture::new();
    fs::write(h.dep_file(), "schema: 1\n").unwrap();
    assert_eq!(h.brg(&As::Operator, &["bus", "check"], None).code, 3);
}

#[test]
fn read_only_commands_create_nothing() {
    let f = Fixture::new();
    // A fresh run (no bus/ yet), then one with records.
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
    let q = json!({"to": "external:ops", "kind": "question", "body": "ship it?"});
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
    let d = json!({"to": "lead", "kind": "decision", "reply_to": ["m1"], "body": "yes"});
    assert_eq!(f.publish(&As::Operator, "d-1", &d).code, 0);
    assert_eq!(ids(&f.inbox(&As::Member("lead"), &[])), ["m2"]);
}

#[test]
fn inspect_summarizes_the_run() {
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
    assert_eq!(v["run"], RUN);
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
fn check_warns_and_fails_on_run_age() {
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
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Out {
        code: out.status.code().unwrap_or(-1),
        json: serde_json::from_str(stdout.trim()).unwrap_or(Value::String(stdout)),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

#[test]
fn authority_is_checked_again_under_the_locks() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, "k1", &note("lead", "a")).code, 0);
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();
    let publish = [
        "msg",
        "publish",
        "--key",
        "k2",
        "--external",
        "ops",
        "--json",
    ];

    // A new generation recorded while a publish waits for generation.lock.
    let generation = hold(&f.run_dir().join("runtime/generation.lock"));
    let child = spawn(&f, &As::Operator, &publish, &draft);
    std::thread::sleep(Duration::from_millis(500));
    f.write_generation(&"0".repeat(64));
    drop(generation);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 4, "{:?} {}", out.json, out.stderr);
    fs::remove_dir_all(f.run_dir().join("runtime/generations")).unwrap();

    // The same for a duplicate-key retry, which would repair the key index.
    let generation = hold(&f.run_dir().join("runtime/generation.lock"));
    let retry = [
        "msg",
        "publish",
        "--key",
        "k1",
        "--external",
        "ops",
        "--json",
    ];
    let child = spawn(
        &f,
        &As::Operator,
        &retry,
        &serde_json::to_vec(&note("lead", "a")).unwrap(),
    );
    std::thread::sleep(Duration::from_millis(500));
    f.write_generation(&"0".repeat(64));
    drop(generation);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 4, "{:?} {}", out.json, out.stderr);
    fs::remove_dir_all(f.run_dir().join("runtime/generations")).unwrap();

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
