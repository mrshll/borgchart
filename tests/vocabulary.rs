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
    for (file, args) in [
        ("registry.json", vec!["status"]),
        ("sample-2222/instance.json", vec!["status", "sample-2222"]),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state").join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"schema":1,"instances":{},"issued":{}}"#).unwrap();
        if file != "registry.json" {
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
        let out = brg(root.path(), &args);
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "old state accepted: {out:?}");
        assert!(error.contains("older brg"), "{error}");
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
