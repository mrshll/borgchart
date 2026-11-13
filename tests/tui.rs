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
        "team/template.yaml",
        "bus: {}\nplugins:\n  watcher: {path: plugins/watcher, process: [./run.sh]}\nroles:\n  alpha: {prompt: alpha.md}\n  beta: {prompt: beta.md, vendor: codex, parent: alpha}\n  scout: {prompt: scout.md, parent: alpha, start: false}\n",
    );
    write("team/alpha.md", "---\n---\nYou are alpha.\n");
    write("team/scout.md", "---\n---\nYou are scout.\n");
    write(
        "team/beta.md",
        "---\nmodels: {codex: example-model low}\n---\nYou are beta.\n",
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
        || screen().contains(" folder ") && screen().contains("alpha"),
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
                l.contains("└─ ? beta")
                    && l.contains("Λ example-model low")
                    && l.contains("blocked (folder trust)")
                    // `brg attach <team>/<beta's member>`
                    && l.contains("brg attach team-")
                    && l.contains("/beta-")
            }) && s.contains("plugin watcher")
        },
        dump,
    );

    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("state/registry.json")).unwrap()).unwrap();
    let instance = registry["teams"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(format!("state/{instance}/team.json"))).unwrap(),
    )
    .unwrap();
    let alpha = record["members"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, member)| member["role"] == "alpha")
        .unwrap()
        .0;
    for _ in 0..2 {
        let out = common::command(brg)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("BORGCHART_STATE_HOME", root.join("state"))
            .env("BORGCHART_CONFIG", root.join("home/config.yaml"))
            .args([
                "msg",
                "send",
                &format!("{instance}/{alpha}"),
                "pending",
                "--external",
                "test",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "r"]);
    wait(
        "unread messages in status -i",
        || screen().contains("2 unacked"),
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

    // scout, a `start: false` role with no member, is hidden (beta above
    // stays alpha's last row) until `a` shows it, and `a` hides it again,
    // across a refresh.
    assert!(!screen().contains("scout"), "{}", dump());
    assert!(screen().contains("a roles"), "{}", dump());
    // Select the plugin's row, below where scout appears: the selection
    // stays on it as rows come and go.
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "j"]);
    let selected = || {
        screen()
            .lines()
            .find(|l| l.contains("> "))
            .unwrap_or_default()
            .to_string()
    };
    wait(
        "the plugin's row selected",
        || selected().contains("watcher"),
        dump,
    );
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "a"]);
    wait(
        "scout shown",
        || {
            let s = screen();
            s.lines().any(|l| l.contains("├─ ? beta"))
                && s.lines()
                    .any(|l| l.contains("└─   scout") && l.contains("not created (start: false)"))
        },
        dump,
    );
    // The 2 s refresh keeps it.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(screen().contains("scout"), "{}", dump());
    assert!(selected().contains("watcher"), "{}", dump());
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "a"]);
    wait(
        "scout hidden again",
        || !screen().contains("scout") && screen().contains("└─ ? beta"),
        dump,
    );
    assert!(selected().contains("watcher"), "{}", dump());

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

#[test]
fn bare_status_i_shows_every_team_and_attaches_in_the_selected_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write(
        "team/template.yaml",
        "roles:\n  alpha: {prompt: alpha.md}\n",
    );
    write("team/alpha.md", "---\n---\nYou are alpha.\n");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    fs::create_dir_all(root.join("work/a")).unwrap();
    fs::create_dir_all(root.join("work/b")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), root.join("bin/claude")).unwrap();

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
            "#!/bin/sh\n{env_line}{brg} create {team} {work}/a >/dev/null || exit 9\n{brg} create {team} {work}/b >/dev/null || exit 9\nexec {brg} status -i\n",
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
            "160",
            "-y",
            "30",
        ])
        .arg(script.display().to_string())
        .status()
        .unwrap();
    let screen = || tmux(&private, &outer, &["capture-pane", "-p", "-t", "=o:"]);
    let attached = || tmux(&private, &inner, &["list-clients", "-F", "#{session_name}"]);
    let dump = || format!("outer screen:\n{}", screen());

    let instances = || -> Vec<String> {
        fs::read(root.join("state/registry.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|r| r["teams"].as_object().map(|m| m.keys().cloned().collect()))
            .unwrap_or_default()
    };
    wait("two teams", || instances().len() == 2, dump);
    let ids = instances();
    // Each team's header line, then its tree with alpha idle.
    wait(
        "both teams on the screen",
        || {
            let s = screen();
            ids.iter().all(|id| s.contains(&format!("{id}  folder ")))
                && s.lines()
                    .filter(|l| l.contains("alpha") && l.contains("idle"))
                    .count()
                    == 2
        },
        dump,
    );
    // The team whose header comes second.
    let s = screen();
    let at = |id: &String| s.find(&format!("{id}  folder ")).unwrap();
    let second = ids.iter().max_by_key(|id| at(id)).unwrap().clone();

    // Rows: first header, its alpha, second header, its alpha.
    tmux(
        &private,
        &outer,
        &["send-keys", "-t", "=o:", "j", "j", "j", "Enter"],
    );
    wait(
        "attached to the second team's alpha",
        || attached().trim() == format!("brg-{second}"),
        dump,
    );
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "C-b", "d"]);
    wait(
        "back in the status screen",
        || attached().trim().is_empty() && screen().contains("back from alpha"),
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
}

