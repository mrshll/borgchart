//! Shared by the integration tests: keep what a test starts away from the
//! caller's team and tmux server.
//!
//! The tests also run inside team members (the git hooks, or a member that
//! runs `cargo test`). There `BORGCHART_*` names the member's real
//! deployment and message storage, and `TMUX` its tmux server. A process
//! that a test starts with those inherited can write into that team.
//!
//! Each test's tmux servers live in a private directory ([`PrivateTmux`]),
//! never in `/tmp/tmux-<uid>`: brg gets it as `BORGCHART_TMUX_TMPDIR`, the
//! harness's own tmux calls as `TMUX_TMPDIR`. A `tmux` guard, first on the
//! PATH tests give brg and run by the harness itself, refuses any call that
//! would reach a server outside it, and the test fails if it refused one.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    key.starts_with("BORGCHART_") || matches!(&*key, "TMUX" | "TMUX_PANE" | "TMUX_TMPDIR")
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

/// A test's private tmux directory and its guard. Dropping it stops every
/// server in the directory and removes it, then fails the test if the
/// guard refused a call.
pub struct PrivateTmux {
    dir: tempfile::TempDir,
    /// The directory as tmux reports socket paths (macOS: `/tmp` is
    /// `/private/tmp`). brg and the harness are given this form.
    path: PathBuf,
    uid: u32,
}

impl PrivateTmux {
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
        let tmux = PrivateTmux { dir, path, uid };
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
        let guard = self.guard();
        std::fs::write(&guard, script).unwrap();
        std::fs::set_permissions(&guard, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        for entry in std::fs::read_dir(&sockets).into_iter().flatten().flatten() {
            let _ = command(self.guard())
                .arg("-S")
                .arg(entry.path())
                .arg("kill-server")
                .output();
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
