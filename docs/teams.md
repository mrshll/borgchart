# Defining a brg team (proposal)

**Status: proposal.** This page describes, in one place, how a team and its plugins are defined and used once the work in review lands. Today's shipped format is in [format.md](format.md). Items marked **Open** are not decided yet.

## The files

```
devteam/
├── team.yaml          # who is on the team, and which plugins they may use
├── lead.md            # one file per member: role, models, standing instruction
├── companion.md
├── implementer.md
├── reviewer.md
├── includes/rules.md  # shared text, pulled in with {{includes/rules}}
├── plugins/notify/    # a plugin: a program in any language
└── borgchart.yaml     # values for this one place: name, folders, messages on/off
```

The team directory is shareable: it says nothing about one machine. `borgchart.yaml` holds what changes per place.

### `team.yaml`

```yaml
default: claude                        # vendor for members that don't name one

plugins:
  notify:
    path: plugins/notify               # a folder in the team, or shared:<name>
    env: [NOTIFY_ROOM]                 # the only variables the plugin receives
    actions:
      say:   notify say <text>         # a command members may be granted
      alert: notify alert <id> <member> <reason> <pending>   # brg's own alert hook

members:
  lead:
    prompt: lead.md
    plugins: {notify: [say]}
  companion:
    prompt: companion.md
    parent: lead
  implementer:
    prompt: implementer.md
    parent: lead
    start: false                       # the lead starts it when there's work: brg up --member implementer
  reviewer:
    prompt: reviewer.md
    vendor: codex
    parent: lead
    start: false
```

- **Members** have a member file (`prompt`), an optional `vendor`, a `parent`, and `start`. `start: false` means the member is started on demand. They also have a `posture` (`vendor`, the default, or `locked` for members that read untrusted text) and plugin grants.
- **`parent`** makes the org chart, and messages follow it: a member writes to its parent or its children. The top member can also write to an outside caller (`external:NAME`).
- **Plugins** are plain programs. brg runs only what `team.yaml` lists. A member can use only the actions it's granted; each granted action appears in its instruction with an absolute path. Secrets reach the plugin through a private file, never the agent's environment.
  - A plugin may also declare a long-running `process` (a watcher of some outside source) with `delivers_to: <member>`.
  - An action that brg runs itself, like `alert` above, gets fixed arguments and never any message text.

### Member files

```markdown
---
role: implementer
models: {claude: opus medium, codex: example-model medium}
---
You implement one assigned task at a time in your own checkout.

{{includes/rules}}
```

- **`models`:** pins a model, and optionally an effort, per vendor. Only the member's own vendor needs an entry.
- **Body:** the standing instruction. It's sent once, when the member starts.
- **Includes:** one level deep.

### `borgchart.yaml`

```yaml
schema: 1
name: devteam                          # the team's name on the command line
auth: login
auto_trust: false
bus:                                   # turns messages on for this team
  root: /tmp/brg-teams-1000            # where messages are kept
  alert: {plugin: notify, action: alert}
members:
  lead:        {cwd: ~/work/project}
  implementer: {cwd: ~/work/project-impl}   # one checkout per writer
  reviewer:    {cwd: ~/work/project-impl}   # reads the checkout; its tasks name no checkout
plugins:
  notify: {env: {NOTIFY_ROOM: example-room}}
```

- **`name`:** defaults to the folder's name. It must be unique among your teams; a second folder with the same name is refused.
- **`bus`:** turns messages on. Without it, the team works as today: you drive it with `prompt`.
- **Per member:** `cwd`, `task` (appended to the instruction), `posture` and extra vendor `args`.
- **Secrets:** never written here. They come from your environment by name when you run `brg up`.

## Commands

A small top level for people:

