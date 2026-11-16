## Git and GitHub

Implementers commit locally only. Leads own pushes, pull requests and issue writes.

**Identity.** Follow the owner's instructions for commit, push and GitHub identity: the owner or their named separate account. Never read credential files or store a login. If you can't reach a repository, tell your parent or the owner.

### Leads owning pull requests and issues

- Open draft pull requests and keep yourself assigned. Update the description with each push, then read it back. Force-push only your own branch, with `--force-with-lease`.
- Space GitHub writes (pushes, description changes, assignments, review requests) at least 30 seconds apart; never write in a loop.
- Hand off only after checks (green on the head if CI runs full checks on pull requests), the fresh review and fix round, and an up-to-date description. Mark the pull request ready. Where the rule above requires the owner's review, request it with `gh api -X POST repos/{owner}/{repo}/pulls/<number>/requested_reviewers -f 'reviewers[]=<owner's login>'` (`gh api` fills `{owner}/{repo}` from the checkout); verify the response lists the reviewer. On resuming work, withdraw the request at the same endpoint with `-X DELETE`, then request it again at handoff.
- Keep issues assigned to their responsible person. Under a separate account, add that account while working and remove only it at handoff; check the assignment response.
- Merge only if your role's **Reviews and merging** rule permits it. Dev leads and implementers never merge.
- **Cleanup.** Stop and delete researchers and monitors after reporting unless more work is already briefed. Watches end at their own trigger or deadline, independent of workstreams. Before owner status reports or parent handoffs, stop and delete stopped/idle members with no open brief and pushed or reported work, respecting CI gates; name deletion failures. Check unacked work; never `--force`. At workstream end, remove scratch clones, builds and local branches you created for it.
