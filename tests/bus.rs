//! Record store tests: `brg msg`, `brg msg list` and `brg bus` against a team's
//! message storage built from store identity, start metadata and incarnations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[cfg(feature = "test-support")]
mod common;

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
    /// A member with an explicit incarnation.
    MemberWith(&'static str, &'static str),
}

impl Fixture {
    /// A deployment with a bus and a started team: `lead` (the root) with
    /// children `helper` and `helper2`, and `sub` under `helper`.
    fn new() -> Self {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        f.register();
        f.write_store(TEAM);
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
    fn write_store(&self, instance: &str) {
        self.write_private(
            &self.store_dir().join("store.json"),
            &json!({"format": borgchart::bus::SCHEMA, "root": instance}),
        );
        let parents = [
            ("lead", None),
            ("helper", Some("lead")),
            ("helper2", Some("lead")),
            ("sub", Some("helper")),
        ];
        let members: BTreeMap<&str, Value> = parents.iter().map(|(m, p)| (*m,
            json!({"role": m, "vendor": "claude", "posture": "vendor", "interactive": false, "parent": p, "vendor_version": null, "started": true})
        )).collect();
        self.write_private(&self.root().join(TEAM).join("start.json"), &json!({
            "team_dir": self.dir.path().join("oncall"), "folder": self.dir.path().join("work"),
            "brg": env!("CARGO_BIN_EXE_brg"), "config": {"version": 1, "digest": "00", "inputs": {}, "team_dirs": {}},
            "socket": "brg-bus-example", "windows_started": true,
            "base_env": {}, "members": members, "plugins": {},
        }));
    }

    fn incarnation_file(&self, member: &str) -> PathBuf {
        self.root()
            .join(TEAM)
            .join("members")
            .join(member)
            .join("incarnation.json")
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
        self.write_private(&self.incarnation_file(member), &v);
    }

    fn command(&self, who: &As, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("BORGCHART_STATE_HOME", self.root())
            .env("BORGCHART_CONFIG", self.dir.path().join("config.yaml"))
            .args(args);
        if !args.starts_with(&["msg", "send"]) {
            cmd.args(["--instance", TEAM]);
        }
        let (m, inc) = match who {
            As::Operator => {
                cmd.env("BORGCHART_WAIT_MIN_MS", "20")
                    .env("BORGCHART_WAIT_TIMEOUT_MS", "1000");
                return cmd;
            }
            As::Member(m) => (m.to_string(), format!("{m}-1")),
            As::MemberWith(m, i) => (m.to_string(), i.to_string()),
        };
        cmd.env("BORGCHART_INSTANCE", TEAM)
            .env("BORGCHART_MEMBER", m)
            .env("BORGCHART_INCARNATION", inc);
        cmd.env("BORGCHART_WAIT_MIN_MS", "20")
            .env("BORGCHART_WAIT_TIMEOUT_MS", "1000");
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

    fn publish(&self, who: &As, draft: &Value) -> Out {
        let target = format!("{TEAM}/{}", draft["to"].as_str().unwrap());
        let mut args = vec!["msg", "send", &target, "-", "--json"];
        if let As::Operator = who {
            args.extend(["--external", "ops"]);
        }
        if let Some(refs) = draft["reply_to"].as_array() {
            for reference in refs {
                args.extend(["--reply-to", reference.as_str().unwrap()]);
            }
        }
        self.brg(
            who,
            &args,
            Some(draft["body"].as_str().unwrap_or("x").as_bytes()),
        )
    }

    fn records(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.store_dir().join("records"))
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
        let mut args = vec!["msg", "list", "--json"];
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
    assert!(!text.contains("/messages/"), "{text}");
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
    json!({"to": to, "body": body})
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
fn records_on_disk_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "x")).code, 0);
    let rec = f.store_dir().join("records/0000000001.json");
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
fn invalid_drafts_write_nothing() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "x")).code, 0);
    let cases: Vec<(&str, Value)> = vec![
        ("bad recipient", json!({"to": "Lead", "body": "bad"})),
        (
            "too many refs",
            json!({"to": "lead", "reply_to": (1..=33).map(|n| format!("m{n}")).collect::<Vec<_>>()}),
        ),
        ("bad ref", json!({"to": "lead", "reply_to": ["x1"]})),
        ("unknown ref", json!({"to": "lead", "reply_to": ["m7"]})),
        ("body too big", note("lead", &"x".repeat(64 * 1024 + 1))),
        ("control char", note("lead", "bell\u{7}")),
        ("not a member", note("nobody", "x")),
    ];
    for (name, draft) in &cases {
        let out = f.publish(&As::Operator, draft);
        assert_eq!(
            out.code,
            if *name == "bad recipient" {
                2
            } else if *name == "not a member" {
                3
            } else {
                5
            },
            "{name}: {:?} {}",
            out.json,
            out.stderr
        );
        assert!(!out.json["ok"].as_bool().unwrap(), "{name}");
    }
    // Topology: members publish only along parent edges.
    for (from, to) in [
        ("helper", "helper2"),
        ("sub", "lead"),
        ("helper", "external:ops"),
    ] {
        let out = f.publish(&As::Member(from), &note(to, "x"));
        assert_eq!(out.code, 5, "{from} -> {to}: {:?}", out.json);
    }
    let out = f.publish(&As::Operator, &note("external:other", "x"));
    assert_eq!(out.code, 5, "external -> external");
    let garbage = f.brg(
        &As::Operator,
        &[
            "msg",
            "send",
            "oncall-2a3b/lead",
            "-",
            "--external",
            "ops",
            "--json",
        ],
        Some(b"/invalid command"),
    );
    assert_eq!(garbage.code, 5);
    let huge = f.brg(
        &As::Operator,
        &[
            "msg",
            "send",
            "oncall-2a3b/lead",
            "-",
            "--external",
            "ops",
            "--json",
        ],
        Some(&vec![b' '; 200 * 1024]),
    );
    assert_eq!(huge.code, 2);
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
                    let out = f.publish(&who, &note("lead", &format!("{w}/{i}")));
                    assert_eq!(out.code, 0, "{:?} {}", out.json, out.stderr);
                }
            });
        }
    });
    let expected: Vec<String> = (1..=writers * per)
        .map(|n| format!("{n:010}.json"))
        .collect();
    assert_eq!(f.records(), expected);
    let all = f.inbox(&As::Operator, &["--member", "lead", "--full"]);
    let bodies: BTreeSet<String> = all["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["body"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(bodies.len(), writers * per, "no lost or duplicate records");
    let check = f.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(check.code, 0, "{:?}", check.json);
}

