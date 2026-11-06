# Contributing

## Checks

Four checks must pass before a change merges. CI runs the Rust checks on Linux and macOS, and the Python tests on Linux. They live in one script, so the git hooks and CI run the same commands:

```
.githooks/checks              # all four: fmt, clippy, test, py
.githooks/checks fmt clippy   # a subset
```

The tests need `tmux`, and `py` needs `uv`.

## Tests that start processes

The tests may run inside a team member, where `BORGCHART_*` and `TMUX` point at that member's real team. Start every `brg`, `tmux` or other program through `tests/common` (`common::command`, `common::tmux`), or with `env_clear()`, and stop each tmux server with `common::kill_server`, which also removes its socket file. `.githooks/checks test` runs the tests with `BORGCHART_*` pointing nowhere, so a test that forgets fails there.

## Git hooks

Turn the hooks on once per checkout:

```
git config core.hooksPath .githooks
```

In a linked worktree, use `git config --worktree core.hooksPath .githooks` (it needs `git config extensions.worktreeConfig true`), so the setting stays with that worktree.

- **pre-commit** runs `fmt` and `clippy`.
- **pre-push** runs all four, including the full test suites.

The hooks check the working tree, not only what is staged. To run every check under a wrapper (for example one that caps memory or how many builds run at once), set `CHECKS_WRAP` to the wrapper command; each check runs as `$CHECKS_WRAP <command>`.
