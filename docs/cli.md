# `brg` commands

`brg` is also installed as `borgchart`. A **team** is a folder holding `team.yaml`; an **instance** is a team running in one working folder, with its members, messages and notes ([concepts](concepts.md)). `brg create <team> <folder>` makes an instance; `start`, `stop` and `delete` act on it, and are safe to repeat.

**Naming things.**
- **A team** is a path to its folder (starting with `/`, `./`, `../` or `~/`) or a name looked up in the search paths of brg's configuration (`teams:` in `~/.config/borgchart/config.yaml`, or `$BORGCHART_CONFIG`; see [the format](format.md#configyaml)). Two folders with one name are refused, listing both; `teams/x` without `./` is refused with that hint.
- **An instance** is `<team>-<suffix>` (`dev-team-k3f9`), and a **member** `<role>-<suffix>` (`lead-q8m3`). A bare name is a role; only `create` takes one. Every member command and message refuses a role name, listing its kept members (including stopped ones) or naming `brg create <instance>/<role>` when it has none. Outside a member, name the instance in full: `dev-team-k3f9/lead-q8m3`. Inside a member, a bare member ID means its own instance (`$BORGCHART_INSTANCE`). brg never guesses an instance from a team's name or from which instances run.
- **A nested copy** has its full path as its instance ID, for example `oncall-h4k2/dev-a7k2`. Addresses resolve the longest registered instance prefix: `start oncall-h4k2/dev-a7k2` starts the copy, `stop oncall-h4k2/dev-a7k2/lead-c4nd` stops just its lead, and `create oncall-h4k2/dev-a7k2/helper` creates an agent member inside it. Copies can contain further copies. Every address segment must be a valid ID; empty segments, `.`, `..`, absolute paths and trailing `/` are refused before reading the registry.
- **Bus commands** (`msg`, `inbox`, `bus`) take `--instance ID`. It is required outside a member; inside one, it can only repeat the member's own instance.

`brg <command> --help` documents each command. The bus pages cover plain drafts and examples (`msg publish --help`), all unacked messages (`inbox --help`), the one wait and its outcomes (`inbox wait --help`), and store health (`bus check --help`).

