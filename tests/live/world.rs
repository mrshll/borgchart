//! What a live (or dry) run works in: the environment the CLIs get, the
//! folders, time limits, preflight, and the helpers the cases share
//! (time-limited calls, teams started through real `brg up`, process
//! facts from `/proc`).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::common;
use super::rules::{Mode, Who};

/// Every time limit the run uses.
#[derive(Clone, Copy)]
pub struct Limits {
    /// The whole run, for the watchdog.
    pub run: Duration,
    /// One `--version`.
    pub version: Duration,
    /// One `brg` call other than `up`.
    pub brg: Duration,
    pub up: Duration,
    /// For a member to reach idle (or a screen) after `up`.
    pub start: Duration,
    /// How long a screen left unanswered must stay.
    pub hold: Duration,
    /// For the background wait to be armed with the member idle.
    pub arm: Duration,
    /// For the reply after the nonce is sent.
    pub reply: Duration,
    /// `BORGCHART_GATE_WAIT_MS` for `up`.
    pub gate_ms: u64,
}

impl Limits {
    pub fn for_mode(mode: Mode) -> Limits {
        match mode {
            Mode::Dry => Limits {
                run: Duration::from_secs(600),
                version: Duration::from_secs(3),
                brg: Duration::from_secs(20),
                up: Duration::from_secs(40),
                start: Duration::from_secs(15),
                hold: Duration::from_secs(2),
                arm: Duration::from_secs(8),
                reply: Duration::from_secs(8),
                gate_ms: 4000,
            },
            _ => Limits {
                run: Duration::from_secs(60 * 60),
                version: Duration::from_secs(20),
                brg: Duration::from_secs(60),
                up: Duration::from_secs(180),
                start: Duration::from_secs(120),
                hold: Duration::from_secs(8),
                arm: Duration::from_secs(240),
                reply: Duration::from_secs(240),
                gate_ms: 15000,
            },
        }
    }
}

pub struct World {
    pub mode: Mode,
    pub limits: Limits,
    /// First, so its servers stop (and its paths go) before anything else.
    pub private: common::PrivateTmux,
    pub socket: String,
    /// The environment every CLI, brg and member gets (`PATH` with the
    /// guard first). Nothing else is passed.
    pub env: BTreeMap<String, String>,
    /// Where the matrix and screens go (kept).
    pub evidence: PathBuf,
    /// Work folders inside a folder the CLIs already trust (removed).
    pub trusted: PathBuf,
    /// Everything else: fresh untrusted folders, teams, state, the bus root
    /// (removed).
    pub scratch: PathBuf,
    /// The CLIs' versions, from preflight: `Err` is why a vendor's cases are
    /// BLOCKED.
    pub versions: BTreeMap<Who, Result<String, String>>,
    /// Dry only: the fake CLIs' directory (`$HOME/fake` controls them).
    pub fake_home: Option<PathBuf>,
    pub brg: PathBuf,
    counter: std::sync::atomic::AtomicU32,
}

/// Names copied from your environment in a live run. Your own vendor
/// configuration homes are passed through; nothing is copied.
const LIVE_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
];

const FAKE: &str = include_str!("fake_vendor.sh");

fn short_dir(prefix: &str) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::env::temp_dir())
        .unwrap()
        .keep();
    dir.canonicalize().unwrap()
}

/// `HHMMSS`-style run name from the UTC time.
pub fn run_name() -> String {
    borgchart::bus::time::utc_now()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(15)
        .collect()
}

