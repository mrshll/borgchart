# Team and configuration format

A **team** is a folder you can share: its roles (under `roles:`), what each one is told, which plugin programs they may use, and whether they share a message bus. **brg's configuration** (`config.yaml`) holds what belongs to you rather than to a team: auth, folder trust, where team names are looked up, shared plugins and tmux. An **instance** is a team running in one working folder, given at `brg create <team> <folder>` ([concepts](concepts.md)).

```
examples/slack-devops/
├── team.yaml           # roles, plugins, bus
├── triager.md          # one Markdown role file per role
├── lead.md
├── includes/           # shared text, pulled in with {{includes/<stem>}}
└── plugins/slackbot/   # a colocated plugin
```

`brg validate <team>` checks the team and the configuration before anything starts and reports every problem at once. A `borgchart.yaml` in a team folder is not read.

## YAML rules

Every borgchart file is YAML 1.2 parsed strictly:

- **Duplicate keys** are errors.
- **Booleans** are `true` and `false` only. `yes`, `no`, `on` and `off` are strings, so `start: no` is an error rather than a silent `false`.
- **Merge keys** (`<<`) are refused.
- **Size limits:** files over 1 MiB and runaway alias expansion are refused.
- **Unknown fields** are errors.

## `team.yaml`

```yaml
default: claude                 # vendor for roles that don't name one: claude | codex
includes: includes              # optional; directory for {{includes/…}} (default: includes)
bus: {}                         # optional: the members share a message bus; `bus: {alert: {plugin: notify, action: alert}}` configures an alert action (not wired now)

plugins:
  slackbot:
    path: plugins/slackbot      # relative to the team dir; or shared:<name>; or omit to use PATH
    process: [./watch.py]       # optional: a long-running program brg starts and stops
    delivers_to: triager        # optional: the role whose member the process hands work to
    env:                        # the only variables the plugin receives
      SLACK_BOT_TOKEN:          # no value: a secret, from the environment brg runs in
      SLACK_CHANNEL_ID: C0000EXAMPLE   # a plain value
    actions:                    # optional: named command lines
      read: slackbot read <ts>
      reply: slackbot reply <thread_ts> <text>
      escalate: slackbot escalate <ts> <severity> <reason>

roles:
  triager:
    prompt: triager.md          # the role file, relative to the team dir
    vendor: claude              # optional; default from `default`
    parent: lead                # optional; another role
    start: true                 # optional; false = its member is made with `brg create <instance>/triager`
    max: 1                      # optional; limit on kept members, running or stopped (default 1)
    posture: locked             # optional: vendor (default) | locked
    cwd: ops                    # optional: where its member starts, inside the instance's folder
    args: []                    # optional: extra vendor launch arguments
    untrusted_input: true       # it reads text from outside (a channel, an inbox)
    plugins: {slackbot: [read, escalate]}
  lead:
    prompt: lead.md
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, reply]}
```

**Names.** Role, plugin and action names match `[a-z][a-z0-9_-]{0,31}`. A role's member is `<role>-<suffix>` (`triager-x7k2`). A role cannot be named `<another role>-<suffix>`, where the suffix is four characters from `23456789abcdefghjkmnpqrstuvwxyz`: that would also name a member. Environment names match `[A-Z_][A-Z0-9_]*`. The team's name is its folder's name, lowercased, with anything outside `[a-z0-9_-]` turned into `-`.

The role's identity is its key under `roles:`. The old `members:` field and frontmatter `role:` are unknown fields, with no aliases. Each role keeps up to `max` members, running or stopped; `max` defaults to 1 and must be at least 1. `brg delete <instance>/<member>` removes a stopped member; its ID stays issued and is never reused. A later `create <instance>/<role>` gets a fresh ID. `create <team>` makes one member per `start: true` role, even with `max > 1`. `start <instance>` reuses kept members and only creates a member for a `start: true` role with none left; it never fills to `max`. Lowering `max` after a full stop keeps all existing members, and further creation is refused until enough are deleted.

**Messages.** With `bus:`, members publish plain `{to, body, reply_to?}` drafts, read and ack their inbox, and use one `inbox wait`. `brg status` (tree, `-i` and `--json`) shows unread message counts; no bus means no counts. The optional alert action and watcher are not wired now; the lead nudges with `brg send`. See [the bus](bus.md) for routing, storage, wait rules and commands.

**Parents.** A `parent` must be another role with `max: 1`, and parent links can't form a cycle. A plugin's `delivers_to:` also requires a `max: 1` role. A member's parent is the member of its parent role. Messages a plugin routes "to the parent" go along this edge. With a bus, only a person or the declared parent's latest running start may create, start, stop or delete a member. Other members, replaced starts and callers from another instance are refused. Creating a child requires its parent role to have a member. Teams without a bus retain their existing caller rules; whole-instance operations retain theirs too.

