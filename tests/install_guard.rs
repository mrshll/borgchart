//! Running trees accept changed catalogs and files; broken inputs fail locally.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::os::unix::fs::symlink;

use instance_env::*;

#[test]
fn starting_a_sibling_does_not_reparent_a_running_child() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/helper")]);
    let lead = env.member(&inst, "lead");
    let helper = env.member(&inst, "helper");
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace("parent: lead", "parent: scout"),
    );
    env.ok(&["create", &format!("{inst}/scout")]);
    let out = env
        .cmd(&["stop", &format!("{inst}/{helper}")])
        .envs(
            serde_json::from_value::<std::collections::BTreeMap<String, String>>(
                env.starts(&lead)[0]["env"].clone(),
            )
            .unwrap(),
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &inst]);
}

fn catalog(env: &Env, root: &str) {
    env.team(&format!("{root}/oncall"));
    env.team(&format!("{root}/dev"));
    env.write(
        &format!("{root}/oncall/template.yaml"),
        &format!("{TEAM}  dev: {{template: dev, parent: lead, max: 2}}\n"),
    );
}

fn create_copy(env: &Env, inst: &str) -> String {
    stdout(&env.ok(&["create", &format!("{inst}/dev")]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn resumed(env: &Env, inst: &str, member: &str, session: &str) {
    env.wait_for("resumed fake agent", || env.starts(member).len() == 2);
    let starts = env.starts(member);
    let args: Vec<&str> = starts[1]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        args.windows(2).any(|a| a == ["--resume", session]),
        "{inst}/{member}: {args:?}"
    );
}

fn catalog_change_allows_joins(flip_symlink: bool) {
    let env = Env::new();
    for root in ["revision-a", "revision-b"] {
        catalog(&env, root);
    }
    if flip_symlink {
        symlink(env.path("revision-a"), env.path("current")).unwrap();
    }
    env.write(
        "config.yaml",
        if flip_symlink {
            "schema: 1\ntemplates: [current]\n"
        } else {
            "schema: 1\ntemplates: [revision-a]\n"
        },
    );
    let inst = env.create("oncall", "work/a");
    let copy = create_copy(&env, &inst);
    env.ok(&["start", &copy]);
    env.wait_for("copy lead idle", || env.state_of(&copy, "lead") == "idle");
    let root_lead = env.member(&inst, "lead");
    let copy_lead = env.member(&copy, "lead");
    let session = |instance: &str, member: &str| {
        let path = env.path(&format!("state/{instance}/members/{member}/session"));
        env.wait_for("vendor session recorded", || path.exists());
        fs::read_to_string(path).unwrap().trim().to_string()
    };
    let root_session = session(&inst, &root_lead);
    let copy_session = session(&copy, &copy_lead);
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env
        .rows(&inst)
        .into_iter()
        .find(|row| row["instance"] == inst && row["role"] == "scout")
        .unwrap()["member"]
        .as_str()
        .unwrap()
        .to_string();
    env.ok(&["stop", &format!("{inst}/{scout}")]);
    let note = format!("state/{inst}/notes/draft.txt");
    env.write(&note, "unfinished draft\n");
    let sent = env.ok(&[
        "msg",
        "send",
        &format!("{inst}/{root_lead}"),
        "Keep the draft",
        "--external",
        "ops",
        "--json",
    ]);
    let message: serde_json::Value = serde_json::from_slice(&sent.stdout).unwrap();
    let message_id = message["id"].as_str().unwrap();

    if flip_symlink {
        fs::remove_file(env.path("current")).unwrap();
        symlink(env.path("revision-b"), env.path("current")).unwrap();
    } else {
        env.write("config.yaml", "schema: 1\ntemplates: [revision-b]\n");
    }
    let next = create_copy(&env, &inst);
    env.ok(&["start", &next]);
    for args in [
        vec!["create".into(), format!("{inst}/helper")],
        vec!["start".into(), format!("{inst}/{scout}")],
        vec!["start".into(), inst.clone()],
        vec!["start".into(), inst.clone(), "--resume".into()],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = env.brg(&args);
        println!("{args:?}: exit {:?}\n{}", out.status.code(), both(&out));
        assert!(out.status.success(), "{}", both(&out));
    }
    // A copy with no team roles still uses its own unchanged source files.
    env.ok(&["start", &copy]);
    env.ok(&["create", &format!("{copy}/helper")]);
    env.ok(&["stop", &inst]);
    let out = env.ok(&["start", &inst, "--resume"]);
    println!("full restart: exit {:?}\n{}", out.status.code(), both(&out));
    resumed(&env, &inst, &root_lead, &root_session);
    resumed(&env, &copy, &copy_lead, &copy_session);
    assert_eq!(
        fs::read_to_string(env.path(&note)).unwrap(),
        "unfinished draft\n"
    );
    env.ok(&["msg", "read", message_id, "--instance", &inst]);
    env.wait_for("new copy lead started", || {
        env.state_of(&next, "lead") == "idle"
    });
    // The public status header identifies the source used by each kept copy.
    for (instance, revision) in [
        (&inst, "revision-a/oncall"),
        (&copy, "revision-a/dev"),
        (&next, "revision-b/dev"),
    ] {
        let status = stdout(&env.ok(&["status", instance]));
        assert!(
            status.contains(&env.path(revision).display().to_string()),
            "{status}"
        );
    }
    env.ok(&["stop", &inst]);
}

#[test]
fn identical_catalog_repoint_does_not_block_running_parent_joins() {
    catalog_change_allows_joins(false);
}

#[test]
fn current_symlink_flip_does_not_block_running_parent_joins() {
    catalog_change_allows_joins(true);
}

#[test]
fn changed_role_and_auth_apply_to_new_starts_without_restarting_siblings() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace("vendor: codex", "vendor: claude"),
    );
    env.write(
        "templates/oncall/helper.md",
        "---\nmodels: {claude: haiku}\n---\nUse the amber checklist.\n",
    );
    env.config("auth: api_key\n");
    let out = env
        .cmd(&["create", &format!("{inst}/helper")])
        .env("ANTHROPIC_API_KEY", "example-key")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let helper = env.member(&inst, "helper");
    env.wait_for("helper launch", || !env.starts(&helper).is_empty());
    let starts = env.starts(&helper);
    assert!(starts[0]["argv"].to_string().contains("amber checklist"));
    assert_eq!(starts[0]["env"]["ANTHROPIC_API_KEY"], "example-key");
    assert_eq!(env.starts(&lead).len(), 1);
    env.ok(&["stop", &format!("{inst}/{helper}")]);
    let team = fs::read_to_string(env.path("templates/oncall/template.yaml")).unwrap();
    env.write(
        "templates/oncall/template.yaml",
        &team.replace(
            "    prompt: helper.md\n",
            "    prompt: helper.md\n    interactive: true\n",
        ),
    );
    env.config("");
    env.ok(&["start", &format!("{inst}/{helper}"), "--resume"]);
    env.wait_for("helper restarted", || env.starts(&helper).len() == 2);
    assert!(
        env.starts(&helper)[1]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    let out = env.ok(&[
        "msg",
        "send",
        &format!("{inst}/{helper}"),
        "Review the checklist",
        "--external",
        "ops",
        "--allow-unknown",
        "--json",
    ]);
    let sent: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sent["delivery"], "stored");
    assert_eq!(env.starts(&lead).len(), 1);
    env.ok(&["stop", &inst]);
}

