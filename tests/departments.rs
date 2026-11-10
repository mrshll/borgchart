//! A department's copies are prepared before launch, even at depth three.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;

use instance_env::*;

fn create_copy(env: &Env, parent: &str, role: &str) -> (String, std::path::PathBuf) {
    let before = env.registry();
    let output = env.ok(&["create", &format!("{parent}/{role}")]);
    let listed: serde_json::Value =
        serde_json::from_slice(&env.ok(&["ls", "--json"]).stdout).unwrap();
    let copy = listed
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|team| team["instances"].as_array().unwrap())
        .find(|instance| {
            instance["instance"].as_str().is_some_and(|id| {
                id.starts_with(&format!("{parent}/{role}-")) && before["instances"][id].is_null()
            })
        })
        .expect("create must register the copy");
    let id = copy["instance"].as_str().unwrap().to_string();
    let folder = std::path::PathBuf::from(copy["folder"].as_str().unwrap());
    assert!(both(&output).contains(&folder.display().to_string()));
    assert_eq!(copy["running"], false, "copies must wait for preparation");
    assert!(folder.is_dir());
    assert!(env.rows(&id).iter().all(|row| row["member"].is_null()));
    (id, folder)
}

fn department() -> Env {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  dev: {team: dev, parent: lead, start: false, max: 2, worktree: true}\n",
    );
    env.team("teams/dev");
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    env
}

#[test]
fn nested_addresses_start_and_stop_only_the_named_copy_or_member() {
    let env = department();
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false}\n  design: {team: design, parent: lead, start: false}\n",
    );
    env.team("teams/design");
    let dept = env.create("oncall", "work/a");
    let (first, folder) = create_copy(&env, &dept, "dev");
    let (peer, peer_folder) = create_copy(&env, &dept, "dev");
    for (copy, _) in [(&first, &folder), (&peer, &peer_folder)] {
        env.ok(&["start", copy]);
    }
    let (panel, _) = create_copy(&env, &first, "design");
    env.ok(&["start", &panel]);
    env.ok(&["create", &format!("{first}/helper")]);
    // Status includes descendants: pick this instance's own role, even
    // when the panel has an uncreated role with the same name.
    let row = |instance: &str, role: &str| {
        env.rows(instance)
            .into_iter()
            .find(|r| r["instance"] == instance && r["role"] == role)
            .unwrap()
    };
    let state_of =
        |instance: &str, role: &str| row(instance, role)["state"].as_str().unwrap().to_string();
    let helper = row(&first, "helper")["member"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!helper.is_empty());
    let lead = env.member(&first, "lead");
    let designer = env.member(&panel, "lead");

    for (instance, member, role) in [
        (&first, &lead, "lead"),
        (&first, &helper, "helper"),
        (&panel, &designer, "lead"),
    ] {
        env.ok(&["stop", &format!("{instance}/{member}")]);
        assert_eq!(state_of(instance, role), "stopped");
        for (other, other_role) in [
            (&dept, "lead"),
            (&peer, "lead"),
            (&first, "lead"),
            (&first, "helper"),
            (&panel, "lead"),
        ] {
            if other != instance || other_role != role {
                assert_eq!(state_of(other, other_role), "idle");
            }
        }
        env.ok(&["start", &format!("{instance}/{member}")]);
        env.wait_for("addressed member idle", || {
            state_of(instance, role) == "idle"
        });
    }
    env.ok(&["stop", &first]);
    assert_eq!(env.state_of(&first, "lead"), "stopped");
    assert_eq!(state_of(&first, "helper"), "stopped");
    assert_eq!(env.state_of(&panel, "lead"), "stopped");
    assert_eq!(env.state_of(&peer, "lead"), "idle");
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    env.ok(&["start", &first]);
    assert_eq!(env.state_of(&first, "lead"), "idle");
    assert_eq!(state_of(&first, "helper"), "idle");
    assert_eq!(env.state_of(&peer, "lead"), "idle");
    env.ok(&["stop", &dept]);
}

