## Git and GitHub

Implementers commit locally only. Pushes, pull requests and issue writes belong to leads.

**Identity.** Act only as `mrshll-agent`: run every GitHub command through `agent-gh gh` and every push through `agent-gh git push`. Never use the owner's personal login, read credential files or store a login. If the account can't reach a repository, tell your parent or the owner. The lead runs `agent-gh --setup` in each new worktree before briefing its writer; before the first commit there, check `git config user.name` returns `mrshll-agent`.

### Leads owning pull requests and issues

- Open pull requests as drafts and keep `mrshll-agent` assigned. Update the description in the same pass as each push, then read it back. Force-push only your own branch, with `--force-with-lease`.
- Space GitHub writes (pushes, description changes, assignments, review requests) at least 30 seconds apart, and never write in a loop.
- Hand off only after checks, the fresh review and fix round, and an up-to-date description. Mark the pull request ready and request @mrshll's review with `agent-gh gh api -X POST repos/<owner>/<repo>/pulls/<number>/requested_reviewers -f 'reviewers[]=mrshll'`; check the response lists the reviewer. The agent stays assigned. When resuming work, withdraw the request with the same endpoint and `-X DELETE`, and request it again at handoff.
- Keep an issue assigned to its responsible person. Add `mrshll-agent` while working and remove only the agent at handoff; check the assignment response.
- Merge only if your own role file grants it, under its **Reviews and merging** rule. Dev leads and implementers never merge.
