//! One isolated cold seating check using brg's vendor launch policy.
use super::{cases, rules, world};
use sha2::{Digest, Sha256};
use std::fs;
use std::time::{Duration, Instant};

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[test]
#[ignore = "isolated real Codex cold seating check; set BORGCHART_LIVE=1"]
fn vendor_codex_seating_probe() {
    if rules::mode(
        std::env::var("BORGCHART_LIVE").ok().as_deref(),
        std::env::var("BORGCHART_LIVE_DRY").ok().as_deref(),
    ) != rules::Mode::Live
    {
        println!("SKIP cold seating: real live mode not selected");
        return;
    }
    let (vendors, _) = rules::select(std::env::var("BORGCHART_LIVE_VENDORS").ok().as_deref())
        .unwrap_or_else(|e| panic!("{e}"));
    if !vendors.contains(&rules::Who::Codex) {
        println!("SKIP cold seating: Codex not selected");
        return;
    }
    let mut w = world::World::live();
    w.limits.start = Duration::from_secs(90);
    w.preflight(&[rules::Who::Codex]);
    let version = w.versions[&rules::Who::Codex].clone().unwrap();
    let config_before = w.codex_config_hash();
    let seat_bin = w
        .brg
        .parent()
        .unwrap()
        .join("brg-seat")
        .canonicalize()
        .unwrap();
    assert_eq!(w.resolve("brg-seat"), Some(seat_bin.clone()));
    let seat_hash = format!("{:x}", Sha256::digest(fs::read(&seat_bin).unwrap()));
    fs::write(w.evidence.join("seat-binary.sha256"), &seat_hash).unwrap();
    // Owned and outside the work folder and /tmp; never the real quota cache.
    let cache = tempfile::Builder::new()
        .prefix("brg-seat-diagnostic-")
        .tempdir_in(
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/state"),
        )
        .unwrap();
    w.private.also_remove(cache.path());
    w.env
        .insert("XDG_CACHE_HOME".into(), cache.path().display().to_string());
    let xdg_state = w.evidence.join("xdg-state");
    fs::create_dir_all(&xdg_state).unwrap();
    w.private.also_remove(&xdg_state);
    w.env
        .insert("XDG_STATE_HOME".into(), xdg_state.display().to_string());
    let work = w.scratch.join("untrusted-seat");
    fs::create_dir_all(&work).unwrap();
    assert!(
        w.command("git")
            .args(["init", "--quiet"])
            .current_dir(&work)
            .status()
            .unwrap()
            .success()
    );
    let instruction = "This is an isolated diagnostic. Run `/bin/sh ./seat-probe.sh` exactly once using your normal shell tool. Do not request escalation or change environment/settings. Do not run any other command, retry, inspect files, or use brg directly. Report the command's output and stop. If blocked, report BLOCKED and stop.";
    let t = cases::Team::new(
        &w,
        "seat-diagnostic",
        &cases::Spec {
            who: rules::Who::Codex,
            member: "seatprobe",
            locked: false,
            cwd: &work,
            auto_trust: true,
            args: &[],
            instruction,
            bus: false,
        },
    );
    let yaml = fs::read_to_string(t.dir.join("template.yaml")).unwrap();
    fs::write(
        t.dir.join("template.yaml"),
        format!(
            "plugins:\n  seat:\n    actions:\n      route: brg-seat route <vendor> <model>\n{}",
            yaml.replace("    prompt:", "    plugins: {seat: [route]}\n    prompt:")
        ),
    )
    .unwrap();
    fs::write(
        t.dir.join("seatprobe.md"),
        format!("---\nmodels:\n  codex: gpt-6-astra medium\n---\n{instruction}\n"),
    )
    .unwrap();
    let state = &w.env["BORGCHART_STATE_HOME"];
    let script = format!(
        r#"#!/bin/sh
set -u
blocked() {{ echo "BLOCKED_context_$1"; exit 78; }}
[ "${{BORGCHART_BIN-}}" = {brg} ] || blocked bin
[ "${{BORGCHART_STATE_HOME-}}" = {state} ] || blocked state
[ "${{BORGCHART_CONFIG-}}" = {config} ] || blocked config
[ "${{XDG_STATE_HOME-}}" = {xdg_state} ] || blocked xdg_state
[ "${{XDG_CACHE_HOME-}}" = {cache} ] || blocked cache
[ "${{BORGCHART_TMUX_TMPDIR-}}" = {tmux_dir} ] || blocked tmux_dir
[ "${{BORGCHART_TMUX_SOCKET-}}" = {socket} ] || blocked socket
[ "${{BORGCHART_ROLE-}}" = seatprobe ] || blocked role
[ -n "${{BORGCHART_INCARNATION-}}" ] || blocked incarnation
case "${{BORGCHART_INSTANCE-}}/${{BORGCHART_MEMBER-}}" in *..*|/*|*/|*/*/*) blocked identity;; esac
shim="$BORGCHART_STATE_HOME/$BORGCHART_INSTANCE/members/$BORGCHART_MEMBER/bin/brg-seat"
[ "$(command -v brg-seat)" = "$shim" ] || blocked shim
(umask 077; : > "$XDG_CACHE_HOME/permission-probe") || blocked cache_unwritable
/bin/rm "$XDG_CACHE_HOME/permission-probe"
printf 'CONTEXT_OK\n' 
"$shim" route codex gpt-6-astra > seat-stdout.txt 2> seat-stderr.txt
code=$?
printf '%s\n' "$code" > seat-exit.txt
printf 'SEAT_EXIT=%s\n' "$code"
cat seat-stdout.txt seat-stderr.txt
"#,
        brg = quote(&w.brg.display().to_string()),
        state = quote(state),
        config = quote(&t.config.display().to_string()),
        xdg_state = quote(&xdg_state.display().to_string()),
        cache = quote(&cache.path().display().to_string()),
        tmux_dir = quote(&w.env["BORGCHART_TMUX_TMPDIR"]),
        socket = quote(&w.socket),
    );
    fs::write(work.join("seat-probe.sh"), &script).unwrap();
    fs::write(w.evidence.join("seat-probe.sh"), &script).unwrap();
    println!("seat diagnostic evidence: {}", w.evidence.display());
    fs::write(w.evidence.join("seat-version.txt"), version).unwrap();
    let up = t.up();
    fs::write(w.evidence.join("seat-create.txt"), format!("{up:?}")).unwrap();
    assert!(
        matches!(up, Ok((true, _))),
        "isolated create failed; see evidence"
    );
    let meta: serde_json::Value = serde_json::from_slice(
        &fs::read(
            std::path::Path::new(state)
                .join(t.instance())
                .join("start.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        meta["base_env"]["XDG_CACHE_HOME"],
        cache.path().display().to_string()
    );
    assert_eq!(
        meta["base_env"]["XDG_STATE_HOME"],
        xdg_state.display().to_string()
    );
    let pane = t.pane_pid().expect("owned member pane");
    let processes = world::vendor_procs(pane, rules::Who::Codex);
    assert!(processes.iter().any(|p| {
        p.argv
            .windows(2)
            .any(|w| w == ["--sandbox", "danger-full-access"])
            && p.argv
                .windows(2)
                .any(|w| w == ["--ask-for-approval", "never"])
    }));
    let end = Instant::now() + Duration::from_secs(90);
    let outcome = loop {
        if work.join("seat-exit.txt").exists() {
            for file in ["seat-exit.txt", "seat-stdout.txt", "seat-stderr.txt"] {
                fs::copy(work.join(file), w.evidence.join(file)).unwrap();
            }
            break "recorded".to_string();
        }
        let (state, rule) = t.state();
        if state == "blocked" || Instant::now() >= end {
            break format!("BLOCKED {state} {rule}");
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    t.save("seat-final");
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&seat_bin).unwrap())),
        seat_hash
    );
    drop(t);
    let config_after = w.codex_config_hash();
    fs::write(w.evidence.join("seat-outcome.txt"), &outcome).unwrap();
    fs::write(
        w.evidence.join("seat-config-hashes.txt"),
        format!("before={config_before}\nafter={config_after}\n"),
    )
    .unwrap();
    assert_eq!(config_after, config_before, "Codex config changed");
    println!("seat diagnostic: {outcome}");
    assert_eq!(outcome, "recorded", "no retry or approval permitted");
    let exit = fs::read_to_string(w.evidence.join("seat-exit.txt")).unwrap();
    let cache_root = cache.path().join("borgchart/seat-v2");
    let mut valid = 0;
    for entry in fs::read_dir(&cache_root)
        .expect("global cache created")
        .map(Result::unwrap)
    {
        if entry.path().extension().is_some_and(|e| e == "json") {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
            if value["status"] == "ok" {
                valid += 1;
            }
        }
    }
    fs::write(
        w.evidence.join("seat-global-cache.txt"),
        format!(
            "valid_observations={valid}\nroute_exit={}\nauto_trust=true\n",
            exit.trim()
        ),
    )
    .unwrap();
    assert_eq!(
        valid, 1,
        "no valid cold quota observation; no retry permitted"
    );
    assert!(matches!(exit.trim(), "0" | "3"));
}
