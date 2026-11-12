//! Installing the current binaries through the real Makefile recipe.

mod common;

use std::fs;

#[test]
fn install_links_the_current_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Makefile"),
        root.join("Makefile"),
    )
    .unwrap();
    fs::create_dir_all(root.join("target/release")).unwrap();
    // Prebuilt stand-ins let the real install recipe run without a release build.
    for name in ["brg", "brg-seat", "brg-slack", "brg-checks"] {
        fs::write(root.join("target/release").join(name), name).unwrap();
    }
    let prefix = root.join("prefix");
    let bindir = root.join("bin");
    fs::create_dir_all(prefix.join("brg/bin")).unwrap();
    fs::create_dir_all(&bindir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Example",
            "-c",
            "user.email=example@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Initial",
            "--allow-empty",
        ],
    ] {
        let out = common::command("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    let out = common::command("make")
        .current_dir(root)
        .args(["--old-file=build", "install"])
        .arg(format!("PREFIX={}", prefix.display()))
        .arg(format!("BINDIR={}", bindir.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    for name in ["brg", "brg-seat", "brg-slack", "brg-checks"] {
        assert_eq!(fs::read_to_string(bindir.join(name)).unwrap(), name);
    }
}