#[test]
fn crash_leftovers_are_tolerated() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "one")).code, 0);
    // A crash before linking leaves a stray temporary file.
    fs::write(f.store_dir().join("records/.tmp-999-0-1"), "partial").unwrap();
    let next = f.publish(&As::Operator, &note("lead", "two"));
    assert_eq!(next.json["id"], "m2");

    let check = f.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(check.code, 0, "{:?}", check.json);
    let warnings = check.json["warnings"].to_string();
    assert!(warnings.contains(".tmp-999-0-1"), "{warnings}");
}

#[test]
fn corrupt_store_stops_mutations_but_not_reads() {
    for damage in ["garbage", "gap", "bad-ack"] {
        let f = Fixture::new();
        for k in ["k1", "k2", "k3"] {
            assert_eq!(f.publish(&As::Operator, &note("lead", k)).code, 0);
        }
        let records = f.store_dir().join("records");
        match damage {
            "garbage" => fs::write(records.join("0000000002.json"), "{oops").unwrap(),
            "gap" => fs::remove_file(records.join("0000000002.json")).unwrap(),
            _ => {
                fs::create_dir_all(f.store_dir().join("acks")).unwrap();
                fs::write(f.store_dir().join("acks/m1.json"), "[]").unwrap()
            }
        }
        let out = f.publish(&As::Operator, &note("lead", "x"));
        assert_eq!(out.code, 5, "{damage}: {:?}", out.json);
        assert_eq!(out.json["outcome"], "refused");
        let ack = f.brg(&As::Member("lead"), &["msg", "ack", "m3", "--json"], None);
        assert_eq!(ack.code, 7, "{damage}");

        let inbox = f.inbox(&As::Operator, &["--member", "lead"]);
        assert_eq!(inbox["store"], "incomplete", "{damage}");
        assert!(ids(&inbox).contains(&"m3".to_string()));
        let check = f.brg(&As::Operator, &["msg", "check", "--json"], None);
        assert_eq!(check.code, 7, "{damage}: {:?}", check.json);
        assert!(!check.json["errors"].as_array().unwrap().is_empty());
    }
}

