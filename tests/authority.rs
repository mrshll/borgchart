//! Member lifecycle authority follows recorded parent edges across copies.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use instance_env::*;

fn department() -> Env {
    let env = Env::new();
    env.write(
        "templates/oncall/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false}\n  scout: {prompt: scout.md, start: false}\n  dev: {template: dev, parent: lead, start: false, max: 4}\n",
    );
    env.team("templates/dev");
    env.write(
        "templates/dev/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false}\n  design: {template: design, parent: lead, start: false}\n",
    );
    env.team("templates/design");
    env.write(
        "templates/design/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  scout: {prompt: scout.md}\n",
    );
    fs::create_dir(env.path("work/a/lead")).unwrap();
    fs::create_dir(env.path("work/b/lead")).unwrap();
    env
}

fn member_cmd(env: &Env, inst: &str, role: &str, start: usize, args: &[&str]) -> Command {
    let member = env.member(inst, role);
    env.wait_for("member launch recorded", || {
        env.starts(&member)
            .iter()
            .filter(|record| record["env"]["BORGCHART_TEAM"] == inst)
            .count()
            > start
    });
    let starts = env.starts(&member);
    let vars = &starts
        .iter()
        .filter(|record| record["env"]["BORGCHART_TEAM"] == inst)
        .nth(start)
        .unwrap()["env"];
    let mut cmd = env.cmd(args);
    for key in [
        "BORGCHART_TEAM",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        cmd.env(key, vars[key].as_str().unwrap());
    }
    cmd
}

fn member_ok(env: &Env, inst: &str, role: &str, args: &[&str]) -> Output {
    let out = member_cmd(env, inst, role, 0, args).output().unwrap();
    assert!(
        out.status.success(),
        "{inst}/{role} {args:?}: {}",
        both(&out)
    );
    out
}

fn member_refused(env: &Env, inst: &str, role: &str, args: &[&str]) {
    let out = member_cmd(env, inst, role, 0, args).output().unwrap();
    assert!(
        !out.status.success(),
        "{inst}/{role} {args:?}: {}",
        both(&out)
    );
    let error = both(&out);
    assert!(
        (error.contains("outside `") && error.contains("'s subtree"))
            || error.contains("start is incomplete, replaced or stopped"),
        "{inst}/{role} {args:?}: expected authority refusal: {error}"
    );
}

#[test]
fn members_cannot_create_independent_bus_instances() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let before = env.registry();
    let folder = env.path("work/b").display().to_string();
    let out = member_cmd(&env, &dept, "lead", 0, &["create", "oncall", &folder])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    let error = both(&out);
    assert!(error.contains("ask a person"), "{error}");
    assert!(error.contains("brg create <instance>/<role>"), "{error}");
    assert_eq!(env.registry(), before);
    env.ok(&["stop", &dept]);
}

#[test]
fn members_cannot_create_independent_teams_without_bus_settings() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    env.team("templates/panel");
    env.write(
        "templates/panel/template.yaml",
        "roles: {lead: {prompt: lead.md}}\n",
    );
    let folder = env.path("work/b").display().to_string();
    let before = env.registry();
    let out = member_cmd(&env, &dept, "lead", 0, &["create", "panel", &folder])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("ask a person"));
    assert_eq!(env.registry(), before);
    env.ok(&["stop", &dept]);
}

#[test]
fn people_can_create_independent_bus_instances() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let other = env.create("oncall", "work/b");
    assert_ne!(dept, other);
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    assert_eq!(env.state_of(&other, "lead"), "idle");
    env.ok(&["stop", &other]);
    env.ok(&["stop", &dept]);
}

fn copy_from_output(env: &Env, out: &Output) -> (String, PathBuf) {
    let id = stdout(out).split_whitespace().next().unwrap().to_string();
    let listed: serde_json::Value =
        serde_json::from_slice(&env.ok(&["templates", "--json"]).stdout).unwrap();
    let copy = listed
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|team| team["teams"].as_array().unwrap())
        .find(|copy| copy["team"] == id)
        .unwrap_or_else(|| panic!("copy {id}: {listed}\n{}", both(out)));
    let folder = PathBuf::from(copy["folder"].as_str().unwrap());
    assert_eq!(copy["running"], false);
    (id, folder)
}

#[test]
fn team_role_creation_refuses_non_ancestors() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    for role in ["helper", "scout"] {
        env.ok(&["create", &format!("{dept}/{role}")]);
        env.wait_for("caller idle", || env.state_of(&dept, role) == "idle");
        let before = env.registry();
        member_refused(&env, &dept, role, &["create", &format!("{dept}/dev")]);
        assert_eq!(env.registry(), before);
        assert!(!env.path("work/a/dev").exists());
    }
    let out = member_ok(&env, &dept, "lead", &["create", &format!("{dept}/dev")]);
    let (copy, folder) = copy_from_output(&env, &out);
    assert!(folder.is_dir());
    assert!(env.rows(&copy).iter().all(|row| row["member"].is_null()));
    env.ok(&["stop", &dept]);
}

