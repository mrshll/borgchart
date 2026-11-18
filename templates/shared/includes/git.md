## Git and GitHub

Implementers commit locally only. Leads own pushes, pull requests and issue writes.

If you can't reach a repository, tell your parent or the owner.

### Leads owning pull requests and issues

- Open draft pull requests. Update the description with each push, then read it back. Force-push only your own branch, with `--force-with-lease`.
- Space GitHub writes (pushes, description changes, assignments, review requests) at least 30 seconds apart; never write in a loop.
- Hand off only after checks (green on the head if CI runs full checks on pull requests), the fresh review and fix round, and an up-to-date description. Mark the pull request ready, then tell your parent, or the owner if parentless.
- Keep issues assigned to their responsible person.
- Merge only if your role's **Reviews and merging** rule permits it. Dev leads and implementers never merge.
- **Cleanup.** Stop and delete researchers and monitors after reporting unless more work is already briefed. Watches end at their own trigger or deadline, independent of workstreams. Before owner status reports or parent handoffs, stop and delete stopped/idle members with no open brief and pushed or reported work, respecting CI gates; name deletion failures. Check unacked work; never `--force`. At workstream end, remove scratch clones, builds and local branches you created for it.
  For finished members or child teams with pushed or reported work, if deletion refuses only commits “on no remote and not accounted for by content”: require empty `git -C <worktree> status --short` and a remote branch from `git -C <worktree> branch -r --contains HEAD`.
  Check every refused commit against the report: only earlier versions of pushed work or deliberately dropped commits may go; push or report real unreported work and stop. Record full hashes and subjects with `git -C <worktree> show -s --format='%H %s' <refused-hashes>` in the cleanup report.
  Run `git -C <worktree> checkout --detach HEAD`, then `git -C <worktree> reflog expire --expire=now HEAD`; never `--all` (it expires other worktrees' history). Detaching avoids branch-reflog fallback without expiring shared branch history.
  List `git -C <worktree> for-each-ref --format='%(refname) %(objectname)' refs/worktree/`; remove each reviewed ref with `git -C <worktree> update-ref -d <ref> <objectname>`, then retry plain `delete`. All other checks remain.