#[test]
fn acks_are_idempotent_and_recipient_only() {
    let f = Fixture::new();
    f.publish(&As::Operator, &note("lead", "a"));
    f.publish(&As::Operator, &note("lead", "b"));
    f.publish(&As::Operator, &note("helper", "c"));

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
    assert!(!f.store_dir().join("acks/m3.json").exists());
    let read = f.brg(&As::Operator, &["msg", "read", "m1", "--json"], None);
    assert_eq!(read.json["record"]["acked"], true);
}

#[test]
fn caller_context_is_checked() {
    let f = Fixture::new();
    // `--external` from inside a member.
    let out = f.brg(
        &As::Member("lead"),
        &[
            "msg",
            "send",
            "oncall-2a3b/lead",
            "-",
            "--external",
            "ops",
            "--json",
        ],
        Some(b"x"),
    );
    assert_eq!(out.code, 5, "{:?}", out.json);
    let out = f.brg(
        &As::Member("lead"),
        &["msg", "list", "--external", "ops", "--json"],
        None,
    );
    assert_eq!(out.code, 4);
    // An operator can't publish.
    let out = f.brg(
        &As::Operator,
        &["msg", "send", "oncall-2a3b/lead", "-", "--json"],
        Some(b"x"),
    );
    assert_eq!(out.code, 2);
    // A partial member context.
    let mut cmd = f.command(&As::Operator, &["msg", "ack", "m1", "--json"]);
    let out = cmd.env("BORGCHART_MEMBER", "lead").output().unwrap();
    assert_eq!(out.status.code(), Some(4));
    // Operators need to name an inbox.
    assert_eq!(
        f.brg(&As::Operator, &["msg", "list", "--json"], None).code,
        2
    );

    let fenced = [
        As::MemberWith("helper", "helper-0"),
        As::MemberWith("stranger", "stranger-1"),
    ];
    for who in &fenced {
        let out = f.publish(who, &note("lead", "x"));
        assert_eq!(out.code, 5, "{:?}", out.json);
        assert_eq!(out.json["outcome"], "refused");
        assert_eq!(f.brg(who, &["msg", "list", "--json"], None).code, 8);
        assert_eq!(f.brg(who, &["msg", "ack", "m1", "--json"], None).code, 8);
    }
    f.write_incarnation("helper2", "helper2-1", true);
    assert_eq!(
        f.publish(&As::Member("helper2"), &note("lead", "x")).code,
        5
    );
    fs::remove_file(f.incarnation_file("sub")).unwrap();
    assert_eq!(f.publish(&As::Member("sub"), &note("helper", "x")).code, 5);
    assert!(f.records().is_empty());
    // Reading someone's inbox by name needs no live identity.
    let out = f.brg(
        &As::MemberWith("helper", "x"),
        &["msg", "list", "--member", "lead", "--json"],
        None,
    );
    assert_eq!(out.code, 0);
}

