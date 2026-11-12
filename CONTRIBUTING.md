# Contributing

## Checks

Four checks must pass before a change merges. CI runs the Rust checks on Linux for pull requests and on Linux and macOS for pushes to main, and the Python tests on Linux. `make check` runs the quick checks (`fmt clippy fast`); `make test` runs the full checks. They use the same script as the git hooks and CI:

```
.githooks/checks              # all four: fmt, clippy, test, py
.githooks/checks fmt clippy   # a subset
.githooks/checks fast         # the fast tier (below)
```

The tests need `tmux`, and `py` needs `uv`. Before running only the department integration target, build its sibling plugin binary with `tools/cargo-slot cargo build --workspace --bins --all-features --locked`; `.githooks/checks test` does this automatically.

`make build` builds the release workspace binaries; `make fmt` formats Rust, `make lint` runs clippy, and `make clean` removes build artifacts. Run `make` for help; [installation](#installing-from-a-checkout) uses `make install`.

## Test tiers

| Tier | Command | What it runs | When |
| --- | --- | --- | --- |
| Fast | `.githooks/checks fast` | Every workspace member’s library tests (`cargo test --workspace --all-features --lib`) and core validation tests (`cargo test -p borgchart --features test-support --test validate`), which start no tmux and no other process. They run with recording stand-ins for `tmux`, `ps`, `sh`, `sleep`, `claude` and `codex` first on `PATH`, and the tier fails if any was started. | In CI. |
| Default | `.githooks/checks test` | Prebuilds workspace binaries, then runs all workspace tests with all features: real private tmux servers, with stand-ins for the vendor CLIs. | In CI. |
| Live | `BORGCHART_LIVE=1 cargo test --locked --test live -- --ignored --nocapture --test-threads=1` | `tests/live.rs`: the real Claude Code and Codex CLIs, started by real `brg create`, locally only (below). | Before rebuilding a stable brg, and after updating Claude Code or Codex. |

The live tier uses your own login and configuration in place: nothing is copied, and brg writes no trust to it (Codex trust is session-only, and every Claude case's configuration sets `auto_trust: false`). Run it from a checkout inside a folder Claude already trusts. It costs a few small real turns per vendor, and never answers a tool approval: one ends its case BLOCKED. Each case ends PASS, FAIL, SKIP (with its reason) or BLOCKED, and the matrix goes to `target/live/<run>/matrix.md`. Only a complete run with every case listed in `tests/live/rules.rs` PASS qualifies a stable rebuild; `BORGCHART_LIVE_VENDORS=claude` (or `codex`) or `BORGCHART_LIVE_SUBMIT=0` makes a partial run. `BORGCHART_LIVE_DRY=1` runs the harness against owned fake CLIs instead; the default tier runs that dry run too.

The separate cold seating probe runs a real Codex `gpt-6-astra medium` turn and a `brg-seat route` quota/app-server call. It requires that model and an existing `$HOME/.local/state`, where it creates and removes a private temporary cache. Its evidence is outside `matrix.md` and does not qualify the matrix.

A test that starts a process (tmux, `brg`, a child) belongs in a default-tier target, not in `src/` or `tests/validate.rs`. A program run by absolute path gets past the fast tier's stand-ins, so check moved tests by hand.

## Tests that start processes

The tests may run inside a team member, where `BORGCHART_*` and `TMUX` point at that member's real instance. Start every program through `tests/common` (`common::command`), or with `env_clear()`. Each test's tmux servers live in its own `common::PrivateTmux`: a private directory with a guard that refuses any tmux call outside it. Run tmux through `PrivateTmux::tmux`, point brg at it with `brg_env`, and put the guard first on brg's `PATH` with `path_with`; dropping it stops the servers and removes the directory. Give brg an explicit `BORGCHART_STATE_HOME` and `BORGCHART_CONFIG` (a test configuration, or a missing file for the defaults), and call library functions that read the configuration through `validate_with` and friends, never the caller's. `.githooks/checks test` runs the tests with `BORGCHART_*` (including `BORGCHART_CONFIG` and `BORGCHART_TEAM`) pointing nowhere, so a test that forgets fails there.

**The manual vendor qualification** (the live tier) also checks, by hand until it is automated, that a real `vendor`-posture Codex member can write the instance's notes (`touch "$BORGCHART_NOTES/x"`) and publish a message, with full host access and no tool approvals. Vendor Codex can also write brg's own files; do not expect a sandbox refusal. Locked members retain their gate and sandbox. Record the result with the matrix.

A test that may be killed or hang before it drops its `common::PrivateTmux` (as a live run can) starts it with `PrivateTmux::with_watchdog(limit)`: if the test process dies or `limit` passes, a detached watchdog stops the servers in the private directory and removes it. Register files the test keeps outside that directory (a work folder, say) with `also_remove(path)`; they go on a normal drop too.

## Git hooks

Turn the hooks on once per checkout:

```
git config core.hooksPath .githooks
```

In a linked worktree, use `git config --worktree core.hooksPath .githooks` (it needs `git config extensions.worktreeConfig true`), so the setting stays with that worktree.

- **pre-commit** and **pre-push** run `fmt` and `clippy`.

Neither hook runs tests: CI runs the fast tier, the full test suites and `py` on every pull request.

The hooks check the working tree, not only what is staged. Each check uses `tools/cargo-slot` automatically: at most two commands across all teams, an 8 GiB memory cap, no swap and six cargo jobs by default. Direct cargo commands use `tools/cargo-slot cargo …`; do not wrap `.githooks/checks` again. The wrapper requires Linux with `flock` and a working systemd user manager, and fails closed if the cap cannot run. Slots live under `$HOME/.local/state/borgchart/cargo-slots`; `HOME` must be absolute and nonempty. Both `XDG_RUNTIME_DIR` and `XDG_STATE_HOME` are ignored, so callers with the same home share the same two slots. Drain builds before migrating older wrappers, including in-tree copies on branches not yet updated from main. Hosted CI explicitly uses `CHECKS_WRAP=env` to preserve its existing runner limits; members never set this override.

## Running the engineering teams

`templates/` holds three team templates (`engineering`, `dev-team` and `dev-design-team`) and a `shared/` folder of includes they all use. Each `template.yaml` and its role files are the only description of that template.

The three aren't self-contained, so they travel together:

- **Child teams resolve by name.** `template: dev-team` is looked up in the `templates:` search paths of brg's configuration.
- **Shared includes sit beside the templates.** `catalog:shared/...` points at `templates/shared/`, which has no `template.yaml` and is not a template.
- **The [seating plugin](plugins/seat/README.md), `brg-seat`, is found on `PATH`,** as it was when the instance was created or fully started.

The template files hold no project rules and no owner. Members get both from their vendor's own instruction files, which load as normal:

- **The project's rules** come from the repository's instruction file: here, [AGENTS.md](AGENTS.md), which `CLAUDE.md` imports so both vendors read one text. It names the build wrapper, the checks, the code style and what may appear in public. A `locked` Codex member without `worktree: true` starts in its temp folder and doesn't load it; none of these teams has one.
- **The owner** is whoever runs the instance. Their own user-level instructions (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`) name their GitHub login and say how members commit, push and run GitHub commands: as the owner, or as a separate account, with any per-checkout identity setup. Leads request the owner's review only under a separate account.

### Setup

Point brg's configuration at the folder that contains `templates/`. With this `config.yaml` beside it:

```yaml
schema: 1
auth: login
templates: [./templates]
```

set `BORGCHART_CONFIG` to that file's absolute path, put `brg-seat` on `PATH`, and validate: `brg validate engineering`, `brg validate dev-team`, `brg validate dev-design-team`. If another configured directory provides one of these names, validation refuses the ambiguity.

Then create an instance from a checkout of the repository: `cd <checkout>; brg create engineering`, or `brg create dev-team`. Its lead works in the checkout, and brg adds a git worktree for each dev copy and implementer as it creates them.

### Installing from a checkout

A team that works on this repository should use binaries and template files from a separate checkout: a half-finished change to brg or to a role file would take effect at the next start. Install the binaries from that separate checkout:

```sh
make install
```

This vocabulary cutover does not migrate state. Destroy existing trees with their original brg before installing this version, then create new instances from templates.

This builds the workspace release binaries with `--locked`, copies `brg` and `brg-seat` into `$HOME/.local/share/brg/bin/`, and records `git rev-parse HEAD` in `$HOME/.local/share/brg/REVISION`. Each binary is copied to a temporary name beside its destination and renamed into place, so readers see a complete file and running processes keep their old inode. Linux builds use `tools/cargo-slot` when executable; other systems use cargo directly.

`PREFIX` changes the parent of `brg/` (default `$HOME/.local/share`); `BINDIR` changes where the `brg` and `brg-seat` symlinks go (default `$HOME/.local/bin`). For example, `make install PREFIX="$HOME/apps" BINDIR="$HOME/bin"`. Use absolute paths. The links point directly at `$PREFIX/brg/bin/<name>` and are replaced with `ln -sfn`. Keep `BINDIR` on `PATH`. The old `borgchart` link is removed only if it points to `$PREFIX/brg/bin/borgchart`; unrelated links and files are left alone.

The installer leaves template files and brg's configuration alone. Set `templates:` in that configuration to point straight at a checkout's `templates/` folder, which follows whatever that checkout has checked out. You may list several catalogs:

```text
templates: [~/workspace/<checkout>/templates, ~/<other>/templates]
```

A template name found in two listed catalogs is refused as ambiguous; for a command, name the template by its path instead. A template file's `template: <name>` role takes no path, so the listed catalogs must not share a template name that a template file refers to.

A running tree meets the installed binary and template files at its next brg call or next new member, by design. To move a tree started from an old per-revision directory, use the new binary to run `brg stop <instance>`, then `brg start <instance> --template "<checkout>/templates/<template>" --resume`. That repoints only the named instance: kept copies retain their recorded template folders and each needs its own full stop and `start --template <new-template-folder> --resume`, working from parent to child; keep the old installation until every copy has moved.
