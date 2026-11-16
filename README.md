# borgchart

borgchart runs teams of coding agents from plain files: describe each role's model and standing task in a template folder, then `brg create <template> <folder>` starts a team from it, working in that folder, `brg status` shows who is alive, what they are doing and their unacked message counts, `brg attach` drops you into a member's live session, and `brg stop` stops it all (`brg start` brings it back). It is early and under active design; see the open pull requests for the plan.

## Status

Early, and expected to change. The commands `brg --help` lists work, on a private tmux server. [docs/concepts.md](docs/concepts.md) describes templates, teams, roles and members. [`examples/slack-devops`](examples/slack-devops) is an on-call pair for a Slack channel. [`templates/engineering/`](templates/engineering) is the team template this repository's engineering runs under, the root of a tree of teams with child teams in its `dev` and `design` roles ([teams as members](docs/teams.md#teams-as-members)), and its leads check [the seating plugin](plugins/seat/README.md) before each launch. See [docs/format.md](docs/format.md) for the template format and [docs/cli.md](docs/cli.md) for the commands. The [message bus](docs/bus.md) provides durable plain messages, acknowledgements, an inbox and one wait; unacked counts appear in `status`.

Template files declare `roles:`; each role keeps up to `max` members (default 1), running or stopped. Members work in the team's folder unless their role sets `worktree: true`: then each member or child team gets its own git worktree from the team's folder, which brg adds when it issues it (`create`, or a full `start` that gives a `start: true` role a new member), detached at the folder's `HEAD` or on `create --branch <name>`. It lives beside brg's state folder, at `~/.local/state/borgchart-worktrees/<address>` (with `/` written as `.`); the command that adds it names it, and `status --json` gives it as `worktree`. `delete` removes it when it is clean, refuses while it holds uncommitted work or commits on no remote, and with `--force` leaves it as it is. Member commands and messages use the full member ID. `brg delete <team>/<member>` removes a stopped member, requiring `--force` for unacked messages; running or unknown liveness, or kept children, is always refused. Deletion reserves its ID permanently within the team.

## Before you start

brg runs real agents as you. Before your first `brg create`:

- **Codex members without restricted access have full host access and no tool approvals** (`--sandbox danger-full-access --ask-for-approval never`): their commands can read and change anything your user can. That includes every Codex helper in the example team.
- **Members act as you.** They use your logged-in vendor accounts, your git and GitHub credentials, and the MCP connectors set up in each CLI ([connectors](docs/format.md#connectors)).
- **Every member draws on your vendor usage:** your Claude or Codex plan's allowance, or API spend. brg sets no spending cap.
- **`brg destroy` deletes a team's notes and messages, without asking.** `brg stop` keeps them.

Start in a disposable folder, as below, not in a checkout you care about.

## Quick start

This runs the example team, [`templates/engineering`](templates/engineering): a Claude lead that starts Codex helpers when its work needs them. You need:

- Rust 1.89 or newer, tmux 3.2 or newer, and git with your name and email set (`git config --global user.name` and `user.email`): members commit as you.
- The [Claude Code](https://docs.anthropic.com/en/docs/claude-code/getting-started) and [Codex](https://github.com/openai/codex) CLIs, each logged in to a plan that offers the models the role files pin: the `models:` lines at the top of each `templates/engineering/*.md` (the lead runs Claude `fable`, helpers run Codex `gpt-6-astra` and others). Before each launch the lead asks the seating plugin, `brg-seat`, whether that plan has room, and waits when it can't tell.

Install brg and the seating plugin from a clone, which also holds the templates:

```sh
git clone https://github.com/mrshll/borgchart ~/borgchart
cd ~/borgchart
cargo install --locked --path .
cargo install --locked --path plugins/seat
```

Point brg at the clone's templates, and check them:

```sh
mkdir -p ~/.config/borgchart
printf 'schema: 1\nauto_trust: true\ntemplates: [~/borgchart/templates]\n' > ~/.config/borgchart/config.yaml
brg validate engineering
```

`validate` ends its first line with `ok: 4 role(s), 1 plugin(s)`. `auto_trust: true` has brg trust each member's folder, so a new helper doesn't stop at its CLI's folder-trust screen ([configuration](docs/format.md#configyaml)).

Make a disposable git repository, and start a team in it:

```sh
mkdir ~/brg-first-team
cd ~/brg-first-team
git init
git commit --allow-empty -m "Start"
brg create engineering
```

`create` prints the team's tree. The team's ID is `engineering-` and four letters or digits, its lead's `lead-` and four more: `engineering-k3f9` and `lead-q8m3` below stand for yours. Run `brg status` until the lead shows `idle`. If it shows `blocked`, at its folder-trust screen or another prompt, open its session with `brg attach engineering-k3f9/lead-q8m3`, answer the prompt yourself, and detach with Ctrl-b, then d.

Send the lead a small task:

```sh
brg msg send engineering-k3f9/lead-q8m3 "Write HELLO.md in this folder with one line saying hello, then reply to me by message. Do it yourself; start no helpers."
```

`send` returns at once; the lead works in its own session, and `brg status` shows it `working` meanwhile. Its reply is a message to you, `external:owner`. Run `brg msg list --team engineering-k3f9 --full` until the reply appears, then see the result, and acknowledge the reply by its ID (`m2` here):

```sh
brg msg list --team engineering-k3f9 --full
cat HELLO.md
brg msg ack --team engineering-k3f9 m2
```

`brg attach` shows the lead's session at any time, and you can talk to the lead there too.

Stop the team when you're done:

```sh
brg stop engineering-k3f9
```

`brg start engineering-k3f9` brings it back, with its notes and messages. `brg destroy engineering-k3f9` stops it and deletes them.

## Install

The quick start installs from a clone. To install without one:

```sh
cargo install --git https://github.com/mrshll/borgchart --locked --bin brg borgchart
cargo install --git https://github.com/mrshll/borgchart --locked brg-seat
cargo install --git https://github.com/mrshll/borgchart --locked brg-slack
cargo install --git https://github.com/mrshll/borgchart --locked brg-checks
```

This needs Rust 1.89 or newer and tmux 3.2 or newer, plus the `claude` and/or `codex` CLIs. Building the Slack plugin also needs a C compiler for TLS. Nothing is published to crates.io yet.

From a checkout, `make install` replaces the binaries; configure template catalogs separately as described in [installing from a checkout](CONTRIBUTING.md#installing-from-a-checkout).

For tab completion of commands, teams and members in fish, run this once:

```sh
echo 'BRG_COMPLETE=fish brg | source' > ~/.config/fish/completions/brg.fish
```

Other shells are covered in [completion](docs/cli.md).


Seating, [Slack](plugins/slack/README.md) and [checks](plugins/checks/README.md) are optional Rust workspace plugins, declared by teams that need them. To install all four binaries and configure template catalogs, follow [installing from a checkout](CONTRIBUTING.md#installing-from-a-checkout). `cargo install --path .` installs only core; the plugins are `cargo install --path plugins/seat`, `cargo install --path plugins/slack` and `cargo install --path plugins/checks`.
