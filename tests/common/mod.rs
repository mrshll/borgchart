//! Shared by the integration tests: keep what a test starts away from the
//! caller's team and tmux server.
//!
//! The tests also run inside team members (the git hooks, or a member that
//! runs `cargo test`). There `BORGCHART_*` names the member's real
//! deployment and message storage, and `TMUX` its tmux server. A process
//! that a test starts with those inherited can write into that team. Inside
//! a git hook, `GIT_DIR` names this repository: a `git` a test starts with
//! it inherited works on the repository instead of the test's own folder.
//!
//! Each test's tmux servers live in a private directory ([`PrivateTmux`]),
//! never in `/tmp/tmux-<uid>`: brg gets it as `BORGCHART_TMUX_TMPDIR`, the
//! harness's own tmux calls as `TMUX_TMPDIR`. A `tmux` guard, first on the
//! PATH tests give brg and run by the harness itself, refuses any call that
//! would reach a server outside it, and the test fails if it refused one.
//! A test that may outlive its drop (killed, or hung) can add a watchdog
//! ([`PrivateTmux::with_watchdog`]) that stops those servers anyway.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Remove every inherited variable that could point at the caller's team or
/// tmux server. Variables the test sets afterwards still apply.
pub fn scrub(cmd: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        if points_at_a_team(&key) {
            cmd.env_remove(&key);
        }
    }
    cmd
}

fn points_at_a_team(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("BORGCHART_")
        || key.starts_with("GIT_")
        || matches!(&*key, "TMUX" | "TMUX_PANE" | "TMUX_TMPDIR")
}

/// `program`, scrubbed.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    scrub(&mut cmd);
    cmd
}

/// Where the private directories go: `$BRG_TEST_TMPDIR_ROOT` (the checks
/// script makes one per run, and stops whatever is left in it), else `/tmp`.
/// Not the system temporary directory: macOS's is long, and a socket path
/// must stay under 104 bytes.
pub const ROOT_VAR: &str = "BRG_TEST_TMPDIR_ROOT";

/// The guard's record of the calls it refused, inside the private directory.
const REFUSED: &str = "refused";

/// The paths [`PrivateTmux::also_remove`] registered, one per line, inside
/// the private directory (so the watchdog reads the same list).
const ALSO_REMOVE: &str = "also-remove";

/// The watchdog's note of each server's PID and start time, inside the
/// private directory.
pub const SERVERS: &str = "watchdog-servers";

/// A test's private tmux directory and its guard. Dropping it stops every
/// server in the directory, removes the paths given to
/// [`also_remove`](Self::also_remove) and the directory, then fails the test
/// if the guard refused a call.
pub struct PrivateTmux {
    dir: tempfile::TempDir,
    /// The directory as tmux reports socket paths (macOS: `/tmp` is
    /// `/private/tmp`). brg and the harness are given this form.
    path: PathBuf,
    uid: u32,
    /// The watchdog, if [`with_watchdog`](Self::with_watchdog) started one.
    watchdog: Option<std::process::Child>,
}

impl PrivateTmux {
    /// A private directory with a watchdog: a detached process, started
    /// before any server, that does the drop's cleanup if the test process
    /// dies without dropping it (killed, or aborted) or once `limit` has
    /// passed, whatever the test is doing then. It stops every server in the
    /// directory (`-S` on each socket in `<dir>/tmux-<uid>/`, never through
    /// a symbolic link), removes the paths given to
    /// [`also_remove`](Self::also_remove) and then the directory, and
    /// touches nothing else. Each tmux call it makes has a time limit. A
    /// server that doesn't answer `kill-server` (stopped, or hung) is
    /// killed by the PID it reported while it still answered, if that PID
    /// still has the same start time. It runs in its own process group, so
    /// an interrupt sent to the test's doesn't stop it.
    pub fn with_watchdog(limit: Duration) -> Self {
        let mut tmux = Self::new();
        let script = tmux.path.join("watchdog");
        std::fs::write(&script, tmux.watchdog_script(limit)).unwrap();
        let child = Command::new("/bin/sh")
            .arg(&script)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start the tmux watchdog: {e}"));
        tmux.watchdog = Some(child);
        tmux
    }

