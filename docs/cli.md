# `brg` commands

`brg` is also installed as `borgchart`. A **team** is a folder holding `team.yaml`; an **instance** is a team running in one working folder, with its members, messages and notes ([concepts](concepts.md)). `brg create <team> <folder>` makes an instance; `start`, `stop` and `delete` act on it, and are safe to repeat.

**Naming things.**
- **A team** is a path to its folder (starting with `/`, `./`, `../` or `~/`) or a name looked up in the search paths of brg's configuration (`teams:` in `~/.config/borgchart/config.yaml`, or `$BORGCHART_CONFIG`; see [the format](format.md#configyaml)). Two folders with one name are refused, listing both; `teams/x` without `./` is refused with that hint.
- **An instance** is `<team>-<suffix>` (`dev-team-k3f9`), and a **member** `<role>-<suffix>` (`lead-q8m3`). A bare name is a role; only `create` takes one. Outside a member, name the instance in full: `dev-team-k3f9/lead-q8m3`. Inside a member, a bare member ID means its own instance (`$BORGCHART_INSTANCE`). brg never guesses an instance from a team's name or from which instances run.
- **Bus commands** (`msg`, `inbox`, `task`, `bus`) take `--instance ID`. It is required outside a member; inside one, it can only repeat the member's own instance.

