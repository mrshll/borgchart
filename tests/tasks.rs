//! Task facts and recovery: the task fold, transition and authority checks
//! at publication, `brg task list|show`, and carrying work into a linked run.
//! Runs are built from fixtures (hand-written `run.json` and incarnations).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use borgchart::bus::manifest::Liveness;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const RUN: &str = "example-run-1";
const NEXT: &str = "example-run-2";

struct Fixture {
    dir: tempfile::TempDir,
}

struct Out {
    code: i32,
    json: Value,
    stderr: String,
}

#[derive(Clone, Copy)]
enum As {
    /// An outside caller, `--external ops`.
    Ops,
    /// No identity at all.
    Operator,
    /// A member's `<member>-1` incarnation.
    Member(&'static str),
    /// A member with an explicit incarnation.
    Inc(&'static str, &'static str),
}

impl Fixture {
    /// `lead` (the root) with children `helper` and `helper2`.
    fn new() -> Self {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(f.dep_file().parent().unwrap()).unwrap();
        f.configure(RUN, None);
        f.start_run(RUN, None);
        fs::write(f.agreement(), "Build the parser. Tests first.\n").unwrap();
        f
    }

    fn dep_file(&self) -> PathBuf {
        self.dir.path().join("deploy/borgchart.yaml")
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("runs")
    }
    fn run_dir(&self, run: &str) -> PathBuf {
        self.root().join(run)
    }
    fn agreement(&self) -> PathBuf {
        self.dir.path().join("agreement.md")
    }
    fn agreement_sha(&self) -> String {
        format!("{:x}", Sha256::digest(fs::read(self.agreement()).unwrap()))
    }

    /// Point the deployment at `run` (optionally continuing another).
    fn configure(&self, run: &str, continues: Option<&str>) {
        let mut text = format!(
            "schema: 1\nbus:\n  root: {}\n  run: {run}\n",
            self.root().display()
        );
        if let Some(c) = continues {
            text.push_str(&format!("  continues: {c}\n"));
        }
        fs::write(self.dep_file(), text).unwrap();
    }

    fn start_run(&self, run: &str, linked_from: Option<&str>) {
        self.start_run_with(run, linked_from, &["lead", "helper", "helper2"]);
    }

    /// A run whose frozen topology has only `names` (from lead, helper, helper2).
    fn start_run_with(&self, run: &str, linked_from: Option<&str>, names: &[&str]) {
        let members: BTreeMap<&str, Value> = [
            ("lead", None),
            ("helper", Some("lead")),
            ("helper2", Some("lead")),
        ]
        .iter()
        .filter(|(m, _)| names.contains(m))
        .map(|(m, p)| (*m, json!({"vendor": "claude", "parent": p})))
        .collect();
        let mut manifest = json!({
            "schema": 1,
            "run": run,
            "created_at": borgchart::bus::time::utc_now(),
            "deployment": borgchart::state::deployment_id(&self.dep_file()),
            "source": self.dep_file(),
            "config": {"version": 1, "digest": "00", "inputs": {}},
            "members": members,
        });
        if let Some(from) = linked_from {
            manifest["linked_from"] =
                json!({"run": from, "root": self.root(), "reason": "rotation"});
        }
        self.write_private(&self.run_dir(run).join("run.json"), &manifest);
        for m in names {
            self.incarnation(run, m, &format!("{m}-1"), false);
        }
    }