#[test]
fn invalid_address_segments_are_refused_before_reading_the_registry() {
    let env = Env::new();
    // A valid address would fail reading this registry. Invalid addresses
    // must be refused by the parser before reaching it, even inside a member.
    env.write("state/registry.json", "not a registry");
    let registry_path = env.path("state/registry.json").display().to_string();
    assert!(
        env.fails(&["start", "oncall-2222"])
            .contains(&registry_path)
    );
    for address in [
        "",
        ".",
        "..",
        "/oncall-2222",
        "oncall-2222/",
        "oncall-2222//lead",
        "oncall-2222/../lead",
        "oncall-2222/./lead",
        "oncall-2222/bad.name",
        "bad.name",
    ] {
        for command in ["start", "stop", "create"] {
            // `create` also accepts team-folder paths; only its nested
            // role-address form goes through address resolution.
            if command == "create" && !address.starts_with("oncall-2222/") {
                continue;
            }
            for inside in [false, true] {
                let mut cmd = env.cmd(&[command, address]);
                if inside {
                    cmd.env("BORGCHART_INSTANCE", "oncall-2222");
                }
                let out = cmd.output().unwrap();
                assert!(!out.status.success());
                let error = both(&out);
                assert!(
                    !error.contains(&registry_path),
                    "{command} {address:?} inside={inside}: {error}"
                );
            }
        }
    }
}

#[test]
fn underscore_team_names_create_addressable_instances() {
    let env = Env::new();
    env.team("teams/_scratch");
    let out = env.ok(&[
        "create",
        "./teams/_scratch",
        &env.path("work/a").display().to_string(),
    ]);
    let id = stdout(&out).split_whitespace().next().unwrap().to_string();
    assert!(well_formed(&id, "scratch"), "{id}");
    env.ok(&["status", &id]);
    assert!(stdout(&env.ok(&["status"])).contains(&id));
    env.ok(&["stop", &id]);
    env.ok(&["start", &id]);
    env.ok(&["stop", &id]);
    env.ok(&["delete", &id, "--force"]);
}

#[test]
fn missing_nested_copy_reports_kept_alternatives() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (copy, _) = create_copy(&env, &dept, "dev");
    fs::create_dir(env.path("work/b/lead")).unwrap();
    let unrelated = env.create("oncall", "work/b");
    let missing = format!("{dept}/dev-zzzz/lead-c4nd");
    let refused = env.fails(&["start", &missing]);
    assert!(
        refused.contains(&dept) && refused.contains("dev-zzzz"),
        "{refused}"
    );
    assert!(
        refused.contains(&copy),
        "recovery must name a kept copy: {refused}"
    );
    assert!(
        !refused.contains(&unrelated),
        "alternatives must belong to the matched instance: {refused}"
    );
    env.ok(&["stop", &dept]);
    env.ok(&["stop", &unrelated]);
}

