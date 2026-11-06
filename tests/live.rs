//! The live tier: the real `claude` and `codex`, started by real `brg up`
//! with your own login and configuration in place. Ignored by default;
//! local only.
//!
//! ```text
//! BORGCHART_LIVE=1 cargo test --locked --test live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! - `BORGCHART_LIVE_VENDORS=claude` (or `codex`) limits the run, which is
//!   then partial and qualifies nothing. An empty or unknown name is refused.
//! - `BORGCHART_LIVE_SUBMIT=0` skips the wake matrix's two real submissions
//!   per vendor (also partial).
//! - `BORGCHART_LIVE_DRY=1` runs the same harness against owned fake CLIs
//!   instead, and wins over `BORGCHART_LIVE`. The default tier runs it too
//!   (`dry_run_reaches_every_expected_result`).
//!
//! What it touches: tmux runs only in a private directory behind the
//! test's guard, under a watchdog that stops it if the run dies or
//! overruns. Nothing of your vendor configuration is copied or written by
//! brg: Codex gets session-only trust (`-c projects=…`), and every Claude
//! deployment sets `auto_trust: false`. Claude cases other than trust run in
//! `target/live/<run>/work/`, which must be inside a folder Claude already
//! trusts (this checkout or a parent); the trust cases use fresh folders
//! under the system temp directory. No tool approval is ever answered: one
//! that appears ends its case BLOCKED.
//!
//! The matrix (UTC time, brg revision, vendor versions) is written as cases
//! finish to `target/live/<run>/matrix.md`, with screens in `screens/`
//! beside it (they show your real screen). Only a complete live run with
//! every case in [`rules::REQUIRED`] PASS qualifies a stable rebuild.

mod common;

#[path = "live/cases.rs"]
mod cases;
#[path = "live/rules.rs"]
mod rules;
#[path = "live/wake_matrix.rs"]
mod wake_matrix;
#[path = "live/world.rs"]
mod world;

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use borgchart::team::Vendor;
use rules::{Matrix, Mode, Outcome, REQUIRED, Who};
use world::World;

/// One case by ID, with any sub-check results it produces.
fn case(w: &World, id: &str, submit: bool) -> Vec<(String, Outcome)> {
    let who = REQUIRED.iter().find(|(r, _)| *r == id).map(|(_, w)| *w);
    if let Some(Err(why)) = who.and_then(|who| w.versions.get(&who)) {
        return vec![(id.into(), Outcome::Blocked(format!("preflight: {why}")))];
    }
    let one = |o: Outcome| vec![(id.to_string(), o)];
    let run = || match id {
        "claude-trust-off" => cases::claude_trust(w, false),
        "claude-trust-on" => cases::claude_trust(w, true),
        "codex-trust-on" => cases::codex_trust(w, Some(true)),
        "codex-trust-off" => cases::codex_trust(w, Some(false)),
        "codex-locked-trust" => cases::codex_trust(w, None),
        "claude-env-colour" => one(cases::env_colour(w, Who::Claude)),
        "codex-env-colour" => one(cases::env_colour(w, Who::Codex)),
        "claude-background-wait" => one(cases::background_wait(w)),
        "codex-queue" => one(Outcome::Skip(
            "`codex queue` isn't available until #20".into(),
        )),
        "claude-wake-matrix" => one(matrix_case(w, Vendor::Claude, submit)),
        "codex-wake-matrix" => one(matrix_case(w, Vendor::Codex, submit)),
        other => one(Outcome::Fail(format!("no such case {other}"))),
    };
    catch_unwind(AssertUnwindSafe(run))
        .unwrap_or_else(|e| {
            let why = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            one(Outcome::Fail(format!("the harness panicked: {why}")))
        })
        .into_iter()
        .map(|(id, o)| (id, o.checked()))
        .collect()
}

