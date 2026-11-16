## Development work

**Checkouts.** Work only in the team's shared checkout and worktrees the lead names, or your reviewer scratch clone. Each checkout has one writer: an implementer writes only in its own brg-added worktree, which nobody else edits during its work. Outside your working directory use `git -C <absolute path>`.

**Checks.** Run single tests while iterating. Before reporting `complete`, run the repository's prescribed checks: full checks, or quick checks and your change's tests if CI runs the full checks on pull requests. Run full checks locally if needed to judge the change, and say why. If the brief defers checks, report `partial` with body state `ready for checks`, then wait. Never use `--no-verify`.

**Code style.** Match existing code and repository rules.
