//! Resolver starts and diagnostics through the CLI, with an isolated fake resolver.
#![cfg(feature = "test-support")]
mod common;
mod team_env;

use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use team_env::*;

fn setup(source: &str) -> Env {
    let env = Env::new();
    env.write("templates/oncall/template.yaml", &format!("{TEAM}\nplugins:\n  pager:\n    process: [sh, -c, 'test \"${{#PAGER_TOKEN}}\" = 15 && test \"$PORT$DEBUG\" = 8080true && touch \"$HOME/plugin-saw-token\"; sleep 60']\n    env: {{PAGER_TOKEN: {source}, PORT: 8080, DEBUG: true}}\n"));
    script(
        &env.path("bin/resolver"),
        r#"#!/bin/sh
file=${1#--env-file=}
shift; shift
printf '%s\n' "$file" >> "$HOME/resolver.log"
test "$(stat -c %a "$file" 2>/dev/null || stat -f %Lp "$file")" = 600 || exit 2
cat "$file" > "$HOME/resolver-input"
case ${MODE:-} in
  fail) echo 'fake://root fake-value-root' >&2; exit 1;;
  timeout) sleep 2;;
  slow) sleep 5;;
esac
while IFS='=' read -r name reference; do
  reference=${reference#\'}; reference=${reference%\'}
  case ${MODE:-} in
    unset) unset "$name";;
    reference) export "$name=$reference";;
    empty) export "$name=";;
    *) export "$name=fake-value-${reference#fake://}";;
  esac
done < "$file"
echo 'chatter fake://root fake-value-root'
echo 'brg-secrets invalid-earlier-line'
"$@" | sed 's/fake-value-[a-z]*/MASK/g'
"#,
    );
    env.config("secrets: {resolver: [resolver, '--env-file={file}', '--']}\n");
    env
}

fn clean_output(out: &std::process::Output) {
    let text = both(out);
    for forbidden in ["fake://", "fake-value-"] {
        assert!(!text.contains(forbidden), "leaked resolver data: {text}");
    }
}

fn runs(env: &Env) -> usize {
    fs::read_to_string(env.path("home/resolver.log"))
        .unwrap_or_default()
        .lines()
        .count()
}

fn cleaned(env: &Env) {
    for path in fs::read_to_string(env.path("home/resolver.log"))
        .unwrap_or_default()
        .lines()
    {
        assert!(
            !std::path::Path::new(path).exists(),
            "env-file survived resolution"
        );
    }
    assert!(
        !fs::read_to_string(env.path("home/resolver-input"))
            .unwrap_or_default()
            .contains("fake-value-")
    );
}

#[test]
fn secrets_check_resolves_once_without_registering_or_writing_settings() {
    let env = setup("{from: 'fake://root'}");
    let id = env.create("oncall", "work/a");
    env.ok(&["stop", &id]);
    let registry = fs::read(env.path("state/registry.json")).unwrap();
    let start = fs::read(env.path(&format!("state/{id}/start.json"))).unwrap();
    let out = env.ok(&["secrets", "--check", &id]);
    clean_output(&out);
    assert!(out.stdout.is_empty());
    assert_eq!(runs(&env), 2);
    assert_eq!(fs::read(env.path("state/registry.json")).unwrap(), registry);
    assert_eq!(
        fs::read(env.path(&format!("state/{id}/start.json"))).unwrap(),
        start
    );
    assert!(!env.path(&format!("state/{id}/secrets/pager.json")).exists());
    cleaned(&env);
    assert!(
        !env.brg(&["secrets", "--check", "unknown-2222"])
            .status
            .success()
    );
}

#[test]
fn secrets_check_reports_resolver_failure_without_leaking_or_changing_settings() {
    let env = setup("{from: 'fake://root'}");
    let id = env.create("oncall", "work/a");
    let settings = fs::read(env.path(&format!("state/{id}/secrets/pager.json"))).unwrap();
    let out = env
        .cmd(&["secrets", "--check", &id])
        .env("MODE", "fail")
        .output()
        .unwrap();
    assert!(!out.status.success());
    clean_output(&out);
    assert!(both(&out).contains("the secrets resolver failed with exit 1"));
    assert_eq!(runs(&env), 2);
    assert_eq!(
        fs::read(env.path(&format!("state/{id}/secrets/pager.json"))).unwrap(),
        settings
    );
    cleaned(&env);
    env.ok(&["stop", &id]);
}

#[test]
fn secrets_check_without_sources_skips_resolver() {
    let env = setup("'fake-value-root'");
    let id = env.create("oncall", "work/a");
    let out = env.ok(&["secrets", "--check", &id]);
    clean_output(&out);
    assert_eq!(runs(&env), 0);
    env.ok(&["stop", &id]);
}

#[test]
fn secrets_check_refuses_members_before_running_resolver() {
    let env = setup("{from: 'fake://root'}");
    let id = env.create("oncall", "work/a");
    let out = env
        .cmd(&["secrets", "--check", &id])
        .env("BORGCHART_MEMBER", "lead-example")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(both(&out).contains("ask the owner"));
    clean_output(&out);
    assert_eq!(runs(&env), 1);
    env.ok(&["stop", &id]);
}

