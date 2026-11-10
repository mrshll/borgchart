# borgchart

borgchart runs teams of coding agents from plain files: describe each role's model and standing task in a team folder, then `brg create <team> <folder>` starts an instance of it working in that folder, `brg status` shows who is alive, what they are doing and their unread message counts, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back). It is early and under active design; see the open pull requests for the plan.

## Status

Early, and expected to change. `brg validate`, `create`, `start`, `stop`, `delete`, `ls`, `status` (with `-w` and `-i`), `secret`, `attach`, `logs` and `send` work, on a private tmux server. [docs/concepts.md](docs/concepts.md) describes teams, instances, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. [`teams/borgchart-engineering/`](teams/borgchart-engineering) is the team this repository's engineering runs under, the root of a tree of teams with its dev team and design panel as child teams ([teams as members](docs/teams.md#teams-as-members)), and its leads check [the seating plugin](docs/seating.md) before each launch. See [docs/format.md](docs/format.md) for the team format and [docs/cli.md](docs/cli.md) for the commands. The [message bus](docs/bus.md) provides durable plain messages, acknowledgements, an inbox and one wait; unread counts appear in `status`.

Team files declare `roles:`; each role keeps up to `max` members (default 1), running or stopped. Copies share the role's folder unless `cwd` uses `{member_id}`; with `cwd: copies/{member_id}`, brg makes the empty leaf under an existing `copies` folder whenever it issues a new member (`create`, or a full `start` that gives a `start: true` role a new member), for any `max`. Member commands and messages use the full member ID. `brg delete <instance>/<member>` removes a stopped member, requiring `--force` for unread messages; running or unknown liveness, or kept children, is always refused. Deletion reserves its ID permanently within the instance. `members:` and frontmatter `role:` are rejected.

## Install

```sh
cargo install --git https://github.com/mrshll/borgchart --locked borgchart
cargo install --git https://github.com/mrshll/borgchart --locked brg-seat
```

This needs Rust 1.89 or newer and tmux 3.2 or newer, plus the `claude` and/or `codex` CLIs. Nothing is published to crates.io yet.

For tab completion of commands, instances and members, see [completion](docs/cli.md) (`BRG_COMPLETE=fish brg | source`).


Seating is an optional Rust workspace plugin, declared only by teams that need subscription quota checks. For a revision-pinned installation containing both binaries and the team catalog, follow [the catalog install recipe](teams/README.md#pinned-installation). `cargo install --path .` installs only core; the plugin is `cargo install --path plugins/seat`.
