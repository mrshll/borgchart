//! A department's copies are prepared before launch, even at depth three.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;

use instance_env::*;

fn create_copy(env: &Env, parent: &str, role: &str) -> (String, std::path::PathBuf) {
    let output = env.ok(&["create", &format!("{parent}/{role}")]);
    let listed: serde_json::Value =
        serde_json::from_slice(&env.ok(&["ls", "--json"]).stdout).unwrap();
    let copy = listed
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|team| team["instances"].as_array().unwrap())
        .find(|instance| {
            instance["instance"]
                .as_str()
                .is_some_and(|id| id.starts_with(&format!("{parent}/{role}-")))
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

#[test]
fn depth_three_copies_wait_for_preparation_and_cascade_before_parent_deletion() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, start: true, cwd: lead}\n  dev: {team: teams/dev, parent: lead, start: false, max: 2, cwd: 'dev/{member_id}'}\n",
    );
    env.write(
        "teams/oncall/teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, start: true, cwd: repo}\n  design: {team: teams/design, parent: lead, start: false, cwd: 'design/{member_id}'}\n",
    );
    env.write(
        "teams/oncall/teams/dev/lead.md",
        "---\nmodels: {claude: opus high}\n---\nLead the fictional development team.\n",
    );
    env.write(
        "teams/oncall/teams/dev/teams/design/team.yaml",
        "bus: {}\nroles:\n  amber: {prompt: amber.md, start: true, cwd: work}\n  violet: {prompt: violet.md, start: true, cwd: work}\n",
    );
    for role in ["amber", "violet"] {
        env.write(
            &format!("teams/oncall/teams/dev/teams/design/{role}.md"),
            "---\nmodels: {claude: opus high}\n---\nReview the fictional design.\n",
        );
    }
    fs::create_dir(env.path("work/a/lead")).unwrap();
    let dept = env.create("oncall", "work/a");
    let dept_lead = env.member(&dept, "lead");
    assert!(!env.path("work/a/dev").exists());

    let (dev, dev_folder) = create_copy(&env, &dept, "dev");
    let dev_id = dev.rsplit('/').next().unwrap();
    assert!(well_formed(dev_id, "dev"));
    assert_eq!(dev_folder, env.path(&format!("work/a/dev/{dev_id}")));
    assert!(!dev_folder.join("repo").exists());
    fs::create_dir(dev_folder.join("repo")).unwrap();
    env.ok(&["start", &dev]);
    env.wait_for("dev lead idle", || env.state_of(&dev, "lead") == "idle");
    let dev_lead = env.member(&dev, "lead");
    let start = &env.starts(&dev_lead)[0];
    assert_eq!(start["cwd"], dev_folder.join("repo").display().to_string());
    assert_eq!(start["env"]["BORGCHART_INSTANCE"], dev);
    assert_eq!(
        start["env"]["BORGCHART_PARENT"],
        format!("{dept}/{dept_lead}")
    );

    let (panel, panel_folder) = create_copy(&env, &dev, "design");
    let panel_id = panel.rsplit('/').next().unwrap();
    assert!(well_formed(panel_id, "design"));
    assert_eq!(panel_folder, dev_folder.join("design").join(panel_id));
    assert!(!panel_folder.join("work").exists());
    fs::create_dir(panel_folder.join("work")).unwrap();
    env.ok(&["start", &panel]);
    for role in ["amber", "violet"] {
        env.wait_for("designer idle", || env.state_of(&panel, role) == "idle");
        let member = env.member(&panel, role);
        let start = &env.starts(&member)[0];
        assert_eq!(
            start["cwd"],
            panel_folder.join("work").display().to_string()
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
            assert!(state.join("messages").is_dir());
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
    assert!(dev_folder.is_dir() && panel_folder.is_dir());
}
