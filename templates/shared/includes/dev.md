## Development work

**Checkouts.** Work only in the team's shared checkout (its folder) and the worktrees the lead names. Each checkout has one writer: an implementer writes only in its own worktree, which brg added for it, and nobody else edits it while that member is on a piece of work. Outside your working directory use `git -C <absolute path>`.

**Checks.** Run single tests while iterating. Before reporting `complete`, run the repository's checks as its instructions name them: its full checks or, where CI runs those on pull requests, its quick checks and the tests for your change, leaving the full run to CI unless the change can't be judged without it (say why). If the brief defers checks, report `ready for checks` and wait. Never use `--no-verify`.

**Code style.** Match the existing code and the repository's style rules.
