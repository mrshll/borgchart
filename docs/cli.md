# `brg` commands

`brg` is also installed as `borgchart`. Every command finds its deployment from `--file`, then `$BORGCHART_FILE`, then `./borgchart.yaml`, then a `./team.yaml` on its own (`--team DIR` for another directory).

| Command | What it does |
| --- | --- |
| `brg validate [--json]` | Check the team and deployment without starting anything |
| `brg up [--member ID]` | Start the team (members with `start: true` and plugin processes), or one member |
| `brg down [--member ID]` | Stop the team, or one member |
| `brg status [--json]` | Show each member and plugin process: state, posture, details |
| `brg attach <member>` | Open a member's live session; detach with your tmux prefix, then `d` (press the prefix twice inside your own tmux) |
| `brg logs <member> [-n LINES]` | Show recent output of a member or plugin process |
| `brg send <member> <text…> [--force] [--json]` | Paste text into a member and press Enter, as one submission |

## Runtime

- **tmux:** brg runs its own tmux server (`tmux -L borgchart`) with its own config; your `~/.tmux.conf` is not read. Each deployment is one session, with one window per member and per plugin process (`_<plugin>`).
- **Environment:** windows start with a small environment copied from your shell (`PATH`, `HOME`, `LANG`, `TMPDIR`, `XDG_*`, …), plus what each needs. Plugin secrets live only in 0600 files under the state directory (`$XDG_STATE_HOME/borgchart`), read by brg's shims, and are removed at `brg down`.
- **`locked` start-up check:** a `locked` member's gate must report in when it starts, or `up` stops it ("did not report in") rather than let it run unprotected.

## States

`status` reads each member's state from its screen with per-vendor rules (bundled; override in `$XDG_CONFIG_HOME/borgchart/rules/<vendor>.yaml`):

| State | Meaning |
| --- | --- |
| `idle` | Waiting for input |
| `working` | In a turn |
| `blocked` | At a prompt that needs a person (a permission, a question, folder trust) |
| `unknown` | Running, but no rule matched (e.g. after a vendor update changed the screen) |
| `exited` | The program ended (with its status when tmux knows it) |
| `stopped` | Not running |

States are advisory: they gate `send` for safety, not access.

## `brg send`

- **One submission:** the text is pasted as one bracketed paste followed by Enter, so multi-line text arrives as one message. A per-member lock keeps concurrent sends from interleaving.
- **Refusals:** a member that isn't running, is `blocked`, or is `unknown` (unless `--force`); text with control characters other than newline and tab; text over 16 KiB; text starting with `/` or `!`.
- **Exit codes:** `--json` prints the same as `{ok, code, message}`.

| Code | Meaning |
| --- | --- |
| 0 | Submitted (not proof the agent started working) |
| 2 | Usage |
| 3 | Member not running |
| 4 | Member blocked at a prompt |
| 5 | State unknown, or text refused |
