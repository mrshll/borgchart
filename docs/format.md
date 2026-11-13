# Template and configuration format

A **team template** is a folder you can share: its roles (under `roles:`), what each one is told, which plugin programs they may use, and their shared message bus. **brg's configuration** (`config.yaml`) holds what belongs to you rather than to a template: auth, folder trust, where template names are looked up, shared plugins and tmux. A **team** is a template running in one working folder, given at `brg create <template> <folder>` ([concepts](concepts.md)).

```
my-team/
├── template.yaml           # roles, plugins, bus
├── triager.md          # one Markdown role file per role
├── lead.md
├── includes/           # shared text, pulled in with {{includes/<stem>}}
└── plugins/slackbot/   # a colocated plugin
```

`brg validate <template>` checks the template and the configuration before anything starts and reports every problem at once.

## YAML rules

Every borgchart file is YAML 1.2 parsed strictly:

- **Duplicate keys** are errors.
- **Booleans** are `true` and `false` only. `yes`, `no`, `on` and `off` are strings, so `start: no` is an error rather than a silent `false`.
- **Merge keys** (`<<`) are refused.
- **Size limits:** files over 1 MiB and runaway alias expansion are refused.
- **Unknown fields** are errors.

## `template.yaml`

```yaml
default: claude                 # vendor for roles that don't name one: claude | codex
includes: includes              # optional; template-local or catalog:shared/includes

plugins:
  slackbot:
    path: plugins/slackbot      # template-local; catalog:shared/…; shared:<name>; or omit for PATH
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
    prompt: triager.md          # the role file, relative to the template dir
    vendor: claude              # optional; default from `default`
    parent: lead                # optional; another role
    start: true                 # optional; false = its member is made with `brg create <team>/triager`
    max: 1                      # optional; limit on kept members, running or stopped (default 1)
    posture: locked             # optional: vendor (default) | locked
    worktree: false             # optional: true = each member gets its own git worktree
    remote_control: false       # optional: start Claude Remote Control, named by its full address
    args: []                    # optional: extra vendor launch arguments
    untrusted_input: true       # it reads text from outside (a channel, an inbox)
    plugins: {slackbot: [read, escalate]}
  lead:
    prompt: lead.md
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, reply]}
```

**Names.** Role, plugin and action names match `[a-z][a-z0-9_-]{0,31}`. A role's member is `<role>-<suffix>` (`triager-x7k2`). A role cannot be named `<another role>-<suffix>`, where the suffix is four characters from `23456789abcdefghjkmnpqrstuvwxyz`: that would also name a member. Environment names match `[A-Z_][A-Z0-9_]*`. The template's name is its folder's name, lowercased, with anything outside `[a-z0-9_-]` turned into `-`.

The role's identity is its key under `roles:`. `brg delete <team>/<member>` removes a stopped member; its ID stays issued and is never reused. A later `create <team>/<role>` gets a fresh ID.

**Messages.** Every team has a bus. `msg send TARGET TEXT` records work, then may type it; ordinary recipients explicitly ack; a Claude member uses one `msg wait`, and a Codex member hears new messages typed or queued into its session. `status` shows ordinary unacked counts. Omit `bus` unless configuring a real `bus: {alert: {plugin: notify, action: alert}}` action; automatic alert dispatch is not wired. Locked recipients have immutable no-ack-required history, no inbox recovery and no new grants. See [the bus](bus.md).

**`interactive`.** An agent role's boolean, default false, applies to every member and is frozen at start. True means brg never pastes or presses Enter, even with `msg send --allow-unknown`: a Claude member is inbox-only, and a Codex member gets each message through `codex queue` as its next turn. Use it for a lead whose session a person shares; remote-session users may leave it false. It is invalid with `posture: locked` or on a nested `template:` role. The bundled dev-team lead is interactive; helpers keep the false default.

