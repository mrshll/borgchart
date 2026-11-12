//! The guarded wake's input-box matrix against the real CLIs (formerly
//! `tests/qualify.rs`): how `wake` reads and treats their input boxes.
//! Typing without submitting costs nothing; each vendor gets two real
//! submissions of the reminder (vim insert mode, with clients attached and a
//! resize during the attempt; and vim normal mode). Approval prompts are
//! never answered: one that appears is recorded and left.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use borgchart::composer::{self, Reading};
use borgchart::lifecycle::PaneIdentity;
use borgchart::rules;
use borgchart::state::StateDir;
use borgchart::team::Vendor;
use borgchart::tmux::{self, Tmux};
use borgchart::wake::{self, Defer, LaunchFacts, Qualification, REMINDER, Snapshot, WakeResult};

use super::common;

const SESSION: &str = "brg-qualify";
const WINDOW: &str = "member";

/// Where the matrix works: `work` is inside a folder Claude already trusts
/// (Codex gets session-only trust for it); screens and state are kept
/// beside the run's matrix.
pub struct Dirs {
    pub work: PathBuf,
    pub keep: PathBuf,
    /// The whole matrix's limit, for each vendor's watchdog.
    pub limit: Duration,
}

static DIRS: OnceLock<Dirs> = OnceLock::new();

pub fn set_dirs(d: Dirs) {
    let _ = DIRS.set(d);
}

fn dirs() -> &'static Dirs {
    DIRS.get().expect("wake_matrix::set_dirs first")
}

fn work() -> PathBuf {
    dirs().work.clone()
}

fn root() -> PathBuf {
    dirs().keep.clone()
}

fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    // Your own configuration homes, in place (nothing is copied).
    for key in [
        "HOME",
        "USER",
        "LANG",
        "PATH",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.insert(key.to_string(), v);
        }
    }
    env.insert("TERM".into(), "xterm-256color".into());
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env
}

/// A direct vendor has no brg member identity. Its brg commands must see
/// only an empty, owned registry/config, never discover a real team.
fn direct_run_env(
    private: &common::PrivateTmux,
    socket: &str,
    keep: &std::path::Path,
    mut env: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    env.retain(|key, _| {
        !key.starts_with("BORGCHART_")
            && !matches!(key.as_str(), "TMUX" | "TMUX_PANE" | "TMUX_TMPDIR")
    });
    let config = keep.join("isolated-brg.yaml");
    let state = keep.join("isolated-brg-state");
    let xdg_state = keep.join("xdg-state");
    std::fs::create_dir_all(&xdg_state).unwrap();
    env.insert("XDG_STATE_HOME".into(), xdg_state.display().to_string());
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(&config, "schema: 1\ntemplates: []\nauto_trust: false\n").unwrap();
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_brg"));
    let path = format!(
        "{}:{}",
        bin.parent().unwrap().display(),
        env.get("PATH").cloned().unwrap_or_default()
    );
    env.insert("PATH".into(), private.path_with(&path));
    env.insert("BORGCHART_BIN".into(), bin.display().to_string());
    env.insert("BORGCHART_CONFIG".into(), config.display().to_string());
    env.insert("BORGCHART_STATE_HOME".into(), state.display().to_string());
    env.insert(
        "BORGCHART_TMUX_TMPDIR".into(),
        private.dir().display().to_string(),
    );
    env.insert("BORGCHART_TMUX_SOCKET".into(), socket.into());
    write_context_probe(keep, &env);
    env
}

/// Run by the real vendor tool shell. Never repairs missing context or invokes
/// brg until every check succeeds; diagnostics contain key names, not values.
fn write_context_probe(keep: &std::path::Path, env: &BTreeMap<String, String>) {
    let quote = borgchart::state::shell_quote;
    let mut script = String::from("#!/bin/sh\nset -eu\n");
    for key in [
        "BORGCHART_BIN",
        "BORGCHART_CONFIG",
        "BORGCHART_STATE_HOME",
        "BORGCHART_TMUX_SOCKET",
        "BORGCHART_TMUX_TMPDIR",
        "XDG_STATE_HOME",
    ] {
        writeln!(
            script,
            "[ \"${{{key}-}}\" = {} ] || {{ echo BLOCKED_context_{key}; exit 78; }}",
            quote(&env[key])
        )
        .unwrap();
    }
    for key in [
        "BORGCHART_MEMBER",
        "BORGCHART_INSTANCE",
        "BORGCHART_STATE_DIR",
        "BORGCHART_ROLE",
        "BORGCHART_NOTES",
        "BORGCHART_INCARNATION",
    ] {
        writeln!(
            script,
            "[ \"${{{key}+present}}\" != present ] || {{ echo BLOCKED_identity_{key}; exit 78; }}"
        )
        .unwrap();
    }
    // Preserve the PATH-resolution probe and its refusal tests. The separate
    // absolute probe never resolves or executes an installed brg.
    let mut absolute = script.clone();
    let bin = &env["BORGCHART_BIN"];
    assert!(std::path::Path::new(bin).is_absolute());
    // Both generation and verification use the same absolute system utility;
    // macOS ships shasum rather than GNU sha256sum. Never resolve it via PATH.
    let hash_command: &[&str] = if cfg!(target_os = "macos") {
        &["/usr/bin/shasum", "-a", "256"]
    } else {
        &["/usr/bin/sha256sum"]
    };
    let digest = Command::new(hash_command[0])
        .args(&hash_command[1..])
        .arg(bin)
        .output()
        .unwrap();
    assert!(digest.status.success());
    let digest = String::from_utf8(digest.stdout).unwrap();
    writeln!(
        absolute,
        "[ -f {0} ] && [ -x {0} ] && [ ! -L {0} ] || {{ echo BLOCKED_absolute_binary; exit 78; }}",
        quote(bin)
    )
    .unwrap();
    writeln!(
        absolute,
        "[ \"$({} {})\" = {} ] || {{ echo BLOCKED_absolute_digest; exit 78; }}",
        hash_command
            .iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" "),
        quote(bin),
        quote(digest.trim_end())
    )
    .unwrap();
    writeln!(
        absolute,
        "echo OWNED_ABSOLUTE_CONTEXT_VERIFIED\n{} status --json",
        quote(bin)
    )
    .unwrap();
    std::fs::write(keep.join("absolute-context-probe.sh"), absolute).unwrap();
    writeln!(
        script,
        "[ \"$(command -v brg)\" = {} ] || {{ echo BLOCKED_binary_resolution; exit 78; }}",
        quote(&env["BORGCHART_BIN"])
    )
    .unwrap();
    writeln!(
        script,
        "echo OWNED_CONTEXT_VERIFIED\n{} status --json",
        quote(&env["BORGCHART_BIN"])
    )
    .unwrap();
    std::fs::write(keep.join("context-probe.sh"), script).unwrap();
}