impl World {
    /// A live world: your login and configuration in place; work folders
    /// under `target/live/<run>/`.
    pub fn live() -> World {
        let limits = Limits::for_mode(Mode::Live);
        let private = common::PrivateTmux::with_watchdog(limits.run);
        let run = run_name();
        let evidence = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/live")
            .join(&run);
        let trusted = evidence.join("work");
        std::fs::create_dir_all(&trusted).unwrap();
        let trusted = trusted.canonicalize().unwrap();
        private.also_remove(&trusted);
        let scratch = short_dir("brg-live-");
        private.also_remove(&scratch);
        let mut env = BTreeMap::new();
        for key in LIVE_ENV {
            if let Ok(v) = std::env::var(key) {
                env.insert(key.to_string(), v);
            }
        }
        env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
        let brg = PathBuf::from(env!("CARGO_BIN_EXE_brg"));
        let path = format!(
            "{}:{}",
            brg.parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Self::finish(
            World {
                mode: Mode::Live,
                limits,
                socket: private.socket("brg-live"),
                private,
                env,
                evidence,
                trusted,
                scratch,
                versions: BTreeMap::new(),
                fake_home: None,
                brg,
                counter: Default::default(),
            },
            path,
        )
    }

    /// A dry world: owned fake CLIs resolved before any subprocess, and
    /// generated fake configuration only. Nothing of yours is read.
    pub fn dry(evidence: &Path) -> World {
        let limits = Limits::for_mode(Mode::Dry);
        let private = common::PrivateTmux::with_watchdog(limits.run);
        let scratch = short_dir("brg-dry-");
        private.also_remove(&scratch);
        let trusted = evidence.join("work");
        std::fs::create_dir_all(&trusted).unwrap();
        let trusted = trusted.canonicalize().unwrap();
        private.also_remove(&trusted);
        let home = scratch.join("home");
        let fakes = scratch.join("fakes");
        for d in [
            &home.join("fake"),
            &fakes,
            &scratch.join("codex-home"),
            &scratch.join("claude-config"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        for name in ["claude", "codex"] {
            let p = fakes.join(name);
            std::fs::write(&p, FAKE).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            home.join("fake/trusted"),
            format!("{}\n", trusted.display()),
        )
        .unwrap();
        std::fs::write(
            scratch.join("codex-home/config.toml"),
            "# generated for the dry run\n[tui]\nvim_mode_default = true\n",
        )
        .unwrap();
        let mut env = BTreeMap::new();
        env.insert("HOME".into(), home.display().to_string());
        env.insert("LANG".into(), "C.UTF-8".into());
        env.insert("USER".into(), "dry".into());
        env.insert(
            "CODEX_HOME".into(),
            scratch.join("codex-home").display().to_string(),
        );
        env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            scratch.join("claude-config").display().to_string(),
        );
        let brg = PathBuf::from(env!("CARGO_BIN_EXE_brg"));
        let path = format!(
            "{}:{}:/usr/bin:/bin",
            fakes.display(),
            brg.parent().unwrap().display()
        );
        let world = Self::finish(
            World {
                mode: Mode::Dry,
                limits,
                socket: private.socket("brg-dry"),
                private,
                env,
                evidence: evidence.to_path_buf(),
                trusted,
                scratch,
                versions: BTreeMap::new(),
                fake_home: Some(home.join("fake")),
                brg,
                counter: Default::default(),
            },
            path,
        );
        // Fail closed: the CLIs on this PATH must be the owned fakes,
        // before anything runs them.
        for name in ["claude", "codex"] {
            let found = world.resolve(name);
            let want = fakes.join(name);
            assert_eq!(
                found.as_deref(),
                Some(want.as_path()),
                "dry run: `{name}` resolves to {found:?}, not the owned fake; refusing to run"
            );
        }
        world
    }

    fn finish(mut w: World, path: String) -> World {
        w.env.insert("PATH".into(), w.private.path_with(&path));
        w.env.insert(
            "BORGCHART_STATE_HOME".into(),
            w.scratch.join("state").display().to_string(),
        );
        w.env.insert(
            "BORGCHART_GATE_WAIT_MS".into(),
            w.limits.gate_ms.to_string(),
        );
        w.env.insert(
            "BORGCHART_TMUX_TMPDIR".into(),
            w.private.dir().display().to_string(),
        );
        w.env
            .insert("BORGCHART_TMUX_SOCKET".into(), w.socket.clone());
        std::fs::create_dir_all(w.evidence.join("screens")).unwrap();
        w
    }

    /// The first `name` on the world's `PATH`, after the guard.
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        let path = self.env.get("PATH")?;
        std::env::split_paths(path)
            .filter(|d| d != &self.private.bin())
            .map(|d| d.join(name))
            .find(|p| {
                p.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
            .map(|p| p.canonicalize().unwrap_or(p))
    }

    /// A fresh name for a case's team, folders and so on.
    pub fn fresh(&self, what: &str) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        format!("{what}-{n}")
    }

    /// `program` with exactly the world's environment.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = common::command(program);
        cmd.env_clear().envs(&self.env).stdin(Stdio::null());
        cmd
    }

    /// Preflight: each selected CLI is on `PATH` and answers `--version`
    /// within its limit. Nothing else starts until this has run.
    pub fn preflight(&mut self, selected: &[Who]) {
        for &who in selected {
            let v = match self.resolve(who.as_str()) {
                None => Err(format!("`{}` is not on PATH", who.as_str())),
                Some(cli) => {
                    let mut cmd = self.command(&cli);
                    cmd.arg("--version").current_dir(&self.trusted);
                    match limited(&mut cmd, self.limits.version) {
                        Err(e) => Err(format!("`{} --version` {e}", who.as_str())),
                        Ok(out) if !out.status.success() => Err(format!(
                            "`{} --version` failed ({})",
                            who.as_str(),
                            out.status
                        )),
                        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string()),
                    }
                }
            };
            self.versions.insert(who, v);
        }
        if !self.trusted.is_dir() {
            panic!(
                "the trusted work folder {} is missing",
                self.trusted.display()
            );
        }
    }