#[test]
fn bare_status_i_with_no_teams_says_so_until_one_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write(
        "team/template.yaml",
        "roles:\n  alpha: {prompt: alpha.md}\n",
    );
    write("team/alpha.md", "---\n---\nYou are alpha.\n");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), root.join("bin/claude")).unwrap();

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
    let script = |name: &str, body: &str| {
        let path = root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{env_line}{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let screen_script = script("run.sh", &format!("exec {brg} status -i"));
    let create = script(
        "create.sh",
        &format!(
            "exec {brg} create {team} {work}",
            team = root.join("team").display(),
            work = root.join("work").display(),
        ),
    );

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
            "160",
            "-y",
            "30",
        ])
        .arg(screen_script.display().to_string())
        .status()
        .unwrap();
    let screen = || tmux(&private, &outer, &["capture-pane", "-p", "-t", "=o:"]);
    let dump = || format!("outer screen:\n{}", screen());

    wait(
        "the screen saying there are no teams",
        || screen().contains("there are no teams"),
        dump,
    );
    // Still there, and still running, after a refresh.
    std::thread::sleep(Duration::from_secs(3));
    assert!(screen().contains("there are no teams"), "{}", dump());

    let out = common::command(&create).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let id = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    wait(
        "the new team's header at a later refresh",
        || {
            let s = screen();
            s.contains(&format!("{id}  folder ")) && !s.contains("there are no teams")
        },
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
}

/// A one-shot command in a private terminal, plus the same status through a pipe.
/// No members start: the fixture has only an uncreated role.
fn status_terminal(no_colour: bool, action: &str) -> (String, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("team")).unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    fs::write(
        root.join("team/template.yaml"),
        "roles:\n  scout: {prompt: scout.md, start: false}\n",
    )
    .unwrap();
    fs::write(
        root.join("team/scout.md"),
        "---\n---\nObserve the harbor.\n",
    )
    .unwrap();
    let brg = env!("CARGO_BIN_EXE_brg");
    let private = common::PrivateTmux::new();
    let inner = private.socket("status-inner");
    let create = common::command(brg)
        .env_clear()
        .env("PATH", private.path_with("/usr/bin:/bin"))
        .env("BORGCHART_TMUX_TMPDIR", private.dir())
        .env("BORGCHART_TMUX_SOCKET", &inner)
        .env("HOME", root)
        .env("BORGCHART_CONFIG", root.join("missing.yaml"))
        .env("BORGCHART_STATE_HOME", root.join("state"))
        .args(["create"])
        .arg(root.join("team"))
        .arg(root.join("work"))
        .output()
        .unwrap();
    assert!(create.status.success(), "{create:?}");
    let created = String::from_utf8(create.stdout).unwrap();
    let id = created.split_whitespace().next().unwrap();
    let pipe = common::command(brg)
        .env_clear()
        .env("PATH", private.path_with("/usr/bin:/bin"))
        .env("BORGCHART_TMUX_TMPDIR", private.dir())
        .env("BORGCHART_TMUX_SOCKET", &inner)
        .env("HOME", root)
        .env("BORGCHART_CONFIG", root.join("missing.yaml"))
        .env("BORGCHART_STATE_HOME", root.join("state"))
        .args(["status", id])
        .output()
        .unwrap();
    assert!(pipe.status.success(), "{pipe:?}");
    let script = root.join("status.sh");
    fs::write(&script, format!(
        "#!/bin/sh\nenv -i PATH={guard}:/usr/bin:/bin HOME={root} TERM=xterm-256color LANG=C.UTF-8 BORGCHART_CONFIG={root}/missing.yaml BORGCHART_STATE_HOME={root}/state BORGCHART_TMUX_TMPDIR={tmpdir} BORGCHART_TMUX_SOCKET={inner} {no_colour} {brg} {action} {id}\nprintf '\\nSCENE-DONE\\n'\nsleep 600\n",
        guard = private.bin().display(), root = root.display(),
        tmpdir = private.dir().display(),
        no_colour = if no_colour { "NO_COLOR=1" } else { "" },
    )).unwrap();
    let socket = private.socket("status-once");
    let out = private
        .tmux(&socket)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "o",
            "-x",
            "160",
            "-y",
            "24",
            "sh",
        ])
        .arg(&script)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let screen = || {
        tmux(
            &private,
            &socket,
            &["capture-pane", "-p", "-e", "-t", "=o:"],
        )
    };
    wait(
        "one-shot status",
        || screen().contains("SCENE-DONE"),
        screen,
    );
    (screen(), String::from_utf8(pipe.stdout).unwrap())
}

