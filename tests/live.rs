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
//! The separate cold seating probe uses Codex `gpt-6-astra medium` and a
//! real `brg-seat route` quota/app-server call. It requires that model and
//! an existing `$HOME/.local/state`, where it creates and removes a private
//! temporary cache. Its evidence is separate from the matrix.
//!
//! The matrix (UTC time, brg revision, vendor versions) is written as cases
//! finish to `target/live/<run>/matrix.md`, with screens in `screens/`
//! beside it (they show your real screen). Only a complete live run with
//! every case in [`rules::REQUIRED`] PASS qualifies a stable rebuild.

mod common;

#[path = "live/seat_qualification.rs"]
mod seat_qualification;

#[path = "live/cases.rs"]
mod cases;
#[path = "live/rules.rs"]
mod rules;
#[path = "live/wake_matrix.rs"]
mod wake_matrix;
#[path = "live/world.rs"]
mod world;

#[test]
#[ignore = "bounded real mid-turn probe; set BORGCHART_LIVE=1"]
fn send_midturn_probe() {
    if std::env::var("BORGCHART_LIVE").as_deref() != Ok("1") {
        return;
    }
    let mut w = world::World::live();
    let (who, vendor) = match std::env::var("BORGCHART_LIVE_VENDORS").as_deref() {
        Ok("claude") => (rules::Who::Claude, borgchart::team::Vendor::Claude),
        Ok("codex") | Err(_) => (rules::Who::Codex, borgchart::team::Vendor::Codex),
        _ => panic!("send probe requires exactly claude or codex"),
    };
    w.preflight(&[who]);
    let version = w.versions[&who].clone().unwrap();
    wake_matrix::set_dirs(wake_matrix::Dirs {
        work: w.trusted.join("send"),
        keep: w.evidence.clone(),
        limit: std::time::Duration::from_secs(300),
    });
    println!("probe evidence: {}", w.evidence.display());
    wake_matrix::send_midturn_probe(vendor, version);
}

#[test]
#[ignore = "real isolated Claude background inbox wait; set BORGCHART_LIVE=1"]
fn claude_background_inbox_probe() {
    if std::env::var("BORGCHART_LIVE").as_deref() != Ok("1") {
        return;
    }
    let mut w = world::World::live();
    w.preflight(&[rules::Who::Claude]);
    assert!(w.versions[&rules::Who::Claude].is_ok());
    println!("background inbox evidence: {}", w.evidence.display());
    let outcome = cases::background_wait(&w);
    std::fs::write(
        w.evidence.join("background-outcome.txt"),
        format!("{outcome:?}\n"),
    )
    .unwrap();
    println!("{outcome:?}");
    assert!(matches!(outcome, rules::Outcome::Pass(_)), "{outcome:?}");
}

#[test]
#[ignore = "real isolated composer matrix; set BORGCHART_LIVE=1"]
fn send_composer_matrix() {
    if std::env::var("BORGCHART_LIVE").as_deref() != Ok("1") {
        return;
    }
    let (who, vendor) = match std::env::var("BORGCHART_LIVE_VENDORS").as_deref() {
        Ok("claude") => (rules::Who::Claude, borgchart::team::Vendor::Claude),
        Ok("codex") => (rules::Who::Codex, borgchart::team::Vendor::Codex),
        _ => panic!("select exactly one vendor"),
    };
    let mut w = world::World::live();
    w.preflight(&[who]);
    let version = w.versions[&who].clone().unwrap();
    wake_matrix::set_dirs(wake_matrix::Dirs {
        work: w.trusted.join("composer"),
        keep: w.evidence.clone(),
        limit: std::time::Duration::from_secs(600),
    });
    println!("composer evidence: {}", w.evidence.display());
    let (rows, qualified) = wake_matrix::qualify(vendor, Some(version), true);
    let table = wake_matrix::table(vendor, &rows);
    std::fs::write(w.evidence.join("composer-matrix.md"), &table).unwrap();
    println!(
        "{table}\nExisting wake qualification: {qualified}; not a full send-gate qualification"
    );
    assert!(rows.iter().all(|r| r[3] != "FAIL"));
}