fn prepared_copy(env: &Env, parent: &str, role: &str) -> String {
    let out = env.ok(&["create", &format!("{parent}/{role}")]);
    let (id, _) = copy_from_output(env, &out);
    id
}

#[test]
fn ancestors_manage_copies_and_members_at_every_depth() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let dev = prepared_copy(&env, &dept, "dev");
    member_ok(&env, &dept, "lead", &["start", &dev]);
    env.wait_for("dev lead idle", || env.state_of(&dev, "lead") == "idle");
    member_ok(&env, &dept, "lead", &["create", &format!("{dev}/helper")]);
    env.wait_for("dev helper idle", || env.state_of(&dev, "helper") == "idle");
    let panel = prepared_copy(&env, &dev, "design");
    member_ok(&env, &dept, "lead", &["start", &panel]);
    env.wait_for("panel lead idle", || env.state_of(&panel, "lead") == "idle");
    for (inst, role) in [(&dev, "lead"), (&dev, "helper"), (&panel, "lead")] {
        let target = format!("{inst}/{}", env.member(inst, role));
        member_ok(&env, &dept, "lead", &["stop", &target]);
        assert_eq!(env.state_of(inst, role), "stopped");
        member_ok(&env, &dept, "lead", &["start", &target]);
        env.wait_for("descendant idle", || env.state_of(inst, role) == "idle");
    }
    for (inst, role) in [(&dev, "helper"), (&panel, "lead"), (&panel, "scout")] {
        let target = format!("{inst}/{}", env.member(inst, role));
        member_ok(&env, &dept, "lead", &["stop", &target]);
        member_ok(&env, &dept, "lead", &["delete", &target]);
        assert!(env.member(inst, role).is_empty());
    }
    member_ok(&env, &dept, "lead", &["stop", &panel]);
    member_ok(&env, &dept, "lead", &["delete", &panel]);
    member_ok(&env, &dept, "lead", &["stop", &dev]);
    assert_eq!(env.state_of(&dev, "lead"), "stopped");
    member_ok(&env, &dept, "lead", &["start", &dev]);
    env.wait_for("dev lead restarted", || {
        env.state_of(&dev, "lead") == "idle"
    });
    member_ok(&env, &dept, "lead", &["stop", &dev]);
    member_ok(&env, &dept, "lead", &["delete", &dev]);
    assert!(env.registry()["teams"][&dev].is_null());
    env.ok(&["stop", &dept]);
}

#[test]
fn dev_leads_manage_their_helpers_and_panel() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let dev = prepared_copy(&env, &dept, "dev");
    env.ok(&["start", &dev]);
    env.wait_for("dev lead idle", || env.state_of(&dev, "lead") == "idle");
    member_ok(&env, &dev, "lead", &["create", &format!("{dev}/helper")]);
    env.wait_for("helper idle", || env.state_of(&dev, "helper") == "idle");
    let helper = format!("{dev}/{}", env.member(&dev, "helper"));
    member_ok(&env, &dev, "lead", &["stop", &helper]);
    member_ok(&env, &dev, "lead", &["start", &helper]);
    member_ok(&env, &dev, "lead", &["stop", &helper]);
    member_ok(&env, &dev, "lead", &["delete", &helper]);
    let out = member_ok(&env, &dev, "lead", &["create", &format!("{dev}/design")]);
    let (panel, _) = copy_from_output(&env, &out);
    member_ok(&env, &dev, "lead", &["start", &panel]);
    env.wait_for("panel lead idle", || env.state_of(&panel, "lead") == "idle");
    let designer = format!("{panel}/{}", env.member(&panel, "lead"));
    member_ok(&env, &dev, "lead", &["stop", &designer]);
    member_ok(&env, &dev, "lead", &["start", &designer]);
    member_ok(&env, &dev, "lead", &["stop", &panel]);
    member_ok(&env, &dev, "lead", &["delete", &panel]);
    assert!(env.registry()["teams"][&panel].is_null());
    env.ok(&["stop", &dept]);
}