#[test]
fn status_nests_copies_once_with_full_paths_and_states() {
    let env = department();
    let dev_team = env.path("teams/dev/team.yaml");
    fs::write(
        &dev_team,
        format!(
            "{}  design: {{team: design, parent: lead, start: false}}\n",
            fs::read_to_string(&dev_team).unwrap()
        ),
    )
    .unwrap();
    env.team("teams/design");
    let team = env.path("teams/oncall/team.yaml");
    fs::write(&team, format!("{}  review: {{team: dev, parent: lead, start: false}}\n  research: {{team: dev, parent: lead, start: false, max: 2}}\n", fs::read_to_string(&team).unwrap())).unwrap();
    let dept = env.create("oncall", "work/a");
    let dept_lead = env.member(&dept, "lead");
    let rows = env.rows(&dept);
    assert_eq!(
        rows.iter().find(|r| r["role"] == "review").unwrap()["state"],
        "not created (start: false)"
    );
    assert_eq!(
        rows.iter().find(|r| r["role"] == "research").unwrap()["state"],
        "on demand (none created)"
    );
    let (first, first_folder) = create_copy(&env, &dept, "dev");
    let (second, second_folder) = create_copy(&env, &dept, "dev");
    for (copy, _) in [(&first, &first_folder), (&second, &second_folder)] {
        let out = env
            .cmd(&["start", copy])
            .env("BORGCHART_SUFFIX_SEED", "73")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
    }
    let lead = env.member(&first, "lead");
    assert_eq!(
        lead,
        env.member(&second, "lead"),
        "exercise repeated local IDs across copies"
    );
    let (panel, _) = create_copy(&env, &first, "design");
    env.ok(&["start", &panel]);
    let panel_lead = env.member(&panel, "lead");
    env.ok(&["stop", &format!("{first}/{lead}")]);
    let rows = env.rows(&dept);
    for (copy, running) in [(&first, 0), (&second, 1)] {
        let index = rows
            .iter()
            .position(|r| r["kind"] == "copy" && r["instance"] == *copy)
            .expect("copy must appear under its parent");
        assert_eq!(rows[index]["parent"], format!("{dept}/{dept_lead}"));
        assert_eq!(rows[index]["team"], "dev");
        assert_eq!(rows[index]["running"], running);
        let member_index = rows
            .iter()
            .position(|r| r["instance"] == *copy && r["member"] == lead)
            .unwrap();
        assert!(member_index > index);
        assert_eq!(
            rows[member_index]["state"],
            if running == 0 { "stopped" } else { "idle" }
        );
    }
    let bare: Vec<serde_json::Value> =
        serde_json::from_slice(&env.ok(&["status", "--json"]).stdout).unwrap();
    let panel_index = bare
        .iter()
        .position(|r| r["kind"] == "copy" && r["instance"] == panel)
        .expect("depth-three copy must be visible");
    assert_eq!(bare[panel_index]["parent"], format!("{first}/{lead}"));
    assert!(
        bare.iter()
            .skip(panel_index + 1)
            .any(|r| r["instance"] == panel && r["member"] == panel_lead)
    );
    for copy in [&first, &second] {
        assert_eq!(
            bare.iter()
                .filter(|r| r["instance"] == *copy && r["member"] == lead)
                .count(),
            1,
            "a nested member must appear once"
        );
    }
    for args in [vec!["status"], vec!["status", &dept]] {
        let text = stdout(&env.ok(&args));
        assert_eq!(text.lines().filter(|line| line.contains(&lead)).count(), 2);
        for copy in [&first, &second] {
            assert!(
                !text
                    .lines()
                    .any(|line| line.starts_with(&format!("{copy}  team ")))
            );
            let name = copy.rsplit('/').next().unwrap();
            assert!(text.lines().any(|line| line.contains(name)
                && line.contains("dev")
                && line.contains("running")));
        }
    }
    env.ok(&["start", &format!("{first}/{lead}")]);
    let rows = env.rows(&dept);
    assert_eq!(
        rows.iter()
            .find(|r| r["kind"] == "copy" && r["instance"] == first)
            .unwrap()["running"],
        1
    );
    env.ok(&["stop", &dept]);
}

