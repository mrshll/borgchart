//! Native tmux popups target the originating client without resizing members.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::path::Path;

use borgchart::state::shell_quote;
use instance_env::{Env, both, script, stdout};

fn outer(env: &Env, args: &[&str]) -> String {
    let out = env.tmux.tmux("popup-out").args(args).output().unwrap();
    assert!(out.status.success(), "outer tmux {args:?}: {}", both(&out));
    stdout(&out)
}

fn screen(env: &Env, name: &str) -> String {
    outer(env, &["capture-pane", "-p", "-t", &format!("={name}:")])
}

fn keys(env: &Env, name: &str, keys: &[&str]) {
    let target = format!("={name}:");
    let mut args = vec!["send-keys", "-t", &target];
    args.extend_from_slice(keys);
    outer(env, &args);
}

fn attach(env: &Env, name: &str, member: &str, width: &str, height: &str) -> String {
    let cmd = env.cmd(&["attach", member]);
    let exports: String = cmd
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                format!(
                    "export {}={}\n",
                    key.to_string_lossy(),
                    shell_quote(&value.to_string_lossy())
                )
            })
        })
        .collect();
    let path = env.path(&format!("{name}.sh"));
    script(
        &path,
        &format!(
            "#!/bin/sh\n{exports}exec {} attach {}\n",
            shell_quote(env!("CARGO_BIN_EXE_brg")),
            shell_quote(member)
        ),
    );
    outer(
        env,
        &[
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            width,
            "-y",
            height,
            path.to_str().unwrap(),
        ],
    );
    let tty = outer(
        env,
        &[
            "display-message",
            "-p",
            "-t",
            &format!("={name}:"),
            "#{pane_tty}",
        ],
    )
    .trim()
    .to_string();
    let find = || {
        env.tmux(&["list-clients", "-F", "#{client_tty}\t#{client_name}"])
            .lines()
            .find_map(|line| {
                let (found, name) = line.split_once('\t')?;
                (found == tty).then(|| name.to_string())
            })
    };
    env.wait_for("original tmux client", || find().is_some());
    find().unwrap()
}

fn client_session(env: &Env, client: &str) -> String {
    env.tmux(&["display-message", "-p", "-c", client, "#{session_name}"])
        .trim()
        .to_string()
}

fn open_popup(env: &Env, name: &str) {
    env.wait_for("attached member screen", || screen(env, name).contains("❯"));
    keys(env, name, &["C-b", "S"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let text = screen(env, name);
        if text.contains("enter switch") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "popup screen: {text}\nbinding: {}",
            env.tmux(&["show-messages"])
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn nested(env: &Env) -> (String, String) {
    env.write("teams/oncall/team.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: teams/dev, parent: lead, start: false, cwd: 'dev/{member_id}'}\n");
    env.team("teams/oncall/teams/dev");
    fs::create_dir_all(env.path("work/a/dev")).unwrap();
    fs::create_dir_all(env.path("work/a/lead")).unwrap();
    let root = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{root}/dev")]);
    let listed: serde_json::Value =
        serde_json::from_slice(&env.ok(&["ls", "--json"]).stdout).unwrap();
    let child = listed
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|team| team["instances"].as_array().unwrap())
        .find_map(|entry| {
            let id = entry["instance"].as_str()?;
            id.starts_with(&format!("{root}/dev-"))
                .then(|| id.to_string())
        })
        .unwrap();
    env.ok(&["start", &child]);
    (root, child)
}

#[test]
fn popup_switches_only_its_client_to_a_nested_member_without_resizing() {
    for (setting, expected) in [
        ("", "on"),
        ("set -g status off\n", "off"),
        ("set -g status 2\n", "2"),
    ] {
        let env = Env::new();
        if !setting.is_empty() {
            env.write("user.conf", setting);
            env.config(&format!(
                "tmux: {{user_config: '{}'}}\n",
                env.path("user.conf").display()
            ));
        }
        let (root, child) = nested(&env);
        let lead = env.member(&root, "lead");
        let child_lead = env.member(&child, "lead");
        let first = attach(&env, "first", &format!("{root}/{lead}"), "140", "30");
        let second = attach(
            &env,
            "second",
            &format!("{child}/{child_lead}"),
            "140",
            "30",
        );
        let sizes = || {
            env.tmux(&[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id} #{pane_width} #{pane_height}",
            ])
        };
        let before = sizes();
        assert_eq!(env.tmux(&["show", "-gv", "status"]).trim(), expected);
        assert_eq!(env.tmux(&["show", "-gv", "status-position"]).trim(), "top");
        open_popup(&env, "first");
        let popup = screen(&env, "first");
        let top: Vec<_> = popup.lines().nth(2).unwrap().chars().collect();
        assert_eq!(top.get(74), Some(&'┌'), "{popup}");
        assert_eq!(top.get(137), Some(&'┐'), "{popup}");
        assert_eq!(sizes(), before);
        keys(&env, "first", &["q"]);
        env.wait_for("popup closes", || {
            !screen(&env, "first").contains("enter switch")
        });
        assert_eq!(sizes(), before);
        open_popup(&env, "first");
        let index = env
            .rows(&root)
            .iter()
            .position(|row| row["instance"] == child && row["member"] == child_lead)
            .unwrap()
            + 1;
        for _ in 0..index {
            keys(&env, "first", &["j"]);
        }
        keys(&env, "first", &["Enter"]);
        env.wait_for("originating client switches to nested session", || {
            client_session(&env, &first) == format!("brg-{child}")
        });
        assert_eq!(client_session(&env, &second), format!("brg-{child}"));
        assert_eq!(
            env.tmux(&["list-clients"]).lines().count(),
            2,
            "no nested client"
        );
        env.wait_for("switch closes popup", || {
            !screen(&env, "first").contains("enter switch")
        });
        assert_eq!(sizes(), before);
    }
}

#[test]
fn small_popup_scrolls_and_keeps_a_failed_switch_visible() {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false, max: 8}\n");
    let root = env.create("oncall", "work/a");
    for _ in 0..8 {
        env.ok(&["create", &format!("{root}/helper")]);
    }
    let lead = env.member(&root, "lead");
    let rows = env.rows(&root);
    let (index, row) = rows
        .iter()
        .enumerate()
        .rev()
        .find(|(_, row)| row["role"] == "helper")
        .unwrap();
    let helper = row["member"].as_str().unwrap();
    let client = attach(&env, "small", &format!("{root}/{lead}"), "40", "12");
    open_popup(&env, "small");
    for _ in 0..=index {
        keys(&env, "small", &["j"]);
    }
    env.wait_for("selection scrolls into the small popup", || {
        screen(&env, "small").contains(helper)
    });
    env.ok(&["stop", &format!("{root}/{helper}")]);
    keys(&env, "small", &["Enter"]);
    env.wait_for("failed switch stays visible", || {
        let text = screen(&env, "small");
        text.contains("enter switch") && text.contains("not running")
    });
    assert_eq!(client_session(&env, &client), format!("brg-{root}"));
    keys(&env, "small", &["q"]);
    env.wait_for("small popup closes", || {
        !screen(&env, "small").contains("enter switch")
    });
}

