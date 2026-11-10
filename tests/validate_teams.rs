//! Team-copy configuration is frozen independently, with actual ancestry checked.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::path::PathBuf;

use instance_env::*;

fn department() -> (Env, String) {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", "roles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: dev, parent: lead, max: 3, cwd: 'dev/{member_id}'}\n");
    env.team("teams/dev");
    env.write("teams/dev/team.yaml", "roles:\n  lead: {prompt: lead.md, cwd: repo}\n  helper: {prompt: helper.md, parent: lead, start: false, cwd: repo}\n");
    fs::create_dir(env.path("work/a/lead")).unwrap();
    let dept = env.create("oncall", "work/a");
    (env, dept)
}

fn copy(env: &Env, dept: &str) -> (String, PathBuf) {
    let before = env.registry();
    env.ok(&["create", &format!("{dept}/dev")]);
    let registry = env.registry();
    let (id, entry) = registry["instances"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(id, _)| {
            id.starts_with(&format!("{dept}/dev-")) && before["instances"][*id].is_null()
        })
        .unwrap();
    let folder = PathBuf::from(entry["folder"].as_str().unwrap());
    fs::create_dir(folder.join("repo")).unwrap();
    (id.clone(), folder)
}

fn latest_prompt(env: &Env, inst: &str, member: &str) -> String {
    env.wait_for("fake agent start", || {
        env.starts(member)
            .iter()
            .any(|s| s["env"]["BORGCHART_INSTANCE"] == inst)
    });
    env.starts(member)
        .into_iter()
        .rev()
        .find(|s| s["env"]["BORGCHART_INSTANCE"] == inst)
        .unwrap()["argv"]
        .to_string()
}

fn broken_subteam_during_department_command(command: &str) {
    let (env, dept) = department();
    let team = env.path("teams/oncall/team.yaml");
    // Add the department's own on-demand role before freezing its config.
    env.ok(&["stop", &dept]);
    let original = fs::read_to_string(&team).unwrap();
    fs::write(
        &team,
        format!(
            "{original}  helper: {{prompt: helper.md, parent: lead, start: false, cwd: lead}}\n"
        ),
    )
    .unwrap();
    env.ok(&["start", &dept]);
    let lead = env.member(&dept, "lead");
    if command == "member start" {
        env.ok(&["stop", &format!("{dept}/{lead}")]);
    }
    let subteam = env.path("teams/dev/team.yaml");
    let valid = fs::read_to_string(&subteam).unwrap();
    fs::write(&subteam, valid.replace("lead.md", "missing.md")).unwrap();
    match command {
        "status" => assert_eq!(env.state_of(&dept, "lead"), "idle"),
        "agent creation" => {
            env.ok(&["create", &format!("{dept}/helper")]);
            env.wait_for("department helper idle", || {
                env.state_of(&dept, "helper") == "idle"
            });
        }
        "member start" => {
            env.ok(&["start", &format!("{dept}/{lead}")]);
            env.wait_for("department lead restarted", || {
                env.state_of(&dept, "lead") == "idle"
            });
        }
        "full start" => {
            env.ok(&["start", &dept]);
            assert_eq!(env.state_of(&dept, "lead"), "idle");
        }
        _ => unreachable!(),
    }
    fs::write(subteam, valid).unwrap();
    env.ok(&["stop", &dept]);
}

#[test]
fn broken_subteam_does_not_block_department_status() {
    broken_subteam_during_department_command("status");
}

#[test]
fn broken_subteam_does_not_block_department_agent_creation() {
    broken_subteam_during_department_command("agent creation");
}

#[test]
fn broken_subteam_does_not_block_department_member_start() {
    broken_subteam_during_department_command("member start");
}

#[test]
fn broken_subteam_does_not_block_department_full_start() {
    broken_subteam_during_department_command("full start");
}

#[test]
fn broken_subteam_still_blocks_copy_creation_and_start() {
    let (env, dept) = department();
    let (child, _) = copy(&env, &dept);
    let team = env.path("teams/dev/team.yaml");
    let original = fs::read_to_string(&team).unwrap();
    fs::write(&team, original.replace("lead.md", "missing.md")).unwrap();
    for args in [
        vec!["validate", "oncall"],
        vec!["create", &format!("{dept}/dev")],
        vec!["start", &child],
    ] {
        assert!(env.fails(&args).contains("missing.md"));
    }
    fs::write(team, original).unwrap();
    env.ok(&["stop", &dept]);
}