#[test]
#[ignore = "real vendor tool context precondition only; set BORGCHART_LIVE=1"]
fn direct_vendor_context_probe() {
    if std::env::var("BORGCHART_LIVE").as_deref() != Ok("1") {
        return;
    }
    let (who, vendor) = match std::env::var("BORGCHART_LIVE_VENDORS").as_deref() {
        Ok("claude") => (rules::Who::Claude, borgchart::team::Vendor::Claude),
        Ok("codex") => (rules::Who::Codex, borgchart::team::Vendor::Codex),
        _ => panic!("select exactly one vendor"),
    };
    let mut w = world::World::live();
    w.preflight(&[who]);
    let version = w.versions[&who].clone().unwrap();
    wake_matrix::set_dirs(wake_matrix::Dirs {
        work: w.trusted.join("context"),
        keep: w.evidence.clone(),
        limit: std::time::Duration::from_secs(240),
    });
    println!("context evidence: {}", w.evidence.display());
    wake_matrix::direct_context_probe(vendor, version);
}

#[test]
#[ignore = "real isolated near-limit paste observation; set BORGCHART_LIVE=1"]
fn send_near_limit_probe() {
    if std::env::var("BORGCHART_LIVE").as_deref() != Ok("1") {
        return;
    }
    let (who, vendor) = match std::env::var("BORGCHART_LIVE_VENDORS").as_deref() {
        Ok("claude") => (rules::Who::Claude, borgchart::team::Vendor::Claude),
        Ok("codex") => (rules::Who::Codex, borgchart::team::Vendor::Codex),
        _ => panic!("select exactly one vendor"),
    };
    let mut w = world::World::live();
    w.preflight(&[who]);
    let version = w.versions[&who].clone().unwrap();
    wake_matrix::set_dirs(wake_matrix::Dirs {
        work: w.trusted.join("near-limit"),
        keep: w.evidence.clone(),
        limit: std::time::Duration::from_secs(120),
    });
    println!("near-limit evidence: {}", w.evidence.display());
    wake_matrix::near_limit_probe(vendor, version);
}

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use borgchart::team::Vendor;
use rules::{Matrix, Mode, Outcome, REQUIRED, Who};
use world::World;

