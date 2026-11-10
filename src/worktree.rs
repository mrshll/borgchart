//! Git worktrees brg adds for `worktree: true` roles, one per member or copy.
//!
//! Each lives at a fixed place beside brg's state folder (never inside it,
//! where members could reach brg's own files): `<state>-worktrees/<address>`,
//! with the address's `/` written as `.`, which no ID contains. brg adds it,
//! detached at the team folder's `HEAD` or on a new branch, when it issues the
//! member or copy, and removes it on `delete` when it is clean.

use std::path::{Path, PathBuf};

use crate::cleanup::{HARDENED, clean_worktree, git_with};

/// The folder that holds every worktree brg adds.
pub fn home() -> PathBuf {
    let state = crate::state::state_home();
    let mut name = state.file_name().unwrap_or_default().to_os_string();
    name.push("-worktrees");
    state.with_file_name(name)
}

/// Whether `folder` is at or inside [`home`], compared both as written and
/// with symlinks resolved on each side.
fn in_home(folder: &Path) -> bool {
    let home = home();
    let resolved = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    folder.starts_with(&home) || resolved(folder).starts_with(resolved(&home))
}

/// The folder of the top instance of `address`'s tree.
pub fn root_folder(address: &str) -> Result<PathBuf, String> {
    let root = address.split('/').next().unwrap_or_default();
    Ok(crate::address::instance(root)?.record.folder)
}

/// Where the worktree of the member or copy at `address` lives.
pub fn path(address: &str) -> PathBuf {
    home().join(address.replace('/', "."))
}

/// Add a worktree at `dir` at the `HEAD` of `from`, a team's folder in the
/// tree whose top folder is `root`: detached, or on a new `branch`. git runs
/// in `root`; a copy's `from` is one of brg's worktrees, read only through
/// git's own record of it ([`admin_dir`]).
pub fn add(root: &Path, from: &Path, dir: &Path, branch: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(home())
        .map_err(|e| format!("cannot create {}: {e}", home().display()))?;
    let hardened: Vec<String> = HARDENED.iter().map(|s| s.to_string()).collect();
    let base = if from == root {
        "HEAD".to_string()
    } else {
        let admin = admin_dir(root, from)?;
        let mut pre = vec!["--git-dir".to_string(), admin.display().to_string()];
        pre.extend(hardened.iter().cloned());
        git_with(root, &pre, &["rev-parse", "HEAD"])?
            .trim()
            .to_string()
    };
    let target = dir.to_string_lossy();
    let mut args = vec!["worktree", "add"];
    match branch {
        Some(branch) => args.extend(["-b", branch]),
        None => args.push("--detach"),
    }
    args.extend([target.as_ref(), base.as_str()]);
    if in_home(root) {
        return Err(format!(
            "the tree's folder {} is one of brg's worktrees",
            root.display()
        ));
    }
    git_with(root, &hardened, &args).map(|_| ()).map_err(|e| {
        format!(
            "cannot add a worktree from {} (it must be in a git repository with a commit): {e}",
            from.display()
        )
    })
}

/// The folder git keeps for the worktree at `dir`, found through the
/// repository's own records of its worktrees. `root` is the folder of the
/// tree's top instance, which a person chose: every worktree in a tree comes
/// from its repository. Never through a worktree's `.git` file, `dir`'s or
/// a copy's, which members working there can rewrite.
pub fn admin_dir(root: &Path, dir: &Path) -> Result<PathBuf, String> {
    if in_home(root) {
        return Err(format!(
            "the tree's folder {} is one of brg's worktrees, whose `.git` file a member may have rewritten",
            root.display()
        ));
    }
    let hardened: Vec<String> = HARDENED.iter().map(|s| s.to_string()).collect();
    let common = git_with(
        root,
        &hardened,
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
        root.display()
    ))
}

/// Why the worktree at `dir`, in the tree whose top folder is `root`, can't
/// be removed without losing work, or `None` when it can (or isn't there).
pub fn unsaved(root: &Path, dir: &Path) -> Result<Option<String>, String> {
    if dir.symlink_metadata().is_err() {
        return Ok(None);
    }
    clean_worktree(&admin_dir(root, dir)?, dir)
}

/// Remove the worktree at `dir`, in the tree whose top folder is `root`, if
/// it is there.
pub fn remove(root: &Path, dir: &Path) -> Result<(), String> {
    if dir.symlink_metadata().is_err() {
        return Ok(());
    }
    admin_dir(root, dir)?;
    let hardened: Vec<String> = HARDENED.iter().map(|s| s.to_string()).collect();
    git_with(
        root,
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