fn missing_subteam_during_department_commands(remove_folder: bool) {
    let (env, dept) = department();
    let team = env.path("teams/oncall/team.yaml");
    env.ok(&["stop", &dept]);
    let original = fs::read_to_string(&team).unwrap();
    fs::write(
        &team,
        format!(
            "{original}  helper: {{prompt: helper.md, parent: lead, start: false, cwd: lead}}\n"
        ),
    )
    .unwrap();
    env.ok(&["start", &dept]);
    let (child, _) = copy(&env, &dept);
    let lead = env.member(&dept, "lead");
    let subteam = env.path("teams/dev");
    let saved = env.path("saved-dev");
    if remove_folder {
        fs::rename(&subteam, &saved).unwrap();
    } else {
        fs::rename(subteam.join("team.yaml"), &saved).unwrap();
    }
    // The declaring instance may still create and manage its own agents.
    env.ok(&["create", &format!("{dept}/helper")]);
    env.ok(&["start", &dept]);
    env.ok(&["stop", &format!("{dept}/{lead}")]);
    env.ok(&["start", &format!("{dept}/{lead}")]);
    let before = env.registry();
    assert!(
        env.fails(&["create", &format!("{dept}/dev")])
            .contains("no team `dev`")
    );
    assert!(env.fails(&["start", &child]).contains("team folder"));
    let validation = env.fails(&["validate", "oncall"]);
    assert!(validation.contains("no team `dev`"), "{validation}");
    assert_eq!(env.registry(), before);
    if remove_folder {
        fs::rename(saved, subteam).unwrap();
    } else {
        fs::rename(saved, subteam.join("team.yaml")).unwrap();
    }
    env.ok(&["stop", &dept]);
}

#[test]
fn missing_subteam_folder_does_not_block_department_commands() {
    missing_subteam_during_department_commands(true);
}

#[test]
fn missing_subteam_yaml_does_not_block_department_commands() {
    missing_subteam_during_department_commands(false);
}