**`remote_control`.** An agent role's boolean, default false; Claude-only and requires your claude.ai login (`auth: login`, not `auth: api_key`); on Team and Enterprise plans an Owner must first enable Remote Control in Claude Code's admin settings, and it isn't available through Bedrock, Google Cloud, Foundry or a custom `ANTHROPIC_BASE_URL`, where the member runs without a remote session or link. True adds `--remote-control <team>/<member>` on fresh starts and resumes, naming the session by its full member address at [claude.ai/code](https://claude.ai/code) and in the Claude app under Code. On first use, if a member shows `blocked` at a Remote Control consent dialog, attach and accept it once, or run `/remote-control` in any Claude session of yours first. `brg status` and `status -w` link a running Claude member's name on a terminal, including sessions enabled by hand, and `status --json` gives `remote_control_url` when known: the address comes from an undocumented field in Claude Code's own session file under `<CLAUDE_CONFIG_DIR or ~/.claude>/sessions/`, matched by the member's recorded session; missing or changed data leaves the name plain and the JSON field absent (checked on Claude Code 2.1.322). Remote prompts arrive over Claude Code's own connection, not the terminal, so they shouldn't disturb the guarded wake's input-box checks; at worst a reminder queues behind a remote turn.

**Catalog assets.** Sibling templates may share includes and plugins from the fixed `shared/` folder beside them. Set `includes: catalog:shared/includes` and a plugin's `path: catalog:shared/plugins/<name>`. These paths are relative to the directory containing the team template and confined to its real `shared/` folder. Other catalog prefixes, absolute paths, `..`, a symlinked shared root and symlink escapes are refused. Ordinary includes/plugins and every prompt file retain their template-root confinement. The existing `shared:<name>` plugin syntax still uses the global `plugins_dir`; it is distinct from catalog assets.

**Team roles.** A role with `template:` creates a child team instead of an agent:

```yaml
roles:
  lead: {prompt: lead.md}
  dev: {template: dev, parent: lead, start: false, max: 2, worktree: true}
```

A team role accepts only `template`, `parent`, `start`, `max` and `worktree`. Agent options, including `prompt`, `vendor`, `posture`, `plugins`, `connectors`, `interactive` and `args`, are errors even when empty or false. `start` must be false or absent: `create <team>/dev` makes a stopped child team (and its worktree), which the parent starts with `start <team>/<child-team>`.

`template` is a template name, resolved through the configured `templates` directories exactly like `brg create <name>`. Missing and ambiguous names are refused; path forms and the old nested template form are not supported. `max` defaults to 1 and must be at least 1. Each runtime tree shares one message store at its root. `validate` checks referenced templates recursively and reports their errors at their own files. A template may not contain itself, directly or indirectly, as judged by resolved folders. Creation and a child team's full start also check its recorded ancestors, including after a template-folder move.

New child teams resolve template names through the current catalog. Each child team records its source folder and reads its current files when a member starts. If a kept child team's source folder is gone, starting it errors without looking up a replacement by name. Explicit `start <child-team> --template <path>` remains available to recover a moved folder.

**Parents.** A `parent` must be another agent role with `max: 1`, and parent links can't form a cycle. A plugin's `delivers_to:` also requires a `max: 1` agent role. A member's parent is the member of its parent role. Messages a plugin routes "to the parent" go along this edge. A person may manage anything; a member at its latest running start may create, start, stop or delete anything in its subtree. Authority follows recorded parent edges within teams and across child teams, at every depth, and is checked under the registry lock with the caller's start in its own team. Members cannot manage themselves, ancestors, siblings or independent teams. A child team's roots cannot manage their own child team or each other. Stopped, replaced and incomplete starts are refused. Creating a child requires its parent role to have a member.

**`env`.** A map of names (`{A: , B: value}`); a name without a value is a secret. Secrets come from the environment `brg create` or `brg start` runs in: brg stores none. `brg secrets <template>` lists them and whether each is set.