#[test]
fn lifecycle_refuses_targets_outside_recorded_subtrees() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let first = prepared_copy(&env, &dept, "dev");
    let second = prepared_copy(&env, &dept, "dev");
    for copy in [&first, &second] {
        env.ok(&["start", copy]);
        env.wait_for("copy lead idle", || env.state_of(copy, "lead") == "idle");
    }
    env.ok(&["create", &format!("{first}/helper")]);
    env.wait_for("helper idle", || env.state_of(&first, "helper") == "idle");
    let panel = prepared_copy(&env, &first, "design");
    env.ok(&["start", &panel]);
    env.wait_for("panel roots idle", || {
        env.state_of(&panel, "lead") == "idle" && env.state_of(&panel, "scout") == "idle"
    });
    let other = env.create("oncall", "work/b");
    let first_lead = format!("{first}/{}", env.member(&first, "lead"));
    let second_lead = format!("{second}/{}", env.member(&second, "lead"));
    let dept_lead = format!("{dept}/{}", env.member(&dept, "lead"));
    let other_lead = format!("{other}/{}", env.member(&other, "lead"));
    let panel_scout = format!("{panel}/{}", env.member(&panel, "scout"));
    let forbidden = [
        (&first, "helper", &first_lead),
        (&first, "lead", &first_lead),
        (&first, "lead", &dept_lead),
        (&first, "lead", &second_lead),
        (&first, "lead", &other_lead),
        (&first, "lead", &first),
        (&first, "lead", &dept),
        (&first, "lead", &second),
        (&first, "lead", &other),
        (&panel, "lead", &panel_scout),
        (&panel, "lead", &panel),
    ];
    for (caller, role, target) in forbidden {
        for verb in ["start", "stop"] {
            let before = env.registry();
            member_refused(&env, caller, role, &[verb, target]);
            assert_eq!(env.registry(), before);
        }
    }
    for inst in [&dept, &first, &second, &panel, &other] {
        assert_eq!(env.state_of(inst, "lead"), "idle");
    }
    // A stopped target makes delete refusals exercise authority, rather than liveness.
    for target in [&second_lead, &other_lead, &panel_scout] {
        env.ok(&["stop", target]);
        let before = env.registry();
        let caller = if target == &panel_scout {
            &panel
        } else {
            &first
        };
        member_refused(&env, caller, "lead", &["delete", target, "--force"]);
        assert_eq!(env.registry(), before);
        env.ok(&["start", target]);
    }
    env.ok(&["stop", &second]);
    let before = env.registry();
    member_refused(&env, &first, "lead", &["delete", &second, "--force"]);
    assert_eq!(env.registry(), before);
    env.ok(&["stop", &dept]);
    env.ok(&["stop", &other]);
}

#[test]
fn stopped_replaced_and_partial_starts_cannot_manage_subtrees() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let dev = prepared_copy(&env, &dept, "dev");
    let lead = env.member(&dept, "lead");
    let lead_address = format!("{dept}/{lead}");
    env.ok(&["stop", &lead_address]);
    for verb in ["create", "start", "stop", "delete"] {
        let target = if verb == "create" {
            format!("{dept}/dev")
        } else {
            dev.clone()
        };
        let before = env.registry();
        member_refused(&env, &dept, "lead", &[verb, &target]);
        assert_eq!(env.registry(), before);
    }
    env.ok(&["start", &lead_address]);
    for verb in ["create", "start", "stop", "delete"] {
        let target = if verb == "create" {
            format!("{dept}/dev")
        } else {
            dev.clone()
        };
        let before = env.registry();
        member_refused(&env, &dept, "lead", &[verb, &target]);
        assert_eq!(env.registry(), before);
    }
    // A partially recorded start has no pane: it must never acquire authority.
    let path = env.path(&format!("state/{dept}/members/{lead}/incarnation.json"));
    let original = fs::read(&path).unwrap();
    let mut partial: serde_json::Value = serde_json::from_slice(&original).unwrap();
    partial.as_object_mut().unwrap().remove("pane");
    fs::write(&path, partial.to_string()).unwrap();
    for verb in ["create", "start", "stop", "delete"] {
        let target = if verb == "create" {
            format!("{dept}/dev")
        } else {
            dev.clone()
        };
        let out = member_cmd(&env, &dept, "lead", 1, &[verb, &target])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{verb}: {}", both(&out));
    }
    fs::write(path, original).unwrap();
    for key in [
        "BORGCHART_TEAM",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        for verb in ["create", "start", "stop", "delete"] {
            let target = if verb == "create" {
                format!("{dept}/dev")
            } else {
                dev.clone()
            };
            let before = env.registry();
            let out = member_cmd(&env, &dept, "lead", 1, &[verb, &target])
                .env_remove(key)
                .output()
                .unwrap();
            assert!(
                !out.status.success(),
                "missing {key}, {verb}: {}",
                both(&out)
            );
            assert_eq!(env.registry(), before);
        }
    }
    let out = member_cmd(&env, &dept, "lead", 1, &["start", &dev])
        .output()
        .unwrap();
    assert!(out.status.success(), "latest start: {}", both(&out));
    env.ok(&["stop", &dept]);
}