fn missing_running_subteam_during_status_and_stop(remove_folder: bool) {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", "roles:\n  lead: {prompt: lead.md, cwd: lead}\n  helper: {prompt: helper.md, parent: lead, cwd: lead}\n  dev: {team: dev, parent: lead, start: false, max: 3, cwd: 'dev/{member_id}'}\n  ops: {team: ops, parent: lead, start: false, max: 3, cwd: 'ops/{member_id}'}\n");
    for role in ["dev", "ops"] {
        env.team(&format!("teams/{role}"));
        env.write(
            &format!("teams/{role}/team.yaml"),
            "roles: {lead: {prompt: lead.md, cwd: repo}}\n",
        );
    }
    fs::create_dir(env.path("work/a/lead")).unwrap();
    let dept = env.create("oncall", "work/a");
    let mut copies = Vec::new();
    for role in ["dev", "ops"] {
        let out = env.ok(&["create", &format!("{dept}/{role}")]);
        let id = stdout(&out).split_whitespace().next().unwrap().to_string();
        let folder = PathBuf::from(env.registry()["instances"][&id]["folder"].as_str().unwrap());
        fs::create_dir(folder.join("repo")).unwrap();
        env.ok(&["start", &id]);
        env.wait_for("copy lead idle", || env.state_of(&id, "lead") == "idle");
        copies.push(id);
    }
    let affected = &copies[0];
    let healthy = &copies[1];
    env.wait_for("department helper idle", || {
        env.state_of(&dept, "helper") == "idle"
    });
    let members = [
        (dept.as_str(), env.member(&dept, "lead")),
        (dept.as_str(), env.member(&dept, "helper")),
        (healthy.as_str(), env.member(healthy, "lead")),
    ];
    let affected_lead = env.member(affected, "lead");
    let subteam = env.path("teams/dev");
    let missing = if remove_folder {
        subteam.clone()
    } else {
        subteam.join("team.yaml")
    };
    let saved = env.path("saved-dev");
    fs::rename(&missing, &saved).unwrap();

    let mut detail = String::new();
    for args in [vec!["status", &dept, "--json"], vec!["status", "--json"]] {
        let out = env.ok(&args);
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
        for (instance, member) in &members {
            assert!(
                rows.iter().any(|row| {
                    row["kind"] == "member"
                        && row["instance"] == *instance
                        && row["member"] == *member
                        && row["liveness"] == "alive"
                }),
                "{args:?} hid a running member {instance}/{member}: {rows:?}"
            );
        }
        assert!(
            rows.iter().any(|row| {
                row["kind"] == "copy" && row["instance"] == *healthy && row["state"] == "running"
            }),
            "{args:?} hid the healthy copy: {rows:?}"
        );
        let row = rows
            .iter()
            .find(|row| row["kind"] == "copy" && row["instance"] == *affected)
            .unwrap();
        assert_eq!(row["state"], "unreadable", "{args:?}: {rows:?}");
        detail = row["detail"].as_str().unwrap().to_string();
        assert!(detail.contains(affected), "{detail}");
        assert!(detail.contains(&subteam.display().to_string()), "{detail}");
    }
    for args in [vec!["status", &dept], vec!["status"]] {
        let text = stdout(&env.ok(&args));
        for (_, member) in &members {
            assert!(text.contains(member), "{args:?}: {text}");
        }
        for copy in [affected, healthy] {
            assert!(text.contains(copy.rsplit('/').next().unwrap()), "{text}");
        }
        assert!(text.contains("unreadable"), "{text}");
        assert!(text.contains(&detail), "{text}");
    }
    env.fails(&["create", &format!("{dept}/dev")]);
    let stopped = stdout(&env.ok(&["stop", &dept]));
    for instance in [&dept, affected, healthy] {
        assert!(stopped.contains(instance), "{stopped}");
    }
    for (instance, member) in &members {
        assert!(
            env.rows(&dept).iter().any(|row| {
                row["kind"] == "member"
                    && row["instance"] == *instance
                    && row["member"] == *member
                    && row["liveness"] == "gone"
            }),
            "stop left {instance}/{member} running"
        );
    }
    // Restoring the source makes the affected copy readable again and lets
    // status confirm that its running member was stopped too.
    fs::rename(saved, missing).unwrap();
    assert_eq!(env.row(affected, "lead")["member"], affected_lead);
    assert_eq!(env.row(affected, "lead")["liveness"], "gone");
}

#[test]
fn missing_running_subteam_folder_keeps_status_and_stop_usable() {
    missing_running_subteam_during_status_and_stop(true);
}

#[test]
fn missing_running_subteam_yaml_keeps_status_and_stop_usable() {
    missing_running_subteam_during_status_and_stop(false);
}

#[test]
fn changed_subteam_reaches_only_the_copy_that_fully_restarts() {
    let (env, dept) = department();
    let (first, _) = copy(&env, &dept);
    let (peer, _) = copy(&env, &dept);
    // Changes after create still reach a copy at its first start.
    env.write(
        "teams/dev/lead.md",
        "---\nmodels: {claude: opus}\n---\nRelease amber.\n",
    );
    for inst in [&first, &peer] {
        env.ok(&["start", inst]);
        let lead = env.member(inst, "lead");
        assert!(latest_prompt(&env, inst, &lead).contains("Release amber."));
    }
    let first_lead = env.member(&first, "lead");
    let peer_lead = env.member(&peer, "lead");
    env.write(
        "teams/dev/lead.md",
        "---\nmodels: {claude: opus}\n---\nRelease blue.\n",
    );
    env.fails(&["start", &first]);
    env.ok(&["stop", &format!("{first}/{first_lead}")]);
    env.fails(&["start", &format!("{first}/{first_lead}")]);
    env.ok(&["stop", &first]);
    env.ok(&["start", &first]);
    env.wait_for("restarted copy's agent", || {
        env.starts(&first_lead)
            .iter()
            .filter(|s| s["env"]["BORGCHART_INSTANCE"] == first)
            .count()
            == 2
    });
    assert!(latest_prompt(&env, &first, &first_lead).contains("Release blue."));
    assert!(latest_prompt(&env, &peer, &peer_lead).contains("Release amber."));
    assert_eq!(env.state_of(&peer, "lead"), "idle");
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    // Neither copy restart required the department to restart. A new copy
    // also reads the sub-team's current files while the department runs.
    let (fresh, _) = copy(&env, &dept);
    env.ok(&["start", &fresh]);
    let lead = env.member(&fresh, "lead");
    assert!(latest_prompt(&env, &fresh, &lead).contains("Release blue."));
    env.ok(&["stop", &dept]);
}

