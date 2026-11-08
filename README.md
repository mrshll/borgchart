# borgchart

borgchart runs teams of coding agents from plain files: describe each role's model and standing task in a team folder, then `brg create <team> <folder>` starts an instance of it working in that folder, `brg status` shows who is alive, what they are doing and their unread message counts, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back). It is early and under active design; see the open pull requests for the plan.

## Status

Early, and expected to change. `brg validate`, `create`, `start`, `stop`, `delete`, `ls`, `status` (with `-w` and `-i`), `secret`, `attach`, `logs` and `send` work, on a private tmux server. [docs/concepts.md](docs/concepts.md) describes teams, instances, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. See [docs/format.md](docs/format.md) for the team format and [docs/cli.md](docs/cli.md) for the commands. The [message bus](docs/bus.md) provides durable plain messages, acknowledgements, an inbox and one wait; unread counts appear in `status`.

Team files declare `roles:`; each role keeps up to `max` members (default 1), running or stopped. Copies use separate folders through `cwd: copies/{member}`. Member commands and messages use the full member ID. `brg delete <instance>/<member>` removes a stopped member, requiring `--force` for unread messages, and reserves its ID permanently within the instance. `members:` and frontmatter `role:` are rejected.

## Install

```sh
cargo install --git https://github.com/mrshll/borgchart --locked
```

This needs Rust 1.89 or newer and tmux 3.2 or newer, plus the `claude` and/or `codex` CLIs. Nothing is published to crates.io yet.

