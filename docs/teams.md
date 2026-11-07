# Defining a brg team (proposal)

**Status: proposal, partly superseded.** This page describes, in one place, how a team and its plugins are defined and used once the work in review lands. Teams, instances, roles, members and the four verbs (`create`, `start`, `stop`, `delete`) are decided in [concepts.md](concepts.md), which wins where this page differs. Today's shipped format is in [format.md](format.md). Items marked **Open** are not decided yet. For command syntax, `brg … --help` is the authority; the tables below are a short summary.

## The files

A real team lives in [`dev-team/`](../dev-team/): the team this repository is developed with, plus [`dev-design-team/`](../dev-design-team/), the design panel its lead seats. The sketch below is a smaller version of it:

```
devteam/
├── team.yaml          # who is on the team, and which plugins they may use
├── orchestrator.md    # one file per member: role, models, standing instruction
├── implementer.md
├── reviewer.md
└── includes/base.md   # shared text, pulled in with {{includes/base}}
```

The team folder is shareable: it says nothing about one machine or one project. What belongs to you (auth, trust, where teams are found) is in brg's configuration, `~/.config/borgchart/config.yaml`; the folder an instance works in is given at `brg create devteam <folder>`.

### `team.yaml`

```yaml
default: claude                        # vendor for members that don't name one

members:
  lead:
    prompt: orchestrator.md
    vendor: claude                     # the lead wakes itself with a background wait
  implementer:
    prompt: implementer.md
    vendor: codex
    parent: lead
    start: false                       # the lead creates it when there's work: brg create <instance>/implementer
  reviewer:
    prompt: reviewer.md
    vendor: claude
    parent: lead
    start: false
```

