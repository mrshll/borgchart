//! Git worktrees brg adds for `worktree: true` roles, one per member or child team.
//!
//! Each lives at a fixed place beside brg's state folder (never inside it,
//! where members could reach brg's own files): `<state>-worktrees/<address>`,
//! with the address's `/` written as `.`, which no ID contains. brg adds it,
//! detached at the team's working folder's `HEAD` or on a new branch, when it issues the
//! member or child team, and removes it on `delete` when it is clean. `create --repo`
//! selects a clone directly in the tree's folder and fetches origin's default
//! branch before taking the registry lock. Worktree ownership is found afresh
//! in the root repository and those clones' own records, never through a
//! member-writable `.git` file. No repository selection is stored.
//! Use one dev team per repository; the root lead sequences child teams.

use std::path::{Path, PathBuf};

use crate::cleanup::{HARDENED, clean_worktree, git_network, git_with};

/// A selected clone and its fetched commit, used only during creation.
pub struct Start {
    repository: PathBuf,
    commit: String,
}

impl Start {
    pub fn detail(&self) -> String {
        format!(
            " · {} at {}",
            self.repository
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            &self.commit[..7]
        )
    }
}

fn clone(dir: &Path) -> bool {
    [dir.to_path_buf(), dir.join(".git")]
        .iter()
        .all(|path| path.symlink_metadata().is_ok_and(|meta| meta.is_dir()))
}

/// Select only a real child clone; network failures happen before issuance
/// or the registry lock. The checkout, its index and branch stay untouched.
pub fn fetch(root: &Path, name: &str) -> Result<Start, String> {
    if in_home(root) {
        return Err(format!(
            "the tree's folder {} is one of brg's worktrees",
            root.display()
        ));
    }
    let path = Path::new(name);
    if path.file_name() != Some(path.as_os_str()) || !clone(&root.join(path)) {
        return Err(format!(
            "`{name}` is not a clone directly in {}; name a real directory with a real `.git` directory, not a linked worktree",
            root.display()
        ));
    }
    let repository = root.join(path);
    let network = |args: &[&str]| {
        git_network(&repository, args).map_err(|e| {
            format!(
                "cannot fetch origin's default branch in `{name}` ({e}); give it an `origin` remote you can reach with your credentials, then try again"
            )
        })
    };
    let remote = network(&["ls-remote", "--symref", "origin", "HEAD"])?;
    let branch = remote
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .ok_or_else(|| {
            format!(
                "origin in `{name}` has no default branch; set its HEAD to a branch before creating a worktree"
            )
        })?;
    let hardened = HARDENED.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    git_with(&repository, &hardened, &["check-ref-format", "--branch", branch]).map_err(|e| {
        format!(
            "origin's default branch in `{name}` is invalid ({e}); set origin's HEAD to a valid branch and try again"
        )
    })?;
    network(&["fetch", "origin", &format!("refs/heads/{branch}")])?;
    let fetched = git_with(
        &repository,
        &hardened,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
    )?;
    let tracking = format!("refs/remotes/origin/{branch}");
    let tracked = || {
        git_with(
            &repository,
            &hardened,
            &["merge-base", "--is-ancestor", fetched.trim(), &tracking],
        )
        .is_ok()
    };
    if !tracked() {
        network(&["fetch", "origin"])?;
        if !tracked() {
            return Err(format!(
                "origin/{branch} did not reach the fetched default; include that branch in remote.origin.fetch and try again"
            ));
        }
    }
    Ok(Start {
        repository,
        commit: fetched.trim().to_string(),
    })
}

/// The tree's original repository (when present), and real child clones.
/// Each pair is the repository folder and its common git directory. Child
/// `.git` files and symlinks are never followed. Nothing is cached or stored.
pub fn repositories(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if in_home(root) {
        return Err(format!(
            "the tree's folder {} is one of brg's worktrees, whose `.git` file a member may have rewritten",
            root.display()
        ));
    }
    let hardened = HARDENED.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut repositories = Vec::new();
    match git_with(
        root,
        &hardened,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        Ok(common) => repositories.push((root.to_path_buf(), PathBuf::from(common.trim()))),
        Err(e) if e.starts_with("git rev-parse failed: fatal: not a git repository") => {}
        Err(e) => return Err(e),
    }
    for entry in
        std::fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?
    {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", root.display()))?;
        if clone(&entry.path()) {
            repositories.push((entry.path(), entry.path().join(".git")));
        }
    }
    Ok(repositories)
}

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

