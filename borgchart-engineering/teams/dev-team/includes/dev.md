## Development work

These rules add to the ones above for the members who work in the repository's checkouts.

**Checkouts.** Work only in the instance's shared checkout and the worktrees the lead names; never in any other checkout of this repository. Each implementer copy works only in its own worktree at `copies/<member ID>` (its cwd), beside the shared checkout `repo/` in the instance's folder, prepared by the lead before its brief; nobody else edits it while the copy is on a piece of work. The lead prepares, verifies and pushes it between pieces; everyone else reads. Outside your working directory use `git -C <absolute path>`.

**One writer per checkout.** The owner has approved parallel implementers: up to `max` implementer copies may work at once, each in its own worktree. The lead may keep a worktree across briefs while the copy is kept, and removes it with `git worktree remove` before deleting the copy.

**Checks.** Run `.githooks/checks`; run single tests while iterating and the full checks before reporting `complete`. Follow the brief's build permission; if deferred, report `ready for checks` and wait. The shared common rules cover the automatic build cap. Never use `--no-verify`.

**Code style.** Match the existing code: small modules, plain error strings that say what to do next, `//!` module docs.