#[test]
fn nested_status_screens_keep_healthy_members_visible_when_a_copy_becomes_unreadable() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (copy, folder) = create_copy(&env, &dept, "dev");
    let (peer, peer_folder) = create_copy(&env, &dept, "dev");
    for (id, _) in [(&copy, &folder), (&peer, &peer_folder)] {
        let out = env
            .cmd(&["start", id])
            .env("BORGCHART_SUFFIX_SEED", "73")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
    }
    let lead = env.member(&copy, "lead");
    assert_eq!(lead, env.member(&peer, "lead"));
    let outer = env.tmux.socket("department-screen");
    let screen = || {
        let out = env
            .tmux
            .tmux(&outer)
            .args(["capture-pane", "-p", "-t", "=screen:"])
            .output()
            .unwrap();
        stdout(&out)
    };
    let keys = |keys: &[&str]| {
        let out = env
            .tmux
            .tmux(&outer)
            .args(["send-keys", "-t", "=screen:"])
            .args(keys)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
    };
    for args in [
        vec!["status", "-w", "--interval", "1"],
        vec!["status", "-i"],
        vec!["status", &dept, "-i"],
    ] {
        let cmd = env.cmd(&args);
        let quoted = borgchart::state::shell_quote;
        let vars: Vec<_> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| format!("{}={}", k.to_string_lossy(), quoted(&v.to_string_lossy())))
            })
            .collect();
        let run = format!(
            "#!/bin/sh\nexec env -i TERM=xterm-256color {} {} {}\n",
            vars.join(" "),
            quoted(env!("CARGO_BIN_EXE_brg")),
            args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(" ")
        );
        let path = env.path("screen.sh");
        script(&path, &run);
        let out = env
            .tmux
            .tmux(&outer)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "screen",
                "-x",
                "220",
                "-y",
                "40",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        let copy_name = copy.rsplit('/').next().unwrap();
        env.wait_for("nested screen", || {
            screen().contains(copy_name) && screen().contains(&lead)
        });
        let text = screen();
        assert_eq!(
            text.lines().filter(|line| line.contains(&lead)).count(),
            2,
            "{text}"
        );
        assert!(
            !text.contains(&format!("{copy}  team ")),
            "nested copy must not have a top-level header: {text}"
        );
        assert!(
            text.lines().any(
                |line| line.contains(copy_name) && (line.contains("└─") || line.contains("├─"))
            ),
            "{text}"
        );
        let path = env.path(&format!("state/{peer}/instance.json"));
        let original = fs::read(&path).unwrap();
        fs::write(&path, "unreadable copy record").unwrap();
        env.wait_for("screen keeps healthy rows beside unreadable copy", || {
            let text = screen();
            text.contains("unreadable")
                && text.contains(copy_name)
                && text.lines().filter(|line| line.contains(&lead)).count() == 1
        });
        fs::write(&path, original).unwrap();
        env.wait_for("screen recovers restored copy", || {
            screen().lines().filter(|line| line.contains(&lead)).count() == 2
        });
        if args.contains(&"-w") {
            env.ok(&["stop", &format!("{copy}/{lead}")]);
            env.wait_for("watch refresh after nested stop", || {
                screen()
                    .lines()
                    .any(|line| line.contains(&lead) && line.contains("stopped"))
            });
            env.ok(&["start", &format!("{copy}/{lead}")]);
        } else {
            // The screen leaves out member-less roles that are on demand or
            // `start: false`.
            let rows: Vec<_> = env
                .rows(&dept)
                .into_iter()
                .filter(|r| {
                    !(r["kind"] == "role"
                        && (r["state"] == "on demand (none created)"
                            || r["state"] == "not created (start: false)"))
                })
                .collect();
            let selected = rows
                .iter()
                .position(|r| r["instance"] == copy && r["member"] == lead)
                .unwrap();
            let copy_index = rows
                .iter()
                .position(|r| r["kind"] == "copy" && r["instance"] == copy)
                .unwrap();
            let moves = copy_index + usize::from(!args.contains(&dept.as_str()));
            for _ in 0..moves {
                keys(&["j"]);
            }
            keys(&["Enter"]);
            env.wait_for("copy row refuses attach", || {
                screen().contains("select one of its members")
            });
            assert!(env.tmux(&["list-clients"]).trim().is_empty());
            for _ in copy_index..selected {
                keys(&["j"]);
            }
            keys(&["Enter"]);
            env.wait_for("attach in selected nested instance", || {
                env.tmux(&["list-clients", "-F", "#{session_name}"]).trim() == format!("brg-{copy}")
            });
            keys(&["C-b", "d"]);
            env.wait_for("detach to nested status", || {
                env.tmux(&["list-clients"]).trim().is_empty() && screen().contains("enter attach")
            });
            keys(&["q"]);
        }
        let _ = env
            .tmux
            .tmux(&outer)
            .args(["kill-session", "-t", "=screen"])
            .output();
    }
    env.ok(&["stop", &dept]);
}