**`worktree`.** Members work in the team's working folder (a `locked` Codex member: in its temp folder). With `worktree: true`, each member, or each child team of a team role, gets its own git worktree from the team's folder instead: brg adds it when it issues the member (`create`, or a full `start` that gives a `start: true` role a new member), detached at the folder's `HEAD`, or on a new branch with `create <team>/<role> --branch <name>`. The folder must be in a git repository with a commit. The worktree lives beside brg's state folder, at `<state>-worktrees/<address>` (`~/.local/state/borgchart-worktrees/` by default, with the address's `/` written as `.`); the command that adds it names it, and `status --json` gives it as `worktree`. Starting an existing member never adds one; if it is gone, the start is refused. `delete` removes it with `git worktree remove` when it is clean; it refuses while the worktree holds uncommitted or untracked changes or commits on no remote, and `--force` deletes the member but leaves the worktree as it is. A child team of a team role without `worktree: true` works in its parent's folder.

For a tree rooted in a plain folder of clones, `create <team>/<role> --repo <name>` selects a clone directly in the tree's top folder for a `worktree: true` role. The name must be one child directory name: no paths, symlinks, linked worktrees (`.git` files), bare repositories or symlinked `.git` directories. brg queries `origin` for its default branch and fetches it before taking the registry lock, then starts at the fetched tip, detached or on `--branch <name>`. Missing origin, unavailable credentials or a failed fetch refuse creation before issuing anything. The clone's checked-out branch, index and working files stay as they are. Without `--repo`, the start remains the working folder's `HEAD`; an implementer inside a child team inherits that child team's committed work. Use one child team per repository; the root lead sequences them. Repository selection is a creation option, never a template-file field or stored setting. Deletion, cleanup and destroy find ownership through the original repository and real child clones' own worktree records; ambiguous ownership is refused.

**`args`.** Extra vendor launch arguments. Treat them as trusted, like the rest of the template: they can widen an agent's permissions. A `-c check_for_update_on_startup=…` (Codex) overrides brg's default of turning the update check off, so an unattended member doesn't stop at Codex's "Update available" prompt; `--add-dir` (Claude) replaces the folders brg adds, and a `-c projects…` replaces brg's Codex trust. Custom Codex vendor-posture permission overrides (`-s`/`--sandbox`, `-a`/`--ask-for-approval`, or equivalent modes) are unsupported; brg supplies full access and no approvals.

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
- The role's name comes from `roles:` in `template.yaml`; frontmatter holds only `models`.
- **`models`** maps a vendor to `"<model>"` or `"<model> <effort>"`.
  - The effort is one of `minimal`, `low`, `medium`, `high`, `xhigh`, `max`.
  - Only the member's own vendor needs an entry; without one, the vendor's default model is used (with a warning).
  - Every supplied vendor pin is checked, including the other vendor's pin.
  - Model names are passed to the vendor unchanged.
- **Create-time choices:** `brg create <team>/<agent-role> --set vendor=codex,model=example-model,effort=medium` replaces only the keys given. A child team takes role-prefixed keys, such as `--set lead.vendor=codex`. A vendor-only choice selects its pin; model alone keeps the pin's effort; effort alone keeps the model. Vendor changes on roles with `args`, non-empty `connectors` or `remote_control` are refused. `posture: locked` and `connectors: []` work on either vendor.
  - `team.json` stores only explicit keys: an optional `overrides` object on an issued member, or a map from role to overrides on a child team's record. Empty choices are omitted. Start, resume and status resolve the template, then team choices, then member choices, key by key. Edits to pins apply on the next start unless that key was explicitly chosen. An invalid stored choice refuses the affected start and names the member or child team to delete and create again; unrelated starts and creates still work. Status retains the rows with the refusal in `detail`, the recorded vendor if any (null for an uncreated role), null model and effort, and the explicit `overrides`.
- **The body** is the member's standing instruction, sent as its opening prompt.
- **Includes:** a line that is exactly `{{includes/<stem>}}` is replaced by `<includes>/<stem>.md`. Includes are one level deep: an included file can't include another.
- **Another template's role file:** a line that is exactly `{{templates/<template>/<role>}}` is replaced by the body of that role's file. Its includes resolve in its own template, and its frontmatter is left out.
  - The template is found by name, like a `template:` reference. A file reached this way, and any include, can't use the directive.
  - The size limit covers the result. Nothing is pinned: if the template goes missing or becomes ambiguous, every command that reads the including template refuses, `status` included. `validate` lists each role's files in order.
- **Paths:** prompts, includes and plugin paths must stay inside the template directory, and another template's role file inside its own. `..` and symlinks that lead outside are refused.

### Reusing a shared template

A private template can be a shared one plus additions, kept in a catalog of your own listed beside the shared one under `templates:`. Its `template.yaml` is a complete template file: it declares the roles and plugins the shared text relies on, plus its own (say an `oncall` plugin and a `support` child team). Templates such as `dev-team` resolve by name across both catalogs. Each role file takes the shared text with one line and adds its own:

```markdown
---
models: {claude: opus high}
---
{{templates/engineering/lead}}

When a release is blocked, page the on-call engineer with `oncall page <text>`.
```

## Plugins

A plugin is a plain script or program, in any language, that members run themselves. borgchart does not wrap plugins in an MCP server.

- **What brg runs:** brg launches only the programs `template.yaml` lists. This limits what brg starts; it is not a sandbox. A listed program, or a member with a shell, can run anything.
- **Location:**
  - `path: plugins/<dir>`: a directory inside the template. It is also the programs' working directory.
  - `path: catalog:shared/<path>`: a shared plugin in the sibling catalog asset folder, confined as above.
  - `path: shared:<name>`: `<plugins_dir>/<name>`, where `plugins_dir` is set in brg's configuration. Where it resolves is frozen while a team runs.
  - No `path`: the commands are whatever those names resolve to on `PATH` when the team starts. The plugin process runs in the template folder; actions run in the member's folder.
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

How much a member may do is set per role, in `template.yaml`.

- **`vendor`** (default): the agent uses your settings and MCP connectors, with these permission rules:
  - **Codex:** brg passes `--sandbox danger-full-access --ask-for-approval never`. Shell commands have full host access and run without tool approvals. This also applies to vendor-posture researchers using `--search`. Folder trust is separate: `auto_trust: true` supplies session-only trust; otherwise an untrusted folder can still stop at its trust dialog.
  - **Claude:** brg leaves the vendor's permission settings unchanged. If it stops at an approval prompt, `brg status` shows it `blocked` and you attach to answer.
  - These launch settings do not authorize work beyond the owner's task or change the team's review and merge rules.
- **`locked`:** for members that read untrusted input.
  - The only tool calls allowed are the member's granted action lines, with plain arguments. That is enforced by brg's own pre-tool-use gate on both Claude and Codex, plus each vendor's no-prompt mode.
  - Everything else is denied without asking: other tools, shell metacharacters, expansions, redirects.
- **`untrusted_input: true`** marks a member that reads text from outside. Such a member must be `locked` unless it also says `accept_risk: true`.

**What `locked` does and doesn't do.** The gate is the barrier: it decides every tool call.
- **Claude:** `locked` has no sandbox. Granted plugin commands run with your full rights.
- **Codex:** `locked` keeps network on, and Codex's sandbox lets commands read any file. Writes are limited to the granted plugins' data directories, the member's temp directory and its working directory. A `locked` Codex member without `worktree: true` starts in its temp directory. It never gets the team's messages or notes, and its folder can't overlap the template folder. Plugin programs that need to write elsewhere, such as `uv` writing its cache, fail inside the sandbox; give such a plugin a program that doesn't (e.g. plain `python3` for a standard-library script). brg doesn't move caches into member-writable directories, because a member could then plant code there.
- **Plugin sends from locked Codex:** the action queues the request in the member's temp directory and prints `queued; brg sends it when the command returns`. brg's session-only PostToolUse hook opens the queue once and sends outside the sandbox, as that member, to its parent only. Granted plugin programs can queue text for that parent under the bus's normal limits. The hook removes permanent refusals and reports failures as extra context. If brg cannot start, the send stays queued and is only retried at the next tool call, which may be the member's next wake; uncertain sends need inspection before retrying. Claude sends directly.
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
- **No connectors:** `connectors: []` starts with no MCP servers on either vendor, in either posture. Claude gets `--strict-mcp-config`. Codex gets `--disable apps --disable plugins` and an `enabled=false` override for every configured server, listed with `codex mcp list --json` in the member's folder and environment. If that bounded listing fails, that member is not started; fix the listing and retry. Codex server names must contain only ASCII letters, digits, `_` or `-`; other names prevent that member from starting, so rename them in Codex's config or drop `connectors: []`. With `connectors` absent, vendor settings apply; a locked Claude member without connectors still disables MCP, while locked Codex loads its configured servers and its gate denies their tools.
- **Acting as you:** connector actions act as **you**, not as a bot. A connector that reads mail or messages is untrusted input.
- **Auth:** Claude's own connectors need your login, so `auth: api_key` can't be used with members that declare them.
- **Probing:** `brg validate --probe` asks each vendor CLI which servers are configured (`claude mcp list`, `codex mcp list --json`, run in the template folder), and warns about missing names. An empty connector list expects nothing and is skipped. Claude's listing health-checks, and so starts, your MCP servers; its list is parsed from text output and may need updating across versions. Codex's listing doesn't start stdio servers, but contacts HTTP servers to check their sign-in status; each unresponsive HTTP server can add a few seconds. Probing can't check which account a connector acts as, or its scopes.

The start-time Codex listing receives brg's session trust override, but does not see the role's own `args`, such as `-c mcp_servers…` or `--profile`.

## `config.yaml`

brg's configuration is `~/.config/borgchart/config.yaml`, or the file `$BORGCHART_CONFIG` names. It is optional: a missing file means the defaults. Relative paths in it are relative to its folder, and `~/` is your home.

```yaml
schema: 1
auth: login                   # login (default) | api_key, for every member
auto_trust: true              # trust each member's folder without a person (Claude default: locked members only)
templates:                    # where template names are looked up, each a folder of templates
  - ~/.config/borgchart/templates # the default
plugins_dir: ~/borgchart-plugins   # where shared:<name> plugins live
tmux: {user_config: true}     # load your own tmux config on brg's server (default: false)
```

- **Settings:** members that start use current `auth`, `auto_trust` and template files. Plugin settings refresh after a full stop/start. A team retains its recorded template folder.
- **`auth: login`:** brg passes no API-key variables, so a stray key in your shell can't replace your login.
- **`auth: api_key`:** brg passes each member its own vendor's key by name: `ANTHROPIC_API_KEY` to Claude members, `OPENAI_API_KEY` to Codex members, from the environment `create` or `start` runs in. Nothing stores the key.
- **`auto_trust`:** trusts each member's folder (the team's working folder, or its own worktree), so the member doesn't stop at its vendor's folder-trust screen. Choosing the folder at `create` is your consent. Each trust is printed and shown in `brg status --json` (`trust_accepted`). Set `auto_trust: false` to attach and answer the screen yourself; `brg status` shows such a member as `blocked (folder trust)` with the `brg attach` command.
  - **Claude:** brg answers Claude's folder-trust dialog for exactly that folder by sending keys through tmux. It acts only on the plain trust screen showing that folder; it never answers any other dialog. While the member's pane is in tmux copy mode, it sends nothing and waits (within the start's start-up wait) for the pane to leave it. By default this applies to `locked` members only, because they ignore project settings. `auto_trust: true` extends it to every member; a start then notes if the folder has project settings (`.claude/`, `.mcp.json`), which would load in `vendor` posture.
  - **Codex:** only with `auto_trust: true`, and only in `vendor` posture. brg starts the member with `-c projects={"<folder>"={trust_level="trusted"}}`, so Codex never shows its trust screen. That trusts exactly that folder, for that session only: nothing is written to `~/.codex/config.toml`. Answering the screen yourself trusts the whole Git repository (every worktree too) and saves it. Trust lets the folder's `.codex/` config, hooks and MCP servers load, while brg's vendor-posture permission flags remain explicit; a start notes if the folder has a `.codex/`. A `-c projects…` in the role's `args` replaces brg's.
  - A `locked` Codex member is never trusted this way. It can write its own folder, so it could write a `.codex/` that would load outside its gate on its next start. `brg validate` warns, and its trust screen is left for you.
- **`tmux.user_config`:** brg's private tmux server normally reads only brg's own config, so `brg attach` doesn't have your prefix, key bindings, mouse settings or status bar (key bindings belong to the tmux server, not to the client that attaches). Set it to `true` to load the first of `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf` and `~/.config/tmux/tmux.conf` that exists, or to a path. Off by default.
  - The path can't contain `'`, `*`, `?`, `[`, `\` or a newline (tmux would read it as a glob pattern).
  - The config is read once, when brg's tmux server starts. Every team on that server shares it, so it is the setting brg's configuration had when the server started. If the server is already running with a different setting, a start says so; the change takes effect once every team on it is stopped. Its scripts run with brg's cleared environment.
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
