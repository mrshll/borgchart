//! Git worktrees brg adds for `worktree: true` roles, one per member or copy.
//!
//! Each lives at a fixed place beside brg's state folder (never inside it,
//! where members could reach brg's own files): `<state>-worktrees/<address>`,
//! with the address's `/` written as `.`, which no ID contains. brg adds it,
//! detached at the team folder's `HEAD` or on a new branch, when it issues the
//! member or copy, and removes it on `delete` when it is clean.

use std::path::{Path, PathBuf};

use crate::cleanup::{HARDENED, clean_worktree, git, git_with};

/// The folder that holds every worktree brg adds.
pub fn home() -> PathBuf {
    let state = crate::state::state_home();
    let mut name = state.file_name().unwrap_or_default().to_os_string();
    name.push("-worktrees");
    state.with_file_name(name)
}

/// Where the worktree of the member or copy at `address` lives.
pub fn path(address: &str) -> PathBuf {
    home().join(address.replace('/', "."))
}

/// Add a worktree at `dir` from the repository `from` works in, at `from`'s
/// `HEAD`: detached, or on a new `branch`.
pub fn add(from: &Path, dir: &Path, branch: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(home())
        .map_err(|e| format!("cannot create {}: {e}", home().display()))?;
    let target = dir.to_string_lossy();
    let mut args = vec!["worktree", "add"];
    match branch {
        Some(branch) => args.extend(["-b", branch]),
        None => args.push("--detach"),
    }
    args.extend([target.as_ref(), "HEAD"]);
    git(from, &args).map(|_| ()).map_err(|e| {
        format!(
            "cannot add a worktree from {} (it must be in a git repository with a commit): {e}",
            from.display()
        )
    })
}

/// The folder git keeps for the worktree at `dir` in the repository `from`
/// works in: found through that repository's own records of its
/// worktrees, never through `dir`'s `.git` file, which a member working in
/// `dir` can rewrite.
pub fn admin_dir(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    let common = git(
        from,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let worktrees = Path::new(common.trim()).join("worktrees");
    let want = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve it: {e}"))?;
    for entry in std::fs::read_dir(&worktrees)
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(gitdir) = std::fs::read_to_string(entry.path().join("gitdir")) else {
            continue;
        };
        let top = Path::new(gitdir.trim()).parent().map(Path::canonicalize);
        if matches!(top, Some(Ok(top)) if top == want) {
            return Ok(entry.path());
        }
    }
    Err(format!(
        "git in {} has no record of this worktree",
        from.display()
    ))
}

/// Why the worktree at `dir`, added from `from`, can't be removed without
/// losing work, or `None` when it can (or isn't there).
pub fn unsaved(from: &Path, dir: &Path) -> Result<Option<String>, String> {
    if dir.symlink_metadata().is_err() {
        return Ok(None);
    }
    clean_worktree(&admin_dir(from, dir)?, dir)
}

/// Remove the worktree at `dir`, added from `from`, if it is there.
pub fn remove(from: &Path, dir: &Path) -> Result<(), String> {
    if dir.symlink_metadata().is_err() {
        return Ok(());
    }
    let hardened: Vec<String> = HARDENED.iter().map(|s| s.to_string()).collect();
    git_with(
        from,
        &hardened,
        &["worktree", "remove", &dir.to_string_lossy()],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_map_to_flat_names_beside_the_state_folder() {
        let home = home();
        assert_eq!(
            path("team-2a3b/dev-4c5d/implementer-6e7f"),
            home.join("team-2a3b.dev-4c5d.implementer-6e7f")
        );
        assert!(!home.starts_with(crate::state::state_home()));
    }
}
