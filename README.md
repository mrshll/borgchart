# borgchart

borgchart runs teams of coding agents from plain files: describe each role's model and standing task in a template folder, then `brg create <template> <folder>` starts a team from it, working in that folder, `brg status` shows who is alive, what they are doing and their unacked message counts, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back). It is early and under active design; see the open pull requests for the plan.

## Status

Early, and expected to change. The commands `brg --help` lists work, on a private tmux server. [docs/concepts.md](docs/concepts.md) describes templates, teams, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. [`templates/engineering/`](templates/engineering) is the team template this repository's engineering runs under, the root of a tree of teams with child teams in its `dev` and `design` roles ([teams as members](docs/teams.md#teams-as-members)), and its leads check [the seating plugin](plugins/seat/README.md) before each launch. See [docs/format.md](docs/format.md) for the template format and [docs/cli.md](docs/cli.md) for the commands. The [message bus](docs/bus.md) provides durable plain messages, acknowledgements, an inbox and one wait; unacked counts appear in `status`.

Template files declare `roles:`; each role keeps up to `max` members (default 1), running or stopped. Members work in the team's folder unless their role sets `worktree: true`: then each member or child team gets its own git worktree from the team's folder, which brg adds when it issues it (`create`, or a full `start` that gives a `start: true` role a new member), detached at the folder's `HEAD` or on `create --branch <name>`. It lives beside brg's state folder, at `~/.local/state/borgchart-worktrees/<address>` (with `/` written as `.`); the command that adds it names it, and `status --json` gives it as `worktree`. `delete` removes it when it is clean, refuses while it holds uncommitted work or commits on no remote, and with `--force` leaves it as it is. Member commands and messages use the full member ID. `brg delete <team>/<member>` removes a stopped member, requiring `--force` for unacked messages; running or unknown liveness, or kept children, is always refused. Deletion reserves its ID permanently within the team.

## Install

```sh
cargo install --git https://github.com/mrshll/borgchart --locked --bin brg borgchart
cargo install --git https://github.com/mrshll/borgchart --locked brg-seat
cargo install --git https://github.com/mrshll/borgchart --locked brg-slack
```

This needs Rust 1.89 or newer and tmux 3.2 or newer, plus the `claude` and/or `codex` CLIs. Building the Slack plugin also needs a C compiler for TLS. Nothing is published to crates.io yet.

From a checkout, `make install` replaces the binaries; configure template catalogs separately as described in [installing from a checkout](CONTRIBUTING.md#installing-from-a-checkout).

For tab completion of commands, teams and members, see [completion](docs/cli.md) (`BRG_COMPLETE=fish brg | source`).


Seating and [Slack](plugins/slack/README.md) are optional Rust workspace plugins, declared by teams that need them. To install all three binaries and configure template catalogs, follow [installing from a checkout](CONTRIBUTING.md#installing-from-a-checkout). `cargo install --path .` installs only core; the plugins are `cargo install --path plugins/seat` and `cargo install --path plugins/slack`.