#[test]
fn moved_copy_team_folder_errors_without_rebinding_to_a_same_named_team() {
    let (env, dept) = department();
    let (child, _) = copy(&env, &dept);
    env.ok(&["start", &child]);
    let lead = env.member(&child, "lead");
    let starts = env.starts(&lead).len();
    env.ok(&["stop", &child]);
    let original = env.path("teams/dev");
    fs::create_dir(env.path("elsewhere")).unwrap();
    fs::rename(&original, env.path("elsewhere/dev")).unwrap();
    env.fails(&["start", &child]);
    let registry = env.registry();
    assert_eq!(
        registry["instances"][&child]["team_dir"],
        original.display().to_string()
    );
    assert_eq!(env.starts(&lead).len(), starts);
    env.ok(&["stop", &dept]);
}

#[test]
fn changed_team_path_resolution_requires_the_declaring_instance_to_restart() {
    let (env, dept) = department();
    env.ok(&["stop", &dept]);
    env.ok(&["start", &dept]);
    let (kept, _) = copy(&env, &dept);
    env.ok(&["start", &kept]);
    env.team("redirect/dev");
    env.write(
        "redirect/dev/team.yaml",
        "roles: {lead: {prompt: lead.md, cwd: repo}}\n",
    );
    env.write(
        "redirect/dev/lead.md",
        "---\nmodels: {claude: opus}\n---\nAlternative team.\n",
    );
    env.write(
        "config.yaml",
        &format!("schema: 1\nteams: [{}]\n", env.path("redirect").display()),
    );
    let before = env.registry();
    assert!(
        env.fails(&["create", &format!("{dept}/dev")])
            .contains("changed since the instance started")
    );
    assert_eq!(env.registry(), before);
    // A kept copy keeps its recorded source, independently of the role's
    // current catalog target and of a later department restart.
    env.ok(&["stop", &kept]);
    env.ok(&["start", &kept]);
    assert_eq!(
        env.registry()["instances"][&kept]["team_dir"],
        env.path("teams/dev").display().to_string()
    );
    env.ok(&["stop", &dept]);
    env.ok(&["start", &dept]);
    let (fresh, _) = copy(&env, &dept);
    env.ok(&["start", &fresh]);
    let lead = env.member(&fresh, "lead");
    assert!(latest_prompt(&env, &fresh, &lead).contains("Alternative team."));
    env.ok(&["stop", &dept]);
}

fn move_ancestor_within_copy(env: &Env, dept: &str) {
    env.ok(&["stop", dept]);
    fs::rename(env.path("teams/oncall"), env.path("old-oncall")).unwrap();
    env.team("relocated/oncall");
    env.write(
        "config.yaml",
        &format!(
            "schema: 1\nteams: [{}, {}]\n",
            env.path("relocated").display(),
            env.path("teams").display()
        ),
    );
    env.write(
        "relocated/oncall/team.yaml",
        "roles: {lead: {prompt: lead.md, cwd: lead}}\n",
    );
    env.ok(&[
        "start",
        dept,
        "--team",
        &env.path("relocated/oncall").display().to_string(),
    ]);
    env.write("teams/dev/team.yaml", "roles:\n  lead: {prompt: lead.md, cwd: repo}\n  panel: {team: oncall, cwd: 'panel/{member_id}'}\n");
}

#[test]
fn create_refuses_a_subteam_edited_to_contain_its_actual_ancestor() {
    let (env, dept) = department();
    let (child, _) = copy(&env, &dept);
    move_ancestor_within_copy(&env, &dept);
    let before = env.registry();
    env.fails(&["create", &format!("{child}/panel")]);
    assert_eq!(env.registry(), before);
    env.ok(&["stop", &dept]);
}

