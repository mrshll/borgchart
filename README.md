# borgchart

borgchart runs teams of coding agents from plain files: describe each member's role, model and standing task, then `brg up` starts the team, `brg status` shows who is alive and what they are doing, `brg attach` drops you into a member's live session, and `brg down` stops it all. It is early and under active design; see the open pull requests for the plan.

## Status

Early. `brg validate`, `up`, `down`, `status`, `attach`, `logs` and `send` work, on a private tmux server. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. The `status -w` / `-i` screens are in progress. See [docs/format.md](docs/format.md) for the team format and [docs/cli.md](docs/cli.md) for the commands.

## Install

```sh
cargo install --git https://github.com/mrshll/borgchart --locked
```

This needs Rust 1.89 or newer and tmux 3.2 or newer, plus the `claude` and/or `codex` CLIs. Nothing is published to crates.io yet.

