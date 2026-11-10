## Development work

**Checkouts.** Work only in the instance's shared checkout (`repo/`) and the worktrees the lead names. Each checkout has one writer: an implementer copy writes only in its own worktree, `copies/<member ID>`, and nobody else edits it while that copy is on a piece of work. Outside your working directory use `git -C <absolute path>`.

**Checks.** Run single tests while iterating, and the full `.githooks/checks` before reporting `complete`. If the brief defers full checks, report `ready for checks` and wait. Never use `--no-verify`.

**Code style.** Match the existing code: small modules, plain error strings that say what to do next, `//!` module docs.