**`env`.** A list of names (`[A, B]`, each a secret), list items with a value (`[A, {B: value}]`), or a map (`{A: , B: value}`); a name without a value is a secret. Secrets come from the environment `brg create` or `brg start` runs in: brg stores none. `brg secret <team>` lists them and whether each is set.

**`cwd`.** Relative to the instance's working folder and inside it (no `..`, no absolute path, and it may not leave the folder through a symlink); it may use `{member_id}`, the member's ID. It must exist when the member starts. A role without one starts in the working folder (a `locked` Codex member: in its temp folder).

`{member_id}` is optional: without it, copies share the role's folder. If used, it must be the last folder name, such as `copies/{member_id}`. When `cwd` ends in `{member_id}`, brg makes only the missing empty leaf when issuing a new member (`create`, or a full `start` that gives a `start: true` role a new member), for any `max`; every folder above it must already exist, and an existing leaf is used as is. The parent can put a worktree there before briefing the copy. Starting an existing member never creates its folder, and `delete` leaves them alone. For a `{member_id}` role, creation and every start refuse a copy whose folder equals, contains or sits inside a kept copy's folder of the same role, including symlinked aliases and stopped copies; delete a member to free its folder. The checks and creation share the registry lock, so concurrent creates cannot claim overlapping folders. Different roles in one instance may still share a folder.

**`args`.** Extra vendor launch arguments. Treat them as trusted, like the rest of the team: they can widen an agent's permissions. A `-c check_for_update_on_startup=…` (Codex) overrides brg's default of turning the update check off, so an unattended member doesn't stop at Codex's "Update available" prompt; a `-c sandbox_workspace_write.writable_roots=…` (Codex) or `--add-dir` (Claude) replaces the folders brg makes writable, and a `-c projects…` replaces brg's Codex trust.

## Role files

```markdown
---
models:
  claude: haiku
  codex: example-model low
---
You read each new message in the channel and decide whether it needs the lead.

{{includes/escalation-rules}}
```

- **Frontmatter** sits between a first line `---` and the next line that is exactly `---`.
  - Empty or whitespace-only frontmatter uses the vendor's default model.
  - A byte-order mark and CRLF line endings are accepted.
  - A later `---` is ordinary body text.
- The role's name comes from `roles:` in `team.yaml`; frontmatter holds only `models`.
- **`models`** maps a vendor to `"<model>"` or `"<model> <effort>"`.
  - The effort is one of `minimal`, `low`, `medium`, `high`, `xhigh`, `max`.
  - Only the member's own vendor needs an entry; without one, the vendor's default model is used (with a warning).
  - Model names are passed to the vendor unchanged.
- **The body** is the member's standing instruction, sent as its opening prompt.
- **Includes:** a line that is exactly `{{includes/<stem>}}` is replaced by `<includes>/<stem>.md`. Includes are one level deep: an included file can't include another.
- **Paths:** prompts, includes and plugin paths must stay inside the team directory. `..` and symlinks that lead outside are refused.

### Older shape

A directory with `topology.yaml`, or member frontmatter with `name` and `claude_model`/`codex_model`, is the older shape. To convert it:

1. Rename `topology.yaml` to `team.yaml`.
2. Rename `members:` to `roles:` in `team.yaml`. Remove `name:` or `role:` from each role file's frontmatter.
3. Replace `claude_model: X` / `codex_model: Y` with `models: {claude: X, codex: Y}`.

## Plugins

A plugin is a plain script or program, in any language, that members run themselves. borgchart does not wrap plugins in an MCP server.

- **What brg runs:** brg launches only the programs `team.yaml` lists. This limits what brg starts; it is not a sandbox. A listed program, or a member with a shell, can run anything.
- **Location:**
  - `path: plugins/<dir>`: a directory inside the team. It is also the programs' working directory.
  - `path: shared:<name>`: `<plugins_dir>/<name>`, where `plugins_dir` is set in brg's configuration. Where it resolves is frozen while an instance runs.
  - No `path`: the commands are whatever those names resolve to on `PATH` when the instance starts.
- **`process`:** a long-running program, e.g. a watcher.
  - A full `brg start` (and `create`) starts it and `brg stop` stops it; it is not restarted.
  - It gets a small base environment plus the names in `env`, and `BORGCHART_DELIVER_TO` when `delivers_to` is set.
  - It may write `health.json` (`{"ok": true, "at": "<UTC time>", "detail": "…"}`) in its state directory, which `brg status` shows.