/// The folder of the top team of `address`'s tree.
pub fn root_folder(address: &str) -> Result<PathBuf, String> {
    let root = address.split('/').next().unwrap_or_default();
    Ok(crate::address::instance(root)?.record.folder)
}

/// Where the worktree of the member or child team at `address` lives.
pub fn path(address: &str) -> PathBuf {
    home().join(address.replace('/', "."))
}

/// Add a worktree at `dir` at the `HEAD` of `from`, a team's working folder in the
/// tree whose top folder is `root`: detached, or on a new `branch`. git runs
/// in the repository found through its own record ([`admin_dir`]), or the
/// selected clone at its fetched commit when `start` is given.
pub fn add(
    root: &Path,
    from: &Path,
    dir: &Path,
    branch: Option<&str>,
    start: Option<&Start>,
) -> Result<(), String> {
    std::fs::create_dir_all(home())
        .map_err(|e| format!("cannot create {}: {e}", home().display()))?;
    let hardened: Vec<String> = HARDENED.iter().map(|s| s.to_string()).collect();
    let (repository, base) = if let Some(start) = start {
        (start.repository.clone(), start.commit.clone())
    } else if from == root {
        (root.to_path_buf(), "HEAD".to_string())
    } else {
        let (repository, admin) = admin_dir(root, from)?;
        let mut pre = vec!["--git-dir".to_string(), admin.display().to_string()];
        pre.extend(hardened.iter().cloned());
        let head = git_with(&repository, &pre, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        (repository, head)
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
    git_with(&repository, &hardened, &args).map(|_| ()).map_err(|e| {
        if start.is_some() {
            format!("cannot add a worktree from {}: {e}", repository.display())
        } else {
            format!(
                "cannot add a worktree from {} (it must be in a git repository with a commit, or name a clone in it: --repo <name>): {e}",
                from.display()
            )
        }
    })
}

/// The folder git keeps for the worktree at `dir`, found through the
/// repository's own records of its worktrees. `root` is the folder of the
/// tree's top team, which a person chose. Search its repository and real
/// child clones. Never through a worktree's `.git` file, `dir`'s or
/// a child team's, which members working there can rewrite.
pub fn admin_dir(root: &Path, dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let want = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve it: {e}"))?;
    let mut found: Option<(PathBuf, PathBuf)> = None;
    for (repository, common) in repositories(root)? {
        for entry in std::fs::read_dir(common.join("worktrees"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(gitdir) = std::fs::read_to_string(entry.path().join("gitdir")) else {
                continue;
            };
            let top = Path::new(gitdir.trim()).parent().map(Path::canonicalize);
            if matches!(top, Some(Ok(top)) if top == want) {
                if let Some((previous, _)) = &found {
                    return Err(format!(
                        "both {} and {} list this worktree; repair their git worktree records before trying again",
                        previous.display(),
                        repository.display()
                    ));
                }
                found = Some((repository.clone(), entry.path()));
            }
        }
    }
    found.ok_or_else(|| format!("git in {} has no record of this worktree", root.display()))
}

/// Why the worktree at `dir`, in the tree whose top folder is `root`, can't
/// be removed without losing work, or `None` when it can (or isn't there).
pub fn unsaved(root: &Path, dir: &Path) -> Result<Option<String>, String> {
    if dir.symlink_metadata().is_err() {
        return Ok(None);
    }
    let (_, admin) = admin_dir(root, dir)?;
    clean_worktree(&admin, dir)
}

/// Remove the worktree at `dir`, in the tree whose top folder is `root`, if
/// it is there.
pub fn remove(root: &Path, dir: &Path) -> Result<(), String> {
    if dir.symlink_metadata().is_err() {
        return Ok(());
    }
    let (repository, _) = admin_dir(root, dir)?;
    let hardened: Vec<String> = HARDENED.iter().map(|s| s.to_string()).collect();
    git_with(
        &repository,
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