#[test]
fn team_copies_share_the_tree_store_and_keep_separate_slots_until_deleted() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (first, first_folder) = create_copy(&env, &dept, "dev");
    let (second, second_folder) = create_copy(&env, &dept, "dev");
    assert_ne!(first_folder, second_folder);
    let launched: Vec<_> = fs::read_dir(env.path("home/fake-agent"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(
        launched.len(),
        1,
        "only the department lead may have started"
    );
    for copy in [&first, &second] {
        assert!(!env.path(&format!("state/{copy}/messages")).exists());
        assert!(env.path(&format!("state/{copy}/notes")).is_dir());
        assert!(env.rows(copy).iter().all(|r| r["member"].is_null()));
    }
    let refused = env.fails(&["create", &format!("{dept}/dev")]);
    assert!(
        refused.contains(&first) && refused.contains(&second),
        "{refused}"
    );
    for copy in [&first, &second] {
        env.ok(&["start", copy]);
        env.wait_for("copy lead idle", || env.state_of(copy, "lead") == "idle");
        env.ok(&["bus", "check", "--instance", copy]);
    }
    let first_lead = env.member(&first, "lead");
    let second_lead = env.member(&second, "lead");
    let sent = env.publish(
        &first,
        &serde_json::json!({"to": first_lead, "body": "Fictional copy task"}),
    );
    assert!(sent.status.success(), "{}", both(&sent));
    assert_eq!(env.row(&first, "lead")["unread"], 1);
    assert_eq!(env.row(&second, "lead")["unread"], 0);
    for (copy, lead, folder) in [
        (&first, &first_lead, &first_folder),
        (&second, &second_lead, &second_folder),
    ] {
        let starts: Vec<_> = env
            .starts(lead)
            .into_iter()
            .filter(|start| start["env"]["BORGCHART_INSTANCE"] == *copy)
            .collect();
        assert_eq!(starts.len(), 1);
        assert_eq!(
            starts[0]["cwd"],
            folder.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(starts[0]["env"]["BORGCHART_INSTANCE"], *copy);
        assert_eq!(
            starts[0]["env"]["BORGCHART_NOTES"],
            env.path(&format!("state/{copy}/notes"))
                .display()
                .to_string()
        );
    }
    env.ok(&["stop", &first]);
    let refused = env.fails(&["create", &format!("{dept}/dev")]);
    assert!(refused.contains(&first) && refused.contains(&second));
    assert_eq!(env.state_of(&second, "lead"), "idle");
    env.ok(&["delete", &first, "--force"]);
    let (replacement, _) = create_copy(&env, &dept, "dev");
    assert_ne!(replacement, first);
    assert_eq!(env.state_of(&second, "lead"), "idle");
    env.ok(&["stop", &dept]);
}

#[test]
fn depth_three_copies_wait_for_preparation_and_cascade_before_parent_deletion() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, start: true}\n  dev: {team: dev, parent: lead, start: false, max: 2, worktree: true}\n",
    );
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, start: true}\n  design: {team: design, parent: lead, start: false}\n",
    );
    env.write(
        "teams/dev/lead.md",
        "---\nmodels: {claude: opus high}\n---\nLead the fictional development team.\n",
    );
    env.write(
        "teams/design/team.yaml",
        "bus: {}\nroles:\n  amber: {prompt: amber.md, start: true}\n  violet: {prompt: violet.md, start: true}\n",
    );
    for role in ["amber", "violet"] {
        env.write(
            &format!("teams/design/{role}.md"),
            "---\nmodels: {claude: opus high}\n---\nReview the fictional design.\n",
        );
    }
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    let dept = env.create("oncall", "work/a");
    let dept_lead = env.member(&dept, "lead");
    assert!(!env.path("work/a/dev").exists());

    let (dev, dev_folder) = create_copy(&env, &dept, "dev");
    let dev_id = dev.rsplit('/').next().unwrap();
    assert!(well_formed(dev_id, "dev"));
    assert_eq!(dev_folder, env.worktree(&dev));
    assert!(dev_folder.join(".git").is_file());
    env.ok(&["start", &dev]);
    env.wait_for("dev lead idle", || env.state_of(&dev, "lead") == "idle");
    let dev_lead = env.member(&dev, "lead");
    let start = &env.starts(&dev_lead)[0];
    assert_eq!(
        start["cwd"],
        dev_folder.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(start["env"]["BORGCHART_INSTANCE"], dev);
    assert_eq!(
        start["env"]["BORGCHART_PARENT"],
        format!("{dept}/{dept_lead}")
    );

    let (panel, panel_folder) = create_copy(&env, &dev, "design");
    let panel_id = panel.rsplit('/').next().unwrap();
    assert!(well_formed(panel_id, "design"));
    assert_eq!(
        panel_folder, dev_folder,
        "a panel without a worktree shares its parent's"
    );
    env.ok(&["start", &panel]);
    for role in ["amber", "violet"] {
        env.wait_for("designer idle", || env.state_of(&panel, role) == "idle");
        let member = env.member(&panel, role);
        let start = &env.starts(&member)[0];
        assert_eq!(
            start["cwd"],
            panel_folder.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(
            start["env"]["BORGCHART_PARENT"],
            format!("{dev}/{dev_lead}")
        );
    }
    let notes: Vec<_> = [&dept, &dev, &panel]
        .into_iter()
        .map(|instance| {
            let role = if instance == &panel { "amber" } else { "lead" };
            let member = env.member(instance, role);
            let start = &env.starts(&member)[0];
            let notes = std::path::PathBuf::from(start["env"]["BORGCHART_NOTES"].as_str().unwrap());
            let state = notes.parent().unwrap().to_path_buf();
            assert_eq!(state, env.path(&format!("state/{instance}")));
            assert!(notes.is_dir());
            assert_eq!(state.join("messages").is_dir(), instance == &dept);
            env.ok(&["bus", "check", "--instance", instance]);
            notes
        })
        .collect();
    assert_ne!(notes[0], notes[1]);
    assert_ne!(notes[1], notes[2]);

    let stopped = stdout(&env.ok(&["stop", &dept]));
    for (instance, roles) in [
        (&dept, vec!["lead"]),
        (&dev, vec!["lead"]),
        (&panel, vec!["amber", "violet"]),
    ] {
        for role in roles {
            assert_eq!(env.state_of(instance, role), "stopped");
        }
    }
    // The cascade prints completion of each instance in the order it stops.
    let order: Vec<_> = stopped
        .lines()
        .filter_map(|line| line.strip_suffix(": stopped"))
        .collect();
    assert_eq!(order, [&panel, &dev, &dept]);
    let refused = env.fails(&["delete", &dept]);
    assert!(refused.contains(&dev));
    for instance in [&dept, &dev, &panel] {
        env.ok(&["status", instance]);
    }
    env.ok(&["delete", &panel]);
    env.ok(&["delete", &dev]);
    env.ok(&["delete", &dept]);
    let remaining = stdout(&env.ok(&["ls", "--json"]));
    assert!(!remaining.contains(&dept));
    // The dev copy's clean worktree goes with it; the root checkout stays.
    assert!(!dev_folder.exists());
    assert!(env.path("work/a/.git").is_dir());
}

#[test]
fn worktrees_in_a_copy_never_follow_the_copys_rewritten_git_file() {
    // Catches: brg running git through a copy's member-writable `.git`
    // file when it adds or removes worktrees inside that copy.
    let env = department();
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false, worktree: true}\n",
    );
    let dept = env.create("oncall", "work/a");
    let (copy, folder) = create_copy(&env, &dept, "dev");
    let hostile = env.path("hostile");
    git(
        env.dir.path(),
        &["init", "-q", &hostile.display().to_string()],
    );
    let ran = env.path("ran");
    git(
        &hostile,
        &[
            "config",
            "core.fsmonitor",
            &format!("touch {}", ran.display()),
        ],
    );
    fs::write(
        folder.join(".git"),
        format!("gitdir: {}\n", hostile.join(".git").display()),
    )
    .unwrap();
    env.ok(&["start", &copy]);
    env.ok(&["create", &format!("{copy}/helper")]);
    let helper = env.member(&copy, "helper");
    env.ok(&["stop", &dept]);
    let _ = env.brg(&["delete", &format!("{copy}/{helper}")]);
    let _ = env.brg(&["cleanup"]);
    assert!(!ran.exists(), "brg ran the hostile repository's command");
    env.ok(&["stop", &dept]);
}