#[test]
fn line_uses_nested_unread_counts_and_excludes_other_servers() {
    let env = Env::new();
    let (root, child) = nested(&env);
    let lead = env.member(&root, "lead");
    let child_lead = env.member(&child, "lead");
    let sent = env.publish(
        &child,
        &serde_json::json!({"to": child_lead, "body": "pending"}),
    );
    assert!(sent.status.success(), "{}", both(&sent));
    env.team("teams/other");
    let path = env.path("teams/other/team.yaml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("  lead:", "  visitor:")
            .replace("parent: lead", "parent: visitor"),
    )
    .unwrap();
    let foreign = env
        .cmd(&["create", "other", env.path("work/b").to_str().unwrap()])
        .env("BORGCHART_TMUX_SOCKET", env.tmux.socket("other-server"))
        .output()
        .unwrap();
    assert!(foreign.status.success(), "{}", both(&foreign));
    let all = stdout(&env.ok(&["status", "--line"]));
    assert!(all.contains("visitor-"), "{all}");
    let socket = env.tmux(&["display-message", "-p", "#{socket_path}"]);
    let line = stdout(&env.ok(&["status", "--line", "--socket", socket.trim()]));
    assert!(
        line.contains(&lead) && line.contains(&child_lead) && line.contains("1✉"),
        "{line}"
    );
    assert!(!line.contains("visitor-"), "{line}");
}

#[test]
fn callbacks_quote_paths_and_refresh_the_binary_on_existing_servers() {
    let env = Env::new();
    let root = env.create("oncall", "work/a");
    let lead = env.member(&root, "lead");
    attach(&env, "quoted", &format!("{root}/{lead}"), "140", "30");
    let binary = env.tmux(&["show", "-gqv", "@brg-status-bin"]);
    assert_eq!(
        Path::new(binary.trim()),
        Path::new(env!("CARGO_BIN_EXE_brg"))
    );
    assert_eq!(env.tmux(&["show", "-gv", "status-interval"]).trim(), "5");
    let fake = env.path("callback dir/quo'te#(literal)");
    fs::create_dir_all(fake.parent().unwrap()).unwrap();
    script(
        &fake,
        &format!(
            "#!/bin/sh\nif [ \"$2\" = -i ]; then\n  printf '%s\\n' \"$@\" > {}\n  exec {} \"$@\"\nfi\nprintf '%s\\n' \"$@\" > {}\nprintf CALLBACK_OK\n",
            shell_quote(env.path("popup-args").to_str().unwrap()),
            shell_quote(env!("CARGO_BIN_EXE_brg")),
            shell_quote(env.path("callback-args").to_str().unwrap())
        ),
    );
    env.tmux(&[
        "set-option",
        "-g",
        "@brg-status-bin",
        fake.to_str().unwrap(),
    ]);
    env.wait_for("quoted callback executes", || {
        screen(&env, "quoted").contains("CALLBACK_OK")
    });
    let args = fs::read_to_string(env.path("callback-args")).unwrap();
    assert_eq!(
        args.lines().take(3).collect::<Vec<_>>(),
        ["status", "--line", "--socket"]
    );
    assert_eq!(
        args.lines().nth(3).unwrap(),
        env.tmux(&["display-message", "-p", "#{socket_path}"])
            .trim()
    );
    open_popup(&env, "quoted");
    let popup_args = fs::read_to_string(env.path("popup-args")).unwrap();
    assert_eq!(
        popup_args.lines().take(3).collect::<Vec<_>>(),
        ["status", "-i", "--socket"]
    );
    keys(&env, "quoted", &["q"]);
    env.wait_for("quoted popup closes", || {
        !screen(&env, "quoted").contains("enter switch")
    });
    env.ok(&["start", &root]);
    assert_eq!(env.tmux(&["show", "-gqv", "@brg-status-bin"]), binary);
}