    /// Remove `path` too (a file, or a directory and everything in it,
    /// never following a symbolic link out of it) when this is dropped, or
    /// when the watchdog cleans up. For what a test keeps outside the
    /// directory, such as scratch configuration.
    pub fn also_remove(&self, path: impl AsRef<Path>) {
        let path = std::path::absolute(path.as_ref()).unwrap();
        let text = path.to_str().expect("a UTF-8 path");
        assert!(
            path.parent().is_some() && !text.contains('\n'),
            "cannot register {path:?} for removal"
        );
        let mut list = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path.join(ALSO_REMOVE))
            .unwrap();
        writeln!(list, "{text}").unwrap();
    }

    /// The uid the directory's `tmux-<uid>` socket folder is named for.
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// The watchdog's own pid, if it has one.
    pub fn watchdog_pid(&self) -> Option<u32> {
        self.watchdog.as_ref().map(|c| c.id())
    }

    fn watchdog_script(&self, limit: Duration) -> String {
        let dir = self.path.to_str().unwrap();
        let real = real_tmux(&self.bin());
        let real = real.to_str().unwrap();
        for s in [dir, real] {
            assert!(!s.contains('\''), "{s}");
        }
        let (uid, pid) = (self.uid, std::process::id());
        // Whole seconds, rounded up.
        let limit = limit.as_millis().div_ceil(1000);
        format!(
            r#"#!/bin/sh
# A test's tmux watchdog: clean up its private directory if the test
# process dies, or after its time limit.
trap '' HUP INT TERM
dir='{dir}'
socks="$dir/tmux-{uid}"
known="$dir/{SERVERS}"
started=$(date +%s)
alive() {{
  stat=$(ps -o stat= -p {pid} 2>/dev/null) || return 1
  case $stat in Z*|'') return 1 ;; esac
}}
# Run a command for at most $1 seconds.
bounded() {{
  secs=$1
  shift
  "$@" &
  p=$!
  n=0
  while kill -0 "$p" 2>/dev/null; do
    if [ "$n" -ge $((secs * 10)) ]; then
      kill -9 "$p" 2>/dev/null
      break
    fi
    sleep 0.1
    n=$((n + 1))
  done
  wait "$p"
}}
# The servers' sockets: real sockets in the real directory, never a
# symbolic link to one elsewhere.
sockets() {{
  if [ -d "$socks" ] && [ ! -L "$socks" ]; then
    for s in "$socks"/*; do
      if [ -S "$s" ] && [ ! -L "$s" ]; then printf '%s\n' "$s"; fi
    done
  fi
}}
started_at() {{
  ps -o lstart= -p "$1" 2>/dev/null | sed 's/^ *//; s/ *$//'
}}
# Note each server's PID and start time while it answers.
learn() {{
  sockets | while IFS= read -r s; do
    # Into a file, not a pipe: tmux hands the client's output to the
    # server, so a stopped server would hold a pipe open for ever.
    bounded 2 '{real}' -S "$s" display-message -p '#{{pid}}' >"$dir/.pid" 2>/dev/null || continue
    spid=$(cat "$dir/.pid")
    case $spid in '' | *[!0-9]*) continue ;; esac
    grep -q "^$spid " "$known" 2>/dev/null && continue
    printf '%s %s\n' "$spid" "$(started_at "$spid")" >>"$known"
  done
}}
while alive && [ $(( $(date +%s) - started )) -lt {limit} ]; do
  learn
  sleep 1
done
sockets | while IFS= read -r s; do
  bounded 3 '{real}' -S "$s" kill-server >/dev/null 2>&1
done
# A server that didn't answer: kill it by the PID it gave, if that is
# still the same process.
if [ -f "$known" ]; then
  while read -r spid start; do
    if [ -n "$spid" ] && [ "$(started_at "$spid")" = "$start" ]; then
      kill -9 "$spid" 2>/dev/null
    fi
  done <"$known"
fi
if [ -f "$dir/{ALSO_REMOVE}" ]; then
  while IFS= read -r path; do
    [ -n "$path" ] && rm -rf -- "$path"
  done <"$dir/{ALSO_REMOVE}"
