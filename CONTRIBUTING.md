# Contributing

## Checks

Four checks must pass before a change merges. CI runs the Rust checks on Linux and macOS, and the Python tests on Linux. They live in one script, so the git hooks and CI run the same commands:

```
.githooks/checks              # all four: fmt, clippy, test, py
.githooks/checks fmt clippy   # a subset
.githooks/checks fast         # the fast tier (below)
```

The tests need `tmux`, and `py` needs `uv`.

## Test tiers

| Tier | Command | What it runs | When |
| --- | --- | --- | --- |
| Fast | `.githooks/checks fast` | The library and validation tests (`cargo test --lib --test validate`), which start no tmux and no other process. They run with recording stand-ins for `tmux`, `ps`, `sh`, `sleep`, `claude` and `codex` first on `PATH`, and the tier fails if any was started. | At every commit (pre-commit). |
| Default | `.githooks/checks test` | Every test: real private tmux servers, with stand-ins for the vendor CLIs. | Before every push (pre-push) and in CI. |
| Live | Its instructions come with `tests/live.rs`; until then `tests/qualify.rs` (see its header) checks the guarded wake against the real CLIs. | The real Claude Code and Codex CLIs, locally only. | Before rebuilding a stable brg, and after updating Claude Code or Codex. |

A test that starts a process (tmux, `brg`, a child) belongs in a default-tier target, not in `src/` or `tests/validate.rs`. A program run by absolute path gets past the fast tier's stand-ins, so check moved tests by hand.

## Tests that start processes

The tests may run inside a team member, where `BORGCHART_*` and `TMUX` point at that member's real team. Start every program through `tests/common` (`common::command`), or with `env_clear()`. Each test's tmux servers live in its own `common::PrivateTmux`: a private directory with a guard that refuses any tmux call outside it. Run tmux through `PrivateTmux::tmux`, point brg at it with `brg_env`, and put the guard first on brg's `PATH` with `path_with`; dropping it stops the servers and removes the directory. `.githooks/checks test` runs the tests with `BORGCHART_*` pointing nowhere, so a test that forgets fails there.

A test that may be killed or hang before it drops its `common::PrivateTmux` (as a live run can) starts it with `PrivateTmux::with_watchdog(limit)`: if the test process dies or `limit` passes, a detached watchdog stops the servers in the private directory and removes it. Register files the test keeps outside that directory (a work folder, say) with `also_remove(path)`; they go on a normal drop too.

## Git hooks

Turn the hooks on once per checkout:

```
git config core.hooksPath .githooks
```

In a linked worktree, use `git config --worktree core.hooksPath .githooks` (it needs `git config extensions.worktreeConfig true`), so the setting stays with that worktree.

- **pre-commit** runs `fmt`, `clippy` and `fast`.
- **pre-push** runs all four, including the full test suites.

The hooks check the working tree, not only what is staged. To run every check under a wrapper (for example one that caps memory or how many builds run at once), set `CHECKS_WRAP` to the wrapper command; each check runs as `$CHECKS_WRAP <command>`.