#[test]
fn a_replaced_or_stopped_member_is_fenced_in_team_terms() {
    let f = Fixture::new();
    // A member whose session was replaced, and one that was stopped.
    let out = f.publish(&As::MemberWith("helper", "helper-0"), &note("lead", "x"));
    assert!(
        out.json["message"]
            .as_str()
            .unwrap()
            .starts_with("oncall-2a3b/helper "),
        "{:?}",
        out.json
    );
    f.write_incarnation("helper2", "helper2-1", true);
    let out = f.publish(&As::Member("helper2"), &note("lead", "x"));
    assert_eq!(out.code, 5);
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
fn another_store_format_refuses_all_commands() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    let identity_path = f.store_dir().join("store.json");
    let mut m: Value = serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    m["format"] = json!(999);
    fs::write(&identity_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let before = f.tree();

    let commands: Vec<(As, Vec<&str>)> = vec![
        (
            As::Operator,
            vec!["msg", "send", "oncall-2a3b/lead", "-", "--external", "ops"],
        ),
        (As::Member("lead"), vec!["msg", "ack", "m1"]),
        (As::Member("lead"), vec!["msg", "list"]),
        (As::Operator, vec!["msg", "list", "--member", "lead"]),
        (As::Member("lead"), vec!["msg", "wait", "--timeout", "60"]),
        (As::Operator, vec!["msg", "read", "m1"]),
        (As::Operator, vec!["msg", "check"]),
        (As::Operator, vec!["msg", "check"]),
    ];
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();
    for (who, args) in &commands {
        let mut json_args = args.clone();
        json_args.push("--json");
        let out = f.brg(who, &json_args, Some(&draft));
        assert_eq!(
            out.code,
            if args.starts_with(&["msg", "send"]) {
                5
            } else {
                7
            },
            "{args:?}: {:?}",
            out.json
        );
        assert_eq!(
            out.json["outcome"],
            if args.starts_with(&["msg", "send"]) {
                "refused"
            } else {
                "corrupt"
            },
            "{args:?}"
        );
        assert!(out.json["message"].as_str().unwrap().contains("damaged"));
        let text = f.brg(who, args, Some(&draft));
        assert_eq!(
            text.code,
            if args.starts_with(&["msg", "send"]) {
                5
            } else {
                7
            },
            "{args:?}"
        );
        assert!(text.stderr.contains("damaged"));
    }
    assert_eq!(f.tree(), before, "nothing written");
}

#[test]
fn inbox_wait_rechecks_the_member() {
    let f = Fixture::new();
    let child = spawn(
        &f,
        &As::Member("lead"),
        &["msg", "wait", "--timeout", "60", "--json"],
        b"",
    );
    std::thread::sleep(Duration::from_millis(200));
    f.write_incarnation("lead", "lead-2", false);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
}

#[cfg(feature = "test-support")]
#[test]
fn background_wait_ends_when_its_fake_agent_exits() {
    for locked in ["none", "initial scan", "ack refresh"] {
        let f = Fixture::new();
        let tmux = common::PrivateTmux::new();
        let socket = tmux.socket("wait-lifetime");
        let env = f.command(&As::Member("lead"), &[]);
        let mut start = tmux.tmux(&socket);
        start.envs(env.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
        let gate = f.dir.path().join("refresh");
        if locked == "ack refresh" {
            fs::create_dir(&gate).unwrap();
            start.env("BORGCHART_TEST_WAIT_BEFORE_ACK_REFRESH", &gate);
        }
        start
            .env_remove("BORGCHART_WAIT_MIN_MS")
            .env("PATH", tmux.path_with("/usr/bin:/bin"))
            .env("BORGCHART_BIN", env!("CARGO_BIN_EXE_brg"))
            .env("BORGCHART_WAIT_TIMEOUT_MS", "60000")
            .args([
                "new-session",
                "-d",
                "-s",
                "agent",
                env!("CARGO_BIN_EXE_fake-agent"),
            ]);
        assert!(start.output().unwrap().status.success());
        assert!(
            tmux.tmux(&socket)
                .args(["set-window-option", "-t", "agent:0", "remain-on-exit", "on"])
                .output()
                .unwrap()
                .status
                .success()
        );
        let pane = tmux
            .tmux(&socket)
            .args([
                "display-message",
                "-p",
                "#{socket_path} #{pane_id} #{pane_pid}",
            ])
            .output()
            .unwrap();
        assert!(pane.status.success());
        let pane = String::from_utf8(pane.stdout).unwrap();
        let parts: Vec<_> = pane.split_whitespace().collect();
        let path = f.incarnation_file("lead");
        let mut inc: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        inc["pane"] = json!({"socket": parts[0], "session": "agent", "window": "0",
            "pane_id": parts[1], "pane_pid": parts[2].parse::<u32>().unwrap()});
        f.write_private(&path, &inc);
        let mut lock = (locked == "initial scan").then(|| hold(&f.store_dir().join("lock")));
        let send = |text: &str| {
            assert!(
                tmux.tmux(&socket)
                    .args(["send-keys", "-l", text])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            assert!(
                tmux.tmux(&socket)
                    .args(["send-keys", "Enter"])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        // The fake's tool starts an orphanable wait, just as an agent does.
        // Ignore terminal hangup so the test proves brg's own lifetime check.
        send(
            "tool (trap '' HUP; \"$BORGCHART_BIN\" msg wait --timeout 60 --json >\"$HOME/wait.json\" 2>\"$HOME/wait.err\" & p=$!; echo $p >\"$HOME/wait.pid.tmp\" && mv \"$HOME/wait.pid.tmp\" \"$HOME/wait.pid\"; wait $p; code=$?; echo $code >\"$HOME/wait.code.tmp\" && mv \"$HOME/wait.code.tmp\" \"$HOME/wait.code\") </dev/null >/dev/null 2>&1 &",
        );
        let until = Instant::now() + Duration::from_secs(5);
        let pid = f.dir.path().join("wait.pid");
        while !pid.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(pid.exists(), "fake agent did not start its wait");
        struct Cleanup(Option<PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(path) = &self.0
                    && let Ok(pid) = fs::read_to_string(path)
                {
                    let _ = common::command("kill").args(["-KILL", pid.trim()]).output();
                }
            }
        }
        let mut cleanup = Cleanup(Some(pid));
        let code = f.dir.path().join("wait.code");
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!code.exists(), "a running member's wait must continue");
        if locked == "ack refresh" {
            assert_eq!(f.publish(&As::Operator, &note("lead", "new")).code, 0);
            let until = Instant::now() + Duration::from_secs(5);
            while !gate.join("ready").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                gate.join("ready").exists(),
                "wait did not reach ack refresh"
            );
            lock = Some(hold(&f.store_dir().join("lock")));
            fs::write(gate.join("proceed"), "").unwrap();
        }
        send("crash 3");
        let until = Instant::now() + Duration::from_secs(5);
        while !code.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(code.exists(), "orphaned wait did not end (locked={locked})");
        assert_eq!(fs::read_to_string(code).unwrap().trim(), "8");
        let result: Value =
            serde_json::from_slice(&fs::read(f.dir.path().join("wait.json")).unwrap()).unwrap();
        assert_eq!(result["outcome"], "fenced");
        assert!(
            result["message"].as_str().unwrap().contains("ended"),
            "{result}"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
            inc,
            "wait must not write lifecycle state"
        );
        cleanup.0 = None; // The recorded wait has already exited.
        drop(lock);
    }
}

#[test]
fn inbox_wait_keeps_unknown_members_and_external_callers_waiting() {
    let f = Fixture::new();
    // No pane identity means Unknown, not Gone.
    for (who, args) in [
        (
            As::Member("lead"),
            vec!["msg", "wait", "--timeout", "60", "--json"],
        ),
        (
            As::Operator,
            vec![
                "msg",
                "wait",
                "--external",
                "ops",
                "--timeout",
                "60",
                "--json",
            ],
        ),
    ] {
        let start = Instant::now();
        let out = f.brg(&who, &args, None);
        assert_eq!(out.code, 9, "{}", out.json);
        assert!(start.elapsed() >= Duration::from_millis(900));
    }
}

#[test]
fn inbox_wait_is_bounded_while_a_writer_holds_the_lock() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    let lock = hold(&f.store_dir().join("lock"));
    let start = Instant::now();
    let mut child = spawn(
        &f,
        &As::Member("lead"),
        &["msg", "wait", "--timeout", "60", "--json"],
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
    fs::write(f.incarnation_file("lead"), "{").unwrap();
    for args in [vec!["msg", "list", "--json"], vec!["msg", "list"]] {
        let out = f.brg(&As::Member("lead"), &args, None);
        assert_eq!(out.code, 1, "{args:?}: {:?} {}", out.json, out.stderr);
        let msg = if args.last() == Some(&"--json") {
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
    assert_eq!(g.publish(&As::Operator, &note("lead", "a")).code, 0);
    let records = g.store_dir().join("records");
    fs::remove_dir_all(&records).unwrap();
    fs::write(&records, "").unwrap();
    let out = g.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(out.code, 7, "{:?}", out.json);
    assert!(
        out.json["message"].as_str().unwrap().contains("damaged"),
        "{:?}",
        out.json
    );
    let out = g.brg(&As::Operator, &["msg", "check"], None);
    assert!(out.stderr.contains("damaged"), "{}", out.stderr);
    // Storage someone else could write to.
    let h = Fixture::new();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(h.store_dir(), fs::Permissions::from_mode(0o777)).unwrap();
    let out = h.publish(&As::Operator, &note("lead", "a"));
    assert_eq!(out.code, 5, "{:?}", out.json);
    assert_eq!(
        out.json["message"],
        "instance `oncall-2a3b`'s message storage can't be used: its messages folder is writable by group or others"
    );
    let text = h.brg(
        &As::Operator,
        &["msg", "send", "oncall-2a3b/lead", "-", "--external", "ops"],
        Some(&serde_json::to_vec(&note("lead", "a")).unwrap()),
    );
    assert_eq!(text.code, 5);
    assert!(
        text.stderr
            .contains("its messages folder is writable by group"),
        "{}",
        text.stderr
    );
}

#[test]
fn storage_without_identity_is_damaged_not_new() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    fs::remove_file(f.store_dir().join("store.json")).unwrap();
    let before = f.tree();
    for args in [
        vec!["msg", "check", "--json"],
        vec!["msg", "list", "--member", "lead", "--json"],
    ] {
        let out = f.brg(&As::Operator, &args, None);
        assert_eq!(out.code, 7, "{args:?}: {:?}", out.json);
        let msg = out.json["message"].as_str().unwrap();
        assert!(
            msg.starts_with("instance `oncall-2a3b`'s message storage is damaged"),
            "{msg}"
        );
    }
    let out = f.publish(&As::Operator, &note("lead", "b"));
    assert_eq!(out.code, 5, "{:?}", out.json);
    assert_eq!(f.tree(), before, "nothing created");
}

#[test]
fn a_record_in_another_format_is_still_corrupt() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    let path = f.store_dir().join("records/0000000001.json");
    let mut r: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    r["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    assert_eq!(f.publish(&As::Operator, &note("lead", "b")).code, 5);
    let check = f.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(check.code, 7, "{:?}", check.json);
}

#[test]
fn a_team_not_started_yet_is_not_configured() {
    let f = Fixture::new();
    // The team's directory exists, but `up` never initialised its store.
    fs::remove_dir_all(f.store_dir()).unwrap();
    fs::create_dir(f.store_dir()).unwrap();
    let before = f.tree();
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();
    let commands: Vec<(As, Vec<&str>)> = vec![
        (
            As::Operator,
            vec![
                "msg",
                "send",
                "oncall-2a3b/lead",
                "-",
                "--external",
                "ops",
                "--json",
            ],
        ),
        (As::Member("lead"), vec!["msg", "ack", "m1", "--json"]),
        (
            As::Operator,
            vec!["msg", "list", "--member", "lead", "--json"],
        ),
        (As::Operator, vec!["msg", "read", "m1", "--json"]),
        (As::Operator, vec!["msg", "check", "--json"]),
        (As::Operator, vec!["msg", "check", "--json"]),
    ];
    for (who, args) in &commands {
        let out = f.brg(who, args, Some(&draft));
        assert_eq!(out.code, 3, "{args:?}: {:?}", out.json);
        assert_eq!(
            out.json["outcome"],
            if args.starts_with(&["msg", "send"]) {
                "not-alive"
            } else {
                "not-configured"
            }
        );
        assert_eq!(
            out.json["message"],
            "instance `oncall-2a3b` has no messages: it has not started"
        );
    }
    assert_eq!(f.tree(), before, "nothing created");
    // Nor when there is no message storage at all.
    fs::remove_dir_all(f.store_dir()).unwrap();
    let out = f.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(out.code, 3, "{:?}", out.json);
    assert!(!f.store_dir().exists());
}

#[test]
fn deployment_and_storage_must_match() {
    let f = Fixture::new();
    f.write_store("someone-else-0000");
    let out = f.publish(&As::Operator, &note("lead", "x"));
    assert_eq!(out.code, 5, "{:?}", out.json);
    assert_eq!(
        f.brg(&As::Operator, &["msg", "check", "--json"], None).code,
        7
    );

    let g = Fixture::new();
    assert_eq!(g.publish(&As::Operator, &note("lead", "a")).code, 0);
    fs::remove_file(g.store_dir().join("store.json")).unwrap();
    // Records without store identity are damaged, never new.
    let out = g.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(out.code, 7, "{:?}", out.json);
    assert_eq!(out.json["outcome"], "corrupt");
}

#[test]
fn read_only_commands_create_nothing() {
    let f = Fixture::new();
    // A fresh team (no bus/ yet), then one with records.
    for populated in [false, true] {
        if populated {
            f.publish(&As::Operator, &note("lead", "x"));
            f.brg(&As::Member("lead"), &["msg", "ack", "m1"], None);
        }
        let before = f.tree();
        let reads: Vec<(As, Vec<&str>)> = vec![
            (
                As::Operator,
                vec!["msg", "list", "--member", "lead", "--json"],
            ),
            (As::Member("lead"), vec!["msg", "list", "--json"]),
            (As::Operator, vec!["msg", "list", "--external", "ops"]),
            (As::Operator, vec!["msg", "read", "m1"]),
            (As::Operator, vec!["msg", "check", "--json"]),
            (As::Operator, vec!["msg", "check"]),
            (As::Operator, vec!["msg", "check"]),
            (
                As::Member("lead"),
                vec!["msg", "wait", "--timeout", "60", "--json"],
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
    let q = json!({"to": "external:ops", "body": "ship it?"});
    let out = f.publish(&As::Member("lead"), &q);
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
    let d = json!({"to": "lead", "reply_to": ["m1"], "body": "yes"});
    assert_eq!(f.publish(&As::Operator, &d).code, 0);
    assert_eq!(ids(&f.inbox(&As::Member("lead"), &[])), ["m2"]);
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
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    let draft = serde_json::to_vec(&note("lead", "b")).unwrap();

    // An incarnation ended while an ack waits for lock.
    let bus = hold(&f.store_dir().join("lock"));
    let child = spawn(
        &f,
        &As::Member("lead"),
        &["msg", "ack", "m1", "--json"],
        b"",
    );
    std::thread::sleep(Duration::from_millis(200));
    f.write_incarnation("lead", "lead-1", true);
    drop(bus);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 8, "{:?} {}", out.json, out.stderr);
    assert!(!f.store_dir().join("acks/m1.json").exists());

    // A member replaced while its publish waits for lock.
    let bus = hold(&f.store_dir().join("lock"));
    let args = ["msg", "send", "oncall-2a3b/lead", "-", "--json"];
    let child = spawn(&f, &As::Member("helper"), &args, &draft);
    std::thread::sleep(Duration::from_millis(500));
    f.write_incarnation("helper", "helper-2", false);
    drop(bus);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 5, "{:?} {}", out.json, out.stderr);
    assert_eq!(f.records().len(), 1);
}

#[test]
fn reads_wait_for_a_writer() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    let bus = hold(&f.store_dir().join("lock"));
    let mut child = spawn(&f, &As::Operator, &["msg", "check", "--json"], b"");
    std::thread::sleep(Duration::from_millis(500));
    assert!(child.try_wait().unwrap().is_none(), "check waits for lock");
    drop(bus);
    let out = finish_within(child, Duration::from_secs(15));
    assert_eq!(out.code, 0, "{:?}", out.json);
}

#[test]
fn a_huge_record_number_is_a_bounded_gap() {
    let f = Fixture::new();
    for k in ["k1", "k2"] {
        assert_eq!(f.publish(&As::Operator, &note("lead", k)).code, 0);
    }
    fs::write(f.store_dir().join("records/9999999999.json"), "{}").unwrap();
    let start = Instant::now();
    let check = f.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(check.code, 7, "{:?} {}", check.json, check.stderr);
    let errors = check.json["errors"].to_string();
    assert!(errors.contains("gap") && errors.contains("m3"), "{errors}");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(
        f.inbox(&As::Operator, &["--member", "lead"])["store"],
        "incomplete"
    );
    assert_eq!(f.publish(&As::Operator, &note("lead", "x")).code, 5);
}

#[test]
fn an_ack_with_another_schema_is_corrupt() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    assert_eq!(
        f.brg(&As::Member("lead"), &["msg", "ack", "m1"], None).code,
        0
    );
    let path = f.store_dir().join("acks/m1.json");
    let mut ack: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ack["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&ack).unwrap()).unwrap();
    let check = f.brg(&As::Operator, &["msg", "check", "--json"], None);
    assert_eq!(check.code, 7, "{:?}", check.json);
    assert!(check.json["errors"].to_string().contains("schema"));
    assert_eq!(f.publish(&As::Operator, &note("lead", "b")).code, 5);
    // Not counted as consumption.
    let inbox = f.inbox(&As::Operator, &["--member", "lead"]);
    assert_eq!(ids(&inbox), ["m1"]);
    assert_eq!(inbox["store"], "incomplete");
}

#[test]
fn inbox_wait_help_names_the_outcomes_wait_returns() {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .args(["msg", "wait", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(out.stdout).unwrap();
    let f = Fixture::new();
    let wait = |extra: &[&str]| {
        let mut args = vec!["msg", "wait", "--json"];
        args.extend(extra);
        f.brg(&As::Member("lead"), &args, None)
    };
    let usage = wait(&["--timeout", "0"]);
    let timeout = wait(&["--timeout", "60"]);
    assert_eq!(f.publish(&As::Operator, &note("lead", "a")).code, 0);
    let ok = wait(&["--timeout", "60"]);
    f.brg(&As::Member("lead"), &["msg", "ack", "m1", "--json"], None);
    let child = spawn(
        &f,
        &As::Member("lead"),
        &["msg", "wait", "--timeout", "60", "--json"],
        b"",
    );
    std::thread::sleep(Duration::from_millis(200));
    f.write_incarnation("lead", "lead-2", false);
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
            "`{named}` missing from msg wait --help"
        );
    }
}

#[test]
fn send_help_names_the_unified_inputs_and_publish_is_removed() {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .args(["msg", "send", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).unwrap();
    for flag in [
        "--external",
        "--reply-to",
        "--from",
        "--allow-unknown",
        "--json",
    ] {
        assert!(help.contains(flag), "{help}");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .args(["msg", "publish", "--help"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn a_thousand_record_history_keeps_pending_counts_and_next_id() {
    let f = Fixture::new();
    assert_eq!(f.publish(&As::Operator, &note("lead", "history")).code, 0);
    let mut record: Value =
        serde_json::from_slice(&fs::read(f.store_dir().join("records/0000000001.json")).unwrap())
            .unwrap();
    for seq in 2..=1000 {
        record["seq"] = json!(seq);
        record["id"] = json!(format!("m{seq}"));
        f.write_private(
            &f.store_dir().join(format!("records/{seq:010}.json")),
            &record,
        );
    }
    let started = Instant::now();
    let inbox = f.inbox(&As::Operator, &["--member", "lead"]);
    assert_eq!(inbox["unacked"], 1000);
    assert_eq!(inbox["records"].as_array().unwrap().len(), 1000);
    let sent = f.publish(&As::Operator, &note("lead", "next"));
    assert_eq!(sent.code, 0, "{:?}", sent.json);
    assert_eq!(sent.json["id"], "m1001");
    eprintln!("1000-record inbox plus next send: {:?}", started.elapsed());
}

#[test]
fn send_target_syntax_errors_are_usage_and_missing_start_is_not_alive() {
    let f = Fixture::new();
    for target in [
        "external:Bad",
        "lead",
        "Bad/lead",
        "oncall-2a3b/external:Bad",
    ] {
        let out = f.brg(
            &As::Operator,
            &["msg", "send", target, "x", "--external", "ops", "--json"],
            None,
        );
        assert_eq!(out.code, 2, "{target}: {:?}", out.json);
        assert_eq!(out.json["outcome"], "usage");
    }
    let path = f.root().join(TEAM).join("start.json");
    let mut meta: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    meta["members"].as_object_mut().unwrap().remove("lead");
    fs::write(&path, meta.to_string()).unwrap();
    let out = f.publish(&As::Operator, &note("lead", "x"));
    assert_eq!(out.code, 3, "{:?}", out.json);
    assert!(f.records().is_empty());
}

#[cfg(feature = "test-support")]
#[test]
fn inbox_wait_does_not_infer_exit_from_a_sandbox_process_view() {
    let f = Fixture::new();
    let bin = f.dir.path().join("sandbox-bin");
    fs::create_dir(&bin).unwrap();
    common::write_executable(&bin.join("tmux"), "#!/bin/sh\nexit 1\n");
    common::write_executable(
        &bin.join("ps"),
        "#!/bin/sh\nprintf '%s\\n' \"${4%,*}\"\n: >\"$HOME/ps-called\"\n",
    );
    let path = f.incarnation_file("lead");
    let mut inc: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    inc["boot_id"] = json!("host-boot");
    inc["pane"] = json!({"socket": "/nonexistent/test-sandbox-socket", "session": "agent", "window": "0", "pane_id": "%0", "pane_pid": std::process::id()});
    f.write_private(&path, &inc);
    let out = f
        .command(
            &As::Member("lead"),
            &["msg", "wait", "--timeout", "60", "--json"],
        )
        .env("PATH", bin)
        .env("BORGCHART_TEST_BOOT_ID", "sandbox-boot")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!f.dir.path().join("ps-called").exists());
}