- **`actions`:** named command lines.
  - **The prompt:** granted lines appear in the member's opening prompt under "Commands you can run".
  - **The allow-list:** in `locked` posture, the literal words before the first placeholder are what the member may run (`slackbot escalate <ts> …` allows `slackbot escalate` with plain arguments).
  - **Placeholders** such as `<ts>` are documentation. The agent fills them in its own shell, so scripts must validate their arguments.
  - **The command** (the first word) must be an executable in the plugin directory.
- **`command`:** for a plugin without actions, the script whose `--help` output stands in for action lines.
- **Granting:** `plugins: {<plugin>: [<action>, …]}` on a member grants those actions; `["*"]` grants all of them, or the whole `command` when there are no actions.
  - brg puts a shim for each granted command first on the member's `PATH`, and the prompt lists the shim by absolute path.
  - The shim loads the plugin's secrets from a private file and runs the real script, so the agent's own environment never holds them.

## Postures

How much a member may do is set per role, in `team.yaml`.

- **`vendor`** (default): brg passes no permission flags.
  - The agent runs with your own Claude or Codex settings (e.g. auto mode, a sandbox, approval rules) and your MCP connectors.
  - If it stops at an approval prompt, `brg status` shows it `blocked` and you attach to answer.
- **`locked`:** for members that read untrusted input.
  - The only tool calls allowed are the member's granted action lines, with plain arguments. That is enforced by brg's own pre-tool-use gate on both Claude and Codex, plus each vendor's no-prompt mode.
  - Everything else is denied without asking: other tools, shell metacharacters, expansions, redirects.
- **`untrusted_input: true`** marks a member that reads text from outside. Such a member must be `locked` unless it also says `accept_risk: true`.