#[test]
fn direct_vendor_brg_cannot_fall_back_to_inherited_defaults() {
    use std::os::unix::fs::PermissionsExt;
    let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&parent).unwrap();
    let keep = tempfile::tempdir_in(parent).unwrap();
    let private = common::PrivateTmux::new();
    let socket = private.socket("context-proof");
    let poison = keep.path().join("poison");
    let home = poison.join("home");
    let default_state = home.join(".local/state/borgchart");
    let inherited_state = poison.join("inherited-state");
    let poisoned_xdg = poison.join("xdg-state");
    let xdg_registry = poisoned_xdg.join("borgchart");
    let fake_bin = poison.join("bin");
    for path in [&default_state, &inherited_state, &xdg_registry, &fake_bin] {
        std::fs::create_dir_all(path).unwrap();
    }
    for state in [&default_state, &inherited_state, &xdg_registry] {
        std::fs::write(
            state.join("registry.json"),
            "INVALID_DEFAULT_REGISTRY_MUST_NOT_BE_READ",
        )
        .unwrap();
    }
    let fake_brg = fake_bin.join("brg");
    std::fs::write(&fake_brg, "#!/bin/sh\necho WRONG_DEFAULT_BINARY\nexit 99\n").unwrap();
    std::fs::set_permissions(&fake_brg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bad_config = poison.join("invalid.yaml");
    std::fs::write(&bad_config, "INVALID_DEFAULT_CONFIG").unwrap();
    let inherited = BTreeMap::from([
        ("HOME".into(), home.display().to_string()),
        ("XDG_STATE_HOME".into(), poisoned_xdg.display().to_string()),
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin", fake_bin.display()),
        ),
        ("BORGCHART_CONFIG".into(), bad_config.display().to_string()),
        (
            "BORGCHART_STATE_HOME".into(),
            inherited_state.display().to_string(),
        ),
        ("BORGCHART_TMUX_SOCKET".into(), "borgchart".into()),
        ("BORGCHART_TMUX_TMPDIR".into(), poison.display().to_string()),
        ("BORGCHART_MEMBER".into(), "foreign-member".into()),
        ("BORGCHART_INSTANCE".into(), "foreign-instance".into()),
        ("BORGCHART_BIN".into(), fake_brg.display().to_string()),
        ("TMUX".into(), "/tmp/tmux-1000/borgchart,1,0".into()),
    ]);
    let context = keep.path().join("owned");
    let env = direct_run_env(&private, &socket, &context, inherited);
    assert_eq!(env["HOME"], home.display().to_string());
    assert_eq!(
        env["XDG_STATE_HOME"],
        context.join("xdg-state").display().to_string()
    );
    let probe = context.join("context-probe.sh");
    for key in [
        "BORGCHART_BIN",
        "BORGCHART_CONFIG",
        "BORGCHART_STATE_HOME",
        "BORGCHART_TMUX_SOCKET",
        "BORGCHART_TMUX_TMPDIR",
        "XDG_STATE_HOME",
        "PATH",
    ] {
        for foreign in [false, true] {
            let mut bad = env.clone();
            if foreign {
                bad.insert(key.into(), "/nonexistent/foreign-context".into());
            } else {
                bad.remove(key);
            }
            let output = Command::new("/bin/sh")
                .arg(&probe)
                .env_clear()
                .envs(bad)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(78), "{key}: {output:?}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains("OWNED_CONTEXT_VERIFIED"));
        }
    }
    let absolute_probe = context.join("absolute-context-probe.sh");
    for key in [
        "BORGCHART_BIN",
        "BORGCHART_CONFIG",
        "BORGCHART_STATE_HOME",
        "BORGCHART_TMUX_SOCKET",
        "BORGCHART_TMUX_TMPDIR",
        "XDG_STATE_HOME",
    ] {
        for foreign in [false, true] {
            let mut bad = env.clone();
            if foreign {
                bad.insert(key.into(), "/nonexistent/foreign-context".into());
            } else {
                bad.remove(key);
            }
            let output = Command::new("/bin/sh")
                .arg(&absolute_probe)
                .env_clear()
                .envs(bad)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(78), "{key}: {output:?}");
            assert!(
                !String::from_utf8_lossy(&output.stdout)
                    .contains("OWNED_ABSOLUTE_CONTEXT_VERIFIED")
            );
        }
    }
    // An unusable PATH cannot select the poison binary or prevent absolute execution.
    let mut no_lookup = env.clone();
    no_lookup.insert("PATH".into(), fake_bin.display().to_string());
    let output = Command::new("/bin/sh")
        .arg(&absolute_probe)
        .env_clear()
        .envs(no_lookup)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "OWNED_ABSOLUTE_CONTEXT_VERIFIED\n[]\n"
    );
    // A replaced executable must be refused before it can run, even when all
    // owned context values match and PATH cannot supply a hashing utility.
    let changed_context = keep.path().join("changed-binary");
    std::fs::create_dir_all(&changed_context).unwrap();
    let changed_bin = changed_context.join("brg");
    std::fs::write(&changed_bin, "#!/bin/sh\necho ORIGINAL_BINARY\n").unwrap();
    std::fs::set_permissions(&changed_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut changed_env = env.clone();
    changed_env.insert("BORGCHART_BIN".into(), changed_bin.display().to_string());
    changed_env.insert("PATH".into(), fake_bin.display().to_string());
    write_context_probe(&changed_context, &changed_env);
    std::fs::write(&changed_bin, "#!/bin/sh\necho REPLACED_BINARY\n").unwrap();
    let refused = Command::new("/bin/sh")
        .arg(changed_context.join("absolute-context-probe.sh"))
        .env_clear()
        .envs(changed_env)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(78), "{refused:?}");
    assert_eq!(
        String::from_utf8_lossy(&refused.stdout),
        "BLOCKED_absolute_digest\n"
    );
    let checked = Command::new("/bin/sh")
        .arg(&probe)
        .env_clear()
        .envs(&env)
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    assert_eq!(
        String::from_utf8_lossy(&checked.stdout),
        "OWNED_CONTEXT_VERIFIED\n[]\n"
    );
    let mut fallback = env.clone();
    fallback.remove("BORGCHART_STATE_HOME");
    let fallback_result = Command::new(env!("CARGO_BIN_EXE_brg"))
        .args(["status", "--json"])
        .env_clear()
        .envs(fallback)
        .output()
        .unwrap();
    assert!(fallback_result.status.success(), "{fallback_result:?}");
    assert_eq!(String::from_utf8_lossy(&fallback_result.stdout), "[]\n");
    let conf = keep.path().join("tmux.conf");
    std::fs::write(&conf, tmux::CONFIG).unwrap();
    let tmux = Tmux::on_socket(&socket, &conf, env);
    let script = keep.path().join("vendor.sh");
    let output = keep.path().join("result");
    std::fs::write(&script, format!("#!/bin/sh\n{{ command -v brg; brg status --json; printf 'status=%s\\n' \"$?\"; brg msg list; printf 'inbox=%s\\n' \"$?\"; printf 'socket=%s\\nstate=%s\\nmember=%s\\n' \"$BORGCHART_TMUX_SOCKET\" \"$BORGCHART_STATE_HOME\" \"${{BORGCHART_MEMBER-unset}}\"; }} >{} 2>&1\n", borgchart::state::shell_quote(&output.display().to_string()))).unwrap();
    tmux.ok(&[
        "new-session",
        "-d",
        "-s",
        "proof",
        "-c",
        keep.path().to_str().unwrap(),
        &format!(
            "sh {}",
            borgchart::state::shell_quote(&script.display().to_string())
        ),
    ])
    .unwrap();
    let start = Instant::now();
    let result = loop {
        let result = std::fs::read_to_string(&output).unwrap_or_default();
        if result.contains("member=") {
            break result;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "{result}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(result.starts_with(env!("CARGO_BIN_EXE_brg")), "{result}");
    assert!(result.contains("[]\nstatus=0"), "{result}");
    assert!(
        !result.contains("inbox=0"),
        "unconfigured direct vendor got an inbox: {result}"
    );
    assert!(result.contains("socket=context-proof\n"), "{result}");
    assert!(
        result.contains(&format!(
            "state={}\nmember=unset",
            context.join("isolated-brg-state").display()
        )),
        "{result}"
    );
    assert!(!result.contains("WRONG_DEFAULT_BINARY"), "{result}");
    assert!(private.refused().is_empty());
    for state in [&default_state, &inherited_state, &xdg_registry] {
        assert_eq!(
            std::fs::read_to_string(state.join("registry.json")).unwrap(),
            "INVALID_DEFAULT_REGISTRY_MUST_NOT_BE_READ"
        );
    }
}

struct Run {
    /// First, so its servers stop before anything else goes.
    private: common::PrivateTmux,
    vendor: Vendor,
    socket: String,
    tmux: Tmux,
    state: StateDir,
    argv: Vec<String>,
    facts: LaunchFacts,
    rows: Vec<[String; 4]>,
    qualified: bool,
}

fn show(r: &WakeResult) -> String {
    match r {
        WakeResult::Submitted => "submitted".into(),
        WakeResult::Deferred(Defer::Busy) => "deferred:busy".into(),
        WakeResult::Deferred(Defer::Draft) => "deferred:draft".into(),
        WakeResult::Deferred(Defer::Locked) => "deferred:locked".into(),
        WakeResult::Deferred(Defer::Uncertain(w)) => format!("deferred:uncertain ({w})"),
        WakeResult::Blocked(rule) => format!("blocked:{rule}"),
        WakeResult::HumanRequired(w) => format!("human-required ({w})"),
        WakeResult::AlertOnly(w) => format!("alert-only ({w})"),
        WakeResult::NotRunning => "not-running".into(),
    }
}

/// Context-only precondition: no wake reminder or inbox command. Actual tool
/// output must be reviewed from this owned session before any later wake run.
pub fn direct_context_probe(vendor: Vendor, version: String) {
    let run = Run::new(vendor, Some(version));
    std::fs::copy(
        root().join("absolute-context-probe.sh"),
        work().join("context-probe.sh"),
    )
    .unwrap();
    run.start(200, 60);
    let rules = rules::load(vendor).unwrap();
    let review = |stage: &str| {
        run.save(stage);
        println!(
            "Review {} before creating {}",
            stage,
            root().join(format!("{stage}.reviewed")).display()
        );
        let start = Instant::now();
        while !root().join(format!("{stage}.reviewed")).exists() {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "BLOCKED: private screen review missing"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    review("context-idle");
    let s = run.snap();
    assert_eq!(
        rules
            .read(&s.title, &composer::plain(&composer::parse(&s.screen)))
            .state,
        rules::State::Idle
    );
    assert_eq!(run.reading(), Reading::Empty);
    assert!(!s.in_mode);
    let prompt = "Run /bin/sh ./context-probe.sh once. Report output; stop if blocked. No other commands, retries, edits or environment changes.";
    run.paste(prompt);
    review("context-before-enter");
    let s = run.snap();
    let state = rules
        .read(&s.title, &composer::plain(&composer::parse(&s.screen)))
        .state;
    let view = composer::view(vendor, &s.screen, Some(s.geometry)).unwrap();
    let exact = view.holds(prompt);
    std::fs::write(root().join("context-enter-guards.txt"), format!(
        "state={state:?}\nnot_blocked={}\nnot_copy_mode={}\nnot_busy={}\nexact={exact}\ngeometry={:?}\nview={view:?}\n",
        state != rules::State::Blocked, !s.in_mode, !view.busy, s.geometry
    )).unwrap();
    assert_ne!(state, rules::State::Blocked);
    assert!(
        !s.in_mode && !view.busy && exact,
        "BLOCKED: initial composer guard"
    );
    run.keys(&["Enter"]);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        std::thread::sleep(Duration::from_secs(1));
        run.save("context-observation");
        let s = run.snap();
        let plain = composer::plain(&composer::parse(&s.screen));
        assert_ne!(
            rules.read(&s.title, &plain).state,
            rules::State::Blocked,
            "BLOCKED: approval; no further keys"
        );
        assert!(
            !plain.contains("BLOCKED_context_")
                && !plain.contains("BLOCKED_identity_")
                && !plain.contains("BLOCKED_binary_resolution")
                && !plain.contains("BLOCKED_absolute_"),
            "BLOCKED: tool context mismatch"
        );
        if start.elapsed() > Duration::from_secs(10)
            && wake::before_paste(vendor, &rules, &s).is_none()
        {
            println!(
                "Context turn ended; owned session tool output still requires verification. No wake submitted."
            );
            return;
        }
    }
    panic!("BLOCKED: context probe completion timeout; no wake submitted");
}

/// A bounded observation, not a full send qualification. Stop at any gate;
/// keep timestamped screens so receipt and original completion can be reviewed.
pub fn send_midturn_probe(vendor: Vendor, version: String) {
    let run = Run::new(vendor, Some(version));
    std::fs::write(root().join("send-facts.txt"), format!("{:#?}\n", run.facts)).unwrap();
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.start(200, 60)));
    run.save("send-start");
    assert!(started.is_ok(), "BLOCKED: startup; see send-start screen");
    let rules = rules::load(vendor).unwrap();
    let review = |stage: &str| {
        run.save(stage);
        println!(
            "Review {} before creating {}",
            stage,
            root().join(format!("{stage}.reviewed")).display()
        );
        let start = Instant::now();
        while !root().join(format!("{stage}.reviewed")).exists() {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "BLOCKED: private screen review missing"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    review("send-idle-review");
    let s = run.snap();
    assert!(!s.in_mode);
    assert_eq!(
        rules
            .read(&s.title, &composer::plain(&composer::parse(&s.screen)))
            .state,
        rules::State::Idle,
        "BLOCKED: startup not positively idle"
    );
    assert_eq!(
        run.reading(),
        Reading::Empty,
        "BLOCKED: startup composer not empty"
    );
    let view = composer::view(vendor, &s.screen, Some(s.geometry)).unwrap();
    std::fs::write(
        root().join("send-mode.txt"),
        format!(
            "configured={:?}, observed={:?}\n",
            run.facts.editor, view.vim
        ),
    )
    .unwrap();
    let nonce = super::world::nonce();
    let completion = format!("ORIGINAL_DONE_{nonce}");
    let receipt = format!("RECEIVED_{nonce}");
    let prompt = match vendor {
        Vendor::Codex => format!(
            "Isolated input delivery test. Do not read files, use network, or change files. Run `sleep 60`, wait for completion, then include {completion} in your reply. Also act on any tagged input received during this turn."
        ),
        Vendor::Claude => {
            std::fs::create_dir_all(work().join("fixtures")).unwrap();
            for n in 1..=96 {
                std::fs::write(
                    work().join(format!("fixtures/fixture-{n:02}.txt")),
                    format!("Synthetic test fixture {n}.\nValue: {n}\n"),
                )
                .unwrap();
            }
            format!(
                "Read only the 96 synthetic files fixtures/fixture-01.txt through fixtures/fixture-96.txt, in order using individual Read tool calls. Sum their Value fields and report the total plus {completion}. No shell, network, other files or edits. Also act on tagged input during this task."
            )
        }
    };
    let payload = format!(
        "[brg test message {nonce} from external:live-probe]\nDuring this current turn, include {receipt} in your next reply alongside the original completion marker.\n[/brg test message {nonce}]"
    );
    std::fs::write(
        root().join("send-inputs.txt"),
        format!("INITIAL\n{prompt}\nPAYLOAD\n{payload}\n"),
    )
    .unwrap();
    run.paste(&prompt);
    review("send-initial-enter-review");
    let s = run.snap();
    assert_ne!(
        rules
            .read(&s.title, &composer::plain(&composer::parse(&s.screen)))
            .state,
        rules::State::Blocked,
        "BLOCKED: dialog before initial Enter"
    );
    assert!(!s.in_mode);
    assert!(
        !composer::view(vendor, &s.screen, Some(s.geometry))
            .unwrap()
            .busy,
        "BLOCKED: turn started before initial Enter"
    );
    assert!(
        composer::view(vendor, &s.screen, Some(s.geometry))
            .unwrap()
            .holds(&prompt),
        "BLOCKED: initial text not exact"
    );
    run.keys(&["Enter"]);
    let start = Instant::now();
    let mut pasted = false;
    let fixture_read =
        regex::Regex::new(r"(?mi)(?:read(?:ing)? [0-9]+ files?|Read\(.*fixture-)").unwrap();
    while start.elapsed() < Duration::from_secs(100) {
        let s = run.snap();
        let reading = rules.read(&s.title, &composer::plain(&composer::parse(&s.screen)));
        let elapsed = start.elapsed().as_millis();
        std::fs::write(
            root()
                .join("screens")
                .join(format!("{}-send-{elapsed:06}.ansi", vendor.as_str())),
            format!(
                "UTC: {}\nTitle: {}\nState: {:?}\n{}",
                borgchart::bus::time::utc_now(),
                s.title,
                reading,
                s.screen
            ),
        )
        .unwrap();
        assert_ne!(
            reading.state,
            rules::State::Blocked,
            "BLOCKED: dialog left unanswered"
        );
        assert!(
            !s.screen.contains("You interrupted"),
            "FAIL: active turn interrupted"
        );
        let active_task = || match vendor {
            Vendor::Codex => super::world::tree(run.pane().pane_pid)
                .iter()
                .any(|p| p.argv == ["sleep", "60"]),
            Vendor::Claude => {
                let plain = composer::plain(&composer::parse(&run.screen()));
                fixture_read.is_match(&plain)
            }
        };
        if !pasted && reading.state == rules::State::Working && active_task() {
            review("send-working-review");
            run.save("send-working-rechecked");
            let s = run.snap();
            assert!(!s.in_mode);
            assert_eq!(
                rules
                    .read(&s.title, &composer::plain(&composer::parse(&s.screen)))
                    .state,
                rules::State::Working,
                "BLOCKED: not working before paste"
            );
            assert!(
                vendor == Vendor::Claude || active_task(),
                "BLOCKED: active task evidence missing before paste"
            );
            run.paste(&payload);
            review("send-before-enter");
            let s = run.snap();
            assert!(!s.in_mode);
            assert_eq!(
                rules
                    .read(&s.title, &composer::plain(&composer::parse(&s.screen)))
                    .state,
                rules::State::Working,
                "BLOCKED: state changed before Enter"
            );
            assert!(
                vendor == Vendor::Claude || active_task(),
                "BLOCKED: active task evidence missing before Enter"
            );
            // holds() deliberately rejects busy composers and hard newlines.
            // This bounded payload has three short, unwrapped lines; compare
            // those directly while separately requiring Working above.
            let view = composer::view(vendor, &s.screen, Some(s.geometry)).unwrap();
            assert!(!view.placeholder);
            assert_eq!(
                view.lines.join("\n"),
                payload,
                "BLOCKED: mid-turn text not exact"
            );
            run.keys(&["Enter"]);
            pasted = true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(pasted, "BLOCKED: no observable active task");
    println!(
        "PARTIAL: probe captured; manually verify original completion, tagged receipt and steering; never a full gate pass"
    );
}

fn show_reading(r: &Reading) -> String {
    match r {
        Reading::Empty => "empty".into(),
        Reading::Draft => "draft".into(),
        Reading::Busy => "busy".into(),
        Reading::Unknown(w) => format!("unknown ({w})"),
    }
}

/// Paste only: large-message placeholders must not be mistaken for exact
/// payload verification. No Enter is sent by this observation case.
pub fn near_limit_probe(vendor: Vendor, version: String) {
    let run = Run::new(vendor, Some(version));
    run.start(200, 60);
    let rules = rules::load(vendor).unwrap();
    let before = run.snap();
    run.save("near-limit-before");
    assert!(
        wake::before_paste(vendor, &rules, &before).is_none(),
        "BLOCKED: not positively empty/idle"
    );
    let line = "Synthetic payload line; harmless fixture data only. 0123456789abcdef\n";
    let mut body = line.repeat(300);
    body.truncate(16 * 1024);
    let text = format!(
        "[brg test message near-limit from external:live-probe]\n{body}\n[/brg test message near-limit]"
    );
    std::fs::write(root().join("near-limit-input.txt"), &text).unwrap();
    run.paste(&text);
    run.save("near-limit-pasted-no-enter");
    let s = run.snap();
    let view = composer::view(vendor, &s.screen, Some(s.geometry));
    let detail = format!(
        "body_bytes={} envelope_bytes={}\nlaunch={:?}\nreading={:?}\nview={view:?}\nNO ENTER SENT; no delivery claim\n",
        body.len(),
        text.len(),
        run.facts,
        run.reading()
    );
    std::fs::write(root().join("near-limit-result.txt"), &detail).unwrap();
    println!(
        "{}: 16384-byte body pasted, screen captured, no Enter; exact-content verification and delivery remain unqualified",
        vendor.as_str()
    );
}

impl Run {
    fn new(vendor: Vendor, version: Option<String>) -> Run {
        let private = common::PrivateTmux::with_watchdog(dirs().limit);
        let socket = private.socket(&format!("brg-qualify-{}", vendor.as_str()));
        std::fs::create_dir_all(work()).unwrap();
        std::fs::create_dir_all(root().join("screens")).unwrap();
        let conf = root().join(format!("tmux-{}.conf", vendor.as_str()));
        std::fs::write(&conf, tmux::CONFIG).unwrap();
        let tmux_env = direct_run_env(&private, &socket, &root(), base_env());
        let tmux = Tmux::on_socket(&socket, &conf, tmux_env);
        let argv: Vec<String> = match vendor {
            Vendor::Claude => vec!["claude".into()],
            // The update check would show a startup dialog; brg answers none.
            // Trust for the work folder is for this session only, as
            // `auto_trust` passes it: nothing is written to your config.
            Vendor::Codex => vec![
                "codex".into(),
                "--sandbox".into(),
                "danger-full-access".into(),
                "--ask-for-approval".into(),
                "never".into(),
                "-c".into(),
                "check_for_update_on_startup=false".into(),
                "-c".into(),
                borgchart::launch::codex_trust_config(&work()),
            ],
        };
        let facts = LaunchFacts::measure(vendor, version, &argv, &work(), &base_env());
        Run {
            private,
            vendor,
            state: StateDir::from_root(&root().join(format!("state-{}", vendor.as_str()))),
            socket,
            tmux,
            argv,
            facts,
            rows: Vec::new(),
            qualified: false,
        }
    }

    /// A tmux client for `socket` in the private directory, with the
    /// vendors' environment.
    fn client(&self, socket: &str) -> Command {
        let mut cmd = self.private.tmux(socket);
        cmd.env_clear()
            .envs(base_env())
            .env("TMUX_TMPDIR", self.private.dir());
        cmd
    }

    /// (Re)start the vendor in a fresh pane and wait for its empty input box.
    fn start(&self, cols: u32, rows: u32) {
        let _ = self.tmux.run(&["kill-server"]);
        std::thread::sleep(Duration::from_millis(500));
        let cmd = self
            .argv
            .iter()
            .map(|a| borgchart::state::shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let cwd = work().display().to_string();
        let (c, r) = (cols.to_string(), rows.to_string());
        let args = [
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-n",
            WINDOW,
            "-x",
            &c,
            "-y",
            &r,
            "-c",
            &cwd,
            &cmd,
        ];
        if let Err(e) = self.tmux.ok(&args) {
            eprintln!("retrying new-session after: {e}");
            std::thread::sleep(Duration::from_secs(1));
            self.tmux.ok(&args).unwrap();
        }
        let start = Instant::now();
        let startup_rules = rules::load(self.vendor).unwrap();
        while start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(500));
            if wake::before_paste(self.vendor, &startup_rules, &self.snap()).is_none() {
                std::thread::sleep(Duration::from_millis(1500));
                if wake::before_paste(self.vendor, &startup_rules, &self.snap()).is_none() {
                    return;
                }
            }
        }
        panic!(
            "{} did not reach an empty input box:\n{}",
            self.vendor.as_str(),
            self.screen()
        );
    }

    fn target(&self) -> String {
        format!("={SESSION}:={WINDOW}")
    }

    fn pane(&self) -> PaneIdentity {
        let out = self
            .tmux
            .ok(&[
                "display-message",
                "-p",
                "-t",
                &self.target(),
                "#{pane_id} #{pane_pid}",
            ])
            .unwrap();
        let (id, pid) = out.trim().split_once(' ').unwrap();
        PaneIdentity {
            socket: self.socket.clone(),
            session: SESSION.into(),
            window: WINDOW.into(),
            pane_id: id.into(),
            pane_pid: pid.parse().unwrap(),
        }
    }

    fn screen(&self) -> String {
        self.tmux
            .ok(&["capture-pane", "-p", "-e", "-t", &self.target()])
            .unwrap_or_default()
    }

    fn snap(&self) -> Snapshot {
        wake::snapshot(&self.tmux, &self.pane()).expect("the pane is gone")
    }

    fn reading(&self) -> Reading {
        let s = self.snap();
        composer::read(self.vendor, &s.screen, Some(s.geometry))
    }

    fn save(&self, case: &str) {
        let path = root()
            .join("screens")
            .join(format!("{}-{case}.ansi", self.vendor.as_str()));
        let _ = std::fs::write(path, self.screen());
    }

    fn keys(&self, args: &[&str]) {
        let target = self.target();
        let mut a = vec!["send-keys", "-t", target.as_str()];
        a.extend_from_slice(args);
        self.tmux.ok(&a).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }

    fn newline(&self) {
        match self.vendor {
            Vendor::Claude => self.keys(&["S-Enter"]),
            Vendor::Codex => self.keys(&["C-j"]),
        }
    }

    fn attempt(&self) -> (WakeResult, u32) {
        let mut calls = 0;
        let r = wake::attempt(
            &self.tmux,
            &self.state,
            WINDOW,
            &self.pane(),
            self.vendor,
            &self.facts,
            &mut || {
                calls += 1;
                Ok(())
            },
        );
        (r, calls)
    }

    fn record(&mut self, case: &str, want: &str, got: String) {
        self.record_checked(case, want, got, &[]);
    }

    /// Record a row that also fails if any invariant is false.
    fn record_checked(&mut self, case: &str, want: &str, got: String, invariants: &[(&str, bool)]) {
        let got = invariants
            .iter()
            .filter(|(_, held)| !held)
            .fold(got, |g, (what, _)| format!("{g} ({what} violated)"));
        let ok = if evaluate(want, &got, invariants) {
            "ok"
        } else {
            "FAIL"
        };
        println!(
            "| {} | {case} | {want} | {got} | {ok} |",
            self.vendor.as_str()
        );
        self.rows.push([case.into(), want.into(), got, ok.into()]);
        if std::env::var("BORGCHART_LIVE_STOP_ON_FAILURE").as_deref() == Ok("1") {
            std::fs::write(
                root().join("composer-matrix-progress.md"),
                table(self.vendor, &self.rows),
            )
            .unwrap();
            assert_ne!(ok, "FAIL", "live composer case failed: {case}");
        }
    }

    /// An attempt on a draft must defer and leave the draft as it was.
    fn draft_case(&mut self, case: &str) {
        std::thread::sleep(Duration::from_millis(500));
        let before = self.snap();
        let (r, calls) = if self.qualified {
            self.attempt()
        } else {
            let rules = rules::load(self.vendor).unwrap();
            let r = wake::before_paste(self.vendor, &rules, &before);
            (r.unwrap_or(WakeResult::Submitted), 0)
        };
        std::thread::sleep(Duration::from_millis(300));
        let after = self.snap();
        let lines = |s: &Snapshot| {
            composer::view(self.vendor, &s.screen, None)
                .ok()
                .map(|v| v.lines)
        };
        let untouched = lines(&before) == lines(&after)
            && (before.geometry.cursor_x, before.geometry.cursor_y)
                == (after.geometry.cursor_x, after.geometry.cursor_y);
        self.save(case);
        self.record_checked(
            case,
            "deferred:draft",
            show(&r),
            &[
                ("no intent recorded", calls == 0),
                ("input box untouched", untouched),
            ],
        );
    }

    /// A whitespace draft may read as a draft or as uncertain, never as empty.
    fn draft_or_uncertain(&mut self, case: &str) {
        std::thread::sleep(Duration::from_millis(500));
        let rules = rules::load(self.vendor).unwrap();
        let s = self.snap();
        self.save(case);
        let got = match wake::before_paste(self.vendor, &rules, &s) {
            None => "would paste".to_string(),
            Some(r) => show(&r),
        };
        self.record(case, "deferred:draft|deferred:uncertain", got);
    }

    /// Paste text by hand (bracketed), the way the wake does, without Enter.
    fn paste(&self, text: &str) {
        self.tmux
            .run_with_stdin(&["load-buffer", "-b", "qualify", "-"], text.as_bytes())
            .unwrap();
        self.tmux
            .ok(&[
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                "qualify",
                "-t",
                &self.target(),
            ])
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
    }

    fn enter_case(&mut self, case: &str, want: &str) {
        std::thread::sleep(Duration::from_millis(300));
        let rules = rules::load(self.vendor).unwrap();
        let s = self.snap();
        self.save(case);
        let g = s.geometry;
        let got = match wake::before_enter(self.vendor, &rules, &s) {
            None => format!("would press Enter (cursor {},{})", g.cursor_x, g.cursor_y),
            Some(why) => format!("withheld: {why}"),
        };
        self.record(case, want, got);
    }

    /// Wait for a turn to end: an empty idle box, or a prompt (left alone).
    fn wait_turn(&self) -> String {
        let start = Instant::now();
        let rules = rules::load(self.vendor).unwrap();
        while start.elapsed() < Duration::from_secs(180) {
            std::thread::sleep(Duration::from_secs(2));
            match wake::before_paste(self.vendor, &rules, &self.snap()) {
                None => return "idle".into(),
                Some(WakeResult::Blocked(rule)) => {
                    return format!("blocked:{rule} (left unanswered)");
                }
                Some(_) => {}
            }
        }
        "still busy after 180 s".into()
    }
}

/// The matrix's rows for one vendor, and whether its wake qualified (typed
/// attempts and submissions ran).
pub fn qualify(vendor: Vendor, version: Option<String>, submit: bool) -> (Vec<[String; 4]>, bool) {
    let mut run = Run::new(vendor, version);
    println!(
        "\n{}: version {:?}, editor {:?}, suggestions {:?}, qualification {:?}",
        vendor.as_str(),
        run.facts.vendor_version,
        run.facts.editor,
        run.facts.suggestions,
        wake::qualification(vendor, &run.facts)
    );
    let qualified = wake::qualification(vendor, &run.facts) == Qualification::Qualified;
    run.qualified = qualified;
    let mut facts_row = format!("{:?}", run.facts.vendor_version.as_deref().unwrap_or("?"));
    let _ = write!(facts_row, " editor={:?}", run.facts.editor);
    run.record("launch facts", "", facts_row);
    if !qualified {
        // Unqualified: the adapter must not type, whatever the screen shows.
        run.start(200, 50);
        let (r, _) = run.attempt();
        run.record("unqualified attempt", "alert-only", show(&r));
        // The readings below are still measured, without typing.
    }

    // Empty idle box.
    run.start(200, 50);
    run.save("empty");
    let rules = rules::load(vendor).unwrap();
    let got = match wake::before_paste(vendor, &rules, &run.snap()) {
        None => "would paste".to_string(),
        Some(r) => show(&r),
    };
    run.record("empty idle", "would paste", got);

    // Single-line draft, then vim normal mode with it.
    run.keys(&["-l", "please look at the example widget"]);
    run.draft_case("draft single-line");
    run.keys(&["Escape"]);
    run.draft_case("draft in vim normal mode");

    // Multiline.
    run.start(200, 50);
    run.keys(&["-l", "first line of a note"]);
    run.newline();
    run.keys(&["-l", "second line"]);
    run.draft_case("draft multiline");

    // A draft scrolled out of view: many lines, then blank ones at the end.
    run.start(200, 50);
    for i in 0..45 {
        run.keys(&["-l", &format!("row {i} of a long note")]);
        run.newline();
    }
    for _ in 0..25 {
        run.newline();
    }
    run.draft_case("draft offscreen (blank tail)");

    // Wrapped single line in a narrow pane.
    run.start(60, 50);
    run.keys(&[
        "-l",
        "a fairly long single line draft that will need to wrap across the narrow pane width",
    ]);
    run.draft_case("draft wrapped");

    // History recall.
    run.start(200, 50);
    run.keys(&["Up"]);
    std::thread::sleep(Duration::from_millis(500));
    let got = show_reading(&run.reading());
    run.save("history-up");
    run.record("history recall (Up)", "draft", got.clone());

    // A draft that clears: draft, then (after a person clears it) empty.
    run.start(200, 50);
    run.keys(&["-l", "temporary"]);
    run.draft_case("draft clears: first attempt");
    run.keys(&["C-u"]);
    std::thread::sleep(Duration::from_millis(500));
    let got = match wake::before_paste(vendor, &rules, &run.snap()) {
        None => "would paste".to_string(),
        Some(r) => show(&r),
    };
    run.record("draft clears: after clearing", "would paste", got);

    // Whitespace-only drafts: never read as empty.
    run.start(200, 50);
    run.keys(&["-l", "   "]);
    run.draft_or_uncertain("spaces-only draft (insert)");
    run.start(200, 50);
    run.keys(&["-l", " "]);
    run.keys(&["Escape"]);
    run.draft_or_uncertain("one space, then vim normal mode");

    // The Enter check, with the reminder pasted by hand and never submitted.
    run.start(200, 50);
    run.paste(REMINDER);
    run.enter_case("reminder pasted", "would press Enter");
    run.keys(&["-l", " "]);
    run.enter_case("reminder + a typed space", "withheld");
    run.start(200, 50);
    run.paste(&REMINDER.replace("brg msg list", "brginbox"));
    run.enter_case("reminder with a space removed", "withheld");
    run.start(60, 50);
    run.paste(REMINDER);
    run.enter_case("reminder wrapped at 60 columns", "would press Enter");

    // A persistent draft keeps deferring, with a resize and attached clients.
    run.start(200, 50);
    run.keys(&["-l", "a draft left overnight"]);
    let outer = format!("{}-outer", run.socket);
    let _ = run
        .client(&outer)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "150",
            "-y",
            "45",
        ])
        .arg(format!(
            "{} -L {} attach -t ={SESSION}",
            run.private.guard().display(),
            run.socket
        ))
        .status();
    let mut control = run
        .client(&run.socket)
        .args(["-C", "attach", "-t", &format!("={SESSION}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    for (i, size) in [("180", "48"), ("120", "40"), ("200", "50")]
        .iter()
        .enumerate()
    {
        let _ = run.tmux.ok(&[
            "resize-window",
            "-t",
            &run.target(),
            "-x",
            size.0,
            "-y",
            size.1,
        ]);
        run.draft_case(&format!("persistent draft #{} (attached, resized)", i + 1));
    }
    let clients = run
        .tmux
        .ok(&["list-clients", "-F", "#{client_control_mode}"])
        .unwrap_or_default();
    run.record(
        "clients attached during attempts",
        "2",
        clients.lines().count().to_string(),
    );

    // Too small a pane.
    let _ = run.tmux.ok(&[
        "resize-window",
        "-t",
        &run.target(),
        "-x",
        "100",
        "-y",
        "12",
    ]);
    run.keys(&["C-u"]);
    std::thread::sleep(Duration::from_millis(800));
    run.save("small");
    let r = if run.qualified {
        run.attempt().0
    } else {
        wake::before_paste(vendor, &rules, &run.snap()).unwrap_or(WakeResult::Submitted)
    };
    run.record("pane 100x12", "deferred:uncertain", show(&r));
    let _ = control.kill();
    let _ = control.wait();
    let _ = run.client(&outer).arg("kill-server").output();

    if !qualified || !submit {
        run.record("submissions", "", "skipped".into());
        return (std::mem::take(&mut run.rows), qualified);
    }

    // Submission 1: insert mode, clients attached, resize during the attempt.
    run.start(200, 50);
    let _ = run
        .client(&outer)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "150",
            "-y",
            "45",
        ])
        .arg(format!(
            "{} -L {} attach -t ={SESSION}",
            run.private.guard().display(),
            run.socket
        ))
        .status();
    let mut control = run
        .client(&run.socket)
        .args(["-C", "attach", "-t", &format!("={SESSION}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let target = run.target();
    let (r, _) = std::thread::scope(|scope| {
        let run = &run;
        scope.spawn(|| {
            for (x, y) in [("190", "50"), ("200", "48"), ("200", "50")] {
                std::thread::sleep(Duration::from_millis(150));
                let _ = run
                    .client(&run.socket)
                    .args(["resize-window", "-t", &target, "-x", x, "-y", y])
                    .output();
            }
        });
        run.attempt()
    });
    // A redraw mid-resize can make the box unreadable: that defers, safely.
    run.record(
        "attempt during a resize",
        "submitted|deferred:uncertain",
        show(&r),
    );
    let mut submitted = r == WakeResult::Submitted;
    if !submitted {
        std::thread::sleep(Duration::from_secs(1));
        let (r, _) = run.attempt();
        run.record(
            "submit: vim insert, 2 clients attached",
            "submitted",
            show(&r),
        );
        submitted = r == WakeResult::Submitted;
    }
    if submitted {
        std::thread::sleep(Duration::from_millis(800));
        run.save("working");
        let (busy, calls) = run.attempt();
        run.record(
            "attempt during the turn",
            "deferred:busy",
            format!(
                "{}{}",
                show(&busy),
                if calls > 0 { " (intent recorded!)" } else { "" }
            ),
        );
    }
    let end = run.wait_turn();
    run.save("after-turn-1");
    run.record("turn 1 ended", "idle", end);
    let _ = control.kill();
    let _ = control.wait();
    let _ = run.client(&outer).arg("kill-server").output();

    // Submission 2: vim normal mode.
    run.start(200, 50);
    run.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(500));
    let mode = composer::view(vendor, &run.screen(), None)
        .ok()
        .and_then(|v| v.vim);
    run.record("normal mode shown", "Some(Normal)", format!("{mode:?}"));
    let (r, _) = run.attempt();
    // Claude can't tell an empty box from one space in normal mode, so it defers.
    let want = match vendor {
        Vendor::Claude => "deferred:uncertain",
        Vendor::Codex => "submitted",
    };
    run.record("submit: vim normal", want, show(&r));
    let end = run.wait_turn();
    run.save("after-turn-2");
    run.record("turn 2 ended", "idle", end);
    (std::mem::take(&mut run.rows), qualified)
}

/// The rows as a Markdown table.
pub fn table(vendor: Vendor, rows: &[[String; 4]]) -> String {
    let mut md =
        String::from("| vendor | case | expected | got | ok |\n| --- | --- | --- | --- | --- |\n");
    for row in rows {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} |",
            vendor.as_str(),
            row[0],
            row[1],
            row[2],
            row[3]
        );
    }
    md
}

/// A row passes when the result has one of the `|`-separated prefixes and
/// every invariant held, whatever the result text says.
fn evaluate(want: &str, got: &str, invariants: &[(&str, bool)]) -> bool {
    want.split('|').any(|w| got.starts_with(w)) && invariants.iter().all(|(_, held)| *held)
}

#[test]
fn rows_fail_on_any_broken_invariant() {
    assert!(evaluate("deferred:draft", "deferred:draft", &[("a", true)]));
    assert!(evaluate(
        "submitted|deferred:uncertain",
        "deferred:uncertain (x)",
        &[]
    ));
    assert!(!evaluate("deferred:draft", "deferred:busy", &[]));
    assert!(!evaluate(
        "deferred:draft",
        "deferred:draft",
        &[("no intent recorded", false)]
    ));
    assert!(!evaluate(
        "deferred:draft",
        "deferred:draft",
        &[("no intent recorded", true), ("input box untouched", false)]
    ));
}
