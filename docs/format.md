# Team and deployment format

A **team** is a directory you can share: who the members are, what each one is told, and which plugin programs they may use. A **deployment** (`borgchart.yaml`) fills in values for one place the team runs: working directories, extra tasks, non-secret settings and auth.

```
examples/slack-devops/
├── team.yaml           # members and plugins
├── triager.md          # one Markdown file per member
├── lead.md
├── includes/           # shared text, pulled in with {{includes/<stem>}}
├── plugins/slackbot/   # a colocated plugin
└── borgchart.yaml      # one deployment of this team
```

`brg validate` checks all of it before anything starts and reports every problem at once.

## YAML rules

Every borgchart file is YAML 1.2 parsed strictly:

- **Duplicate keys** are errors.
- **Booleans** are `true` and `false` only. `yes`, `no`, `on` and `off` are strings, so `start: no` is an error rather than a silent `false`.
- **Merge keys** (`<<`) are refused.
- **Size limits:** files over 1 MiB and runaway alias expansion are refused.
- **Unknown fields** are errors.

## `team.yaml`

```yaml
default: claude                 # vendor for members that don't name one: claude | codex
includes: includes              # optional; directory for {{includes/…}} (default: includes)

plugins:
  slackbot:
    path: plugins/slackbot      # relative to the team dir; or shared:<name>; or omit to use PATH
    process: [./watch.py]       # optional: a long-running program brg starts and stops
    delivers_to: triager        # optional: the member the process hands work to
    env: [SLACK_BOT_TOKEN, SLACK_CHANNEL_ID]   # the only variables the plugin receives
    actions:                    # optional: named command lines
      read: slackbot read <ts>
      reply: slackbot reply <thread_ts> <text>
      escalate: slackbot escalate <ts> <severity> <reason>

members:
  triager:
    prompt: triager.md          # the member file, relative to the team dir
    vendor: claude              # optional; default from `default`
    parent: lead                # optional; another member
    start: true                 # optional; false = started only with `brg up --member`
    posture: locked             # optional: vendor (default) | locked
    untrusted_input: true       # it reads text from outside (a channel, an inbox)
    plugins: {slackbot: [read, escalate]}
  lead:
    prompt: lead.md
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, reply]}
```

**Names.** Member, plugin and action names match `[a-z][a-z0-9_-]{0,31}`. Environment names match `[A-Z_][A-Z0-9_]*`.

**Parents.** A `parent` must be another member, and parent links can't form a cycle. Messages a plugin routes "to the parent" go along this edge.

## Member files

```markdown
---
role: triager
models:
  claude: haiku
  codex: example-model low
---
You read each new message in the channel and decide whether it needs the lead.

{{includes/escalation-rules}}
```

- **Frontmatter** sits between a first line `---` and the next line that is exactly `---`.
  - A byte-order mark and CRLF line endings are accepted.
  - A later `---` is ordinary body text.
- **`role`** is a free-form label.
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
2. In each member file, replace `name:` with `role:`.
3. Replace `claude_model: X` / `codex_model: Y` with `models: {claude: X, codex: Y}`.

## Plugins

A plugin is a plain script or program, in any language, that members run themselves. borgchart does not wrap plugins in an MCP server.

- **What brg runs:** brg launches only the programs `team.yaml` lists. This limits what brg starts; it is not a sandbox. A listed program, or a member with a shell, can run anything.
- **Location:**
  - `path: plugins/<dir>`: a directory inside the team. It is also the programs' working directory.
  - `path: shared:<name>`: `<plugins_dir>/<name>`, where `plugins_dir` is set in `borgchart.yaml`.
  - No `path`: the commands are whatever those names resolve to on `PATH` when you run `brg up`.
- **`process`:** a long-running program, e.g. a watcher.
  - `brg up` starts it and `brg down` stops it; it is not restarted.
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

How much a member may do is set per member, overridable per deployment.

- **`vendor`** (default): brg passes no permission flags.
  - The agent runs with your own Claude or Codex settings (e.g. auto mode, a sandbox, approval rules) and your MCP connectors.
  - If it stops at an approval prompt, `brg status` shows it `blocked` and you attach to answer.
- **`locked`:** for members that read untrusted input.
  - The only tool calls allowed are the member's granted action lines, with plain arguments. That is enforced by brg's own pre-tool-use gate on both Claude and Codex, plus each vendor's no-prompt mode.
  - Everything else is denied without asking: other tools, shell metacharacters, expansions, redirects.
- **`untrusted_input: true`** marks a member that reads text from outside. Such a member must be `locked` unless it also says `accept_risk: true`.

**What `brg validate` warns about.** A `vendor`-posture member that:

- receives work from a plugin process (a `delivers_to` target);
- is the parent of such a member;
- or is granted a plugin that runs a process.

**Safety depends on posture.** In `vendor` posture, an agent that reads untrusted text can be steered into anything its vendor mode allows, including acting as you through your connectors.

## Connectors

A member can use MCP connections you've already set up in its vendor CLI, such as a personal Slack or mail connector. borgchart doesn't proxy or configure them.

```yaml
members:
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
- **Probing:** `brg validate --probe` (coming with the runtime) asks the vendor which servers are configured. It can't check that a connector is signed in, which account it acts as, or its scopes.

## `borgchart.yaml`

```yaml
schema: 1
team: .                       # the team directory, relative to this file (default: .)
auth: login                   # login (default) | api_key, for every member
plugins_dir: ~/borgchart-plugins   # where shared:<name> plugins live

members:
  lead:
    task: Also post a short summary at 17:00 UTC.   # appended to the standing instruction
    cwd: ~/work/ops           # the member's working directory
    posture: locked           # override the team's posture
    args: []                  # extra vendor launch arguments

plugins:
  slackbot:
    env: {SLACK_CHANNEL_ID: C0000EXAMPLE}   # non-secret values only
```

- **Secrets** are never written here. They come by name from the environment `brg up` runs in. `brg validate` warns about a declared name that nothing supplies yet.
- **Trust:** treat this file as trusted, like the team: `posture` and `args` can widen an agent's permissions.
- **`auth: login`:** brg passes no API-key variables, so a stray key in your shell can't replace your login.
- **`auth: api_key`:** brg passes `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` by name.