**What `locked` does and doesn't do.** The gate is the barrier: it decides every tool call.
- **Claude:** `locked` has no sandbox. Granted plugin commands run with your full rights.
- **Codex:** `locked` keeps network on, and Codex's sandbox lets commands read any file. Writes are limited to the granted plugins' data directories, the member's temp directory and its working directory. A `locked` Codex member whose role sets no `cwd` starts in its temp directory. It never gets the instance's messages or notes, and its folder can't overlap the team folder. Plugin programs that need to write elsewhere, such as `uv` writing its cache, fail inside the sandbox; give such a plugin a program that doesn't (e.g. plain `python3` for a standard-library script). brg doesn't move caches into member-writable directories, because a member could then plant code there.
- **The tmux socket:** brg's tmux socket is a full capability. Anything that can reach it can type into, read or stop any member. That is why `locked` members get no general shell.
- **Fail closed:** if a `locked` member's gate doesn't report in when it starts, it is stopped, or (behind Claude's folder-trust prompt) gets no input until the gate does report in.

**What `brg validate` warns about.** A `vendor`-posture member that:

- receives work from a plugin process (a `delivers_to` target);
- is the parent of such a member;
- or is granted a plugin that runs a process.

**Safety depends on posture.** In `vendor` posture, an agent that reads untrusted text can be steered into anything its vendor mode allows, including acting as you through your connectors.

## Connectors

A member can use MCP connections you've already set up in its vendor CLI, such as a personal Slack or mail connector. borgchart doesn't proxy or configure them.

```yaml
roles:
  helper:
    prompt: helper.md
    connectors: [Example Mail]                    # vendor posture: server names
  reader:
    prompt: reader.md
    posture: locked
    connectors: {Example Mail: [search, read]}    # locked posture: allowed tools
```

- **Names:** use server names exactly as `claude mcp list` / `codex mcp list` print them.
- **Acting as you:** connector actions act as **you**, not as a bot. A connector that reads mail or messages is untrusted input.
- **Auth:** Claude's own connectors need your login, so `auth: api_key` can't be used with members that declare them.
- **Probing:** `brg validate --probe` asks each vendor CLI which servers are configured (`claude mcp list`, `codex mcp list --json`, run in the team folder), and warns about missing names. Listing health-checks, and so starts, your MCP servers, which is why it only runs when asked. Claude's list is parsed from its text output and may need updating across versions. It can't check that a connector is signed in, which account it acts as, or its scopes.

## `config.yaml`

brg's configuration is `~/.config/borgchart/config.yaml`, or the file `$BORGCHART_CONFIG` names. It is optional: a missing file means the defaults. Relative paths in it are relative to its folder, and `~/` is your home.

```yaml
schema: 1
auth: login                   # login (default) | api_key, for every member
auto_trust: true              # trust each member's folder without a person (Claude default: locked members only)
teams:                        # where team names are looked up, each a folder of teams
  - ~/.config/borgchart/teams # the default
plugins_dir: ~/borgchart-plugins   # where shared:<name> plugins live
tmux: {user_config: true}     # load your own tmux config on brg's server (default: false)
```

- **Frozen:** `auth`, `auto_trust` and where `plugins_dir` puts each shared plugin are frozen from a start to the next full stop, with the team's files. A member started into a running instance with any of them changed is refused; stop the instance fully and start it again to apply them. The search paths aren't frozen: an instance records its team's folder.
- **`auth: login`:** brg passes no API-key variables, so a stray key in your shell can't replace your login.
- **`auth: api_key`:** brg passes each member its own vendor's key by name: `ANTHROPIC_API_KEY` to Claude members, `OPENAI_API_KEY` to Codex members, from the environment `create` or `start` runs in. Nothing stores the key.
- **`auto_trust`:** trusts each member's folder (the instance's working folder, or its role's `cwd`), so the member doesn't stop at its vendor's folder-trust screen. Choosing the folder at `create` is your consent. Each trust is printed and shown in `brg status --json` (`trust_accepted`). Set `auto_trust: false` to attach and answer the screen yourself; `brg status` shows such a member as `blocked (folder trust)` with the `brg attach` command.
  - **Claude:** brg answers Claude's folder-trust dialog for exactly that folder by sending keys through tmux. It acts only on the plain trust screen showing that folder; it never answers any other dialog. While the member's pane is in tmux copy mode, it sends nothing and waits (within the start's start-up wait) for the pane to leave it. By default this applies to `locked` members only, because they ignore project settings. `auto_trust: true` extends it to every member; a start then notes if the folder has project settings (`.claude/`, `.mcp.json`), which would load in `vendor` posture.
  - **Codex:** only with `auto_trust: true`, and only in `vendor` posture. brg starts the member with `-c projects={"<folder>"={trust_level="trusted"}}`, so Codex never shows its trust screen. That trusts exactly that folder, for that session only: nothing is written to `~/.codex/config.toml`. Answering the screen yourself trusts the whole Git repository (every worktree too) and saves it. Trust lets the folder's `.codex/` config, hooks and MCP servers load, and selects Codex's default permissions for a trusted folder; a start notes if the folder has a `.codex/`. A `-c projects…` in the role's `args` replaces brg's.
  - A `locked` Codex member is never trusted this way. It can write its own folder, so it could write a `.codex/` that would load outside its gate on its next start. `brg validate` warns, and its trust screen is left for you.
- **`tmux.user_config`:** brg's private tmux server normally reads only brg's own config, so `brg attach` doesn't have your prefix, key bindings, mouse settings or status bar (key bindings belong to the tmux server, not to the client that attaches). Set it to `true` to load the first of `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf` and `~/.config/tmux/tmux.conf` that exists, or to a path. Off by default.
  - The path can't contain `'`, `*`, `?`, `[`, `\` or a newline (tmux would read it as a glob pattern).
  - The config is read once, when brg's tmux server starts. Every instance on that server shares it, so it is the setting brg's configuration had when the server started. If the server is already running with a different setting, a start says so; the change takes effect once every instance on it is stopped. Its scripts run with brg's cleared environment.
  - After your config loads, brg sets again the options it relies on, so your config can't change them:
    - `allow-rename off` and `automatic-rename off`: brg finds members by window name.
    - `remain-on-exit on` and `history-limit 20000`: brg reads a member's screen and its exit.
    - An empty `update-environment`: an attaching client's variables don't reach members.
    - The server's own `default-shell`: brg starts each window through it.
    - `allow-set-title on` (on tmux versions that have it): brg reads members' pane titles.
    - `destroy-unattached off` and `exit-unattached off`: nothing ends brg's server or sessions while nobody is attached.
    - tmux's built-in `default-terminal` and the RGB terminal feature: members keep colour.
  - `brg validate` warns, with file and line numbers, about what can still act after that:
    - shell commands at load (`run`, `run-shell`, `if-shell`): a background script or one that sets options later can still override brg's;
    - hooks (`set-hook`) and plugin managers (tpm);
    - session save/restore plugins (resurrect, continuum): on brg's server they can save brg's sessions over the ones you saved from your own tmux, or restore yours into it;
    - sessions created or attached as the config loads: brg's server then outlives `brg stop`.
    The check is best effort. It reads each line's first command, skips key bindings, and follows files your config sources by an absolute or `~/` path. A relative include is reported, not followed: tmux finds it from the folder brg runs in.
  - To keep your own bindings without this, run `brg attach` from inside your own tmux: your outer server's prefix and bindings still work around it (press the prefix twice to reach brg's).