| Command | What it does |
| --- | --- |
| `brg validate <team> [--json] [--probe]` | Check the team folder and brg's configuration without starting anything; `--probe` also lists each vendor's MCP connectors (this starts your MCP servers) |
| `brg create <team> <folder>` | Make a new instance of the team working in `<folder>` (required; it must exist), start its `start: true` roles and plugin processes, and print its ID and tree. Run it twice and you have two instances |
| `brg create <instance>/<role>` | For an agent role, make and start the next member (`<role>-<suffix>`) up to the role's `max` (default 1). For a team role, make a stopped nested copy and its folders; prepare its working folders before `start <instance>/<copy>`. Kept members and copies count, running or stopped. At the limit, list them and refuse; for `max: 1`, point to `start`. With a bus, a child needs its parent role to have a member |
| `brg start <instance>` | Bring the instance to what should run: the `start: true` roles and the members running at its last stop. Running members are left alone. Changed files apply only from a full stop |
| `brg start <instance>/<member>` | Start that member again, or say it is already running |
| `brg start <instance> --team <path>` | Point a stopped instance at its team folder's new place (the same team name), keeping its ID and messages |
| `brg stop <instance>[/<member>]` | Stop the instance (or one member). Stopping a stopped one changes nothing, and a second stop keeps the record of who ran |
| `brg delete <instance> [--force]` | Remove a stopped instance: its messages, notes, brg's files and its registration. Refused unless every member is certainly stopped (`--force` never overrides this). Unacked messages, or messages brg can't read, need `--force`. The working folder is never touched |
| `brg delete <instance>/<member> [--force]` | Remove a stopped member's live state, reserving its ID forever within the instance. Unacked messages are listed with a count and need `--force`; unreadable storage also needs `--force`. Running or unknown liveness is always refused. Kept children, running or stopped, are refused even with `--force` and listed by full address; delete them first. Immutable messages remain as history; messages to a deleted member no longer count as pending work and new ones are refused. `create <instance>/<role>` issues a new ID; `start <instance>` also replaces a deleted member of a `start: true` role. |
| `brg ls [--json]` | The teams in the search paths, each with its instances (running or stopped), then instances of teams elsewhere. Copies remain listed as registered instances with their full nested IDs and working folders |
| `brg status [<instance>] [--json \| -w \| -i]` | Show an instance tree, or, bare, every top-level instance with its copies nested once beneath their parent members. Each copy shows its team and how many of its own members run, with those members beneath it. A header names the team folder and working folder; members show vendor, model, state, positive unread count and details. A member at folder trust is `blocked (folder trust)` with its `brg attach` command. An agent or team role with no member or copy shows `not created (start: false)` or `not created (start: true)` for `max: 1`, or `on demand (none created)` for `max > 1`. Plugin processes follow their instance's members. `--json` gives the same tree order; every row's `instance` is its full instance path. Rows carry `kind` (`member`, `role`, `copy` or `plugin`), `name`, `team`, and applicable `member`, `role`, `parent`, `start_id`, `model`, `liveness` and `unread`. Copy rows carry their own instance path and a numeric `running` count, excluding deeper copies. Parents within an instance are local member IDs; a copy's parent is a full member address, and its root members point to the copy's instance path. `start_id` is retained after stop and changes on restart. A readable bus includes zero unread counts; no bus omits them, and an unreadable bus adds a check note. `-w` refreshes every `--interval` seconds; `-i` shows the same tree (j/k move, Enter attach, tmux prefix then `d` return, `r` refresh, `q` or Esc quit). Bare screens pick up instances created or deleted at the next refresh. Enter attaches to the selected member in its own instance; copy and role rows do not attach |
| `brg secret <team> [--json]` | List the secrets the team's plugins need and whether each is set in your environment. Prints no values and writes nothing |
| `brg attach <instance>/<member>` | Open a member's live session; detach with your tmux prefix, then `d` (press the prefix twice inside your own tmux) |
| `brg logs <instance>/<member> [-n LINES]` | Show recent output of a member, or of a plugin process (`<instance>/<plugin>`) |
| `brg send <instance>/<member> <text…> [--force] [--from MEMBER] [--json]` | Paste text into a member and press Enter, as one submission. `--from` (for plugins relaying a member's message) requires the target to be that member's parent |
| `brg msg publish [--record FILE\|-] [--external NAME] [--json]` | Publish a new message from `{to, body, reply_to?}` on stdin by default; each publish creates a distinct message. See [the bus](bus.md#commands) |
| `brg inbox [--member ID \| --external NAME] [--full] [--json]` | List every unacked message to you (or another member's inbox, read-only), oldest first; --full includes bodies. Reading never acknowledges |
| `brg inbox wait [--external NAME] [--timeout SECONDS] [--full] [--json]` | One wait for your inbox: new unacked messages return at once, existing messages after 60 seconds only if still unacked. Errors also wait at least 60 seconds; help/signals are exempt. Timeout 60–86400 seconds, default 86400, exits 9. A stopped/replaced member exits 8 (`fenced`); stop waiting |
| `brg msg read ID [--json]` | Show one message in full, including an acknowledged one |
| `brg msg ack ID… [--external NAME] [--json]` | Acknowledge messages addressed to you; acking twice is a no-op |
| `brg bus check [--json]` | Check the store for gaps, unreadable files and stray temporaries; never writes |

## Runtime

- **tmux:** brg runs its own tmux server (`tmux -L borgchart`) with its own config; your `~/.tmux.conf` is read only with `tmux.user_config` in brg's configuration. Each instance is one session, `brg-<instance>`, with one window per member and per plugin process (`_<plugin>`).
- **Folders:** members start in the instance's working folder, or in their role's `cwd` inside it. brg's own files live in the instance's state folder (`$XDG_STATE_HOME/borgchart/<instance>/`, or `$BORGCHART_STATE_HOME/<instance>/`): `messages/`, `notes/` and brg's private files. A working folder or role `cwd` that is the same as, inside or around brg's state folder is refused, and so is a `locked` member's folder around the team folder; a state folder under `/tmp` is refused.

- **Copies:** `{member_id}` in `cwd` is optional: without it, copies share the role's folder. If used, it must be the last folder name, with no `..`. When `cwd` ends in `{member_id}`, brg makes only the missing empty leaf when issuing a new member (`create`, or a full `start` that gives a `start: true` role a new member), for any `max`; its ancestors must exist, and an existing leaf is used as is. The parent puts a worktree there before briefing the copy. Starting an existing member never creates its folder, and `delete` leaves them alone. Kept copies of a `{member_id}` role cannot have equal, nested or symlinked aliases of each other's folders, even while stopped; different roles may still share a folder. Creation and every start check folders under one registry lock. A `start: true` role gets one member at instance creation, and `start` reuses kept members without filling to `max`. Lowering `max` after a full stop keeps all members and refuses creation until enough are deleted. A member-less `max > 1` role appears as a dim `on demand (none created)` line in the tree, and with that state in `--json`.
- **Member lifecycle callers:** with a bus, a person may act on anything. A member at its latest running start may create, start, stop or delete members and team copies anywhere in its subtree. Authority follows recorded parent edges within an instance and across copies; the caller's start is checked in its own instance under the registry lock that covers the change. Members cannot manage themselves, ancestors, siblings or independent instances. A copy's roots cannot manage their own copy or each other. Stopped, replaced and incomplete starts are refused. Creating a child requires a kept parent member. Parent roles and plugin `delivers_to:` targets must have `max: 1`. No-bus teams retain their existing caller rules.
- **Checkouts:** a start is refused when a member's folder is the same as, inside or around a folder another instance holds: one of its members runs there, or brg can't tell that member stopped. A stopped instance holds no folder against other instances. Different roles of one instance may share a folder.
- **Old sessions:** while a tmux session from before instances (`brg-<name>-<hash>`) is alive on brg's server, every start is refused, listing it. Stop it with the brg that started it.
- **Writable folders:** `vendor`-posture members can write the instance's `messages/` and `notes/` (Codex: sandbox writable roots; Claude: `--add-dir`); a role's own `args` setting either replaces brg's. `locked` members get only their granted plugins' data and their temp folder.
- **Environment:** windows start with a small environment copied from your shell (`PATH`, `HOME`, `LANG`, `TMPDIR`, `XDG_*`, `BORGCHART_CONFIG`, …), plus what each needs: `BORGCHART_INSTANCE`, `BORGCHART_MEMBER`, `BORGCHART_ROLE`, `BORGCHART_NOTES`, `BORGCHART_BIN` and, for bus members, the current start ID `BORGCHART_INCARNATION`. With `auth: api_key`, each member gets its own vendor's key (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) by name; with `login`, neither. Plugin secrets come from the environment `create` or `start` runs in, live only in 0600 files in the instance's state folder, read by brg's shims, and are removed at `brg stop`. 0600 keeps other users out, not members: a member that isn't `locked` could read the file while it exists.
- **`locked` start-up check:** a `locked` member's gate must report in when it starts. If it doesn't, or the member stops at any prompt first, the start stops it rather than let it run unprotected. The one exception is its vendor's folder-trust screen (Claude or Codex): the start leaves it there and says to attach. brg never answers Codex's, and answering either yourself is your decision to trust that folder; `send` still refuses the member until its gate reports in.
- **Inactive gate:** `send` refuses any `locked` member whose gate has not reported in, and `status` shows it as `gate-inactive`.
- **Copy mode:** a member's pane that tmux has in copy mode (or another mode, after scrolling back in `brg attach`) would send keys to tmux, not the member. So `send`, the wake and a start's folder-trust answering type nothing into it until it leaves that mode (usually `q`), and `status` adds that to the member's details.

## States

`status` reads each member's state from its screen with per-vendor rules (bundled; override in `$XDG_CONFIG_HOME/borgchart/rules/<vendor>.yaml`):

| State | Meaning |
| --- | --- |
| `idle` | Waiting for input |
| `working` | In a turn |
| `blocked` | At a prompt that needs a person (a permission, a question); `blocked (folder trust)` at its vendor's folder-trust screen, with the detail "waiting at folder trust: `brg attach <instance>/<member>`" (`--json` keeps `"state": "blocked"` with `"rule": "trust-dialog"`) |
| `unknown` | Running, but no rule matched (e.g. after a vendor update changed the screen) |
| `gate-inactive` | A `locked` member whose gate hasn't reported in; it gets no input |
| `exited` | The program ended (with its status when tmux knows it) |
| `not created (start: false)` or `not created (start: true)` | A role with no member yet (a row with `kind: role`), following its `start` setting |
| `on demand (none created)` | A `max > 1` role with no kept members |
| `not started` | A member that has never started in the instance |
| `stopped` | Ran and has ended; printed `stopped (should be running)` for a `start: true` member while the rest of the instance runs, with `"expected_running": true` in `--json` |

States are advisory: they gate `send` for safety, not access.

## `brg send`

- **One submission:** the text is pasted as one bracketed paste followed by Enter, so multi-line text arrives as one message. A per-member lock keeps concurrent sends from interleaving.
- **Refusals:** a member that isn't running, is `blocked`, or is `unknown` (unless `--force`); a `vendor`-posture member that is mid-turn; a member whose pane is in tmux copy mode (exit 4, even with `--force`); text with control characters other than newline and tab; text over 16 KiB; text starting with `/` or `!`.
- **Second check:** after pasting, `send` reads the screen again and presses Enter only if no prompt appeared and the pane didn't go into copy mode. If either happened, the pasted text stays in the member's input box (exit 4); attach to deal with both.
- **Paste mode:** text goes in as a bracketed paste when the member's CLI has turned that mode on, as Claude Code and Codex do. Otherwise tmux turns each newline into Enter, so multi-line text arrives as several lines.
- **Exit codes:** `--json` prints the same as `{ok, code, message}`.

| Code | Meaning |
| --- | --- |
| 0 | Submitted (not proof the agent started working) |
| 2 | Usage |
| 3 | Member not running |
| 4 | Needs a person: the member is blocked at a prompt, or its pane is in tmux copy mode |
| 5 | State unknown, or text refused |
| 6 | Busy: a `vendor`-posture member is mid-turn (a dialog could appear and catch the Enter) |
