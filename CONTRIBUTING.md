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
| Live | `BORGCHART_LIVE=1 cargo test --locked --test live -- --ignored --nocapture --test-threads=1` | `tests/live.rs`: the real Claude Code and Codex CLIs, started by real `brg create`, locally only (below). | Before rebuilding a stable brg, and after updating Claude Code or Codex. |

The live tier uses your own login and configuration in place: nothing is copied, and brg writes no trust to it (Codex trust is session-only, and every Claude case's configuration sets `auto_trust: false`). Run it from a checkout inside a folder Claude already trusts. It costs a few small real turns per vendor, and never answers a tool approval: one ends its case BLOCKED. Each case ends PASS, FAIL, SKIP (with its reason) or BLOCKED, and the matrix goes to `target/live/<run>/matrix.md`. Only a complete run with every case listed in `tests/live/rules.rs` PASS qualifies a stable rebuild; `BORGCHART_LIVE_VENDORS=claude` (or `codex`) or `BORGCHART_LIVE_SUBMIT=0` makes a partial run. `BORGCHART_LIVE_DRY=1` runs the harness against owned fake CLIs instead; the default tier runs that dry run too.

A test that starts a process (tmux, `brg`, a child) belongs in a default-tier target, not in `src/` or `tests/validate.rs`. A program run by absolute path gets past the fast tier's stand-ins, so check moved tests by hand.

## Tests that start processes

The tests may run inside a team member, where `BORGCHART_*` and `TMUX` point at that member's real instance. Start every program through `tests/common` (`common::command`), or with `env_clear()`. Each test's tmux servers live in its own `common::PrivateTmux`: a private directory with a guard that refuses any tmux call outside it. Run tmux through `PrivateTmux::tmux`, point brg at it with `brg_env`, and put the guard first on brg's `PATH` with `path_with`; dropping it stops the servers and removes the directory. Give brg an explicit `BORGCHART_STATE_HOME` and `BORGCHART_CONFIG` (a test configuration, or a missing file for the defaults), and call library functions that read the configuration through `validate_with` and friends, never the caller's. `.githooks/checks test` runs the tests with `BORGCHART_*` (including `BORGCHART_CONFIG` and `BORGCHART_INSTANCE`) pointing nowhere, so a test that forgets fails there.

**The manual vendor qualification** (the live tier) also checks, by hand until it is automated, that a real `vendor`-posture Codex member can write the instance's notes (`touch "$BORGCHART_NOTES/x"`) and publish a message, and that a write to brg's own files (`touch "$(dirname "$BORGCHART_NOTES")/members/x"`) is refused by its sandbox. Record the result with the matrix.

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
