# borgchart

**Teams of coding agents, defined in plain text files.** A folder of YAML and Markdown is the team; brg runs it in tmux. See it at [borgchart.ai](https://borgchart.ai).

- **The team is a folder.** A `template.yaml` and one Markdown prompt per role. Keep it in git and review it like code.
- **Every prompt is a file you can read.** A member opens with its role file. brg appends only the commands it may run and a one-line note; your `CLAUDE.md` and `AGENTS.md` load as they always do.
- **Your own agents, in tmux.** Members are the `claude` and `codex` you already use, with your settings and connectors, signed in as you, on your plans. No API keys needed and no hosted service: each runs in a tmux window on your machine, and `brg attach` puts you in it.

## Quick start

```sh
curl -fsSL https://borgchart.ai/install.sh | sh
cd path/to/your/repository
brg create engineering
```

You need tmux, git, and the `claude` and `codex` CLIs, logged in.

`create` prints your team and its lead, for example `engineering-k3f9/lead-q8m3`. Talk to the lead, watch the team, and stop it:

```sh
brg attach engineering-k3f9/lead-q8m3
brg status
brg stop engineering-k3f9
```

See [Install](#install) for supported systems, versions and building from source, and [concepts](docs/concepts.md).

## Status

Describe each role's model and standing task in a template folder, then `brg create <template> <folder> [--name <name>]` starts a team from it, working in that folder; `brg status` shows who is alive, what they are doing and their unacked message counts, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back).

The commands listed by `brg --help` run teams on a private tmux server. [docs/concepts.md](docs/concepts.md) describes templates, teams, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. [`templates/engineering/`](templates/engineering) is the team template this repository's engineering runs under, the root of a tree of teams with child teams in its `dev` and `design` roles ([teams as members](docs/teams.md#teams-as-members)), and its leads check [the seating plugin](plugins/seat/README.md) before each launch. See [docs/format.md](docs/format.md) for the template format and [docs/cli.md](docs/cli.md) for the commands. The [message bus](docs/bus.md) provides durable plain messages, acknowledgements, an inbox and one wait; unacked counts appear in `status`.

How many members a role keeps (`max`) and when each gets its own git worktree are in [docs/format.md](docs/format.md#templateyaml); how members are named, started and deleted is in [docs/concepts.md](docs/concepts.md#commands).

## What your own instruction files should say

brg's teams leave personal rules to your `CLAUDE.md` / `AGENTS.md`.
Before starting a team, cover:

- [ ] Which account members commit, push and run GitHub commands as,
  including any per-checkout setup.
- [ ] Who "the owner" is on GitHub and how to hand them a pull request
  (review request, assignment).
- [ ] How the owner approves a merge.
- [ ] How secrets are obtained.
- [ ] What must stay out of public repositories.
- [ ] Language and test conventions.

For example, with fictional accounts:

> Act as `sam-example-bot`; configure each checkout's commit identity before work.
> The owner is `sam-example`; request their review when a pull request is ready.
> Merge only after they explicitly authorize the current head in a comment.
> Obtain secrets through the configured secret manager, without printing them.
> Keep private customer data out of public repositories; use fictional fixtures.
> For Python, use `uv run`, annotate function types and run the full test suite.

## Install

Install the latest published release, including all five binaries and the team templates:

```sh
curl -fsSL https://borgchart.ai/install.sh | sh
```

Linux x86_64 (glibc 2.35+) and aarch64 (glibc 2.39+), and macOS arm64 and x86_64 are supported. The installer needs curl, tar and sha256sum or shasum. Binaries go in `~/.local/share/brg/bin`, with links in `~/.local/bin`; `PREFIX` and `BINDIR` override those locations using absolute paths. It verifies the archive against the release's SHA256SUMS. Set `BRG_VERSION=v0.1.0` on the `sh` command to pin a published version. Reinstalling replaces bundled templates; keep custom templates in a separate catalog.

Keep `~/.local/bin` on `PATH`. Your plans must offer the models pinned in the role files.

### From source

With Rust 1.89 or newer and a C compiler, clone the repository and install:

```sh
git clone https://github.com/mrshll/borgchart
cd borgchart
cargo install --locked --path .
cargo install --locked --path plugins/seat
cargo install --locked --path plugins/slack
cargo install --locked --path plugins/timer
cargo install --locked --path plugins/github
```

Configure the clone's templates as described in [installing from a checkout](CONTRIBUTING.md#installing-from-a-checkout). Nothing is published to crates.io yet.

For tab completion of commands, teams and members in fish, run this once:

```sh
echo 'BRG_COMPLETE=fish brg | source' > ~/.config/fish/completions/brg.fish
```

Other shells are covered in [completion](docs/cli.md).

Seating, [Slack](plugins/slack/README.md), [timer](plugins/timer/README.md) and [GitHub](plugins/github/README.md) are workspace plugins. The engineering team uses seating, timer and GitHub intake; intake stays off until you configure it.