fn token(env: &Env, id: &str) -> String {
    let path = env.path(&format!("state/{id}/secrets/pager.json"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    value["env"]["PAGER_TOKEN"].as_str().unwrap().to_string()
}

fn private_tree(path: &std::path::Path) {
    for entry in fs::read_dir(path).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            private_tree(&path);
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        assert!(!text.contains("fake://"), "reference in {}", path.display());
        assert!(
            !text.contains("fake-value-") || path.ends_with("secrets/pager.json"),
            "value in {}",
            path.display()
        );
    }
}

#[test]
fn fresh_and_joining_starts_resolve_only_when_needed_and_keep_values_private() {
    let env = setup("{from: 'fake://root'}");
    env.write("state/resolver-2147483647.env", "stale reference");
    let live = format!("state/resolver-{}.env", std::process::id());
    env.write(&live, "live reference");
    let out = env
        .cmd(&["create", "oncall", env.path("work/a").to_str().unwrap()])
        .env("PAGER_TOKEN", "wrong")
        .env("UNRELATED", std::ffi::OsString::from_vec(vec![0xff]))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    clean_output(&out);
    assert!(!env.path("state/resolver-2147483647.env").exists());
    assert!(env.path(&live).exists());
    fs::remove_file(env.path(&live)).unwrap();
    let id = stdout(&out).split_whitespace().next().unwrap().to_string();
    assert_eq!(token(&env, &id), "fake-value-root");
    env.wait_for("plugin received its value", || {
        env.path("home/plugin-saw-token").exists()
    });
    let lead = env.member(&id, "lead");
    env.wait_for("member started", || !env.starts(&lead).is_empty());
    assert!(env.starts(&lead)[0]["env"].get("PAGER_TOKEN").is_none());
    assert!(
        !env.tmux(&["show-environment", "-g"])
            .contains("fake-value-")
    );
    let start = fs::read_to_string(env.path(&format!("state/{id}/start.json"))).unwrap();
    assert!(!start.contains("fake-value-") && !start.contains("fake://"));
    env.ok(&["stop", &format!("{id}/{lead}")]);
    let template = fs::read_to_string(env.path("templates/oncall/template.yaml")).unwrap();
    env.write(
        "templates/oncall/template.yaml",
        &format!("{template}  extra: {{command: extra}}\n"),
    );
    env.ok(&["start", &format!("{id}/{lead}")]);
    env.write("templates/oncall/template.yaml", &template);
    env.ok(&["create", &format!("{id}/helper")]);
    assert_eq!(runs(&env), 1);
    env.ok(&["stop", &id]);
    env.ok(&["create", &format!("{id}/scout")]);
    assert_eq!(runs(&env), 2);
    cleaned(&env);
    private_tree(&env.path("state"));
    env.ok(&["stop", &id]);
}

#[test]
fn joining_start_refuses_missing_or_corrupt_plugin_settings() {
    for corrupt in [false, true] {
        let env = setup("");
        env.config("");
        let out = env
            .cmd(&["create", "oncall", env.path("work/a").to_str().unwrap()])
            .env("PAGER_TOKEN", "fake-value-root")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        let id = stdout(&out).split_whitespace().next().unwrap().to_string();
        let lead = env.member(&id, "lead");
        env.ok(&["stop", &format!("{id}/{lead}")]);
        let file = env.path(&format!("state/{id}/secrets/pager.json"));
        if corrupt {
            fs::write(file, "invalid JSON").unwrap();
        } else {
            fs::remove_file(file).unwrap();
        }
        assert!(
            !env.brg(&["start", &format!("{id}/{lead}")])
                .status
                .success()
        );
        assert_eq!(env.state_of(&id, "lead"), "stopped");
        env.ok(&["stop", &id]);
    }
}

#[test]
fn restored_children_resolve_same_named_secrets_from_their_own_references() {
    let env = setup("{from: 'fake://root'}");
    let path = "templates/oncall/template.yaml";
    let text = fs::read_to_string(env.path(path)).unwrap().replace(
        "roles:\n",
        "roles:\n  dev: {template: dev, parent: lead, start: false}\n",
    );
    env.write(path, &text);
    env.team("templates/dev");
    env.write("templates/dev/template.yaml", &format!("{TEAM}\nplugins:\n  pager:\n    process: [sleep, '60']\n    env: {{PAGER_TOKEN: {{from: 'fake://child'}}}}\n"));
    let root = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{root}/dev")]);
    let child = env.registry()["teams"]
        .as_object()
        .unwrap()
        .keys()
        .find(|id| id.contains('/'))
        .unwrap()
        .clone();
    env.ok(&["start", &child]);
    env.ok(&["stop", &root]);
    env.ok(&["start", &root]);
    assert_eq!(token(&env, &root), "fake-value-root");
    assert_eq!(token(&env, &child), "fake-value-child");
    assert_eq!(runs(&env), 4);
    cleaned(&env);
    env.ok(&["stop", &root]);
}

#[test]
fn resolver_failures_leave_no_team_or_settings_and_do_not_relay_child_output() {
    for mode in ["fail", "timeout", "unset", "reference", "empty"] {
        let env = setup("{from: 'fake://root'}");
        let out = env
            .cmd(&["create", "oncall", env.path("work/a").to_str().unwrap()])
            .env("MODE", mode)
            .env("BORGCHART_TEST_SECRETS_TIMEOUT_MS", "100")
            .output()
            .unwrap();
        assert!(!out.status.success(), "{mode}: {}", both(&out));
        clean_output(&out);
        let phrase = match mode {
            "fail" => "failed with exit 1",
            "timeout" => "timed out",
            _ => "left pager: PAGER_TOKEN unset",
        };
        assert!(both(&out).contains(phrase), "{}", both(&out));
        assert_eq!(runs(&env), 1, "{mode}: {}", both(&out));
        assert!(!env.path("state/registry.json").exists());
        cleaned(&env);
    }
}

#[test]
fn environment_secrets_skip_the_configured_resolver_and_sourced_ones_refuse_members() {
    let env = setup("");
    let out = env
        .cmd(&["create", "oncall", env.path("work/a").to_str().unwrap()])
        .env("PAGER_TOKEN", "fake-value-root")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let id = stdout(&out).split_whitespace().next().unwrap().to_string();
    assert_eq!(token(&env, &id), "fake-value-root");
    assert_eq!(runs(&env), 0);
    env.ok(&["stop", &id]);
    let env = setup("{from: 'fake://root'}");
    let id = env.create("oncall", "work/a");
    env.ok(&["stop", &id]);
    let out = env
        .cmd(&["start", &id])
        .env("BORGCHART_MEMBER", "lead-example")
        .output()
        .unwrap();
    assert!(!out.status.success());
    clean_output(&out);
    assert!(both(&out).contains("ask the owner"));
    assert_eq!(runs(&env), 1);
    env.config("");
    let out = env.brg(&["start", &id]);
    assert!(!out.status.success());
    assert!(both(&out).contains("plugin secrets need a resolver"));
    assert!(both(&out).contains(env.path("config.yaml").to_str().unwrap()));
    clean_output(&out);
}

#[test]
fn source_diagnostics_hide_references_and_reject_unsafe_configuration() {
    let env = setup("{from: 'fake://root'}");
    let out = env.ok(&["secrets", "oncall", "--json"]);
    clean_output(&out);
    let data: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(data["secrets"][0]["source"], "resolver");
    assert!(data["secrets"][0].get("set").is_none());
    let out = env.ok(&["secrets", "oncall"]);
    clean_output(&out);
    assert!(stdout(&out).contains("resolver"));
    clean_output(&env.ok(&["validate", "oncall"]));
    for value in ["{form: 'fake://root'}", "{from: 42}", "[value]"] {
        let text = fs::read_to_string(env.path("templates/oncall/template.yaml")).unwrap();
        env.write(
            "templates/oncall/template.yaml",
            &text.replace("{from: 'fake://root'}", value),
        );
        let out = env.brg(&["validate", "oncall"]);
        assert!(!out.status.success());
        assert!(both(&out).contains("env `PAGER_TOKEN`: give a plain value"));
        clean_output(&out);
        env.write("templates/oncall/template.yaml", &text);
    }
    for reference in [
        "''",
        "'fake://bad''quote'",
        "\"line\\nbreak\"",
        "\"control\\tcharacter\"",
    ] {
        let text = fs::read_to_string(env.path("templates/oncall/template.yaml"))
            .unwrap()
            .replace("'fake://root'", reference);
        env.write("templates/oncall/template.yaml", &text);
        let out = env.brg(&["validate", "oncall"]);
        assert!(!out.status.success(), "{reference:?}");
        clean_output(&out);
        env.write(
            "templates/oncall/template.yaml",
            &text.replace(reference, "'fake://root'"),
        );
    }
    for resolver in ["[]", "[resolver]", "[resolver, '{file}{file}']"] {
        env.config(&format!("secrets: {{resolver: {resolver}}}\n"));
        assert!(!env.brg(&["validate", "oncall"]).status.success());
    }
}

#[test]
fn slow_resolution_does_not_block_status_or_another_start() {
    let env = setup("{from: 'fake://root'}");
    let id = env.create("oncall", "work/a");
    env.ok(&["stop", &id]);
    let child = env
        .cmd(&["start", &id])
        .env("MODE", "slow")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    env.wait_for("resolver waiting", || runs(&env) == 2);
    let began = std::time::Instant::now();
    env.ok(&["status", &id]);
    env.ok(&["create", &format!("{id}/helper")]);
    assert!(began.elapsed() < std::time::Duration::from_secs(3));
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &id]);
}
