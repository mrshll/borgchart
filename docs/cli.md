# `brg` commands

`brg` is also installed as `borgchart`. Every command finds its deployment from `--file`, then `$BORGCHART_FILE`, then `./borgchart.yaml`, then a `./team.yaml` on its own (`--team DIR` for another directory).

| Command | What it does |
| --- | --- |
| `brg validate [--json] [--probe]` | Check the team and deployment without starting anything; `--probe` also lists each vendor's MCP connectors (this starts your MCP servers) |
| `brg up [--member ID]` | Start the team (members with `start: true` and plugin processes), or one member |
| `brg down [--member ID]` | Stop the team, or one member |
| `brg status [--json \| -w \| -i]` | Show each member and plugin process: state, posture, details. `-w` refreshes the table every `--interval` seconds; `-i` is an interactive screen (j/k to move, Enter to attach, your tmux prefix then `d` to come back, `r` to refresh, `q` to quit) |
| `brg attach <member>` | Open a member's live session; detach with your tmux prefix, then `d` (press the prefix twice inside your own tmux) |
| `brg logs <member> [-n LINES]` | Show recent output of a member or plugin process |
| `brg send <member> <text…> [--force] [--from MEMBER] [--json]` | Paste text into a member and press Enter, as one submission. `--from` (for plugins relaying a member's message) requires the target to be that member's parent |
| `brg msg publish --key KEY [--record FILE\|-] [--external NAME] [--json]` | Publish a bus record from a JSON draft (stdin by default). A retry with the same key returns the same record; see [the bus](bus.md#commands--records) |
| `brg inbox [--member ID \| --external NAME] [--after ID] [--limit N] [--all] [--full] [--json]` | List records addressed to you (or another inbox, read-only), oldest first, a page at a time. Never acknowledges |
| `brg inbox wait … --timeout SECONDS` | Wait (up to 600 s) for a new unacknowledged record; exits 9 on timeout |
| `brg msg read ID [--json]` | Show one record in full |
| `brg msg ack ID… [--external NAME] [--json]` | Acknowledge records addressed to you; acking twice is a no-op |
| `brg bus inspect [--last N] [--json]` | Summarize the team's messages: members, inbox counts, recent records, store health |
| `brg bus check [--json]` | Check the store (gaps, unreadable files, stray temporaries) and the storage's age. Never writes |

## Runtime

- **tmux:** brg runs its own tmux server (`tmux -L borgchart`) with its own config; your `~/.tmux.conf` is not read. Each deployment is one session, with one window per member and per plugin process (`_<plugin>`).
- **Environment:** windows start with a small environment copied from your shell (`PATH`, `HOME`, `LANG`, `TMPDIR`, `XDG_*`, …), plus what each needs. Plugin secrets live only in 0600 files under the state directory (`$XDG_STATE_HOME/borgchart`), read by brg's shims, and are removed at `brg down`.
- **`locked` start-up check:** a `locked` member's gate must report in when it starts. If it doesn't, or the member stops at any prompt first, `up` stops it rather than let it run unprotected. The one exception is Claude's folder-trust prompt, since nothing runs before it is answered.
- **Inactive gate:** `send` refuses any `locked` member whose gate has not reported in, and `status` shows it as `gate-inactive`.

## States

`status` reads each member's state from its screen with per-vendor rules (bundled; override in `$XDG_CONFIG_HOME/borgchart/rules/<vendor>.yaml`):

| State | Meaning |
| --- | --- |
| `idle` | Waiting for input |
| `working` | In a turn |
| `blocked` | At a prompt that needs a person (a permission, a question, folder trust) |
| `unknown` | Running, but no rule matched (e.g. after a vendor update changed the screen) |
| `gate-inactive` | A `locked` member whose gate hasn't reported in; it gets no input |
| `exited` | The program ended (with its status when tmux knows it) |
| `stopped` | Not running |

States are advisory: they gate `send` for safety, not access.

## `brg send`

- **One submission:** the text is pasted as one bracketed paste followed by Enter, so multi-line text arrives as one message. A per-member lock keeps concurrent sends from interleaving.
- **Refusals:** a member that isn't running, is `blocked`, or is `unknown` (unless `--force`); a `vendor`-posture member that is mid-turn; text with control characters other than newline and tab; text over 16 KiB; text starting with `/` or `!`.
- **Second check:** after pasting, `send` reads the screen again and presses Enter only if no prompt appeared. If one did, the pasted text stays in the member's input box (exit 4); attach to deal with both.
- **Paste mode:** text goes in as a bracketed paste when the member's CLI has turned that mode on, as Claude Code and Codex do. Otherwise tmux turns each newline into Enter, so multi-line text arrives as several lines.
- **Exit codes:** `--json` prints the same as `{ok, code, message}`.

| Code | Meaning |
| --- | --- |
| 0 | Submitted (not proof the agent started working) |
| 2 | Usage |
| 3 | Member not running |
| 4 | Member blocked at a prompt |
| 5 | State unknown, or text refused |
| 6 | Busy: a `vendor`-posture member is mid-turn (a dialog could appear and catch the Enter) |