fn matrix_case(w: &World, vendor: Vendor, submit: bool) -> Outcome {
    if w.mode == Mode::Dry {
        return Outcome::Skip(
            "dry: needs the real input boxes (tests/wake.rs covers the logic with fake-composer)"
                .into(),
        );
    }
    let who = match vendor {
        Vendor::Claude => Who::Claude,
        Vendor::Codex => Who::Codex,
    };
    let version = w.versions.get(&who).and_then(|v| v.clone().ok());
    let (rows, qualified) = wake_matrix::qualify(vendor, version, submit);
    let table = wake_matrix::table(vendor, &rows);
    let _ = std::fs::write(
        w.evidence
            .join(format!("{}-wake-matrix.md", vendor.as_str())),
        &table,
    );
    let failed: Vec<&str> = rows
        .iter()
        .filter(|r| r[3] == "FAIL")
        .map(|r| r[0].as_str())
        .collect();
    if !failed.is_empty() {
        Outcome::Fail(format!("rows failed: {}", failed.join(", ")))
    } else if !qualified {
        Outcome::Skip(format!(
            "this {} isn't qualified for typed reminders (see the launch facts row); typed attempts and submissions skipped",
            vendor.as_str()
        ))
    } else if !submit {
        Outcome::Skip("submissions turned off (BORGCHART_LIVE_SUBMIT=0)".into())
    } else {
        Outcome::Pass(format!("{} rows ok", rows.len()))
    }
}

/// Run every selected case in order, writing each row as it finishes.
fn run_cases(
    w: &World,
    selected: &[Who],
    submit: bool,
    matrix: &Matrix,
) -> BTreeMap<String, Outcome> {
    let mut results = BTreeMap::new();
    for (id, who) in REQUIRED {
        if !selected.contains(who) || results.contains_key(*id) {
            continue;
        }
        for (id, o) in case(w, id, submit) {
            matrix.row(&id, &o);
            results.insert(id, o);
        }
    }
    results
}