#[test]
fn full_start_refuses_a_subteam_edited_to_contain_its_actual_ancestor() {
    let (env, dept) = department();
    let (child, _) = copy(&env, &dept);
    env.ok(&["start", &child]);
    let lead = env.member(&child, "lead");
    move_ancestor_within_copy(&env, &dept);
    env.wait_for("restored copy lead launch recorded", || {
        env.starts(&lead).len() == 2
    });
    let count = env.starts(&lead).len();
    env.fails(&["start", &child]);
    assert_eq!(
        env.starts(&lead).len(),
        count,
        "a refused start must launch nothing"
    );
    env.ok(&["stop", &dept]);
}

#[test]
fn interactive_is_not_an_option_on_nested_team_roles() {
    let (env, dept) = department();
    env.ok(&["stop", &dept]);
    let path = env.path("teams/oncall/team.yaml");
    let original = fs::read_to_string(&path).unwrap();
    for value in ["true", "false"] {
        fs::write(
            &path,
            original.replace("team: dev,", &format!("team: dev, interactive: {value},")),
        )
        .unwrap();
        let error = env.fails(&["validate", "oncall"]);
        assert!(
            error.contains("interactive") && error.contains("team role"),
            "{value}: {error}"
        );
    }
}

#[test]
fn department_and_copy_share_a_store_without_bus_opt_in() {
    let (env, dept) = department();
    let (child, _) = copy(&env, &dept);
    env.ok(&["start", &child]);
    let lead = env.member(&child, "lead");
    env.ok(&[
        "send",
        &format!("{child}/{lead}"),
        "Copy task",
        "--external",
        "ops",
    ]);
    for instance in [&dept, &child] {
        let checked: serde_json::Value = serde_json::from_slice(
            &env.ok(&["bus", "check", "--instance", instance, "--json"])
                .stdout,
        )
        .unwrap();
        assert_eq!(checked["records"], 1);
    }
    assert!(!env.path(&format!("state/{child}/messages")).exists());
    env.ok(&["stop", &dept]);
}

#[test]
fn missing_named_child_blocks_fresh_start_without_losing_status_or_stop() {
    let (env, dept) = department();
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path(&format!("state/{dept}/start.json"))).unwrap())
            .unwrap();
    assert_eq!(
        meta["config"]["team_dirs"]["dev"],
        env.path("teams/dev").display().to_string()
    );
    fs::rename(env.path("teams/dev"), env.path("saved-dev")).unwrap();
    env.ok(&["status", &dept]);
    env.fails(&["create", &format!("{dept}/dev")]);
    env.ok(&["stop", &dept]);
    assert!(env.fails(&["start", &dept]).contains("no team `dev`"));
    env.ok(&["status", &dept]);
    env.ok(&["stop", &dept]);
}

#[test]
fn ambiguous_catalog_does_not_block_status_but_refuses_new_copies() {
    let (env, dept) = department();
    env.team("other/dev");
    env.write(
        "config.yaml",
        &format!(
            "schema: 1\nteams: [{}, {}]\n",
            env.path("teams").display(),
            env.path("other").display()
        ),
    );
    env.ok(&["status", &dept]);
    assert!(
        env.fails(&["create", &format!("{dept}/dev")])
            .contains("more than one team")
    );
    env.ok(&["stop", &dept]);
    assert!(env.fails(&["start", &dept]).contains("more than one team"));
}

#[test]
fn unclean_restart_refuses_missing_named_child() {
    let (env, dept) = department();
    env.tmux(&["kill-server"]);
    fs::rename(env.path("teams/dev"), env.path("saved-dev")).unwrap();
    let output = env.brg(&["start", &dept]);
    assert!(!output.status.success(), "{}", both(&output));
    assert!(both(&output).contains("no team `dev`"), "{}", both(&output));
    env.ok(&["status", &dept]);
}

#[test]
fn unstarted_copy_keeps_missing_member_creation_hint() {
    let (env, dept) = department();
    env.team("teams/design");
    let team = env.path("teams/dev/team.yaml");
    let original = fs::read_to_string(&team).unwrap();
    fs::write(
        &team,
        format!(
            "{original}  design: {{team: design, parent: lead, cwd: 'design/{{member_id}}'}}\n"
        ),
    )
    .unwrap();
    let (child, _) = copy(&env, &dept);
    let error = env.fails(&["start", &format!("{child}/helper")]);
    assert!(
        error.contains(&format!(
            "role `helper` has no member; create one with `brg create {child}/helper`"
        )),
        "{error}"
    );
}
