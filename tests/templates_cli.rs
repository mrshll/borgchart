//! Template selection and catalog output through the command-line boundary.

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
fn template_catalog_exposes_template_sources() {
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
}
