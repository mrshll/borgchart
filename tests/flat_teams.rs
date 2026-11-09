//! Flat definitions resolve through the configured catalog; assets stay confined.
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use borgchart::config::Config;
use borgchart::validate::{Validated, validate_with};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn check(root: &Path, roots: &[&Path]) -> Result<Validated, String> {
    let (report, validated) = validate_with(
        root,
        Config {
            teams: roots.iter().map(|p| p.to_path_buf()).collect(),
            ..Config::default()
        },
    );
    validated.ok_or_else(|| {
        report
            .errors()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn parent(root: &Path, name: &str) {
    write(
        root,
        "parent/team.yaml",
        &format!("bus: {{}}\nroles: {{child: {{team: '{name}', cwd: 'copies/{{member_id}}'}}}}\n"),
    );
}

#[test]
fn names_resolve_across_configured_roots_and_refuse_missing_or_ambiguous() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    parent(a.path(), "child");
    write(b.path(), "child/team.yaml", "bus: {}\nroles: {}\n");
    let v = check(&a.path().join("parent"), &[a.path(), b.path()]).unwrap();
    assert_eq!(v.team.team_roles["child"].dir, b.path().join("child"));
    assert!(
        check(&a.path().join("parent"), &[a.path()])
            .unwrap_err()
            .contains("no team `child`")
    );
    write(a.path(), "child/team.yaml", "bus: {}\nroles: {}\n");
    assert!(
        check(&a.path().join("parent"), &[a.path(), b.path()])
            .unwrap_err()
            .contains("more than one team")
    );
}

#[test]
fn paths_and_shared_forms_are_not_team_names() {
    let root = tempfile::tempdir().unwrap();
    for name in [
        ".",
        "./child",
        "../child",
        "/tmp/child",
        "teams/child",
        "shared:child",
    ] {
        parent(root.path(), name);
        assert!(
            check(&root.path().join("parent"), &[root.path()])
                .unwrap_err()
                .contains("team name"),
            "{name}"
        );
    }
}

#[test]
fn canonical_identity_refuses_cycles_but_allows_reuse() {
    let root = tempfile::tempdir().unwrap();
    parent(root.path(), "parent");
    assert!(
        check(&root.path().join("parent"), &[root.path()])
            .unwrap_err()
            .contains("team cycle")
    );
    parent(root.path(), "child");
    write(
        root.path(),
        "child/team.yaml",
        "bus: {}\nroles: {back: {team: parent, cwd: 'copies/{member_id}'}}\n",
    );
    assert!(
        check(&root.path().join("parent"), &[root.path()])
            .unwrap_err()
            .contains("team cycle")
    );
    write(root.path(), "child/team.yaml", "bus: {}\nroles: {}\n");
    write(
        root.path(),
        "parent/team.yaml",
        "bus: {}\nroles:\n  first: {team: child, cwd: 'copies/{member_id}'}\n  second: {team: child, cwd: 'copies/{member_id}'}\n",
    );
    symlink(root.path().join("child"), root.path().join("alias")).unwrap();
    assert!(check(&root.path().join("parent"), &[root.path()]).is_ok());
}

fn assets(root: &Path) {
    write(
        root,
        "parent/team.yaml",
        "includes: catalog:shared/includes\nplugins: {seat: {path: 'catalog:shared/plugins/seat', command: seat}}\nroles: {lead: {prompt: lead.md}}\n",
    );
    write(
        root,
        "parent/lead.md",
        "---\nmodels: {claude: opus}\n---\n{{includes/rules}}\n",
    );
    write(root, "shared/includes/rules.md", "Be brief.\n");
    write(root, "shared/plugins/seat/seat", "#!/bin/sh\nexit 0\n");
    fs::set_permissions(
        root.join("shared/plugins/seat/seat"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

#[test]
fn catalog_assets_resolve_and_location_changes_affect_snapshots() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    assets(a.path());
    assets(b.path());
    let first = check(&a.path().join("parent"), &[a.path()]).unwrap();
    let second = check(&b.path().join("parent"), &[b.path()]).unwrap();
    assert_eq!(first.team.roles["lead"].instruction, "Be brief.\n");
    assert_eq!(
        first.plugin_dir("seat").unwrap(),
        a.path().join("shared/plugins/seat")
    );
    assert_ne!(
        first.config_snapshot().digest,
        second.config_snapshot().digest
    );
    assert_eq!(borgchart::team::list(&[a.path().to_path_buf()]).len(), 1);
    let frozen = first.config_snapshot();
    fs::remove_dir_all(a.path().join("shared")).unwrap();
    // A validated snapshot describes the locations it actually validated.
    assert_eq!(first.config_snapshot(), frozen);
}

#[test]
fn catalog_assets_refuse_traversal_and_symlinked_shared_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    assets(root.path());
    assets(outside.path());
    for path in [
        "catalog:.ssh",
        "catalog:/shared/includes",
        "catalog:shared/../shared/includes",
        "catalog:shared/includes/../../parent",
    ] {
        write(
            root.path(),
            "parent/team.yaml",
            &format!("includes: '{path}'\nroles: {{lead: {{prompt: lead.md}}}}\n"),
        );
        assert!(
            check(&root.path().join("parent"), &[root.path()]).is_err(),
            "{path}"
        );
    }
    assets(root.path());
    fs::remove_dir_all(root.path().join("shared")).unwrap();
    symlink(outside.path().join("shared"), root.path().join("shared")).unwrap();
    assert!(
        check(&root.path().join("parent"), &[root.path()])
            .unwrap_err()
            .contains("symlink")
    );
}

#[test]
fn each_catalog_asset_location_is_frozen_even_with_identical_contents() {
    for plugin in [false, true] {
        let root = tempfile::tempdir().unwrap();
        assets(root.path());
        let original = if plugin {
            "shared/plugins/seat"
        } else {
            "shared/includes"
        };
        let moved = if plugin {
            "shared/plugins/other"
        } else {
            "shared/other"
        };
        let first = check(&root.path().join("parent"), &[root.path()])
            .unwrap()
            .config_snapshot();
        fs::rename(root.path().join(original), root.path().join(moved)).unwrap();
        symlink(root.path().join(moved), root.path().join(original)).unwrap();
        let second = check(&root.path().join("parent"), &[root.path()])
            .unwrap()
            .config_snapshot();
        assert_eq!(
            first.inputs["members/lead/instruction"],
            second.inputs["members/lead/instruction"]
        );
        assert_ne!(first.digest, second.digest, "plugin: {plugin}");
    }
}

#[test]
fn catalog_descendants_cannot_escape_shared_and_prompts_remain_team_local() {
    for plugin in [false, true] {
        let root = tempfile::tempdir().unwrap();
        assets(root.path());
        let original = if plugin {
            "shared/plugins/seat"
        } else {
            "shared/includes"
        };
        fs::rename(root.path().join(original), root.path().join("outside")).unwrap();
        symlink(root.path().join("outside"), root.path().join(original)).unwrap();
        assert!(
            check(&root.path().join("parent"), &[root.path()])
                .unwrap_err()
                .contains("resolves outside")
        );
    }
    let root = tempfile::tempdir().unwrap();
    assets(root.path());
    write(
        root.path(),
        "parent/team.yaml",
        "roles: {lead: {prompt: 'catalog:shared/includes/rules.md'}}\n",
    );
    assert!(check(&root.path().join("parent"), &[root.path()]).is_err());
}

#[test]
fn catalog_plugins_reject_nonshared_paths_and_shared_root_aliases() {
    let root = tempfile::tempdir().unwrap();
    assets(root.path());
    for path in [
        "catalog:.local",
        "catalog:.ssh",
        "catalog:/shared/plugins/seat",
        "catalog:shared/../shared/plugins/seat",
        "catalog:shared//etc",
    ] {
        write(
            root.path(),
            "parent/team.yaml",
            &format!("plugins: {{seat: {{path: '{path}', command: seat}}}}\nroles: {{}}\n"),
        );
        assert!(
            check(&root.path().join("parent"), &[root.path()]).is_err(),
            "{path}"
        );
    }
    assets(root.path());
    fs::rename(root.path().join("shared"), root.path().join("other")).unwrap();
    symlink(root.path().join("other"), root.path().join("shared")).unwrap();
    assert!(
        check(&root.path().join("parent"), &[root.path()])
            .unwrap_err()
            .contains("must not be a symlink")
    );
}

#[test]
fn a_local_nested_folder_is_not_an_implicit_name_search_root() {
    let root = tempfile::tempdir().unwrap();
    parent(root.path(), "child");
    write(root.path(), "parent/child/team.yaml", "roles: {}\n");
    assert!(
        check(&root.path().join("parent"), &[root.path()])
            .unwrap_err()
            .contains("no team `child`")
    );
}

#[test]
fn digit_leading_team_names_keep_find_semantics() {
    let root = tempfile::tempdir().unwrap();
    parent(root.path(), "123-dev");
    write(root.path(), "123-dev/team.yaml", "roles: {}\n");
    let v = check(&root.path().join("parent"), &[root.path()]).unwrap();
    assert_eq!(
        v.team.team_roles["child"].dir,
        borgchart::team::find("123-dev", &[root.path().to_path_buf()]).unwrap()
    );
}
