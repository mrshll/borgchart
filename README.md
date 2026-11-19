# borgchart

borgchart runs teams of coding agents from plain files: describe each role's model and standing task in a template folder, then `brg create <template> <folder>` starts a team from it, working in that folder, `brg status` shows who is alive, what they are doing and their unacked message counts, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back).

## Status

The commands listed by `brg --help` run teams on a private tmux server. [docs/concepts.md](docs/concepts.md) describes templates, teams, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. [`templates/engineering/`](templates/engineering) is the team template this repository's engineering runs under, the root of a tree of teams with child teams in its `dev` and `design` roles ([teams as members](docs/teams.md#teams-as-members)), and its leads check [the seating plugin](plugins/seat/README.md) before each launch. See [docs/format.md](docs/format.md) for the template format and [docs/cli.md](docs/cli.md) for the commands. The [message bus](docs/bus.md) provides durable plain messages, acknowledgements, an inbox and one wait; unacked counts appear in `status`.

Template files declare `roles:`; each role keeps up to `max` members (default 1), running or stopped. Members work in the team's folder unless their role sets `worktree: true`: then each member or child team gets its own git worktree from the team's folder, which brg adds when it issues it (`create`, or a full `start` that gives a `start: true` role a new member), detached at the folder's `HEAD` or on `create --branch <name>`. It lives beside brg's state folder, at `~/.local/state/borgchart-worktrees/<address>` (with `/` written as `.`); the command that adds it names it, and `status --json` gives it as `worktree`. `delete` removes it when it is clean, refuses while it holds uncommitted work or committed work not accounted for by remote-tracking refs, and with `--force` leaves it as it is. Member commands and messages use the full member ID. `brg delete <team>/<member>` removes a stopped member, requiring `--force` for unacked messages; running or unknown liveness, or kept children, is always refused. Deletion reserves its ID permanently within the team.

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

## Quick start

```sh
curl -fsSL https://borgchart.ai/install.sh | sh
cd path/to/your/repository
brg create engineering
```

You need tmux 3.2 or newer, git with a commit name and email set, and the [Claude Code](https://docs.anthropic.com/en/docs/claude-code/getting-started) and [Codex](https://github.com/openai/codex) CLIs logged in. Keep `~/.local/bin` on `PATH`.

This starts [the engineering team](templates/engineering) in a git repository you want the team to work on: a Claude lead that starts Codex helpers as needed. Your plans must offer the models pinned in the role files. The included seating plugin checks subscription quota before helpers launch and waits if it cannot establish availability.

The installer configures the bundled templates and `auto_trust: true` when no configuration exists; it preserves an existing config and tells you if you need to add the templates folder. See [configuration](docs/format.md#configyaml).

`create` prints the team's tree. Replace `engineering-k3f9/lead-q8m3` below with your team and lead IDs. Wait for `brg status` to show the lead idle. If blocked at a prompt, use `brg attach engineering-k3f9/lead-q8m3`, answer it yourself, and detach with Ctrl-b, then d.

Send a first task:

```sh
brg msg send engineering-k3f9/lead-q8m3 "Write HELLO.md in this folder with one line saying hello, then reply to me by message. Do it yourself; start no helpers."
```

Read the reply, check the result, and acknowledge the message (replace `m2` with its ID):

```sh
brg msg list --team engineering-k3f9 --full
cat HELLO.md
brg msg ack --team engineering-k3f9 m2
```

The reply appears when the lead finishes; run `msg list` again if it has not arrived. You can also talk to the lead through `brg attach`.

Stop when done:

```sh
brg stop engineering-k3f9
```

`brg start engineering-k3f9` brings it back with its notes and messages.

## Install

Install the latest published release, including all five binaries and the team templates:

```sh
curl -fsSL https://borgchart.ai/install.sh | sh
```

Linux x86_64 (glibc 2.35+) and aarch64 (glibc 2.39+), and macOS arm64 and x86_64 are supported. The installer needs curl, tar and sha256sum or shasum. Binaries go in `~/.local/share/brg/bin`, with links in `~/.local/bin`; `PREFIX` and `BINDIR` override those locations using absolute paths. It verifies the archive against the release's SHA256SUMS. Set `BRG_VERSION=v0.1.0` on the `sh` command to pin a published version. Reinstalling replaces bundled templates; keep custom templates in a separate catalog.

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
