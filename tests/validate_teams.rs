//! Team-copy configuration is frozen independently, with actual ancestry checked.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::path::PathBuf;

use instance_env::*;

fn department() -> (Env, String) {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: teams/dev, parent: lead, max: 3, cwd: 'dev/{member_id}'}\n");
    env.team("teams/oncall/teams/dev");
    env.write("teams/oncall/teams/dev/team.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n  helper: {prompt: helper.md, parent: lead, start: false, cwd: repo}\n");
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
    let subteam = env.path("teams/oncall/teams/dev/team.yaml");
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
    let team = env.path("teams/oncall/teams/dev/team.yaml");
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
    let subteam = env.path("teams/oncall/teams/dev");
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
            .contains("team.yaml")
    );
    assert!(env.fails(&["start", &child]).contains("team folder"));
    let validation = env.fails(&["validate", "oncall"]);
    assert!(
        validation.contains(if remove_folder {
            "does not exist"
        } else {
            "needs a `team.yaml`"
        }),
        "{validation}"
    );
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

#[test]
fn changed_subteam_reaches_only_the_copy_that_fully_restarts() {
    let (env, dept) = department();
    let (first, _) = copy(&env, &dept);
    let (peer, _) = copy(&env, &dept);
    // Changes after create still reach a copy at its first start.
    env.write(
        "teams/oncall/teams/dev/lead.md",
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
        "teams/oncall/teams/dev/lead.md",
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
    let original = env.path("teams/oncall/teams/dev");
    fs::rename(&original, env.path("teams/dev")).unwrap();
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
    let selected = env.path("teams/oncall/teams/selected");
    std::os::unix::fs::symlink("dev", &selected).unwrap();
    env.write("teams/oncall/team.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: teams/selected, parent: lead, max: 3, cwd: 'dev/{member_id}'}\n");
    env.ok(&["start", &dept]);
    let (kept, _) = copy(&env, &dept);
    env.ok(&["start", &kept]);
    env.team("teams/oncall/teams/other");
    env.write(
        "teams/oncall/teams/other/team.yaml",
        "bus: {}\nroles: {lead: {prompt: lead.md, cwd: repo}}\n",
    );
    env.write(
        "teams/oncall/teams/other/lead.md",
        "---\nmodels: {claude: opus}\n---\nAlternative team.\n",
    );
    fs::remove_file(&selected).unwrap();
    std::os::unix::fs::symlink("other", &selected).unwrap();
    env.fails(&["create", &format!("{dept}/dev")]);
    // A kept copy keeps its recorded source, independently of the role's
    // current symlink target and of a later department restart.
    env.ok(&["stop", &kept]);
    env.ok(&["start", &kept]);
    assert_eq!(
        env.registry()["instances"][&kept]["team_dir"],
        env.path("teams/oncall/teams/dev").display().to_string()
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
    env.team("teams/oncall/teams/dev/teams/oncall");
    env.write(
        "teams/oncall/teams/dev/teams/oncall/team.yaml",
        "bus: {}\nroles: {lead: {prompt: lead.md, cwd: lead}}\n",
    );
    env.ok(&[
        "start",
        dept,
        "--team",
        &env.path("teams/oncall/teams/dev/teams/oncall")
            .display()
            .to_string(),
    ]);
    env.write("teams/oncall/teams/dev/team.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n  panel: {team: teams/oncall, cwd: 'panel/{member_id}'}\n");
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
