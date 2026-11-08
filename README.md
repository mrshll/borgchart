# borgchart

borgchart runs teams of coding agents from plain files: describe each role's model and standing task in a team folder, then `brg create <team> <folder>` starts an instance of it working in that folder, `brg status` shows who is alive and what they are doing, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back). It is early and under active design; see the open pull requests for the plan.

## Status

Early, and expected to change. `brg validate`, `create`, `start`, `stop`, `delete`, `ls`, `status` (with `-w` and `-i`), `secret`, `attach`, `logs` and `send` work, on a private tmux server. [docs/concepts.md](docs/concepts.md) describes teams, instances, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. See [docs/format.md](docs/format.md) for the team format and [docs/cli.md](docs/cli.md) for the commands. A coordination bus is in progress: [docs/bus.md](docs/bus.md).

## Install

```sh
cargo install --git https://github.com/mrshll/borgchart --locked
```

This needs Rust 1.89 or newer and tmux 3.2 or newer, plus the `claude` and/or `codex` CLIs. Nothing is published to crates.io yet.

