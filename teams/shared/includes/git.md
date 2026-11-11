## Git and GitHub

Implementers commit locally only. Pushes, pull requests and issue writes belong to leads.

**Identity.** Act only as the agent account your owner's instructions name, and run every GitHub command and push the way they say. If they name no agent account, make no GitHub write or push: tell your parent or the owner. Never use the owner's personal login, read credential files or store a login. If the account can't reach a repository, tell your parent or the owner. In each new worktree, the lead does the per-checkout identity setup those instructions describe before briefing its writer; before the first commit there, check `git config user.name` returns the agent account.

### Leads owning pull requests and issues

- Open pull requests as drafts and keep the agent account assigned. Update the description in the same pass as each push, then read it back. Force-push only your own branch, with `--force-with-lease`.
- Space GitHub writes (pushes, description changes, assignments, review requests) at least 30 seconds apart, and never write in a loop.
- Hand off only after checks, the fresh review and fix round, and an up-to-date description. Mark the pull request ready and request the owner's review, as the agent account, with `gh api -X POST repos/{owner}/{repo}/pulls/<number>/requested_reviewers -f 'reviewers[]=<owner's login>'`, using the owner's login as your owner's instructions name it (`gh api` fills `{owner}/{repo}` from the checkout); check the response lists the reviewer. The agent stays assigned. When resuming work, withdraw the request with the same endpoint and `-X DELETE`, and request it again at handoff.
- Keep an issue assigned to its responsible person. Add the agent account while working and remove only the agent at handoff; check the assignment response.
- Merge only if your own role file grants it, under its **Reviews and merging** rule. Dev leads and implementers never merge.
