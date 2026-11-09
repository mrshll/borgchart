//! What members are started with and who may act on an instance: writable
//! folders by posture, API keys and secrets, `brg secret`, the notes
//! `create` prints about a folder's project settings, and instance
//! selection for bus commands.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;
use std::process::Output;

use instance_env::*;
use serde_json::{Value, json};

fn first_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn argv(start: &Value) -> Vec<String> {
    start["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

impl Env {
    fn folder(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }

    /// A member's own environment, from its first start, as a command
    /// running `brg` inside it would have.
    fn as_member(&self, member: &str, start: usize, args: &[&str]) -> Output {
        let env: BTreeMap<String, String> =
            serde_json::from_value(self.starts(member)[start]["env"].clone()).unwrap();
        common::command(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .envs(env)
            .args(args)
            .output()
            .unwrap()
    }

    /// Add `lines` to `role`'s entry in the team file.
    fn role_extra(&self, role: &str, lines: &str) {
        let path = self.path("teams/oncall/team.yaml");
        let text = fs::read_to_string(&path).unwrap();
        let entry = format!("  {role}:\n    prompt: {role}.md\n");
        assert!(text.contains(&entry), "{text}");
        fs::write(&path, text.replace(&entry, &format!("{entry}{lines}"))).unwrap();
    }
}

// --- 14. Writable folders ---

#[test]
fn vendor_members_get_messages_and_notes_and_never_brgs_own_folders() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{a}/helper")]);
    let state = env.path(&format!("state/{a}")).canonicalize().unwrap();
    let messages = state.join("messages").display().to_string();
    let notes = state.join("notes").display().to_string();

    // Codex: sandbox writable roots.
    let helper = env.member(&a, "helper");
    let codex = argv(&env.starts(&helper)[0]);
    let roots: Vec<&String> = codex
        .iter()
        .filter(|a| a.starts_with("sandbox_workspace_write.writable_roots="))
        .collect();
    assert_eq!(roots.len(), 1, "{codex:?}");
    assert!(
        roots[0].contains(&messages) && roots[0].contains(&notes),
        "{roots:?}"
    );
    for own in ["members", "secrets", "exec", "plugins", "tmp"] {
        assert!(
            !roots[0].contains(&state.join(own).display().to_string()),
            "{own}: {roots:?}"
        );
    }
    // Claude: added folders.
    let lead = env.member(&a, "lead");
    let claude = argv(&env.starts(&lead)[0]);
    let added: Vec<&String> = claude
        .windows(2)
        .filter(|w| w[0] == "--add-dir")
        .map(|w| &w[1])
        .collect();
    assert_eq!(added, [&messages, &notes], "{claude:?}");
    env.ok(&["stop", &a]);
}

#[test]
fn a_roles_own_writable_roots_replace_brgs() {
    let env = Env::new();
    env.role_extra(
        "helper",
        "    args: [-c, 'sandbox_workspace_write.writable_roots=[\"/srv/example\"]']\n",
    );
    let a = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{a}/helper")]);
    let codex = argv(&env.starts(&env.member(&a, "helper"))[0]);
    let roots: Vec<&String> = codex
        .iter()
        .filter(|a| a.starts_with("sandbox_workspace_write.writable_roots="))
        .collect();
    assert_eq!(
        roots,
        ["sandbox_workspace_write.writable_roots=[\"/srv/example\"]"],
        "{codex:?}"
    );
    env.ok(&["stop", &a]);
}

#[test]
fn locked_members_keep_their_launch() {
    let env = Env::new();
    // No bus: a team with one can't have `locked` members.
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace("bus: {}\n", "").replace(
            "    vendor: codex\n    parent: lead\n    start: false\n",
            "    vendor: codex\n    parent: lead\n    start: false\n    posture: locked\n",
        ),
    );
    env.role_extra("lead", "    posture: locked\n");
    env.config("auto_trust: true\n");
    fs::create_dir_all(env.path("work/a/.codex")).unwrap();
    fs::create_dir_all(env.path("work/a/.claude")).unwrap();
    let a = first_word(&stdout(&env.ok(&[
        "create",
        "oncall",
        &env.folder("work/a"),
    ])));
    env.wait_for("lead start", || {
        !env.starts(&env.member(&a, "lead")).is_empty()
    });
    env.ok(&["create", &format!("{a}/helper")]);
    let state = env.path(&format!("state/{a}")).canonicalize().unwrap();
    let lead = env.member(&a, "lead");
    let helper = env.member(&a, "helper");

    // Claude: the gate and restricted flags, and no added folders.
    let claude = argv(&env.starts(&lead)[0]);
    for flag in ["--restricted", "--strict-mcp-config", "--settings"] {
        assert!(claude.iter().any(|a| a == flag), "{flag}: {claude:?}");
    }
    assert!(!claude.iter().any(|a| a == "--add-dir"), "{claude:?}");

    // Codex: no trust, only plugin data and its temp folder writable, and
    // it starts in its temp folder.
    let start = &env.starts(&helper)[0];
    let codex = argv(start);
    assert!(
        !codex.iter().any(|a| a.starts_with("projects=")),
        "{codex:?}"
    );
    let roots: Vec<&String> = codex
        .iter()
        .filter(|a| a.starts_with("sandbox_workspace_write.writable_roots="))
        .collect();
    assert_eq!(
        roots,
        [&format!(
            "sandbox_workspace_write.writable_roots=[{}]",
            serde_json::to_string(&state.join(format!("tmp/{helper}")).display().to_string())
                .unwrap()
        )],
        "{codex:?}"
    );
    assert_eq!(
        start["cwd"].as_str().unwrap(),
        state.join(format!("tmp/{helper}")).display().to_string()
    );
    assert!(
        codex.iter().any(|a| a == "-a")
            && codex
                .iter()
                .any(|a| a.starts_with("hooks") || a.contains("hooks")),
        "{codex:?}"
    );
    env.ok(&["stop", &a]);
}

// --- 14. Keys and secrets ---

const ANTHROPIC: &str = "sk-ant-not-real-5e1a";
const OPENAI: &str = "sk-oai-not-real-9c3b";
const PAGER: &str = "pager-not-real-77d0";

/// The test team with a plugin that needs a secret and has a plain value.
fn with_pager(env: &Env) {
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "roles:\n  lead:\n    prompt: lead.md\n",
            "plugins:\n  pager:\n    path: plugins/pager\n    env: {PAGER_TOKEN: , PAGER_ROOM: R0000EXAMPLE}\n    actions:\n      read: pager read <id>\nroles:\n  lead:\n    prompt: lead.md\n    plugins: {pager: [read]}\n",
        ),
    );
    let script = env.path("teams/oncall/plugins/pager/pager");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "#!/bin/sh\necho ok\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