fn revision() -> String {
    let git = |args: &[&str]| {
        common::command("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let rev = git(&["rev-parse", "--short", "HEAD"]);
    let dirty = !git(&["status", "--porcelain", "--untracked-files=no"]).is_empty();
    format!("{rev}{}", if dirty { " (modified)" } else { "" })
}

fn header(w: &World, selected: &[Who], partial: bool) -> String {
    let mut h = String::from(if w.mode == Mode::Dry {
        "# Live tier: DRY run (owned fake CLIs; qualifies nothing)\n\n"
    } else {
        "# Live tier run\n\n"
    });
    h.push_str(&format!("- UTC: {}\n", borgchart::bus::time::utc_now()));
    h.push_str(&format!("- brg: {}\n", revision()));
    for (who, v) in &w.versions {
        let v = match v {
            Ok(v) => v.clone(),
            Err(e) => format!("unavailable: {e}"),
        };
        h.push_str(&format!("- {}: {v}\n", who.as_str()));
    }
    let names: Vec<&str> = selected.iter().map(|w| w.as_str()).collect();
    h.push_str(&format!(
        "- selection: {}{}\n",
        names.join(", "),
        if partial { " (partial)" } else { "" }
    ));
    h
}

#[test]
#[ignore = "launches the real vendor CLIs; set BORGCHART_LIVE=1"]
fn live_matrix() {
    let env = |k: &str| std::env::var(k).ok();
    let mode = rules::mode(
        env("BORGCHART_LIVE").as_deref(),
        env("BORGCHART_LIVE_DRY").as_deref(),
    );
    if mode == Mode::Off {
        eprintln!("set BORGCHART_LIVE=1 to run (or BORGCHART_LIVE_DRY=1 for the fakes)");
        return;
    }
    let (selected, mut partial) =
        rules::select(env("BORGCHART_LIVE_VENDORS").as_deref()).unwrap_or_else(|e| panic!("{e}"));
    let submit = env("BORGCHART_LIVE_SUBMIT").as_deref() != Some("0");
    partial |= !submit;
    let mut w = match mode {
        Mode::Dry => {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/live")
                .join(format!("{}-dry", world::run_name()));
            World::dry(&dir)
        }
        _ => World::live(),
    };
    w.preflight(&selected);
    wake_matrix::set_dirs(wake_matrix::Dirs {
        work: w.trusted.join("wake"),
        keep: w.evidence.clone(),
        limit: w.limits.run,
    });
    let matrix = Matrix::create(&w.evidence, &header(&w, &selected, partial));
    println!("matrix: {}", matrix.path.display());
    let results = run_cases(&w, &selected, submit, &matrix);
    let v = rules::verdict(&selected, &results, partial, mode == Mode::Dry);
    matrix.finish(&v);
    assert!(
        !v.fails(),
        "{}\n(see {})",
        v.summary(),
        matrix.path.display()
    );
}

/// The dry run, in the default tier: every case and every false-positive
/// control reaches its expected result against the owned fakes, the matrix
/// is stamped DRY and kept, and no server outlives the run.
#[test]
fn dry_run_reaches_every_expected_result() {
    let keep = tempfile::tempdir().unwrap();
    let evidence = keep.path().join("run");
    let both = [Who::Claude, Who::Codex];
    let mut w = World::dry(&evidence);
    w.preflight(&both);
    let private = w.private.dir().to_path_buf();
    let matrix = Matrix::create(&w.evidence, &header(&w, &both, false));

    // Every case, all fakes well behaved.
    let results = run_cases(&w, &both, true, &matrix);
    let skips = [
        "claude-trust-status",
        "codex-trust-status",
        "claude-trust-on",
        "codex-queue",
        "claude-wake-matrix",
        "codex-wake-matrix",
    ];
    let mut wrong = Vec::new();
    for (id, _) in REQUIRED {
        let want = if skips.contains(id) { "SKIP" } else { "PASS" };
        match results.get(*id) {
            Some(o) if o.label() == want => {}
            got => wrong.push(format!("{id}: want {want}, got {got:?}")),
        }
    }
    let v = rules::verdict(&both, &results, false, true);
    matrix.finish(&v);
    assert!(!v.fails() && !v.qualifies(), "{v:?}");

    // A missing case: the run is incomplete and fails.
    let mut short = results.clone();
    short.remove("codex-env-colour");
    let v = rules::verdict(&both, &short, false, true);
    assert!(v.fails() && v.missing == ["codex-env-colour"], "{v:?}");

    // False-positive controls: (case, member, fake scenario, expected).
    let controls: &[(&str, &str, &str, &str)] = &[
        ("claude-background-wait", cases::WAITER, "stale", "FAIL"),
        (
            "claude-background-wait",
            cases::WAITER,
            "foreground",
            "FAIL",
        ),
        (
            "claude-background-wait",
            cases::WAITER,
            "never-armed",
            "FAIL",
        ),
        (
            "claude-background-wait",
            cases::WAITER,
            "approval",
            "BLOCKED",
        ),
        ("claude-env-colour", "painter", "nocolour", "FAIL"),
        ("claude-env-colour", "painter", "wrongenv", "FAIL"),
        ("codex-env-colour", "painter", "nocolour", "FAIL"),
        ("codex-env-colour", "painter", "wrongenv", "FAIL"),
        ("codex-trust-off", "truster", "write-config", "FAIL"),
        ("codex-trust-on", "truster", "approval", "BLOCKED"),
    ];
    w.fake_file("stale-nonce", Some("n0123456789abcdefz"));
    let control = |w: &World, id: &str, what: &str, want: &str, wrong: &mut Vec<String>| {
        let got = case(w, id, true).remove(0).1;
        matrix.row(&format!("control {id} ({what})"), &got);
        if got.label() != want {
            wrong.push(format!("control {id} ({what}): want {want}, got {got:?}"));
        }
    };
    for (id, member, scenario, want) in controls {
        w.scenario(member, scenario);
        control(&w, id, scenario, want, &mut wrong);
        w.scenario(member, "ok");
    }
    // A folder that turns out to be trusted already: never PASS.
    let trusted = format!("{}\n{}\n", w.trusted.display(), w.scratch.display());
    w.fake_file("trusted", Some(&trusted));
    control(
        &w,
        "claude-trust-off",
        "already trusted",
        "BLOCKED",
        &mut wrong,
    );
    control(
        &w,
        "codex-trust-off",
        "already trusted",
        "BLOCKED",
        &mut wrong,
    );
    w.fake_file("trusted", Some(&format!("{}\n", w.trusted.display())));
    // A hung CLI: preflight's limit makes its cases BLOCKED.
    w.fake_file("hang-version-codex", Some(""));
    w.preflight(&[Who::Codex]);
    control(
        &w,
        "codex-env-colour",
        "hung --version",
        "BLOCKED",
        &mut wrong,
    );
    w.fake_file("hang-version-codex", None);

    drop(w);
    let kept = std::fs::read_to_string(evidence.join("matrix.md")).unwrap();
    assert!(kept.starts_with("# Live tier: DRY run"), "{kept}");
    assert!(
        kept.contains("control claude-background-wait (stale)"),
        "{kept}"
    );
    assert!(
        !private.exists(),
        "the private tmux directory outlived the run"
    );
    assert!(wrong.is_empty(), "{}\n\n{kept}", wrong.join("\n"));
}
