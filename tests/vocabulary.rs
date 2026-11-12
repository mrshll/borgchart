//! Vocabulary cutovers through the command-line boundary.

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

fn brg(root: &Path, args: &[&str]) -> Output {
    common::command(env!("CARGO_BIN_EXE_brg"))
        .env("BORGCHART_CONFIG", root.join("config.yaml"))
        .env("BORGCHART_STATE_HOME", root.join("state"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn retired_template_inputs_are_refused_with_the_replacement() {
    for (file, text, replacement) in [
        ("team.yaml", "roles: {}\n", "template.yaml"),
        ("config.yaml", "schema: 1\nteams: []\n", "templates:"),
        (
            "template.yaml",
            "roles: {dev: {team: example}}\n",
            "template:",
        ),
        (
            "lead.md",
            "---\nmodels: {claude: opus}\n---\n{{teams/example/lead}}\n",
            "{{templates/",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("config.yaml"), "schema: 1\n").unwrap();
        fs::write(
            root.path().join("template.yaml"),
            "roles: {lead: {prompt: lead.md}}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("lead.md"),
            "---\nmodels: {claude: opus}\n---\nLead.\n",
        )
        .unwrap();
        if file == "team.yaml" {
            fs::remove_file(root.path().join("template.yaml")).unwrap();
        }
        fs::write(root.path().join(file), text).unwrap();
        let out = brg(root.path(), &["validate", root.path().to_str().unwrap()]);
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "retired {file} accepted: {out:?}");
        assert!(error.contains(replacement), "{file}: {error}");
    }
}

#[test]
fn schema_one_state_is_refused_before_deserializing_retired_fields() {
    for file in [
        "registry.json",
        "sample-2222/team.json",
        "sample-2222/start.json",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state").join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old_entry = serde_json::json!({
            "team": "sample", "team_dir": root.path().join("template"),
            "folder": root.path().join("work"), "created_at": "2026-01-01T00:00:00Z"
        });
        let mut old_record = old_entry.clone();
        old_record["schema"] = 1.into();
        old_record["id"] = "sample-2222".into();
        old_record["members"] = serde_json::json!({"lead-abcd": {"role": "lead"}});
        old_record["copies"] = serde_json::json!({});
        let old_meta = serde_json::json!({
            "team_dir": root.path().join("template"), "folder": root.path().join("work"),
            "brg": "/usr/bin/brg", "config": {"version": 1, "digest": "00", "inputs": {}, "team_dirs": {}},
            "socket": "example", "windows_started": false, "base_env": {}, "members": {}, "plugins": {}
        });
        let contents = match file {
            "registry.json" => {
                serde_json::json!({"schema": 1, "instances": {"sample-2222": old_entry}, "issued": {"sample": ["2222"]}})
            }
            "sample-2222/start.json" => old_meta,
            _ => old_record,
        };
        fs::write(&path, serde_json::to_vec(&contents).unwrap()).unwrap();
        if file.ends_with("team.json") {
            let mut registry = borgchart::registry::Registry {
                schema: borgchart::registry::SCHEMA,
                ..Default::default()
            };
            registry.instances.insert(
                "sample-2222".into(),
                borgchart::registry::Entry {
                    team: "sample".into(),
                    team_dir: root.path().join("template"),
                    folder: root.path().join("work"),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            );
            fs::write(
                root.path().join("state/registry.json"),
                serde_json::to_vec(&registry).unwrap(),
            )
            .unwrap();
        }
        let state_dir = root.path().join("state/sample-2222");
        let args = match file {
            "registry.json" => vec!["status"],
            "sample-2222/start.json" => vec![
                "_run",
                state_dir.to_str().unwrap(),
                "lead-abcd",
                "example",
                "run",
            ],
            _ => vec!["status", "sample-2222"],
        };
        let out = brg(root.path(), &args);
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "old state accepted: {out:?}");
        assert!(error.contains("older brg"), "{file}: {error}");
        assert!(error.contains("destroy it with that brg"), "{error}");
        assert!(
            error.contains(&*path.parent().unwrap().to_string_lossy()),
            "{error}"
        );
    }
}