#[test]
fn changed_child_team_is_used_by_new_copies_while_parent_runs() {
    let env = Env::new();
    catalog(&env, "templates");
    let inst = env.create("oncall", "work/a");
    let first = create_copy(&env, &inst);
    env.ok(&["start", &first]);
    let lead = env.member(&first, "lead");
    env.wait_for("first copy launch", || !env.starts(&lead).is_empty());
    env.write(
        "templates/dev/lead.md",
        "---\nmodels: {claude: haiku}\n---\nUse the blue checklist.\n",
    );
    let next = create_copy(&env, &inst);
    env.ok(&["start", &next]);
    let next_lead = env.member(&next, "lead");
    env.wait_for("new copy launch", || !env.starts(&next_lead).is_empty());
    assert!(
        env.starts(&next_lead)[0]["argv"]
            .to_string()
            .contains("blue checklist")
    );
    assert_eq!(env.starts(&lead).len(), 1);
    env.ok(&["stop", &format!("{first}/{lead}")]);
    env.ok(&["start", &format!("{first}/{lead}")]);
    env.wait_for("copy member restarted", || env.starts(&lead).len() == 2);
    assert!(
        env.starts(&lead)[1]["argv"]
            .to_string()
            .contains("blue checklist")
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn damaged_start_record_still_refuses_copy_creation() {
    let env = Env::new();
    catalog(&env, "templates");
    let inst = env.create("oncall", "work/a");
    let path = env.path(&format!("state/{inst}/start.json"));
    let recorded = fs::read(&path).unwrap();
    fs::write(&path, "not json").unwrap();
    let err = env.fails(&["create", &format!("{inst}/dev")]);
    assert!(err.contains("start.json"), "{err}");
    fs::write(path, recorded).unwrap();
    create_copy(&env, &inst);
    env.ok(&["stop", &inst]);
}
