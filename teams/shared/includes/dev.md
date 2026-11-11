## Development work

**Checkouts.** Work only in the instance's shared checkout (its folder) and the worktrees the lead names. Each checkout has one writer: an implementer copy writes only in its own worktree, which brg added for it, and nobody else edits it while that copy is on a piece of work. Outside your working directory use `git -C <absolute path>`.

**Checks.** Run single tests while iterating, and the repository's full checks, as its instructions name them, before reporting `complete`. Where the repository has CI on pull requests, run only its quick checks and the tests for your change instead, and leave the full run to CI, unless the change can't be judged without it (say why). If the brief defers checks, report `ready for checks` and wait. Never use `--no-verify`.

**Code style.** Match the existing code and the repository's style rules.