| Command | What it does |
| --- | --- |
| `brg validate` | Check everything without starting anything |
| `brg up [team] [--member M]` | Start the team (or one member). Prints the team as a tree with each member's model and what happened. |
| `brg down [team] [--member M]` | Stop it |
| `brg status [team] [--check]` | The same tree, live: who is idle, working or blocked, plus open tasks, questions and alerts. `--check` exits non-zero when a person is needed. |
| `brg ls` | Your teams, running or stopped |
| `brg attach [team/]member`, `brg logs [team/]member` | Watch a member |
| `brg prompt [team/]member TEXT` | Type into a member now (today's `brg send`) |
| `brg message …` | Messages, below |

`brg up devteam` prints:

```
devteam
└─ lead            claude opus high       started
   ├─ companion    claude opus high       started
   ├─ implementer  claude opus medium     not started (start: false)
   └─ reviewer     codex example-model    not started (start: false)
```

**Messages**, mostly used by members:

| Command | What it does |
| --- | --- |
| `brg message send [team/]member TEXT` | Send a durable message (or a task record with `--record FILE`) |
| `brg message inbox [[team/]member]` | List messages not yet marked handled |
| `brg message read [team/]ID` | Read one (or a task's history with `--task T`) |
| `brg message ack [team/]ID…` | Mark as handled |
| `brg message wait [[team/]member] --timeout S` | Wait for the next one |

**Addressing.** `devteam/lead`, or just `lead` when that's unambiguous: inside a member (its own team), in the team's folder, or when only one running team has a `lead`. Otherwise brg asks you to name the team. An outside caller, for example a person's script or a coordinating agent, adds `--external NAME` (refused inside a member) and is answered at `external:NAME`.

## How members talk

- **`prompt`** types text into a member's input box right away, under checks: not mid-turn, not at a dialog. It's for people steering a member.
- **`message send`** writes a durable message. It's never typed into a terminal, survives restarts and is acknowledged explicitly.
- **Tasks** are messages too: assign, accept or decline, result, verify, release, question and decision. brg checks who may send each one and keeps the task's state.
  - Only a member's parent assigns and verifies.
  - Each member has one open task at a time.
  - A checkout is never released while the member writing it might still be running.

**Getting a member's attention.** A member with messages waiting has to notice them. Nothing is typed into its terminal in the normal case:

- **Claude members wake themselves.** At the start of each turn they run `brg message wait` in the background, and Claude Code starts a new turn when it returns.
- **Codex members are woken by `codex queue`.** It hands the waiting session a one-line pointer to its inbox and starts a turn, with nothing typed.
- **A hidden watcher per team is the safety net.** It looks for members with unread messages and no working wait, and wakes them the vendor's way. Its last resort is to type one fixed reminder, and only into a provably empty input box. When it can't do that safely, it alerts a person.
- **Version check.** Each of these is enabled per vendor and version only after it has been tested on that CLI.

## Secrets (proposed, not in this work)

Today a plugin's secrets come from your environment when you run `brg up`, so they must be present at every start. The proposal is to register them with brg once, as Modal or Kubernetes do:

```
$ passh read op://vault/notify/token | brg secret add devteam NOTIFY_TOKEN
$ brg secret ls devteam
NOTIFY_TOKEN   added 2026-11-06
$ brg secret rm devteam NOTIFY_TOKEN
```

- **`add`** reads the value from stdin. Paste it, or pipe it from a password manager; where it comes from is your business. It never appears in a command line, a prompt or a log.
- **`ls`** shows names and dates, never values.
- **Scope:** a secret belongs to one team. A secret shared by every team in a session is an open question.
- **Persistence:** registered secrets survive restarts and reboots. Plugins receive them through brg's existing private-file mechanism, as declared in `env`; agents never see them.

## Roles with several copies (Open: design only, a later change)

Some roles are needed as work appears, sometimes several at once: three implementers on three tasks. The direction:

```yaml
members:
  implementer:
    prompt: implementer.md
    parent: lead
    start: false
    max: 3                             # up to three copies at once
```

- **Starting a copy.** Each `brg up --member implementer` starts one new copy: `implementer-1`, then `implementer-2`, and so on. Past `max` it's refused, and the refusal lists the running copies. Each copy gets the same member file, plus its own name and checkout (below).
- **The tree** shows running copies with their live state. A role with none running gets one dim line:

  ```
  devteam
  └─ lead              claude opus high      working
     ├─ implementer-1  claude opus medium    working
     ├─ implementer-2  claude opus medium    idle
     └─ reviewer       codex example-model   not started (start: false)

  devteam
  └─ lead            claude opus high      idle
     └─ implementer  on demand (none running)
  ```

  Ended copies leave the tree; `brg status --all` could list them.
- **Addressing.** `devteam/implementer-2` reaches one copy. A bare `implementer` works only while exactly one copy is running; otherwise brg errors and lists the copies, the same rule as for team names. `brg down --member implementer` follows the same rule.
- **Messages to the role.** A message sent to `implementer`, rather than to one copy, waits in a shared queue for the role. The first copy to take it gets it.

Proposed changes to this direction, and why:

1. **`max` alone turns copies on; no `start: on-demand`.** `start: false` already means "on demand", and `max` defaults to 1, today's single member. This adds one key instead of a third `start` value.
2. **Copies hang under the role's declared `parent`, and only that parent (or a person) may start one.** Messages and task rights follow `parent`. If "whoever started it" became the parent, the org chart would change at runtime. With this rule the two readings agree.
3. **Copy numbers aren't reused while the team's storage lives.** A new `implementer-2` would otherwise inherit an ended copy's inbox and task.
4. **"Take" means `accept`, not `ack`, for tasks.** The first copy to accept an assignment sent to the role becomes its assignee; later accepts are told it's taken. Each copy still holds at most one open task. Plain notes to the role go to the first copy that acks them. If ack meant take, a copy that crashed between acking and working would lose the task.
5. **Wake one copy, not all.** A role message wakes the idle copy with the lowest number. If nobody takes it within a few minutes, the next copy is woken. Waking every copy would spend a turn on each for one message.
6. **An ended copy stays in the tree while it still holds an open task or a checkout,** shown as `stopped (open task T3)`. It leaves the tree once that's released. Otherwise unfinished work disappears from view.

Open questions:

- **Is `max` required?** Proposed: optional, default 1, with a sane upper bound (e.g. 8).
- **Checkouts.** A checkout holds one writer, so each copy needs its own. Options:
  - a `cwd` template in `borgchart.yaml`, like `cwd: ~/work/project-impl-{n}`, with folders you create (simplest, and brg doesn't manage git);
  - brg creates and later removes a git worktree per copy.

  Proposed: the template first.
- **When every copy has ended,** unacknowledged role messages stay queued. `status` shows them (`implementer  on demand (none running) · 2 waiting`), and the parent is told. Should brg start a copy by itself instead?
- **A copy that ends with an open task:** does the task go back to the parent for reassignment, or wait for `brg up --member implementer-2` to restart that same copy?

## Teams as members (departments) (Open: design only, a later change)

A member can be a whole team. A **department** is just a team whose members include teams. It's a name for that shape, not a new kind of object.

```yaml
# coding/team.yaml
members:
  lead:
    prompt: lead.md
  ws:                                  # a workstream: a whole sub-team
    team: teams/medium                 # a team folder inside this one, or shared:<name>
    parent: lead
    start: false
    max: 4
```

- **Starting one.** `brg up --member ws` starts `ws-1`, a whole copy of the sub-team. Its top member is the member itself: the department's lead writes to `ws-1` and reaches `ws-1`'s lead. The sub-team's own on-demand roles sit under that lead and are started by it, as in any team.
- **The rules already decided apply at every level:**
  - **The tree nests:**

    ```
    coding
    └─ lead                     claude opus high     working
       ├─ ws-1
       │  └─ lead               claude opus high     working
       │     ├─ implementer-1   claude opus medium   working
       │     └─ reviewer        codex example-model  not started (start: false)
       └─ ws                    on demand (1 of 4 running)
    ```

  - **Addressing extends:** `coding/ws-2/lead`, `coding/ws-2/implementer-1`. Bare names resolve relative to the sender, so inside `ws-2`, `implementer-1` means `ws-2`'s own copy.
  - **`parent` chains escalation upward:** a sub-team's lead escalates to the member it hangs under, and so on up to the person.
  - **Each copy freezes its own configuration** at its start and gets its own checkouts. The `cwd` template from the copies section keeps one writer per checkout, now per sub-team copy.
- **Quota and model routing are not core.** They're a plugin the lead uses before seating a team or a copy, for example a `route` action that answers "go" or "wait", based on usage, before the lead runs `brg up --member ws`. brg itself never checks quotas.

Proposed changes to this direction, and why:

1. **`team:` must be inside the team folder, or `shared:<name>` as for plugins, never a path like `../medium`.** The team folder is meant to be shareable and say nothing about one machine. A path outside it breaks that.
2. **One message store per department, not one per sub-team.** Copies are namespaced members (`ws-2/lead`) in the department's single store. A parent-to-child message is then an ordinary local record, with one sequence, one inspect and one check. Separate stores would need delivery between stores, and a rollover would have to coordinate them.
3. **A sub-team copy can't write to outside callers.** Only the outermost team's top member answers `external:NAME`. Inside a department, a sub-team's top member writes to its parent. Otherwise every workstream could reach the person directly, around the department's lead.
4. **Per-copy values come from the department's `borgchart.yaml`,** under that member, with `{n}` in paths, for example `ws: {members: {lead: {cwd: ~/work/project-ws{n}}}}`. A sub-team's own `borgchart.yaml` is ignored when it runs as a member. One place configures the whole department.
5. **The routing plugin starts as go/wait only.** Letting it change a copy's vendor or model would make `up --member` take overrides that member files currently pin. Add overrides only if go/wait proves too little.

Open questions:

- **Depth.** Cycles (a team containing itself, directly or not) are refused at `validate`. Is there also a depth limit? Proposed: 3 levels, raise it if needed.
- **Standalone use.** Can a sub-team's folder also run as an ordinary team? Proposed: yes, with its own `borgchart.yaml`, and that file is ignored when the team runs as a member.
- **Storage.** Shared with the department (proposed above) or one store per sub-team.
- **Stopping.** `brg down --member ws-2` stops the whole copy. What about the open tasks of its members (the same question as for a copy that ends with an open task)?
- **Siblings.** Can `ws-1/lead` message `ws-2/lead` directly? Proposed: no, only through their common parent, as for any siblings.
- **Display.** Should `status` fold a busy sub-team into one line by default, with `--all` expanding it?

## What you never see

- **Storage.** No runs, run IDs, generations or storage paths in commands, output or errors. brg rolls a team's message storage over by itself when its configuration changes or the storage reaches its age limit, and carries open work across.
- **brg versions.** Nothing is pinned: every call uses the installed `brg`. If an upgraded brg can't read a team's messages, it says exactly what to do:

  ```
  brg: team `devteam`'s messages use format 1; this brg uses format 2.
  Restart the team to start fresh storage in the new format: brg down devteam && brg up devteam
  (Open work in the old format isn't carried over; the old messages stay on disk.)
  ```

  So after such an upgrade, unfinished tasks have to be sent again.

- **Configuration changes** take effect at the next `brg down`/`brg up`. Starting one member into a running team with changed files says so, in team and member terms.

## Limits

- **Trusted, same-user operation.** Members can reach each other's terminals; nothing isolates them.
- **A reboot stops every team.** Messages stored under `/tmp` may not survive it, so keep `max_age_days` under your system's temporary-file cleanup age.
- **`locked` members can't use messages yet.**

## Open

- **Roles with several copies** and **teams as members** (above).
- **Sessions:** whether separate groups of teams (`--session`) are needed at all. They are kept minimal for now.
- **Storage key:** messages are keyed by the deployment's location, so moving the team's folder starts empty. Should `name` be the key instead?
- **Secrets:**
  - whether secrets can also be shared across a session;
  - how they're stored at rest (an OS keystore, systemd credentials, the macOS Keychain, or private files);
  - rotation;
  - which plugin or member may read which secret;
  - how a deployed team maps them to a cloud secret manager.
- **When renames land:** `prompt`, `message …` and the folded commands arrive in one follow-up change after the reworked pieces merge. Until then the commands under review keep today's names.