#[test]
fn private_socketless_vendor_reads_remain_isolated() {
    let private = common::PrivateTmux::new();
    let other = common::PrivateTmux::new();
    assert!(
        other
            .tmux("other")
            .args(["new-session", "-d", "-s", "other", "sleep 60"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        private
            .tmux("queries")
            .args(["new-session", "-d", "-s", "g", "sleep 60"])
            .status()
            .unwrap()
            .success()
    );
    let out = private
        .tmux("queries")
        .args(["display-message", "-p", "#{socket_path},#{pid},0"])
        .output()
        .unwrap();
    let tmux_env = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let query = |env: Option<&str>, args: &[&str]| {
        let mut cmd = common::command(private.guard());
        if let Some(env) = env {
            cmd.env("TMUX", env);
        }
        cmd.args(args).output().unwrap()
    };
    for args in [
        vec!["show", "-Av", "mouse"],
        vec!["show", "-gv", "focus-events"],
        vec![
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{session_name}:#{window_id}.#{pane_id}",
        ],
        vec![
            "display-message",
            "-p",
            "-t",
            "%0",
            "#{client_termtype}\t#{allow-passthrough}\t#{focus-events}\t#{pane_top}\t#{pane_left}\t#{status}\t#{status-position}\t#{window_bigger}",
        ],
    ] {
        assert!(query(Some(&tmux_env), &args).status.success(), "{args:?}");
    }
    let socket = tmux_env.split(',').next().unwrap();
    let uid = std::fs::metadata(private.dir())
        .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
        .unwrap();
    let default_server = format!("/tmp/tmux-{uid}/default,1,0");
    let real_server = format!("/tmp/tmux-{uid}/{},1,0", borgchart::state::DEFAULT_SOCKET);
    let outside = format!("{}/tmux-{uid}/other,1,0", other.dir().display(),);
    let regular = std::path::Path::new(socket).with_file_name("regular");
    std::fs::write(&regular, "not a socket").unwrap();
    let regular = format!("{},1,0", regular.display());
    let link = std::path::Path::new(socket).with_file_name("linked");
    std::os::unix::fs::symlink(socket, &link).unwrap();
    let linked = format!("{},1,0", link.display());
    let missing = format!(
        "{}/missing,1,0",
        std::path::Path::new(socket).parent().unwrap().display()
    );
    let traversal = format!(
        "{}/../queries,1,0",
        std::path::Path::new(socket).parent().unwrap().display()
    );
    let malformed = format!("{socket},1,0,extra");
    for env in [
        None,
        Some(default_server.as_str()),
        Some(real_server.as_str()),
        Some(regular.as_str()),
        Some(outside.as_str()),
        Some(linked.as_str()),
        Some(missing.as_str()),
        Some(traversal.as_str()),
        Some(malformed.as_str()),
        Some(socket),
        Some("bad,1,0"),
    ] {
        assert_eq!(
            query(env, &["show", "-Av", "mouse"]).status.code(),
            Some(2),
            "{env:?}"
        );
        private.clear_refused();
    }
    for args in [
        vec!["kill-server"],
        vec!["show", "-Av", "mouse", ";", "kill-server"],
        vec![
            "display-message",
            "-p",
            "-t",
            "%0",
            "#(touch /tmp/never-run)",
        ],
        vec![
            "display-message",
            "-p",
            "-t",
            "other:0",
            "#{session_name}:#{window_id}.#{pane_id}",
        ],
        vec!["-f", "/dev/null", "show", "-Av", "mouse"],
    ] {
        assert_eq!(
            query(Some(&tmux_env), &args).status.code(),
            Some(2),
            "{args:?}"
        );
        private.clear_refused();
    }
    assert!(
        private
            .tmux("queries")
            .args(["has-session", "-t", "g"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        other
            .tmux("other")
            .args(["has-session", "-t", "other"])
            .status()
            .unwrap()
            .success()
    );
}

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

/// The dry run: every case and every false-positive control reaches its
/// expected result against the owned fakes, the matrix is stamped DRY and
/// kept, and no server outlives the run. It tests the live tier's harness,
/// not brg, and takes most of a minute, so it is outside the default tier:
/// `.githooks/checks harness` runs it (CI does, once, on Linux).
#[test]
#[ignore = "harness self-test; run with `.githooks/checks harness`"]
fn dry_run_reaches_every_expected_result() {
    let keep = tempfile::tempdir().unwrap();
    let evidence = keep.path().join("run");
    let both = [Who::Claude, Who::Codex];
    let mut w = World::dry(&evidence);
    w.preflight(&both);
    let private = w.private.dir().to_path_buf();
    let matrix = Matrix::create(&w.evidence, &header(&w, &both, false));
    // Without process facts (/proc: macOS), these cases and their controls
    // are SKIP, and say why; every other case and control is the same.
    let procs = world::has_proc();
    let needs_proc = [
        "claude-env-colour",
        "codex-env-colour",
        "claude-background-wait",
    ];
    let expect = |id: &str, want: &'static str| {
        if !procs && needs_proc.contains(&id) {
            "SKIP"
        } else {
            want
        }
    };
    let mut wrong = Vec::new();
    let mut check = |what: &str, id: &str, want: &'static str, got: Option<&Outcome>| {
        let want = expect(id, want);
        match got {
            Some(o) if o.label() == want => {
                if want == "SKIP" && needs_proc.contains(&id) && !o.detail().contains("/proc") {
                    wrong.push(format!("{what}: SKIP without the /proc reason: {o:?}"));
                }
            }
            got => wrong.push(format!("{what}: want {want}, got {got:?}")),
        }
    };

    // Every case, all fakes well behaved.
    let results = run_cases(&w, &both, true, &matrix);
    let skips = [
        "claude-trust-on",
        "codex-queue",
        "claude-wake-matrix",
        "codex-wake-matrix",
    ];
    for (id, _) in REQUIRED {
        let want = if skips.contains(id) { "SKIP" } else { "PASS" };
        check(id, id, want, results.get(*id));
    }
    let v = rules::verdict(&both, &results, false, true);
    matrix.finish(&v);
    assert!(!v.fails() && !v.qualifies(), "{v:?}");

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
        (
            "claude-background-wait",
            cases::WAITER,
            "early-turn",
            "FAIL",
        ),
        ("claude-background-wait", cases::WAITER, "same-poll", "FAIL"),
        ("claude-env-colour", "painter", "nocolour", "FAIL"),
        ("claude-env-colour", "painter", "wrongenv", "FAIL"),
        ("codex-env-colour", "painter", "nocolour", "FAIL"),
        ("codex-env-colour", "painter", "wrongenv", "FAIL"),
        ("codex-trust-off", "truster", "write-config", "FAIL"),
        ("codex-trust-off", "truster", "write-at-exit", "FAIL"),
        ("codex-trust-on", "truster", "approval", "BLOCKED"),
        ("claude-trust-off", "truster", "trust-exit", "FAIL"),
        ("claude-trust-off", "truster", "trust-unknown", "FAIL"),
        ("codex-trust-off", "truster", "trust-exit", "FAIL"),
        ("codex-trust-off", "truster", "trust-unknown", "FAIL"),
        ("codex-locked-trust", "truster", "trust-exit", "FAIL"),
        ("codex-locked-trust", "truster", "trust-unknown", "FAIL"),
    ];
    w.fake_file("stale-nonce", Some("n0123456789abcdefz"));
    let mut control = |w: &World, id: &str, what: &str, want: &'static str| {
        let got = case(w, id, true).remove(0).1;
        matrix.row(&format!("control {id} ({what})"), &got);
        check(&format!("control {id} ({what})"), id, want, Some(&got));
    };
    for (id, member, scenario, want) in controls {
        w.scenario(member, scenario);
        control(&w, id, scenario, want);
        w.scenario(member, "ok");
    }
    // A folder that turns out to be trusted already: never PASS.
    let trusted = format!("{}\n{}\n", w.trusted.display(), w.scratch.display());
    w.fake_file("trusted", Some(&trusted));
    control(&w, "claude-trust-off", "already trusted", "BLOCKED");
    control(&w, "codex-trust-off", "already trusted", "BLOCKED");
    w.fake_file("trusted", Some(&format!("{}\n", w.trusted.display())));
    // A hung CLI: preflight's limit makes its cases BLOCKED.
    w.fake_file("hang-version-codex", Some(""));
    w.preflight(&[Who::Codex]);
    control(&w, "codex-trust-on", "hung --version", "BLOCKED");
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

/// A timed-out call takes its whole process group with it, and a finished
/// call whose output pipes a descendant still holds returns anyway.
#[test]
fn time_limited_calls_end_their_group_and_never_wait_on_held_pipes() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let gone = |pid: &str| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < end {
            let alive = common::command("kill")
                .args(["-0", pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    };
    // Timed out: the shell and the sleep it started both end.
    let mut cmd = common::command("sh");
    cmd.arg("-c")
        .arg("sleep 60 & echo $! > \"$1\"; wait")
        .arg("sh")
        .arg(&pid_file);
    let r = world::limited(&mut cmd, std::time::Duration::from_secs(1));
    assert!(r.is_err(), "{r:?}");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    assert!(
        gone(pid.trim()),
        "the timed-out call's child {pid} still runs"
    );
    // Finished, with a background child holding stdout: bounded.
    let start = std::time::Instant::now();
    let mut cmd = common::command("sh");
    cmd.arg("-c")
        .arg("sleep 60 & echo $! > \"$1\"; echo done")
        .arg("sh")
        .arg(&pid_file);
    let out = world::limited(&mut cmd, std::time::Duration::from_secs(30)).unwrap();
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let _ = common::command("kill").arg(pid.trim()).status();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "waited {:?} on pipes a descendant held",
        start.elapsed()
    );
    assert!(out.status.success());
}