fi
rm -rf -- "$dir"
"#
        )
    }

    pub fn new() -> Self {
        let root = std::env::var_os(ROOT_VAR)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let dir = tempfile::Builder::new()
            .prefix("brg")
            .tempdir_in(&root)
            .unwrap_or_else(|e| panic!("cannot make a private tmux directory in {root:?}: {e}"));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap();
        let uid = std::fs::metadata(&path).unwrap().uid();
        let tmux = PrivateTmux {
            dir,
            path,
            uid,
            watchdog: None,
        };
        tmux.write_guard();
        tmux
    }

    /// The directory, to give tmux as `TMUX_TMPDIR`.
    pub fn dir(&self) -> &Path {
        &self.path
    }

    /// The directory holding only the guard, named `tmux`: put it first on
    /// a PATH so brg and its members find the guard, not tmux.
    pub fn bin(&self) -> PathBuf {
        self.path.join("g")
    }

    /// The guard itself.
    pub fn guard(&self) -> PathBuf {
        self.bin().join("tmux")
    }

    /// `name`, checked to make a socket path tmux can bind on every
    /// platform the tests run on.
    pub fn socket(&self, name: &str) -> String {
        let path = self.path.join(format!("tmux-{}", self.uid)).join(name);
        let len = path.as_os_str().len();
        assert!(
            len < 100,
            "tmux socket path {path:?} is {len} bytes; sockets need under 104 (macOS). Set {ROOT_VAR} to a shorter directory"
        );
        name.to_string()
    }

    /// `tmux -L <socket>` on a server in the directory: through the guard,
    /// scrubbed, with `TMUX_TMPDIR` set. A server it starts reads no tmux
    /// configuration (yours could run commands); a test that wants one adds
    /// its own `-f`.
    pub fn tmux(&self, socket: &str) -> Command {
        let mut cmd = command(self.guard());
        cmd.env("TMUX_TMPDIR", &self.path)
            .args(["-f", "/dev/null", "-L"])
            .arg(socket);
        cmd
    }

    /// Point brg (and the members and plugins it starts) at `socket` in the
    /// directory.
    pub fn brg_env<'a>(&self, cmd: &'a mut Command, socket: &str) -> &'a mut Command {
        cmd.env("BORGCHART_TMUX_TMPDIR", &self.path)
            .env("BORGCHART_TMUX_SOCKET", socket)
    }

    /// `PATH` with the guard's directory first.
    pub fn path_with(&self, rest: &str) -> String {
        format!("{}:{rest}", self.bin().display())
    }

    /// What the guard refused so far.
    pub fn refused(&self) -> String {
        std::fs::read_to_string(self.path.join(REFUSED)).unwrap_or_default()
    }

    /// Forget the calls refused so far (for a test of the guard itself).
    pub fn clear_refused(&self) {
        let _ = std::fs::remove_file(self.path.join(REFUSED));
    }

    /// The guard runs the real tmux only when the server it would reach is
    /// in the directory: `-S` with a socket path where tmux puts them
    /// (`<dir>/tmux-<uid>/<name>`, which the drop stops), or `-L` with
    /// `TMUX_TMPDIR` set to it; every `-L` and `-S` given must be. It reads
    /// tmux's global options up to the command (`capture-pane -S -20` names
    /// no socket) and allows only `-L name`, `-S path`, `-f file` and `-C`,
    /// each a separate argument: any other would need a full option parser
    /// to tell what follows it. Without `-L` or `-S`, tmux would use `$TMUX`
    /// or the default server: refused. A refusal exits 2 and adds a line to
    /// the directory's refusal record.
    fn write_guard(&self) {
        let canon = self.path.to_str().unwrap();
        let literal = self.dir.path().to_str().unwrap();
        let real = real_tmux(&self.bin());
        let real = real.to_str().unwrap();
        for s in [canon, literal, real] {
            assert!(!s.contains('\''), "{s}");
        }
        let uid = self.uid;
        let script = format!(
            r#"#!/bin/sh
# A test's tmux guard: only servers in the test's private directory.
dir='{canon}'
lit='{literal}'
all="$*"
refuse() {{
  printf 'refused (%s): TMUX_TMPDIR=%s tmux %s\n' "$1" "${{TMUX_TMPDIR-<unset>}}" "$all" >>"$dir/{REFUSED}"
  echo "tmux guard: refused ($1): tmux $all" >&2
  exit 2
}}
tmpdir_ok() {{
  [ "${{TMUX_TMPDIR-}}" = "$dir" ] || [ "${{TMUX_TMPDIR-}}" = "$lit" ]
}}
# Only where tmux itself puts sockets (and the drop finds them):
# <dir>/tmux-<uid>/<name>.
inside() {{
  # Never through a symbolic link, to a socket or its directory.
  if [ -L "$1" ] || [ -L "$dir/tmux-{uid}" ] || [ -L "$lit/tmux-{uid}" ]; then return 1; fi
  case $1 in
    */../*|*/..) return 1 ;;
    "$dir"/tmux-{uid}/*/*|"$lit"/tmux-{uid}/*/*) return 1 ;;
    "$dir"/tmux-{uid}/?*|"$lit"/tmux-{uid}/?*) return 0 ;;
  esac
  return 1
}}
server=
want=
for a in "$@"; do
  case $want in
    -L)
      case $a in ''|*/*) refuse "-L needs a plain name" ;; esac
      tmpdir_ok || refuse "-L without TMUX_TMPDIR set to the test's directory"
      server=1 ;;
    -S) inside "$a" || refuse "-S outside the test's directory"; server=1 ;;
    -f) ;;
    *)
      case $a in
        -L|-S|-f) want=$a; continue ;;
        -C) ;;
        --) break ;;
        -*) refuse "global option $a is not one the tests use" ;;
        *) break ;;
      esac ;;
  esac
  want=
done
[ -z "$want" ] || refuse "$want without a value"
[ -n "$server" ] || refuse "no -L or -S"
exec '{real}' "$@"
"#
        );
        std::fs::create_dir_all(self.bin()).unwrap();
        write_executable(&self.guard(), &script);
        // A child another test thread forked while the guard's file was open
        // for writing holds that descriptor until it runs its own program,
        // and starting the guard meanwhile fails with "Text file busy"
        // (`ETXTBSY`). Children forked later never hold it, so once one
        // start succeeds it can't happen again: start it now, harmlessly
        // (`list-sessions` on a server that doesn't exist), until it runs.
        for _ in 0..100 {
            match command(self.guard())
                .env("TMUX_TMPDIR", &self.path)
                .args(["-f", "/dev/null", "-L", "brg-probe", "list-sessions"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => return,
            }
        }
    }
}

impl Default for PrivateTmux {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let sockets = self.path.join(format!("tmux-{}", self.uid));
        let linked = std::fs::symlink_metadata(&sockets).is_ok_and(|m| m.is_symlink());
        for entry in std::fs::read_dir(&sockets)
            .into_iter()
            .filter(|_| !linked)
            .flatten()
            .flatten()
        {
            // Never through a symbolic link to a server elsewhere.
            if entry.file_type().map_or(true, |t| t.is_symlink()) {
                continue;
            }
            let _ = command(self.guard())
                .arg("-S")
                .arg(entry.path())
                .arg("kill-server")
                .output();
        }
        let also = std::fs::read_to_string(self.path.join(ALSO_REMOVE)).unwrap_or_default();
        for path in also.lines().filter(|p| !p.is_empty()) {
            remove(Path::new(path));
        }
        if let Some(mut watchdog) = self.watchdog.take() {
            let _ = watchdog.kill();
            let _ = watchdog.wait();
        }
        let refused = self.refused();
        if refused.is_empty() {
            return;
        }
        if std::thread::panicking() {
            eprintln!("the tmux guard refused:\n{refused}");
        } else {
            panic!("the tmux guard refused:\n{refused}");
        }
    }
}

/// Write an executable script under a temporary name, then rename it into
/// place, so it is never run half-written. (A child another thread forks
/// while it is open can still hold it briefly: see `write_guard`.)
pub fn write_executable(path: &Path, text: &str) {
    let tmp = path.with_extension("brg-tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// Remove a file or symbolic link, or a directory with everything in it
/// (`remove_dir_all` removes the links inside, never what they point at).
fn remove(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// The real tmux: the first on the test process's own PATH (or the usual
/// places), never a guard.
fn real_tmux(guard_dir: &Path) -> PathBuf {
    let search = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&search)
        .chain(
            ["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"]
                .iter()
                .map(PathBuf::from),
        )
        .filter(|d| d != guard_dir)
        .map(|d| d.join("tmux"))
        .find(|p| p.is_file())
        .expect("tmux on PATH")
}
