//! Installing brg retires only the legacy command link it owns.

mod common;

use std::fs;
use std::os::unix::fs::symlink;

#[test]
fn install_removes_only_its_legacy_binary_link() {
    for kind in ["owned", "unrelated", "regular"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Makefile"),
            root.join("Makefile"),
        )
        .unwrap();
        fs::create_dir_all(root.join("target/release")).unwrap();
        // Prebuilt stand-ins let the real install recipe run without a release build.
        for name in ["brg", "brg-seat"] {
            fs::write(root.join("target/release").join(name), name).unwrap();
        }
        let prefix = root.join("prefix");
        let bindir = root.join("bin");
        fs::create_dir_all(prefix.join("brg/bin")).unwrap();
        fs::create_dir_all(&bindir).unwrap();
        let legacy = bindir.join("borgchart");
        let owned = prefix.join("brg/bin/borgchart");
        fs::write(&owned, "old binary").unwrap();
        let unrelated = root.join("unrelated");
        fs::write(&unrelated, "keep").unwrap();
        match kind {
            "owned" => symlink(&owned, &legacy).unwrap(),
            "unrelated" => symlink(&unrelated, &legacy).unwrap(),
            "regular" => fs::write(&legacy, "keep").unwrap(),
            _ => unreachable!(),
        }
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
        assert!(out.status.success(), "{kind}: {out:?}");
        if kind == "owned" {
            assert!(legacy.symlink_metadata().is_err(), "old command remains");
        } else {
            assert_eq!(fs::read_to_string(&legacy).unwrap(), "keep", "{kind}");
            if kind == "unrelated" {
                assert_eq!(fs::read_link(&legacy).unwrap(), unrelated);
            } else {
                assert!(!legacy.is_symlink());
            }
        }
        for name in ["brg", "brg-seat"] {
            assert_eq!(fs::read_to_string(bindir.join(name)).unwrap(), name);
        }
    }
}