fn keyed(env: &Env, args: &[&str]) -> Output {
    env.cmd(args)
        .env("ANTHROPIC_API_KEY", ANTHROPIC)
        .env("OPENAI_API_KEY", OPENAI)
        .env("PAGER_TOKEN", PAGER)
        .output()
        .unwrap()
}

/// Every file under the test's state, home and work folders, except the
/// fake agents' own logs of what they were started with.
fn every_file(env: &Env) -> Vec<(std::path::PathBuf, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push((
                    p.clone(),
                    String::from_utf8_lossy(&fs::read(&p).unwrap_or_default()).to_string(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    for d in ["state", "home", "work"] {
        walk(&env.path(d), &mut out);
    }
    let agents = env.path("home/fake-agent");
    out.retain(|(p, _)| !p.starts_with(&agents));
    out
}

#[test]
fn login_passes_no_key_and_api_key_only_the_members_own() {
    for auth in ["login", "api_key"] {
        let env = Env::new();
        with_pager(&env);
        env.config(&format!("auth: {auth}\n"));
        let out = keyed(&env, &["create", "oncall", &env.folder("work/a")]);
        assert!(out.status.success(), "{}", both(&out));
        for v in [ANTHROPIC, OPENAI, PAGER] {
            assert!(!both(&out).contains(v), "create printed a value");
        }
        let a = first_word(&stdout(&out));
        env.wait_for("lead start", || {
            !env.starts(&env.member(&a, "lead")).is_empty()
        });
        let out = keyed(&env, &["create", &format!("{a}/helper")]);
        assert!(out.status.success(), "{}", both(&out));
        let lead = &env.starts(&env.member(&a, "lead"))[0]["env"];
        let helper = &env.starts(&env.member(&a, "helper"))[0]["env"];
        if auth == "login" {
            for e in [lead, helper] {
                assert!(e.get("ANTHROPIC_API_KEY").is_none(), "{e}");
                assert!(e.get("OPENAI_API_KEY").is_none(), "{e}");
            }
        } else {
            assert_eq!(lead["ANTHROPIC_API_KEY"], ANTHROPIC);
            assert!(lead.get("OPENAI_API_KEY").is_none(), "{lead}");
            assert_eq!(helper["OPENAI_API_KEY"], OPENAI);
            assert!(helper.get("ANTHROPIC_API_KEY").is_none(), "{helper}");
        }
        // Plugin secrets never reach a member's window.
        for e in [lead, helper] {
            assert!(!e.to_string().contains(PAGER), "{e}");
        }
        // Nothing prints a value, and only the plugin's 0600 file holds its
        // secret while the instance runs.
        assert!(!both(&out).contains(ANTHROPIC) && !both(&out).contains(OPENAI));
        let status = both(&env.ok(&["status", &a, "--json"]));
        for v in [ANTHROPIC, OPENAI, PAGER] {
            assert!(!status.contains(v), "{status}");
        }
        for (path, text) in every_file(&env) {
            assert!(
                !text.contains(ANTHROPIC) && !text.contains(OPENAI),
                "{} holds a key",
                path.display()
            );
            if text.contains(PAGER) {
                assert!(path.ends_with("secrets/pager.json"), "{}", path.display());
            }
        }
        env.ok(&["stop", &a]);
        for (path, text) in every_file(&env) {
            assert!(!text.contains(PAGER), "{} kept the secret", path.display());
        }
    }
}

#[test]
fn brg_secret_lists_the_names_and_whether_each_is_set() {
    let env = Env::new();
    with_pager(&env);
    let before = every_file(&env).len();
    let out = env.ok(&["secret", "oncall"]);
    let text = stdout(&out);
    assert!(text.contains("pager: PAGER_TOKEN"), "{text}");
    assert!(text.contains("not set"), "{text}");
    assert!(
        !text.contains("PAGER_ROOM"),
        "plain values aren't secrets: {text}"
    );
    let out = keyed(&env, &["secret", "oncall", "--json"]);
    assert!(out.status.success(), "{}", both(&out));
    assert!(!both(&out).contains(PAGER), "{}", both(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["secrets"],
        json!([{"plugin": "pager", "name": "PAGER_TOKEN", "set": true}])
    );
    assert_eq!(every_file(&env).len(), before, "nothing written");
    assert!(!env.path("state").exists());
}

// --- Project settings in the folder ---

#[test]
fn create_notes_project_settings_auto_trust_will_load() {
    let env = Env::new();
    env.config("auto_trust: true\n");
    fs::write(env.path("work/a/.mcp.json"), "{}").unwrap();
    fs::create_dir_all(env.path("work/a/.codex")).unwrap();
    let out = env.ok(&["create", "oncall", &env.folder("work/a")]);
    let text = stdout(&out);
    assert!(
        text.contains("project settings (`.claude/` or `.mcp.json`)"),
        "{text}"
    );
    let a = first_word(&text);
    let out = env.ok(&["create", &format!("{a}/helper")]);
    assert!(stdout(&out).contains("(`.codex/`)"), "{}", both(&out));
    env.ok(&["stop", &a]);
    // Without `auto_trust: true`, nothing is trusted for them, so no note.
    env.config("");
    let out = env.ok(&["create", "oncall", &env.folder("work/b")]);
    assert!(!stdout(&out).contains("project settings"), "{}", both(&out));
    env.ok(&["stop", &first_word(&stdout(&out))]);
}

// --- 12. Instance selection ---

#[test]
fn a_member_acts_only_on_its_own_instance() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    let lead = env.member(&a, "lead");
    // Its own instance by default, and by name.
    let out = env.as_member(&lead, 0, &["inbox", "--json"]);
    assert!(out.status.success(), "{}", both(&out));
    let out = env.as_member(&lead, 0, &["inbox", "--json", "--instance", &a]);
    assert!(out.status.success(), "{}", both(&out));
    // Another instance: refused, for reads too.
    for args in [
        vec!["inbox", "--json", "--instance", b.as_str()],
        vec!["bus", "check", "--instance", b.as_str()],
    ] {
        let out = env.as_member(&lead, 0, &args);
        assert!(!out.status.success(), "{args:?}: {}", both(&out));
        assert!(
            both(&out).contains(&format!("can't act on instance `{b}`")),
            "{}",
            both(&out)
        );
    }
    // Nor can it pass itself off as an outside caller.
    let out = env.as_member(
        &lead,
        0,
        &["send", &format!("{b}/lead"), "hello", "--external", "ops"],
    );
    assert!(!out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn partial_and_stale_member_contexts_are_refused() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let mut start: BTreeMap<String, String> =
        serde_json::from_value(env.starts(&lead)[0]["env"].clone()).unwrap();
    start.remove("BORGCHART_INCARNATION");
    let out = common::command(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .envs(&start)
        .args(["inbox", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(
        both(&out).contains("incomplete member context"),
        "{}",
        both(&out)
    );
    // A start replaced by a newer one is fenced.
    env.ok(&["stop", &a]);
    env.ok(&["start", &a]);
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    let out = env.as_member(&lead, 0, &["inbox", "--json"]);
    assert_eq!(out.status.code(), Some(8), "{}", both(&out));
    let out = env.as_member(&lead, 1, &["inbox", "--json"]);
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
}

#[test]
fn outside_callers_name_the_instance_and_a_stopped_one_can_be_inspected() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(&a, &json!({"to": lead, "body": "hi"}));
    assert!(out.status.success(), "{}", both(&out));
    // An operator reads a member's inbox; without `--instance`, refused.
    env.ok(&["inbox", "--member", &lead, "--instance", &a]);
    let err = env.fails(&["inbox", "--member", &lead]);
    assert!(err.contains("--instance") && err.contains(&a), "{err}");
    let err = env.fails(&["bus", "check"]);
    assert!(err.contains("--instance"), "{err}");
    // A role's name isn't a member.
    let err = env.fails(&["inbox", "--member", "lead", "--instance", &a]);
    assert!(err.contains(&lead), "{err}");
    env.ok(&["stop", &a]);
    for args in [
        vec!["bus", "check", "--instance", a.as_str()],
        vec!["bus", "check", "--instance", a.as_str()],
        vec!["inbox", "--member", lead.as_str(), "--instance", a.as_str()],
    ] {
        env.ok(&args);
    }
}

// --- Review fixes ---

#[test]
fn a_shared_folder_replaced_by_a_symlink_refuses_the_restart() {
    for how in ["symlink", "group-writable"] {
        let env = Env::new();
        let a = env.create("oncall", "work/a");
        env.ok(&["create", &format!("{a}/helper")]);
        let lead = env.member(&a, "lead");
        let helper = env.member(&a, "helper");
        env.ok(&["stop", &a]);
        let state = env.path(&format!("state/{a}"));
        if how == "symlink" {
            fs::remove_dir(state.join("notes")).unwrap();
            std::os::unix::fs::symlink(".", state.join("notes")).unwrap();
        } else {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state.join("notes"), fs::Permissions::from_mode(0o777)).unwrap();
        }
        let err = env.fails(&["start", &a]);
        assert!(err.contains("notes"), "{how}: {err}");
        // Neither vendor started again, so neither got the folder.
        assert_eq!(env.starts(&lead).len(), 1, "{how}");
        assert_eq!(env.starts(&helper).len(), 1, "{how}");
        assert_eq!(env.tmux(&["list-sessions"]), "", "{how}");
    }
}

#[test]
fn a_roles_added_folders_replace_brgs_in_either_form() {
    for args in [
        "['--add-dir', '/srv/example']",
        "['--add-dir=/srv/example']",
    ] {
        let env = Env::new();
        env.role_extra("lead", &format!("    args: {args}\n"));
        let a = env.create("oncall", "work/a");
        let claude = argv(&env.starts(&env.member(&a, "lead"))[0]);
        let state = env.path(&format!("state/{a}")).canonicalize().unwrap();
        assert!(
            !claude
                .iter()
                .any(|x| x.contains(&state.join("messages").display().to_string())),
            "{args}: {claude:?}"
        );
        assert!(
            claude.iter().any(|x| x.contains("/srv/example")),
            "{args}: {claude:?}"
        );
        env.ok(&["stop", &a]);
    }
}

#[test]
fn plugin_actions_preserve_and_fence_the_launch_incarnation() {
    let env = Env::new();
    with_pager(&env);
    fs::write(env.path("teams/oncall/plugins/pager/pager"),
        "#!/bin/sh\nif [ \"${1-}\" = send ]; then exec \"$BORGCHART_BIN\" send external:ops hello --json; fi\nprintf '%s\\n' \"$BORGCHART_INSTANCE\" \"$BORGCHART_MEMBER\" \"$BORGCHART_INCARNATION\"\n").unwrap();
    let out = keyed(&env, &["create", "oncall", &env.folder("work/a")]);
    assert!(out.status.success(), "{}", both(&out));
    let instance = first_word(&stdout(&out));
    let lead = env.member(&instance, "lead");
    let state = env.folder(&format!("state/{instance}"));
    let args = ["_run", &state, &lead, "pager", "pager"];
    let start = &env.starts(&lead)[0]["env"];
    let out = env.as_member(&lead, 0, &args);
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "{instance}\n{lead}\n{}\n",
            start["BORGCHART_INCARNATION"].as_str().unwrap()
        )
    );
    let vars: BTreeMap<String, String> = serde_json::from_value(start.clone()).unwrap();
    for (key, value) in [
        ("BORGCHART_INCARNATION", "stale-1"),
        ("BORGCHART_INCARNATION", ""),
        ("BORGCHART_MEMBER", "another"),
        ("BORGCHART_INSTANCE", "other-1234"),
    ] {
        let out = common::command(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .envs(&vars)
            .env(key, value)
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{key}: {}", both(&out));
        assert!(out.stdout.is_empty());
    }
    let out = env.cmd(&args).output().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(out.stdout.is_empty());
    let mut send_args = args.to_vec();
    send_args.push("send");
    let out = env.as_member(&lead, 0, &send_args);
    assert!(out.status.success(), "{}", both(&out));
    let record: Value = serde_json::from_slice(
        &env.ok(&["msg", "read", "m1", "--instance", &instance, "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(record["record"]["from"], lead);
    // End the identity without deleting plugin settings, so the fence itself is tested.
    let path = env.path(&format!("state/{instance}/members/{lead}/incarnation.json"));
    let mut ended: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ended["ended_at"] = json!("2026-11-09T00:00:00Z");
    fs::write(&path, ended.to_string()).unwrap();
    let out = env.as_member(&lead, 0, &args);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("was stopped"));
    env.ok(&["stop", &instance]);
}