#[test]
fn no_color_keeps_one_shot_terminal_status_unpainted() {
    for action in ["status", "start"] {
        let (painted, _) = status_terminal(false, action);
        assert!(
            painted.contains("\x1b["),
            "{action} positive control must paint: {painted:?}"
        );
        let (plain, _) = status_terminal(true, action);
        assert!(
            !plain.contains("\x1b["),
            "NO_COLOR must suppress {action} painting: {plain:?}"
        );
    }
}

#[test]
fn terminal_status_summarizes_without_adding_a_summary_to_the_pipe() {
    let (terminal, pipe) = status_terminal(false, "status");
    let summary = borgchart::runtime::summary(&[], false);
    assert!(terminal.contains(&summary), "{terminal:?}");
    assert!(!pipe.contains(&summary), "{pipe:?}");
    let id = pipe.split_whitespace().next().unwrap();
    assert!(terminal.find(&summary).unwrap() < terminal.find(id).unwrap());
    assert_eq!(terminal.matches("borgchart").count(), 1);
}

#[test]
fn watch_tabs_follow_messages_without_leaking_bodies_and_restore_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for name in ["team", "work", "home", "bin"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    fs::write(
        root.join("team/template.yaml"),
        "bus: {}\nroles:\n  alpha: {prompt: alpha.md}\n",
    )
    .unwrap();
    fs::write(
        root.join("team/alpha.md"),
        "---\n---\nObserve the harbor.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), root.join("bin/claude")).unwrap();
    let private = common::PrivateTmux::new();
    let inner = private.socket("graph-inner");
    let outer = private.socket("graph-outer");
    let brg = env!("CARGO_BIN_EXE_brg");
    let mut create = common::command(brg);
    create
        .env_clear()
        .env(
            "PATH",
            private.path_with(&format!("{}:/usr/bin:/bin", root.join("bin").display())),
        )
        .env("HOME", root.join("home"))
        .env("BORGCHART_STATE_HOME", root.join("state"))
        .env("BORGCHART_CONFIG", root.join("missing.yaml"));
    private.brg_env(&mut create, &inner);
    let out = create
        .arg("create")
        .arg(root.join("team"))
        .arg(root.join("work"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let output = String::from_utf8(out.stdout).unwrap();
    let id = output.split_whitespace().next().unwrap();
    let script = root.join("watch.sh");
    fs::write(&script,format!("#!/bin/sh\nenv -i PATH={guard}:{root}/bin:/usr/bin:/bin HOME={root}/home TERM=xterm-256color LANG=C.UTF-8 BORGCHART_CONFIG={root}/missing.yaml BORGCHART_STATE_HOME={root}/state BORGCHART_TMUX_TMPDIR={tmpdir} BORGCHART_TMUX_SOCKET={inner} {brg} status -w {id} --interval 1\nprintf 'RESTORED:'\nstty -a\nsleep 600\n",guard=private.bin().display(),root=root.display(),tmpdir=private.dir().display())).unwrap();
    let out = private
        .tmux(&outer)
        .args(["new-session", "-d", "-s", "o", "-x", "80", "-y", "24", "sh"])
        .arg(script)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let screen = || tmux(&private, &outer, &["capture-pane", "-p", "-t", "=o:"]);
    wait("tree tab", || screen().contains("▸tree"), screen);
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "Tab"]);
    wait("messages tab", || screen().contains("▸messages"), screen);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(format!("state/{id}/team.json"))).unwrap())
            .unwrap();
    let member = record["members"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let mut send = common::command(brg);
    send.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("BORGCHART_STATE_HOME", root.join("state"))
        .env("BORGCHART_CONFIG", root.join("missing.yaml"))
        .args([
            "msg",
            "send",
            "--external",
            "owner",
            &format!("{id}/{member}"),
            "PRIVATE-BODY-9b7d",
        ]);
    let out = send.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    wait(
        "waiting message",
        || {
            let s = screen();
            s.contains("[1]") && s.contains("m1")
        },
        screen,
    );
    assert!(!screen().contains("PRIVATE-BODY-9b7d"));
    println!(
        "create: {output}send: {}graph:\n{}",
        String::from_utf8_lossy(&out.stdout),
        screen()
    );
    tmux(&private, &outer, &["send-keys", "-t", "=o:", "q"]);
    wait(
        "restored terminal",
        || screen().contains("RESTORED:"),
        screen,
    );
    let s = screen();
    assert!(!s.contains("-icanon") && !s.contains("-echo "), "{s}");
    assert!(
        !s.contains("▸messages"),
        "alternate screen not restored: {s}"
    );
    println!("exit: {s}");
}