    /// The effective Codex `config.toml` (honouring `CODEX_HOME`) as a
    /// SHA-256, or `missing`. Its contents are never shown.
    pub fn codex_config_hash(&self) -> String {
        let home = match self.env.get("CODEX_HOME") {
            Some(h) => PathBuf::from(h),
            None => PathBuf::from(self.env.get("HOME").cloned().unwrap_or_default()).join(".codex"),
        };
        match std::fs::read(home.join("config.toml")) {
            Ok(bytes) => Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing".into(),
            Err(e) => format!("unreadable ({})", e.kind()),
        }
    }

    /// Dry only: set a member's fake scenario (`ok` clears it).
    pub fn scenario(&self, member: &str, scenario: &str) {
        let Some(home) = &self.fake_home else { return };
        let p = home.join(format!("scenario-{member}"));
        if scenario == "ok" {
            let _ = std::fs::remove_file(p);
        } else {
            std::fs::write(p, scenario).unwrap();
        }
    }

    /// Dry only: a file under the fakes' control directory.
    pub fn fake_file(&self, name: &str, contents: Option<&str>) {
        let Some(home) = &self.fake_home else { return };
        match contents {
            Some(c) => std::fs::write(home.join(name), c).unwrap(),
            None => {
                let _ = std::fs::remove_file(home.join(name));
            }
        }
    }

    /// A tmux client for the world's server.
    pub fn tmux(&self, args: &[&str]) -> String {
        let mut cmd = self.private.tmux(&self.socket);
        cmd.env("PATH", self.env["PATH"].clone()).args(args);
        limited(&mut cmd, Duration::from_secs(10))
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }
}

/// Run `cmd` to completion within `limit`, or kill it.
pub fn limited(cmd: &mut Command, limit: Duration) -> Result<Output, String> {
    limited_with_stdin(cmd, limit, None)
}

pub fn limited_with_stdin(
    cmd: &mut Command,
    limit: Duration,
    stdin: Option<&[u8]>,
) -> Result<Output, String> {
    use std::io::Write as _;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start: {e}"))?;
    if let Some(bytes) = stdin {
        let _ = child.stdin.take().unwrap().write_all(bytes);
    }
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("did not finish within {} s", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Ok(Output {
        status,
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

/// One process, from `/proc`.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub argv: Vec<String>,
}

pub fn has_proc() -> bool {
    Path::new("/proc/self/environ").exists()
}

pub fn procs() -> Vec<Proc> {
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid …`; comm may hold spaces and parentheses.
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let ppid = rest
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let argv = std::fs::read(e.path().join("cmdline"))
            .unwrap_or_default()
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        out.push(Proc { pid, ppid, argv });
    }
    out
}

/// `root` and every process below it.
pub fn tree(root: u32) -> Vec<Proc> {
    let all = procs();
    let mut keep = vec![root];
    let mut i = 0;
    while i < keep.len() {
        let p = keep[i];
        keep.extend(all.iter().filter(|q| q.ppid == p).map(|q| q.pid));
        i += 1;
    }
    all.into_iter().filter(|p| keep.contains(&p.pid)).collect()
}

pub fn environ(pid: u32) -> BTreeMap<String, String> {
    std::fs::read(format!("/proc/{pid}/environ"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter_map(|kv| {
            let kv = String::from_utf8_lossy(kv);
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// The vendor's own processes in a pane's tree: those whose program (or,
/// for a script, the script) is named after the vendor.
pub fn vendor_procs(pane_pid: u32, who: Who) -> Vec<Proc> {
    tree(pane_pid)
        .into_iter()
        .filter(|p| {
            p.argv.iter().take(2).any(|a| {
                Path::new(a)
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(who.as_str()))
            })
        })
        .collect()
}

/// A short, never-palindromic nonce.
pub fn nonce() -> String {
    let mut seed = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut seed);
    }
    let mut s: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    s.insert(0, 'n');
    s.push('z');
    s
}

pub fn reversed(s: &str) -> String {
    s.chars().rev().collect()
}