#[test]
fn start_template_flag_replaces_the_retired_team_flag() {
    let root = tempfile::tempdir().unwrap();
    for (flag, expected) in [("--template", 0), ("--team", 2)] {
        let out = brg(
            root.path(),
            &["start", "sample-2222", flag, "./example", "--help"],
        );
        assert_eq!(out.status.code(), Some(expected), "{flag}: {out:?}");
        if expected == 2 {
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("--template"),
                "{out:?}"
            );
        }
    }
}

#[test]
fn template_catalog_exposes_template_sources_without_retired_keys() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("config.yaml"),
        "schema: 1\ntemplates: [./templates]\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join("templates/example")).unwrap();
    fs::write(
        root.path().join("templates/example/template.yaml"),
        "roles: {}\n",
    )
    .unwrap();
    let out = brg(root.path(), &["templates", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows[0]["template"], "example");
    assert_eq!(
        rows[0]["template_dir"],
        root.path()
            .join("templates/example")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(rows[0].get("team").is_none() && rows[0].get("dir").is_none());
}

#[test]
fn legacy_catalog_entries_are_reported_without_changing_catalog_stdout() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("config.yaml"),
        "schema: 1\ntemplates: [./templates]\n",
    )
    .unwrap();
    for name in ["first", "second"] {
        let dir = root.path().join("templates").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("team.yaml"), "roles: {}\n").unwrap();
    }
    for args in [vec!["templates"], vec!["templates", "--json"]] {
        let out = brg(root.path(), &args);
        assert!(out.status.success(), "{out:?}");
        let warnings = String::from_utf8_lossy(&out.stderr);
        assert_eq!(warnings.lines().count(), 2, "{warnings}");
        for name in ["first", "second"] {
            assert!(
                warnings.contains(&*root.path().join("templates").join(name).to_string_lossy()),
                "{warnings}"
            );
        }
        assert!(warnings.contains("template.yaml"), "{warnings}");
        if args.len() == 2 {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
                serde_json::json!([])
            );
        }
    }
}

#[test]
fn legacy_default_catalog_and_shadowed_names_are_not_silently_ignored() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let old_catalog = home.join(".config/borgchart/teams");
    fs::create_dir_all(old_catalog.join("example")).unwrap();
    fs::write(old_catalog.join("example/template.yaml"), "roles: {}\n").unwrap();
    for args in [vec!["templates", "--json"], vec!["validate", "example"]] {
        let out = common::command(env!("CARGO_BIN_EXE_brg"))
            .env("HOME", &home)
            .env("BORGCHART_CONFIG", root.path().join("missing.yaml"))
            .env("BORGCHART_STATE_HOME", root.path().join("state"))
            .args(&args)
            .output()
            .unwrap();
        let message = String::from_utf8_lossy(&out.stderr);
        assert!(message.contains(&*old_catalog.to_string_lossy()), "{out:?}");
        assert!(message.contains("rename it to templates"), "{message}");
        if args[0] == "templates" {
            assert!(out.status.success());
            assert_eq!(message.lines().count(), 1);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
                serde_json::json!([])
            );
        } else {
            assert!(!out.status.success());
        }
    }
    for (catalog, filename) in [("new", "template.yaml"), ("old", "team.yaml")] {
        fs::create_dir_all(root.path().join(catalog).join("example")).unwrap();
        fs::write(
            root.path().join(catalog).join("example").join(filename),
            "roles: {}\n",
        )
        .unwrap();
    }
    fs::write(
        root.path().join("config.yaml"),
        "schema: 1\ntemplates: [./new, ./old]\n",
    )
    .unwrap();
    let out = brg(root.path(), &["validate", "example"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains(&*root.path().join("old/example").to_string_lossy()),
        "{out:?}"
    );
}

#[test]
fn retired_role_key_hints_preserve_location_and_do_not_mislabel_plugin_errors() {
    let root = tempfile::tempdir().unwrap();
    for (text, role_hint) in [
        ("roles: {dev: {team: example}}\n", true),
        ("plugins: {example: {team: example}}\nroles: {}\n", false),
        (
            "plugins: {example: {team: example}}\nroles: {dev: {team: example}}\n",
            false,
        ),
    ] {
        fs::write(root.path().join("template.yaml"), text).unwrap();
        let out = brg(root.path(), &["validate", root.path().to_str().unwrap()]);
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(error.contains("unknown field `team`"), "{error}");
        assert_eq!(error.contains("template:"), role_hint, "{error}");
        if role_hint {
            assert!(error.contains("line 1 column"), "{error}");
        }
    }
}
