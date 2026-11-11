## Git and GitHub

Implementers commit locally only. Pushes, pull requests and issue writes belong to leads.

**Identity.** Commit, push and run GitHub commands the way your owner's instructions say: as the owner, or as a separate account they name. Never read credential files or store a login. If you can't reach a repository, tell your parent or the owner.

### Leads owning pull requests and issues

- Open pull requests as drafts and keep yourself assigned. Update the description in the same pass as each push, then read it back. Force-push only your own branch, with `--force-with-lease`.
- Space GitHub writes (pushes, description changes, assignments, review requests) at least 30 seconds apart, and never write in a loop.
- Hand off only after checks (where the repository has CI on pull requests, green on the head), the fresh review and fix round, and an up-to-date description. Mark the pull request ready. Where the pull-request rule above calls for the owner's review, request it with `gh api -X POST repos/{owner}/{repo}/pulls/<number>/requested_reviewers -f 'reviewers[]=<owner's login>'` (`gh api` fills `{owner}/{repo}` from the checkout) and check the response lists the reviewer. You stay assigned. When resuming work, withdraw the request with the same endpoint and `-X DELETE`, and request it again at handoff.
- Keep an issue assigned to its responsible person. If you act as a separate account, add it while working and remove only it at handoff; check the assignment response.
- Merge only if your own role file grants it, under its **Reviews and merging** rule. Dev leads and implementers never merge.