    fn write_private(&self, path: &Path, v: &Value) {
        use std::os::unix::fs::PermissionsExt;
        for dir in [self.root(), path.parent().unwrap().to_path_buf()] {
            fs::create_dir_all(&dir).unwrap();
        }
        for dir in [self.root(), self.run_dir(RUN), self.run_dir(NEXT)] {
            if dir.exists() {
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        fs::write(path, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    /// Make `incarnation` the member's current one (also kept in history).
    fn incarnation(&self, run: &str, member: &str, incarnation: &str, ended: bool) {
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
        let inc = self.run_dir(run).join("runtime/incarnations");
        self.write_private(&inc.join(format!("{member}.json")), &v);
        self.write_private(&inc.join(member).join(format!("{incarnation}.json")), &v);
    }

    /// Record that an earlier incarnation ended (history only).
    fn ended(&self, member: &str, incarnation: &str) {
        let v = json!({
            "member": member,
            "incarnation": incarnation,
            "generation": 1,
            "started_at": "2026-11-05T19:00:00.000Z",
            "ended_at": "2026-11-05T20:00:00.000Z",
            "ended_reason": "replaced",
        });
        self.write_private(
            &self
                .run_dir(RUN)
                .join(format!("runtime/incarnations/{member}/{incarnation}.json")),
            &v,
        );
    }

    fn brg(&self, who: As, args: &[&str], stdin: Option<&[u8]>) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", "/usr/bin:/bin")
            .args(args)
            .arg("-f")
            .arg(self.dep_file());
        let run = fs::read_to_string(self.dep_file())
            .unwrap()
            .lines()
            .find_map(|l| l.trim().strip_prefix("run: ").map(str::to_string))
            .unwrap();
        match who {
            As::Ops => {
                cmd.args(["--external", "ops"]);
            }
            As::Operator => {}
            As::Member(m) => {
                cmd.env("BORGCHART_MEMBER", m)
                    .env("BORGCHART_RUN", &run)
                    .env("BORGCHART_INCARNATION", format!("{m}-1"));
            }
            As::Inc(m, i) => {
                cmd.env("BORGCHART_MEMBER", m)
                    .env("BORGCHART_RUN", &run)
                    .env("BORGCHART_INCARNATION", i);
            }
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        {
            let mut input = child.stdin.take().unwrap();
            if let Some(bytes) = stdin {
                input.write_all(bytes).unwrap();
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

    fn publish(&self, who: As, key: &str, draft: &Value) -> Out {
        self.brg(
            who,
            &["msg", "publish", "--key", key, "--json"],
            Some(&serde_json::to_vec(draft).unwrap()),
        )
    }

    /// Publish and expect success; returns the record ID.
    fn ok(&self, who: As, key: &str, draft: &Value) -> String {
        let out = self.publish(who, key, draft);
        assert_eq!(out.code, 0, "{key}: {:?} {}", out.json, out.stderr);
        out.json["id"].as_str().unwrap().to_string()
    }

    /// Publish and expect a failure with this exit code.
    fn refused(&self, who: As, key: &str, draft: &Value, code: i32) -> String {
        let out = self.publish(who, key, draft);
        assert_eq!(out.code, code, "{key}: {:?} {}", out.json, out.stderr);
        out.json["message"].as_str().unwrap_or_default().to_string()
    }

    fn show(&self, task: &str) -> Value {
        let out = self.brg(As::Operator, &["task", "show", task, "--json"], None);
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        out.json
    }

    fn state(&self, task: &str) -> String {
        self.show(task)["task"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn list(&self) -> Value {
        let out = self.brg(As::Operator, &["task", "list", "--json"], None);
        assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
        out.json
    }

    fn assign(&self, to: &str, task: &str, attempt: u32) -> Value {
        json!({"to": to, "kind": "assign", "task": task, "attempt": attempt,
        "facts": {
            "agreement": {"path": self.agreement(), "sha256": self.agreement_sha()},
            "scope": "the parser module",
            "provenance": "approved in the planning thread",
            "checkout": self.dir.path().join(format!("checkout-{to}")),
        }})
    }

    /// The SHA-256 of every file under a run directory.
    fn hashes(&self, run: &str) -> BTreeMap<String, String> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().display().to_string();
                    out.insert(rel, format!("{:x}", Sha256::digest(fs::read(&p).unwrap())));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.run_dir(run), &self.run_dir(run), &mut out);
        out
    }
}

fn simple(to: &str, kind: &str, task: &str, facts: Value) -> Value {
    json!({"to": to, "kind": kind, "task": task, "facts": facts})
}

fn accept(task: &str) -> Value {
    simple("lead", "accept", task, json!({}))
}

fn result(task: &str) -> Value {
    simple(
        "lead",
        "result",
        task,
        json!({"status": "complete", "checks": [{"name": "tests", "outcome": "pass"}]}),
    )
}

fn verify(to: &str, task: &str, result: &str, outcome: &str) -> Value {
    simple(
        to,
        "verify",
        task,
        json!({"result": result, "outcome": outcome}),
    )
}

fn release(to: &str, task: &str) -> Value {
    simple(to, "release", task, json!({"evidence": "pane closed"}))
}

#[test]
fn a_task_runs_from_assign_to_release() {
    let f = Fixture::new();
    let a = f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    let show = f.show("t-1");
    let t = &show["task"];
    assert_eq!(t["state"], "assigned");
    assert_eq!(t["assignee"], "helper");
    assert_eq!(t["authority"], "lead");
    assert_eq!(t["attempt"], 1);
    assert_eq!(t["agreement"]["sha256"], f.agreement_sha());
    assert_eq!(t["scope"], "the parser module");
    assert_eq!(t["provenance"], "approved in the planning thread");
    assert_eq!(t["records"]["assign"], a);
    assert_eq!(show["next"]["actor"], "helper");
    assert_eq!(show["next"]["kinds"], json!(["accept", "decline"]));

    // Acknowledging the assignment is consumption, not acceptance.
    let ack = f.brg(As::Member("helper"), &["msg", "ack", &a, "--json"], None);
    assert_eq!(ack.code, 0, "{:?}", ack.json);
    assert_eq!(f.state("t-1"), "assigned");

    let acc = f.ok(As::Member("helper"), "c1", &accept("t-1"));
    assert_eq!(f.state("t-1"), "accepted");
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    let show = f.show("t-1");
    assert_eq!(show["task"]["state"], "result-claimed");
    assert_eq!(show["task"]["records"]["accept"], acc);
    assert_eq!(show["task"]["records"]["result"], res);
    assert_eq!(show["next"]["actor"], "lead");
    assert_eq!(show["next"]["kinds"], json!(["verify", "cancel-request"]));

    f.ok(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "accepted"),
    );
    assert_eq!(f.state("t-1"), "verified");

    // The helper's incarnation hasn't ended, so its checkout isn't free.
    let msg = f.refused(As::Member("lead"), "x1", &release("helper", "t-1"), 4);
    assert!(msg.contains("helper-1"), "{msg}");
    f.incarnation(RUN, "helper", "helper-1", true);
    f.ok(As::Member("lead"), "x1", &release("helper", "t-1"));
    let show = f.show("t-1");
    assert_eq!(show["task"]["state"], "released");
    assert_eq!(show["next"], Value::Null);

    let list = f.list();
    assert_eq!(list["store"], "ok");
    assert_eq!(list["tasks"][0]["task"], "t-1");
    assert_eq!(list["tasks"][0]["state"], "released");
    let missing = f.brg(As::Operator, &["task", "show", "nope", "--json"], None);
    assert_eq!(missing.code, 10);
}

#[test]
fn invalid_transitions_are_refused() {
    let f = Fixture::new();
    // Nothing exists yet.
    f.refused(As::Member("helper"), "k0", &accept("t-1"), 5);
    // The agreement must exist and match.
    let mut bad = f.assign("helper", "t-1", 1);
    bad["facts"]["agreement"]["sha256"] = json!("0".repeat(64));
    assert!(
        f.refused(As::Member("lead"), "k1", &bad, 5)
            .contains("agreement")
    );
    let mut gone = f.assign("helper", "t-1", 1);
    gone["facts"]["agreement"]["path"] = json!(f.dir.path().join("missing.md"));
    f.refused(As::Member("lead"), "k2", &gone, 5);
    // A first assignment is attempt 1.
    f.refused(As::Member("lead"), "k3", &f.assign("helper", "t-1", 2), 5);

    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    // Same task again; a second unresolved task for the same helper.
    f.refused(As::Member("lead"), "a2", &f.assign("helper", "t-1", 1), 5);
    assert!(
        f.refused(As::Member("lead"), "a3", &f.assign("helper", "t-2", 1), 5)
            .contains("unresolved")
    );
    // Results need an acceptance; confirmations need a request; releases
    // need a verdict or cancellation.
    f.refused(As::Member("helper"), "r0", &result("t-1"), 5);
    f.refused(
        As::Member("helper"),
        "cc0",
        &simple(
            "lead",
            "cancel-confirm",
            "t-1",
            json!({"quiescent": true, "evidence": "idle"}),
        ),
        5,
    );
    f.refused(As::Member("lead"), "x0", &release("helper", "t-1"), 5);
    // Authority kinds go to the assignee; assignee kinds to its authority.
    f.refused(
        As::Member("lead"),
        "cr0",
        &simple("helper2", "cancel-request", "t-1", json!({"reason": "r"})),
        5,
    );
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    f.refused(As::Member("helper"), "c2", &accept("t-1"), 5);
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    // A verify must name this attempt's result.
    f.refused(
        As::Member("lead"),
        "v0",
        &verify("helper", "t-1", "m1", "accepted"),
        5,
    );
    f.ok(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "accepted"),
    );
    // A decision must answer a question.
    f.refused(
        As::Member("lead"),
        "d0",
        &json!({"to": "helper", "kind": "decision", "reply_to": [res], "body": "x"}),
        5,
    );
    // ...asked by the decision's recipient: helper2's question can't be
    // closed by a decision sent to helper.
    let q = f.ok(
        As::Member("helper2"),
        "q1",
        &json!({"to": "lead", "kind": "question", "body": "which branch?"}),
    );
    f.refused(
        As::Member("lead"),
        "d0b",
        &json!({"to": "helper", "kind": "decision", "reply_to": [q], "body": "main"}),
        5,
    );
    assert_eq!(f.list()["questions"][0]["id"], q);
    // An agreement that isn't a regular file is refused without opening it
    // (a FIFO would block while the lock is held).
    let fifo = f.dir.path().join("agreement.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut piped = f.assign("helper2", "t-9", 1);
    piped["facts"]["agreement"]["path"] = json!(fifo);
    assert!(
        f.refused(As::Member("lead"), "a9", &piped, 5)
            .contains("regular file")
    );
    // A declined task is resolved: the helper may take another.
    f.ok(As::Member("lead"), "a4", &f.assign("helper2", "t-3", 1));
    f.ok(
        As::Member("helper2"),
        "d1",
        &simple("lead", "decline", "t-3", json!({"reason": "busy"})),
    );
    assert_eq!(f.state("t-3"), "declined");
    let mut other = f.assign("helper2", "t-4", 1);
    other["facts"]["checkout"] = json!(f.dir.path().join("checkout-helper"));
    assert!(
        f.refused(As::Member("lead"), "a5", &other, 5)
            .contains("checkout"),
        "a checkout held by an unresolved task can't be assigned again"
    );
    f.ok(As::Member("lead"), "a6", &f.assign("helper2", "t-4", 1));
}

#[test]
fn only_the_authority_and_the_assignee_may_act() {
    let f = Fixture::new();
    // helper reports to lead, so an outside caller is not its authority.
    let msg = f.refused(As::Ops, "a0", &f.assign("helper", "t-1", 1), 8);
    assert!(msg.contains("lead"), "{msg}");
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    // Only the assignee accepts.
    f.refused(As::Member("helper2"), "c0", &accept("t-1"), 8);
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    // The assignee can't verify its own result (it isn't the authority).
    f.refused(
        As::Member("helper"),
        "v0",
        &verify("lead", "t-1", &res, "accepted"),
        5,
    );

    // The lead is the root: an outside caller is its authority.
    f.ok(As::Ops, "a2", &f.assign("lead", "t-lead", 1));
    f.refused(As::Member("helper"), "q0", &accept("t-lead"), 8);
    let c = f.ok(
        As::Member("lead"),
        "c2",
        &simple("external:ops", "accept", "t-lead", json!({})),
    );
    assert_eq!(f.show("t-lead")["task"]["records"]["accept"], c);
    assert_eq!(f.show("t-lead")["task"]["authority"], "external");
}

#[test]
fn stale_actors_and_attempts_are_fenced() {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    // The lead is replaced: its old incarnation can no longer verify.
    f.incarnation(RUN, "lead", "lead-2", false);
    f.ended("lead", "lead-1");
    f.refused(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "rejected"),
        8,
    );
    f.ok(
        As::Inc("lead", "lead-2"),
        "v1",
        &verify("helper", "t-1", &res, "rejected"),
    );
    assert_eq!(f.state("t-1"), "rejected");
    f.ok(
        As::Inc("lead", "lead-2"),
        "o1",
        &json!({"to": "helper", "kind": "reopen", "task": "t-1", "attempt": 2,
            "facts": {"reason": "tests fail on empty input"}}),
    );
    let show = f.show("t-1");
    assert_eq!(show["task"]["state"], "reopened");
    assert_eq!(show["task"]["attempt"], 2);
    // An acceptance for the old attempt is stale.
    let mut old = accept("t-1");
    old["attempt"] = json!(1);
    f.refused(As::Member("helper"), "c2", &old, 8);
    let mut ahead = accept("t-1");
    ahead["attempt"] = json!(3);
    f.refused(As::Member("helper"), "c3", &ahead, 5);
    let mut now = accept("t-1");
    now["attempt"] = json!(2);
    f.ok(As::Member("helper"), "c4", &now);
    assert_eq!(f.state("t-1"), "accepted");
    // A verify of attempt 1's result no longer applies.
    let res2 = f.ok(As::Member("helper"), "r2", &result("t-1"));
    f.refused(
        As::Inc("lead", "lead-2"),
        "v2",
        &verify("helper", "t-1", &res, "accepted"),
        5,
    );
    f.ok(
        As::Inc("lead", "lead-2"),
        "v3",
        &verify("helper", "t-1", &res2, "accepted"),
    );
    let history = f.show("t-1")["task"]["history"].as_array().unwrap().len();
    assert_eq!(history, 8);
}

#[test]
fn an_unanswered_question_survives_lead_replacement() {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    let q = f.ok(
        As::Member("helper"),
        "q1",
        &json!({"to": "lead", "kind": "question", "task": "t-1", "body": "Keep the old API?"}),
    );
    f.incarnation(RUN, "lead", "lead-2", false);
    f.ended("lead", "lead-1");
    // The new lead finds it from the task, not from its own (empty) memory.
    let show = f.show("t-1");
    assert_eq!(show["questions"][0]["id"], q);
    assert_eq!(show["questions"][0]["from"], "helper");
    assert_eq!(f.list()["questions"][0]["id"], q);
    let d = json!({"to": "helper", "kind": "decision", "task": "t-1", "reply_to": [q],
        "body": "Yes, keep it."});
    // Only the current lead may decide.
    f.refused(As::Member("lead"), "d1", &d, 8);
    f.ok(As::Inc("lead", "lead-2"), "d1", &d);
    assert_eq!(f.show("t-1")["questions"], json!([]));
    assert_eq!(f.list()["questions"], json!([]));
}

#[test]
fn rejected_and_cancelled_tasks() {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    f.ok(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "rejected"),
    );
    assert_eq!(f.state("t-1"), "rejected");
    f.ok(
        As::Member("lead"),
        "o1",
        &json!({"to": "helper", "kind": "reopen", "task": "t-1",
            "facts": {"reason": "try again"}}),
    );
    f.ok(As::Member("helper"), "c2", &accept("t-1"));
    f.ok(
        As::Member("lead"),
        "cr1",
        &simple(
            "helper",
            "cancel-request",
            "t-1",
            json!({"reason": "plan changed"}),
        ),
    );
    let show = f.show("t-1");
    assert_eq!(show["task"]["state"], "cancel-requested");
    assert_eq!(show["next"]["actor"], "helper");
    f.ok(
        As::Member("helper"),
        "cc1",
        &simple(
            "lead",
            "cancel-confirm",
            "t-1",
            json!({"quiescent": true, "evidence": "stopped; no edits since m6"}),
        ),
    );
    assert_eq!(f.state("t-1"), "cancelled");
    // Still unresolved until released.
    f.refused(As::Member("lead"), "a2", &f.assign("helper", "t-2", 1), 5);
    f.refused(As::Member("lead"), "x1", &release("helper", "t-1"), 4);
    f.incarnation(RUN, "helper", "helper-1", true);
    f.ok(As::Member("lead"), "x1", &release("helper", "t-1"));
    f.ok(As::Member("lead"), "a2", &f.assign("helper", "t-2", 1));
}

#[test]
fn replacement_keeps_pending_work() {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    // The helper is replaced while the task is accepted.
    f.incarnation(RUN, "helper", "helper-2", false);
    f.ended("helper", "helper-1");
    let show = f.show("t-1");
    assert_eq!(show["task"]["state"], "accepted");
    let writers = &show["task"]["writers"];
    assert_eq!(writers[0]["incarnation"], "helper-1");
    assert_eq!(writers[0]["current"], false);
    assert_eq!(writers[0]["liveness"], "gone");
    // The old incarnation is fenced; the new one carries on.
    f.refused(As::Member("helper"), "r1", &result("t-1"), 8);
    let res = f.ok(As::Inc("helper", "helper-2"), "r1", &result("t-1"));
    f.ok(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "accepted"),
    );
    // helper-1 is gone, but helper-2 may still be writing.
    let msg = f.refused(As::Member("lead"), "x1", &release("helper", "t-1"), 4);
    assert!(msg.contains("helper-2"), "{msg}");
    f.incarnation(RUN, "helper", "helper-2", true);
    f.ok(As::Member("lead"), "x1", &release("helper", "t-1"));
}

#[test]
fn release_waits_until_the_writer_is_gone() {
    use borgchart::bus::{record::Actor, store, tasks};
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    f.ok(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "accepted"),
    );
    let mut report = borgchart::diag::Report::default();
    let deployment = borgchart::deploy::load(&f.dep_file(), &mut report).unwrap();
    let bus = store::open(&deployment).unwrap();
    let snap = store::scan(&bus.paths, RUN);
    let lead = Actor::Member {
        member: "lead".into(),
        incarnation: "lead-1".into(),
    };
    let draft =
        store::parse_draft(&serde_json::to_vec(&release("helper", "t-1")).unwrap()).unwrap();
    for (liveness, allowed) in [
        (Liveness::Alive, false),
        (Liveness::Unknown, false),
        (Liveness::Gone, true),
    ] {
        let probe = |_: &str, _: &str| liveness;
        let out = tasks::check_publish(&bus, &snap, &lead, &draft, &probe);
        assert_eq!(out.is_ok(), allowed, "{liveness:?}: {out:?}");
        if let Err(e) = out {
            assert_eq!(e.code, borgchart::bus::outcome::Code::Refused);
        }
    }
}

#[test]
fn a_corrupt_store_stops_task_mutations_but_still_lists() {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    f.ok(As::Member("helper"), "r1", &result("t-1"));
    // Record 4 is garbage and record 5 follows it.
    let records = f.run_dir(RUN).join("bus/records");
    fs::write(records.join("0000000004.json"), "{not json").unwrap();
    let mut five: Value =
        serde_json::from_slice(&fs::read(records.join("0000000003.json")).unwrap()).unwrap();
    five["id"] = json!("m5");
    five["seq"] = json!(5);
    five["key"] = json!("r9");
    fs::write(records.join("0000000005.json"), five.to_string()).unwrap();

    f.refused(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", "m3", "accepted"),
        7,
    );
    let list = f.list();
    assert_eq!(list["store"], "incomplete");
    assert_eq!(list["through"], "m3");
    assert_eq!(list["tasks"][0]["state"], "result-claimed");
    let text = f.brg(As::Operator, &["task", "list"], None);
    assert_eq!(text.code, 0);
    assert!(
        text.json.as_str().unwrap().contains("incomplete"),
        "{:?}",
        text.json
    );
}

/// Old run `RUN` with: an accepted task, a released task, an open question,
/// an unacknowledged note and an acknowledged one.
fn populated() -> Fixture {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper2", "t-done", 1));
    f.ok(
        As::Member("helper2"),
        "d1",
        &simple("lead", "decline", "t-done", json!({"reason": "busy"})),
    );
    f.ok(As::Member("lead"), "a2", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    f.ok(
        As::Member("helper"),
        "q1",
        &json!({"to": "lead", "kind": "question", "task": "t-1", "body": "Which format?"}),
    );
    f.ok(
        As::Ops,
        "n1",
        &json!({"to": "lead", "kind": "note", "body": "FYI"}),
    );
    let seen = f.ok(
        As::Ops,
        "n2",
        &json!({"to": "helper2", "kind": "note", "body": "read me"}),
    );
    let ack = f.brg(
        As::Member("helper2"),
        &["msg", "ack", &seen, "--json"],
        None,
    );
    assert_eq!(ack.code, 0);
    f
}

/// Rotate the deployment into `NEXT`, continuing `RUN`.
fn rotate(f: &Fixture) {
    f.configure(NEXT, Some(RUN));
    f.start_run(NEXT, Some(RUN));
}

fn carry(f: &Fixture) -> Out {
    f.brg(
        As::Operator,
        &["bus", "carry", "--from", RUN, "--json"],
        None,
    )
}

#[test]
fn carrying_into_a_linked_run_preserves_the_original() {
    let f = populated();
    let before = f.hashes(RUN);
    rotate(&f);
    let check = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert!(
        check.json["warnings"].to_string().contains("bus carry"),
        "{:?}",
        check.json
    );

    let out = carry(&f);
    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
    assert_eq!(out.json["outcome"], "carried");
    assert_eq!(out.json["incomplete"], false);
    let carried = out.json["carried"].as_array().unwrap();
    let summary: Vec<(String, String, String)> = carried
        .iter()
        .map(|c| {
            (
                c["source"].as_str().unwrap().to_string(),
                c["reason"].as_str().unwrap().to_string(),
                c["to"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    // The accepted task (by its assign), the open question, the unacked
    // records not already covered. Not the declined task's records, nor
    // the acknowledged note.
    assert_eq!(
        summary,
        vec![
            ("m3".into(), "unresolved-task".into(), "helper".into()),
            ("m4".into(), "unacked".into(), "lead".into()),
            ("m5".into(), "open-question".into(), "lead".into()),
            ("m6".into(), "unacked".into(), "lead".into()),
        ],
        "{carried:?}"
    );
    // The new run's records are notes by the bus, linked to the originals.
    let rec = f.brg(As::Operator, &["msg", "read", "m1", "--json"], None);
    let r = &rec.json["record"];
    assert_eq!(r["actor"], json!({"type": "bus"}));
    assert_eq!(r["kind"], "note");
    assert_eq!(r["to"], "helper");
    let original = fs::read(f.run_dir(RUN).join("bus/records/0000000003.json")).unwrap();
    assert_eq!(
        r["facts"]["carried_from"],
        json!({"run": RUN, "id": "m3", "sha256": format!("{:x}", Sha256::digest(&original))})
    );
    assert_eq!(r["facts"]["task"], "t-1");

    // Again: nothing new.
    let again = carry(&f);
    assert_eq!(again.code, 0, "{:?}", again.json);
    assert!(
        again.json["carried"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["outcome"] == "duplicate"),
        "{:?}",
        again.json
    );
    assert_eq!(
        fs::read_dir(f.run_dir(NEXT).join("bus/records"))
            .unwrap()
            .count(),
        4
    );
    // The old run is byte-for-byte unchanged.
    assert_eq!(f.hashes(RUN), before);

    // The new run no longer asks for a carry; the old run says where it went.
    let check = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert!(!check.json["warnings"].to_string().contains("bus carry"));
    f.configure(RUN, None);
    let old = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(old.code, 0, "{:?}", old.json);
    assert_eq!(old.json["carried_into"][0]["into"], NEXT);
    assert_eq!(f.hashes(RUN), before);
}

#[test]
fn carrying_from_a_corrupt_run_stops_at_the_first_bad_record() {
    let f = populated();
    fs::remove_file(f.run_dir(RUN).join("bus/records/0000000005.json")).unwrap();
    let before = f.hashes(RUN);
    rotate(&f);
    let out = carry(&f);
    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
    assert_eq!(out.json["outcome"], "partial");
    assert_eq!(out.json["incomplete"], true);
    assert_eq!(out.json["through"], "m4");
    let sources: Vec<&str> = out.json["carried"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["m3", "m4"]);
    let text = f.brg(As::Operator, &["bus", "carry", "--from", RUN], None);
    assert!(text.stderr.contains("incomplete"), "{}", text.stderr);
    assert_eq!(f.hashes(RUN), before);
}

#[test]
fn carry_needs_the_linked_run() {
    let f = populated();
    // The deployment doesn't continue RUN.
    f.configure(NEXT, None);
    f.start_run(NEXT, None);
    assert_eq!(carry(&f).code, 5);
    f.configure(NEXT, Some("example-run-0"));
    assert_eq!(carry(&f).code, 5);
}

#[test]
fn an_aging_run_rotates_into_a_new_one() {
    let f = populated();
    let manifest = f.run_dir(RUN).join("run.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    m["created_at"] = json!(borgchart::bus::time::format_utc(
        std::time::SystemTime::now() - Duration::from_secs(8 * 86_400 + 60)
    ));
    fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let check = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 0);
    assert!(check.json["warnings"].to_string().contains("days old"));

    rotate(&f);
    assert_eq!(carry(&f).code, 0);
    // In the new run the lead re-authorizes the carried task explicitly.
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    assert_eq!(f.state("t-1"), "assigned");
    let check = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(check.code, 0, "{:?}", check.json);
}

fn reopen(task: &str) -> Value {
    simple("helper", "reopen", task, json!({"reason": "again"}))
}

/// helper-1 accepts `t-1` (checkout X), claims a result; lead rejects and
/// reopens it. helper-1 never ends.
fn rejected_and_reopened(f: &Fixture) {
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(As::Member("helper"), "c1", &accept("t-1"));
    let res = f.ok(As::Member("helper"), "r1", &result("t-1"));
    f.ok(
        As::Member("lead"),
        "v1",
        &verify("helper", "t-1", &res, "rejected"),
    );
    f.ok(As::Member("lead"), "o1", &reopen("t-1"));
}

/// An `assign` of another task to helper2, on helper's checkout path.
fn on_helpers_checkout(f: &Fixture, task: &str) -> Value {
    let mut a = f.assign("helper2", task, 1);
    a["facts"]["checkout"] = json!(f.dir.path().join("checkout-helper"));
    a
}

#[test]
fn review_reopen_checkout_collision() {
    let f = Fixture::new();
    f.ok(As::Member("lead"), "a1", &f.assign("helper", "t-1", 1));
    f.ok(
        As::Member("helper"),
        "d1",
        &simple("lead", "decline", "t-1", json!({"reason": "busy"})),
    );
    // A fresh decline holds nothing, so the checkout can go to helper2...
    f.ok(As::Member("lead"), "a2", &on_helpers_checkout(&f, "t-2"));
    // ...and then t-1 can't be reopened onto it.
    let msg = f.refused(As::Member("lead"), "o1", &reopen("t-1"), 5);
    assert!(msg.contains("checkout"), "{msg}");
    assert_eq!(f.state("t-1"), "declined");
}

#[test]
fn review_decline_discards_live_writer_hold() {
    let f = Fixture::new();
    rejected_and_reopened(&f);
    // helper is replaced without helper-1 ending; helper-2 declines attempt 2.
    f.incarnation(RUN, "helper", "helper-2", false);
    f.ok(
        As::Inc("helper", "helper-2"),
        "d2",
        &simple("lead", "decline", "t-1", json!({"reason": "not mine"})),
    );
    let show = f.show("t-1");
    assert_eq!(show["task"]["state"], "declined");
    // helper-1 may still be writing: the task still holds its checkout and
    // its assignee until a release finds every writer gone.
    assert_eq!(show["next"]["kinds"], json!(["release", "reopen"]));
    assert!(
        f.refused(As::Member("lead"), "a2", &on_helpers_checkout(&f, "t-2"), 5)
            .contains("checkout")
    );
    f.refused(As::Member("lead"), "a3", &f.assign("helper", "t-3", 1), 5);
    assert!(
        f.refused(As::Member("lead"), "x1", &release("helper", "t-1"), 4)
            .contains("helper-1")
    );
    f.ended("helper", "helper-1");
    f.ok(As::Member("lead"), "x1", &release("helper", "t-1"));
    f.ok(As::Member("lead"), "a2", &on_helpers_checkout(&f, "t-2"));
}

#[test]
fn review_repin_discards_live_checkout() {
    let f = Fixture::new();
    rejected_and_reopened(&f);
    // Re-pinning attempt 2 can't move or drop checkout X while helper-1,
    // which wrote there, may still be running.
    let mut moved = f.assign("helper", "t-1", 2);
    moved["facts"]["checkout"] = json!(f.dir.path().join("checkout-y"));
    assert!(
        f.refused(As::Member("lead"), "p1", &moved, 4)
            .contains("helper-1")
    );
    let mut dropped = f.assign("helper", "t-1", 2);
    dropped["facts"].as_object_mut().unwrap().remove("checkout");
    f.refused(As::Member("lead"), "p2", &dropped, 4);
    f.refused(As::Member("lead"), "a2", &on_helpers_checkout(&f, "t-2"), 5);
    // Once helper-1 is gone the checkout may move, and X is free.
    f.incarnation(RUN, "helper", "helper-2", false);
    f.ended("helper", "helper-1");
    f.ok(As::Member("lead"), "p1", &moved);
    f.ok(As::Member("lead"), "a2", &on_helpers_checkout(&f, "t-2"));
}

/// Make `run` look `days` old.
fn age(f: &Fixture, run: &str, days: u64) {
    let manifest = f.run_dir(run).join("run.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    m["created_at"] = json!(borgchart::bus::time::format_utc(
        std::time::SystemTime::now() - Duration::from_secs(days * 86_400 + 60)
    ));
    fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
}

#[test]
fn review_skipped_carry_is_marked_complete() {
    let f = populated();
    age(&f, RUN, 15);
    // The new run has no `helper`, so its task can't be carried.
    f.configure(NEXT, Some(RUN));
    f.start_run_with(NEXT, Some(RUN), &["lead", "helper2"]);
    let out = carry(&f);
    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
    assert_eq!(out.json["outcome"], "partial");
    assert_eq!(out.json["incomplete"], true);
    let skipped: Vec<&Value> = out.json["carried"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["outcome"] == "skipped")
        .collect();
    assert_eq!(skipped[0]["source"], "m3", "{:?}", out.json);
    let text = f.brg(As::Operator, &["bus", "carry", "--from", RUN], None);
    assert!(text.stderr.contains("skipped"), "{}", text.stderr);
    // The new run still says the carry is unfinished.
    let check = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert!(
        check.json["warnings"].to_string().contains("partial"),
        "{:?}",
        check.json
    );
    // The old run is not retired: its age is still an error.
    f.configure(RUN, None);
    let old = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(old.code, 1, "{:?}", old.json);
    assert!(old.json["errors"].to_string().contains("max_age_days"));
}

#[test]
fn review_late_record_does_not_invalidate_retirement() {
    let f = populated();
    rotate(&f);
    assert_eq!(carry(&f).json["outcome"], "carried");
    // Back in the old run, something arrives after the carry.
    f.configure(RUN, None);
    f.ok(
        As::Ops,
        "n9",
        &json!({"to": "lead", "kind": "note", "body": "late"}),
    );
    age(&f, RUN, 15);
    let old = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(old.code, 1, "{:?}", old.json);
    assert!(
        old.json["warnings"].to_string().contains("m8"),
        "{:?}",
        old.json
    );
    // Carrying again covers it, and the old run is retired.
    f.configure(NEXT, Some(RUN));
    let again = carry(&f);
    assert!(
        again.json["carried"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["source"] == "m8" && c["outcome"] == "committed"),
        "{:?}",
        again.json
    );
    f.configure(RUN, None);
    let old = f.brg(As::Operator, &["bus", "check", "--json"], None);
    assert_eq!(old.code, 0, "{:?}", old.json);
}
