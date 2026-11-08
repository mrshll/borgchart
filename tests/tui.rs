//! `brg status -i` in a real terminal: an outer tmux server provides the pty.
//! Enter attaches to a member, the detach key returns to the screen, q quits.
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

fn tmux(private: &common::PrivateTmux, socket: &str, args: &[&str]) -> String {
    let out = private.tmux(socket).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn wait(what: &str, f: impl Fn() -> bool, dump: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    panic!("timed out waiting for {what}\n{}", dump());
}

#[test]
fn status_i_attaches_detaches_and_quits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write(
        "team/team.yaml",
        "bus: {}\nplugins:\n  watcher: {path: plugins/watcher, process: [./run.sh]}\nmembers:\n  alpha: {prompt: alpha.md}\n  beta: {prompt: beta.md, vendor: codex, parent: alpha}\n",
    );
    write("team/alpha.md", "---\nrole: helper\n---\nYou are alpha.\n");
    write(
        "team/beta.md",
        "---\nrole: helper\nmodels: {codex: example-model low}\n---\nYou are beta.\n",
    );
    write("team/plugins/watcher/run.sh", "#!/bin/sh\nexec sleep 600\n");
    fs::set_permissions(
        root.join("team/plugins/watcher/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    // beta (Codex) stops at its folder-trust screen.
    write("home/fake-agent/codex-trust", "");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), root.join("bin/claude")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), root.join("bin/codex")).unwrap();

    // Both servers, brg's and the outer one, in the test's private directory.
    let private = common::PrivateTmux::new();
    let inner = private.socket("brg-tui-in");
    let outer = private.socket("brg-tui-out");
    let env_line = format!(
        "export PATH={guard}:{bin}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin HOME={home} LANG=C.UTF-8 TERM=xterm-256color BORGCHART_STATE_HOME={state} BORGCHART_CONFIG={home}/config.yaml BORGCHART_TMUX_TMPDIR={tmpdir} BORGCHART_TMUX_SOCKET={inner}\ncd {team}\n",
        guard = private.bin().display(),
        bin = root.join("bin").display(),
        home = root.join("home").display(),
        state = root.join("state").display(),
        tmpdir = private.dir().display(),
        team = root.join("team").display(),
    );
    let brg = env!("CARGO_BIN_EXE_brg");
    let script = root.join("run.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n{env_line}id=$({brg} create {team} {work} | head -n 1 | cut -d' ' -f1) || exit 9\nexec {brg} status \"$id\" -i\n",
            team = root.join("team").display(),
            work = root.join("work").display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    private
        .tmux(&outer)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "o",
            "-x",
            "140",
            "-y",
            "30",
        ])
        .arg(script.display().to_string())
        .status()
        .unwrap();
    let screen = || tmux(&private, &outer, &["capture-pane", "-p", "-t", "=o:"]);
    let clients = || tmux(&private, &inner, &["list-clients"]).lines().count();
    let dump = || format!("outer screen:\n{}", screen());

    wait(
        "the status screen",
        || screen().contains(" team ") && screen().contains("alpha"),
        dump,
    );
    wait("alpha idle", || screen().contains("idle"), dump);
    // The same tree and columns as `brg status`, and beta's row says to
    // attach and answer its trust screen.
    wait(
        "the tree with beta waiting at folder trust",
        || {
            let s = screen();
            s.lines().any(|l| {
                l.contains("└─ beta")
                    && l.contains("codex example-model low")
                    && l.contains("blocked (folder trust)")
                    // `brg attach <instance>/<beta's member>`
                    && l.contains("brg attach team-")
                    && l.contains("/beta-")
            }) && s.contains("plugin watcher")
        },
        dump,
    );

    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("state/registry.json")).unwrap()).unwrap();
    let instance = registry["instances"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(format!("state/{instance}/instance.json"))).unwrap(),
    )
    .unwrap();
    let alpha = record["members"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, member)| member["role"] == "alpha")
        .unwrap()
        .0;
    let draft = root.join("message.json");
    fs::write(
        &draft,
        serde_json::json!({"to": alpha, "body": "pending"}).to_string(),
    )
    .unwrap();
    for _ in 0..2 {
        let out = common::command(brg)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("BORGCHART_STATE_HOME", root.join("state"))
            .env("BORGCHART_CONFIG", root.join("home/config.yaml"))
            .args([
                "msg",
                "publish",
                "--instance",
                instance,
                "--external",
                "test",
                "--record",
            ])
            .arg(&draft)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "r"]);
    wait(
        "unread messages in status -i",
        || screen().contains("2 unread"),
        dump,
    );

    tmux(&private, &outer, &["send-keys", "-t", "=o:", "Enter"]);
    wait(
        "attached to alpha",
        || clients() == 1 && screen().contains("don't ask on"),
        dump,
    );

    // The detach key reaches brg's tmux client, which returns to the screen.
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "C-b", "d"]);
    wait(
        "back in the status screen",
        || clients() == 0 && screen().contains("back from alpha"),
        dump,
    );

    // Enter on beta's row attaches to its trust screen.
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "j", "Enter"]);
    wait(
        "attached to beta",
        || clients() == 1 && screen().contains("Trust and continue"),
        dump,
    );
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "C-b", "d"]);
    wait(
        "back from beta",
        || clients() == 0 && screen().contains("back from beta"),
        dump,
    );

    tmux(&private, &outer, &["send-keys", "-t", "=o:", "q"]);
    wait(
        "the screen to close",
        || {
            !private
                .tmux(&outer)
                .args(["has-session", "-t", "=o"])
                .status()
                .unwrap()
                .success()
        },
        dump,
    );
    let _ = Path::new(brg);
}