- **Members** have a member file (`prompt`), an optional `vendor`, a `parent`, and `start`. `start: false` means the member is started on demand. They also have a `posture` (`vendor`, the default, or `locked` for members that read untrusted text) and plugin grants.
- **`parent`** makes the org chart, and messages follow it: a member writes to its parent or its children. The top member can also write to an outside caller (`external:NAME`).
- **Plugins** are plain programs, declared under `plugins:` (a folder in the team, or `shared:<name>`, with the variables they receive and the actions members may be granted). brg runs only what `team.yaml` lists. A member can use only the actions it's granted; each granted action appears in its instruction with an absolute path. Secrets reach the plugin through a private file, never the agent's environment.
  - A plugin may also declare a long-running `process` (a watcher of some outside source) with `delivers_to: <member>`.
  - An action that brg runs itself, like a bus `alert` action, gets fixed arguments and never any message text. The alert action (#8) is parked until the watcher (#20); see "Getting a member's attention".

### Member files

```markdown
---
role: implementer
models: {claude: opus medium, codex: gpt-6.1-sol high}
---
You are the implementer on a development team. The lead assigns you one bounded task at a time, in your own checkout.

{{includes/base}}
```

- **`models`:** pins a model, and optionally an effort, per vendor. Only the member's own vendor needs an entry; `dev-team/` names both, so moving a member to the other vendor is a one-line change to its `vendor:` in `team.yaml`.
- **Body:** the standing instruction. It's sent once, when the member starts.
- **Includes:** one level deep.

### Instances

```
$ brg create devteam ~/work/project
devteam-k3f9  team ~/.config/borgchart/teams/devteam · folder ~/work/project
└─ lead-q8m3            claude opus high          started
   ├─ implementer       on demand (none created)
   └─ reviewer          on demand (none created)
```

- **`create`** makes a new instance in that folder, with a suffixed ID; run it twice and you have two. The team's `bus: {}` turns messages on.
- **Per role** in `team.yaml`: `posture`, extra vendor `args` and an optional `cwd` inside the instance's folder. Standing rules go in the team's own files; context for one piece of work goes to the lead in a message.
- **Secrets:** brg stores none. They come from your environment by name when you run `brg create` or `brg start`; `brg secret devteam` lists which are set.

## Commands

A small top level for people. `brg --help` and `brg <command> --help` give the full syntax; inside a member, the help also covers the message kinds and facts, waiting and the task lifecycle.

| Command | What it does |
| --- | --- |
| `brg validate <team>` | Check everything without starting anything |
| `brg create <team> <folder>`, `brg create <instance>/<role>` | Make a new instance, or a role's member, and start it. Prints the tree with each member's model and what happened. |
| `brg start <instance>[/<member>]`, `brg stop <instance>[/<member>]` | Bring it back, or stop it |
| `brg delete <instance>` | Remove a stopped instance |
| `brg status [instance] [--check]` | The same tree, live: who is idle, working or blocked, plus open tasks, questions and alerts. `--check` (proposed) exits non-zero when a person is needed. |
| `brg ls` | Your teams, each with its instances |
| `brg attach <instance>/<member>`, `brg logs <instance>/<member>` | Watch a member |
| `brg prompt <instance>/<member> TEXT` | Type into a member now (today's `brg send`) |
| `brg message …` | Messages, below |

**Messages**, mostly used by members:

| Command | What it does |
| --- | --- |
| `brg message send [team/]member TEXT` | Send a durable message (or a task record with `--record FILE`) |
| `brg message inbox [[team/]member]` | List messages not yet marked handled |
| `brg message read [team/]ID` | Read one (or a task's history with `--task T`) |
| `brg message ack [team/]ID…` | Mark as handled |
| `brg message wait [[team/]member] --timeout S` | Wait for the next one |

**Addressing.** `devteam-k3f9/lead-q8m3`. Inside a member, a bare member ID means its own instance; everywhere else the instance is named in full ([concepts](concepts.md#names-and-ids)). An outside caller, for example a person's script or a coordinating agent, adds `--external NAME` (refused inside a member) and is answered at `external:NAME`.

## How members talk

- **`prompt`** types text into a member's input box right away, under checks: not mid-turn, not at a dialog. It's for people steering a member.
- **`message send`** writes a durable message. It's never typed into a terminal, survives restarts and is acknowledged explicitly.
- **Tasks** are messages too: assign, accept or decline, result, verify, release, question and decision. brg checks who may send each one and keeps the task's state.
  - Only a member's parent assigns and verifies.
  - Each member has one open task at a time.
  - A checkout is never released while the member writing it might still be running.

**Getting a member's attention.** A member with messages waiting has to notice them. Nothing is typed into its terminal in the normal case:

- **Work is waiting before a member starts.** The lead publishes the assignment first, then starts the member, whose first action is to read its inbox. So an on-demand member never needs waking for its first task.
- **Claude members wake themselves.** At the start of each turn they run `brg message wait` in the background, and Claude Code starts a new turn when it returns. That's why `dev-team/`'s lead runs on Claude.
- **Codex members are woken by `codex queue`** once the watcher (#20) lands. It hands the waiting session a one-line pointer to its inbox and starts a turn, with nothing typed. Until then a Codex member waits only in the foreground, for the answer to its own question, and a Codex lead isn't supported.
- **A hidden watcher per team is the safety net** (#20). It looks for members with unread messages and no working wait, and wakes them the vendor's way. Its last resort is to type one fixed reminder, and only into a provably empty input box. When it can't do that safely, it alerts a person through the team's alert action (#8), which is parked until the watcher lands with it.
- **Version check.** Each of these is enabled per vendor and version only after it has been tested on that CLI.

## Secrets

Decided: brg stores no secrets. A plugin's secrets come from the environment `brg create` or `brg start` runs in (for example under your password manager's `run`), go only to that plugin's 0600 file, and are removed at `brg stop`. `brg secret <team>` lists the names the plugins need and whether each is set, without values. 0600 keeps other users out, not the members: a member that isn't `locked` could read the file while it exists.

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

- **Starting a copy.** Each `brg create <instance>/implementer` starts one new copy, with its own suffixed ID. Past `max` it's refused, and the refusal lists the running copies. Each copy gets the same member file, plus its own name and checkout (below).
- **The tree** shows running copies with their live state. A role with none running gets one dim line:

  ```
  devteam
  └─ lead              claude opus high      working
     ├─ implementer-1  claude opus medium    working
     ├─ implementer-2  claude opus medium    idle
     └─ reviewer       claude opus high      not started (start: false)

  devteam
  └─ lead            claude opus high      idle
     └─ implementer  on demand (none running)
  ```

  Ended copies leave the tree; `brg status --all` could list them.
- **Addressing.** `devteam/implementer-2` reaches one copy. A bare `implementer` works only while exactly one copy is running; otherwise brg errors and lists the copies, the same rule as for team names.
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
  - a role `cwd` with `{member}`, like `cwd: copies/{member}`, inside the instance's folder, with folders you create (simplest, and brg doesn't manage git);
  - brg creates and later removes a git worktree per copy.

  Proposed: the template first.
- **When every copy has ended,** unacknowledged role messages stay queued. `status` shows them (`implementer  on demand (none running) · 2 waiting`), and the parent is told. Should brg start a copy by itself instead?
- **A copy that ends with an open task:** does the task go back to the parent for reassignment, or wait for `brg start <instance>/<copy>` to restart that same copy?

## Teams as members (departments) (Open: design only, a later change)

A member can be a whole team. A **department** is just a team whose members include teams. It's a name for that shape, not a new kind of object.

```yaml
# coding/team.yaml
members:
  lead:
    prompt: lead.md
  ws:                                  # a workstream: a whole sub-team
    team: teams/dev-team               # a team folder inside this one, or shared:<name>
    parent: lead
    start: false
    max: 4
```

- **Starting one.** `brg create <instance>/ws` starts `ws-1`, a whole copy of the sub-team. Its top member is the member itself: the department's lead writes to `ws-1` and reaches `ws-1`'s lead. The sub-team's own on-demand roles sit under that lead and are started by it, as in any team.
- **The rules already decided apply at every level:**
  - **The tree nests:**

    ```
    coding
    └─ lead                     claude opus high     working
       ├─ ws-1
       │  └─ lead               claude opus high     working
       │     ├─ implementer-1   claude opus medium   working
       │     └─ reviewer        claude opus high     not started (start: false)
       └─ ws                    on demand (1 of 4 running)
    ```

  - **Addressing extends:** `coding/ws-2/lead`, `coding/ws-2/implementer-1`. Bare names resolve relative to the sender, so inside `ws-2`, `implementer-1` means `ws-2`'s own copy.
  - **`parent` chains escalation upward:** a sub-team's lead escalates to the member it hangs under, and so on up to the person.
  - **Each copy freezes its own configuration** at its start and gets its own checkouts. The `cwd` template from the copies section keeps one writer per checkout, now per sub-team copy.
- **Quota and model routing are not core.** They're a plugin the lead uses before seating a team or a copy, for example a `route` action that answers "go" or "wait", based on usage, before the lead runs `brg create <instance>/ws`. brg itself never checks quotas.

Proposed changes to this direction, and why:

1. **`team:` must be inside the team folder, or `shared:<name>` as for plugins, never a path like `../medium`.** The team folder is meant to be shareable and say nothing about one machine. A path outside it breaks that.
2. **One message store per department, not one per sub-team.** Copies are namespaced members (`ws-2/lead`) in the department's single store. A parent-to-child message is then an ordinary local record, with one sequence, one inspect and one check. Separate stores would need delivery between stores, and a rollover would have to coordinate them.
3. **A sub-team copy can't write to outside callers.** Only the outermost team's top member answers `external:NAME`. Inside a department, a sub-team's top member writes to its parent. Otherwise every workstream could reach the person directly, around the department's lead.
4. **Per-copy folders come from the role's `cwd` with `{member}`,** inside the department's one working folder.
5. **The routing plugin starts as go/wait only.** Letting it change a copy's vendor or model would make `create` take overrides that member files currently pin. Add overrides only if go/wait proves too little.

Open questions:

- **Depth.** Cycles (a team containing itself, directly or not) are refused at `validate`. Is there also a depth limit? Proposed: 3 levels, raise it if needed.
- **Standalone use.** Can a sub-team's folder also run as an ordinary team? Proposed: yes, as its own instance.
- **Storage.** Shared with the department (proposed above) or one store per sub-team.
- **Stopping.** `brg stop <instance>/ws-2` stops the whole copy. What about the open tasks of its members (the same question as for a copy that ends with an open task)?
- **Siblings.** Can `ws-1/lead` message `ws-2/lead` directly? Proposed: no, only through their common parent, as for any siblings.
- **Display.** Should `status` fold a busy sub-team into one line by default, with `--all` expanding it?

## What you never see

- **Storage.** No runs, run IDs, generations or storage paths in commands, output or errors. brg rolls a team's message storage over by itself when its configuration changes or the storage reaches its age limit, and carries open work across.
- **brg versions.** Nothing is pinned: every call uses the installed `brg`. If an upgraded brg can't read a team's messages, it says exactly what to do:

  ```
  brg: instance `devteam-k3f9`'s messages use format 1; this brg uses format 2.
  ```

  brg refuses to start such an instance, and points to `brg create` for a fresh one; its open work has to be sent again.

- **Configuration changes** take effect at the next full `brg stop` and `brg start`. Starting one member into a running instance with changed files says so, in instance and role terms.

## Limits

- **Trusted, same-user operation.** Members can reach each other's terminals; nothing isolates them.
- **A reboot stops every instance.** brg refuses a state folder under `/tmp`, so messages survive it.
- **`locked` members can't use messages yet.**

## Open

- **Roles with several copies** and **teams as members** (above).
- **Sessions:** whether separate groups of teams (`--session`) are needed at all. They are kept minimal for now.
- **Secrets:** how a hosted brg would map them to a cloud secret manager.
- **When renames land:** `prompt`, `message …`, `rules:` and the folded commands arrive in one follow-up change (#19). Until then the commands keep today's names (`send`, `msg`, `inbox`, `task:`).