#[test]
fn recorded_copy_parents_survive_live_team_edits() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{dept}/scout")]);
    env.wait_for("scout idle", || env.state_of(&dept, "scout") == "idle");
    let dev = prepared_copy(&env, &dept, "dev");
    env.ok(&["start", &dev]);
    env.wait_for("dev lead idle", || env.state_of(&dev, "lead") == "idle");
    let team = env.path("templates/oncall/template.yaml");
    let original = fs::read_to_string(&team).unwrap();
    fs::write(
        &team,
        original.replace("dev, parent: lead", "dev, parent: scout"),
    )
    .unwrap();
    member_refused(&env, &dept, "scout", &["stop", &dev]);
    assert_eq!(env.state_of(&dev, "lead"), "idle");
    member_ok(&env, &dept, "lead", &["stop", &dev]);
    assert_eq!(env.state_of(&dev, "lead"), "stopped");
    member_refused(&env, &dept, "scout", &["delete", &dev]);
    member_ok(&env, &dept, "lead", &["delete", &dev]);
    assert!(env.registry()["teams"][&dev].is_null());
    fs::write(team, original).unwrap();
    env.ok(&["stop", &dept]);
}

#[test]
fn ancestors_manage_grandchildren_within_one_instance() {
    let env = department();
    let team = env.path("templates/oncall/template.yaml");
    let original = fs::read_to_string(&team).unwrap();
    fs::write(
        &team,
        format!("{original}  aide: {{prompt: helper.md, parent: helper, start: false}}\n"),
    )
    .unwrap();
    let dept = env.create("oncall", "work/a");
    member_ok(&env, &dept, "lead", &["create", &format!("{dept}/helper")]);
    env.wait_for("helper idle", || env.state_of(&dept, "helper") == "idle");
    member_ok(&env, &dept, "lead", &["create", &format!("{dept}/aide")]);
    env.wait_for("aide idle", || env.state_of(&dept, "aide") == "idle");
    let aide = format!("{dept}/{}", env.member(&dept, "aide"));
    let helper = format!("{dept}/{}", env.member(&dept, "helper"));
    // The ancestor can still manage the grandchild after its direct parent stops.
    env.ok(&["stop", &helper]);
    member_ok(&env, &dept, "lead", &["stop", &aide]);
    member_ok(&env, &dept, "lead", &["start", &aide]);
    member_ok(&env, &dept, "lead", &["stop", &aide]);
    member_ok(&env, &dept, "lead", &["delete", &aide]);
    assert!(env.member(&dept, "aide").is_empty());
    env.ok(&["stop", &dept]);
}

fn restart_racing_create_refuses_the_old_start(role: &str) {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let lead = format!("{dept}/{}", env.member(&dept, "lead"));
    let gate = env.path("create-gate");
    let child = member_cmd(
        &env,
        &dept,
        "lead",
        0,
        &["create", &format!("{dept}/{role}")],
    )
    .env("BORGCHART_TEST_CREATE_AUTHORITY_PAUSE", &gate)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    env.wait_for("create precheck completed", || {
        gate.with_extension("waiting").exists()
    });
    env.ok(&["stop", &lead]);
    env.ok(&["start", &lead]);
    let before = env.registry();
    fs::write(&gate, "").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "stale create {role}: {}", both(&out));
    assert_eq!(env.registry(), before);
    assert!(env.member(&dept, "helper").is_empty());
    assert!(!env.path("work/a/dev").exists());
    let out = member_cmd(
        &env,
        &dept,
        "lead",
        1,
        &["create", &format!("{dept}/{role}")],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "current create {role}: {}",
        both(&out)
    );
    env.ok(&["stop", &dept]);
}

#[test]
fn parent_restart_racing_agent_create_refuses_the_old_start() {
    restart_racing_create_refuses_the_old_start("helper");
}

#[test]
fn parent_restart_racing_team_copy_create_refuses_the_old_start() {
    restart_racing_create_refuses_the_old_start("dev");
}

#[test]
fn missing_parent_records_do_not_grant_authority_from_live_config() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{dept}/helper")]);
    let helper = env.member(&dept, "helper");
    let target = format!("{dept}/{helper}");
    env.ok(&["stop", &target]);
    let path =
        borgchart::state::StateDir::from_root(&env.path(&format!("state/{dept}"))).meta_file();
    let original = fs::read(&path).unwrap();
    let mut meta: serde_json::Value = serde_json::from_slice(&original).unwrap();
    meta["members"].as_object_mut().unwrap().remove(&helper);
    fs::write(&path, meta.to_string()).unwrap();
    for verb in ["stop", "delete"] {
        let out = member_cmd(&env, &dept, "lead", 0, &[verb, &target])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{}", both(&out));
        assert!(both(&out).contains("has no recorded parent; ask a person"));
        assert_eq!(env.member(&dept, "helper"), helper);
    }
    fs::write(path, original).unwrap();
    member_ok(&env, &dept, "lead", &["delete", &target]);
    env.ok(&["stop", &dept]);
}