`brg <command> --help` documents each command. The bus pages are written for agents in a team and need nothing else: `msg publish --help` (draft fields, every record kind with its facts, operation keys, an example, outcomes), `inbox --help` and `inbox wait --help` (the `--after` cursor, acknowledging, outcomes), and `task --help` (the task lifecycle and releasing a task's writers).

| Command | What it does |
| --- | --- |
| `brg validate <team> [--json] [--probe]` | Check the team folder and brg's configuration without starting anything; `--probe` also lists each vendor's MCP connectors (this starts your MCP servers) |
| `brg create <team> <folder>` | Make a new instance of the team working in `<folder>` (required; it must exist), start its `start: true` roles and plugin processes, and print its ID and tree. Run it twice and you have two instances |
| `brg create <instance>/<role>` | Make that role's member (`<role>-<suffix>`) and start it. Refused when the role has its member already (one per role for now), running or stopped: start that one instead |
| `brg start <instance>` | Bring the instance to what should run: the `start: true` roles, the members running at its last stop, and members holding open work. Running members are left alone. Changed files apply only from a full stop |
| `brg start <instance>/<member>` | Start that member again, or say it is already running |
| `brg start <instance> --team <path>` | Point a stopped instance at its team folder's new place (the same team name), keeping its ID and messages |
| `brg stop <instance>[/<member>]` | Stop the instance (or one member). Stopping a stopped one changes nothing, and a second stop keeps the record of who ran |
| `brg delete <instance> [--force]` | Remove a stopped instance: its messages, notes, brg's files and its registration. Refused unless every member is certainly stopped (`--force` never overrides this). Open tasks, open questions or unread messages, or messages brg can't read, need `--force`. The working folder is never touched |
| `brg ls [--json]` | The teams in the search paths, each with its instances (running or stopped), then instances of teams elsewhere |
| `brg status [<instance>] [--json \| -w \| -i]` | Show an instance (or, bare, every instance) as a tree: a header line with its team folder and working folder, each member with its vendor, model, state and details, a role with no member yet as `on demand (none created)`, then each plugin process. `--json` gives the rows in tree order: `kind` (`member`, `role` or `plugin`), `name`, `instance`, `team`, `member`, `role`, `parent` (a member ID), `model` and, with a `bus:`, `liveness`. `-w` refreshes every `--interval` seconds and `-i` is an interactive screen (j/k to move, Enter to attach, your tmux prefix then `d` to come back, `r` to refresh, `q` to quit); both need an instance |
| `brg secret <team> [--json]` | List the secrets the team's plugins need and whether each is set in your environment. Prints no values and writes nothing |
| `brg attach <instance>/<member>` | Open a member's live session; detach with your tmux prefix, then `d` (press the prefix twice inside your own tmux) |
| `brg logs <instance>/<member> [-n LINES]` | Show recent output of a member, or of a plugin process (`<instance>/<plugin>`) |
| `brg send <instance>/<member> <text…> [--force] [--from MEMBER] [--json]` | Paste text into a member and press Enter, as one submission. `--from` (for plugins relaying a member's message) requires the target to be that member's parent |
| `brg msg publish --key KEY [--record FILE\|-] [--external NAME] [--json]` | Publish a bus record from a JSON draft (stdin by default). A retry with the same key returns the same record; see [the bus](bus.md#commands--records) |
| `brg inbox [--member ID \| --external NAME] [--after ID] [--limit N] [--all] [--full] [--json]` | List records addressed to you (or another inbox, read-only), oldest first, a page at a time. Never acknowledges |
| `brg inbox wait … --timeout SECONDS` | Wait (up to 600 s) for a new unacknowledged record; exits 9 on timeout |
| `brg inbox wait --background [--timeout SECONDS]` | Read-only wait for unseen IDs: records beyond the first snapshot return immediately; older unseen records and errors wait at least 60 s. Timeout 60–86400 s (default 24 h); explicit help and signals end it immediately |
| `brg msg read ID [--json]` | Show one record in full |
| `brg msg ack ID… [--external NAME] [--json]` | Acknowledge records addressed to you; acking twice is a no-op |
| `brg bus inspect [--last N] [--json]` | Summarize the instance's messages: members, inbox counts, recent records, store health |
| `brg bus check [--json]` | Check the store (gaps, unreadable files, stray temporaries), the storage's age and carries. Never writes |
| `brg bus carry --from RUN [--json]` | Carry unresolved tasks, open questions and unacknowledged records from the internal storage the current one is linked from, as linked notes; the old storage is only read. Idempotent; see [the bus](bus.md#commands--tasks) |
| `brg task list [--json]` | List tasks (state, assignee, attempt, next step) and open questions, folded from the instance's messages |
| `brg task show TASK [--json]` | Show one task: agreement, scope, checkout, each fact's record, writers and their liveness, history, next step, open questions |

## Runtime

- **tmux:** brg runs its own tmux server (`tmux -L borgchart`) with its own config; your `~/.tmux.conf` is read only with `tmux.user_config` in brg's configuration. Each instance is one session, `brg-<instance>`, with one window per member and per plugin process (`_<plugin>`).
- **Folders:** members start in the instance's working folder, or in their role's `cwd` inside it. brg's own files live in the instance's state folder (`$XDG_STATE_HOME/borgchart/<instance>/`, or `$BORGCHART_STATE_HOME/<instance>/`): `messages/`, `notes/` and brg's private files. A working folder or role `cwd` that is the same as, inside or around brg's state folder is refused, and so is a `locked` member's folder around the team folder; a state folder under `/tmp` is refused.
- **Checkouts:** a start is refused when a member's folder is the same as, inside or around a folder another instance holds: one of its members runs there, brg can't tell that member stopped, or an open task names it. Members of one instance may share a folder.
- **Old sessions:** while a tmux session from before instances (`brg-<name>-<hash>`) is alive on brg's server, every start is refused, listing it. Stop it with the brg that started it.
- **Writable folders:** `vendor`-posture members can write the instance's `messages/` and `notes/` (Codex: sandbox writable roots; Claude: `--add-dir`); a role's own `args` setting either replaces brg's. `locked` members get only their granted plugins' data and their temp folder.
- **Environment:** windows start with a small environment copied from your shell (`PATH`, `HOME`, `LANG`, `TMPDIR`, `XDG_*`, `BORGCHART_CONFIG`, …), plus what each needs: `BORGCHART_INSTANCE`, `BORGCHART_MEMBER`, `BORGCHART_ROLE`, `BORGCHART_NOTES` and `BORGCHART_BIN`. With `auth: api_key`, each member gets its own vendor's key (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) by name; with `login`, neither. Plugin secrets come from the environment `create` or `start` runs in, live only in 0600 files in the instance's state folder, read by brg's shims, and are removed at `brg stop`. 0600 keeps other users out, not members: a member that isn't `locked` could read the file while it exists.
- **`locked` start-up check:** a `locked` member's gate must report in when it starts. If it doesn't, or the member stops at any prompt first, the start stops it rather than let it run unprotected. The one exception is Claude's folder-trust prompt, since nothing runs before it is answered.
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
| `on demand (none created)` | A role with no member yet (a row with `kind: role`) |
| `not started` | A member that has never run in the instance's current storage |
| `stopped` | Ran and has ended; printed `stopped (should be running)` for a `start: true` member while the rest of the instance runs, with `"expected_running": true` in `--json` |

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
