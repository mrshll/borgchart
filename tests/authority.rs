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
        "teams/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  helper: {prompt: helper.md, parent: lead, start: false, cwd: lead}\n  scout: {prompt: scout.md, start: false, cwd: lead}\n  dev: {team: teams/dev, parent: lead, start: false, max: 4, cwd: 'dev/{member_id}'}\n",
    );
    env.team("teams/oncall/teams/dev");
    env.write(
        "teams/oncall/teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n  helper: {prompt: helper.md, parent: lead, start: false, cwd: repo}\n  design: {team: teams/design, parent: lead, start: false, cwd: 'design/{member_id}'}\n",
    );
    env.team("teams/oncall/teams/dev/teams/design");
    env.write(
        "teams/oncall/teams/dev/teams/design/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: work}\n  scout: {prompt: scout.md, cwd: work}\n",
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
            .filter(|record| record["env"]["BORGCHART_INSTANCE"] == inst)
            .count()
            > start
    });
    let starts = env.starts(&member);
    let vars = &starts
        .iter()
        .filter(|record| record["env"]["BORGCHART_INSTANCE"] == inst)
        .nth(start)
        .unwrap()["env"];
    let mut cmd = env.cmd(args);
    for key in [
        "BORGCHART_INSTANCE",
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
}

fn copy_from_output(env: &Env, out: &Output) -> (String, PathBuf) {
    let id = stdout(out).split_whitespace().next().unwrap().to_string();
    let listed: serde_json::Value =
        serde_json::from_slice(&env.ok(&["ls", "--json"]).stdout).unwrap();
    let copy = listed
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|team| team["instances"].as_array().unwrap())
        .find(|copy| copy["instance"] == id)
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
