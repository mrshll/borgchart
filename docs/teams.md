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

**Getting a member's attention (Open).** A member with messages waiting has to notice them. Two ways are on the table, being tested now on the real CLIs:

1. **The member waits for itself.** Before going idle it runs `brg message wait` in the background, and the CLI wakes it when a message arrives. Nothing is typed.
2. **A watcher nudges it.** A hidden window per team types one fixed reminder ("new messages in your inbox") into an idle member whose input box is provably empty. It waits while the member is busy. It alerts a person when it can't type safely: a draft in the box, a dialog, or a CLI version that hasn't been tested.

Pending the test, the likely answer is (1) where the vendor supports it, with the watcher as the fallback and alarm.

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

- **Waking members:** self-wake, the watcher, or both (above).
- **Sessions:** whether separate groups of teams (`--session`) are needed at all. They are kept minimal for now.
- **Storage key:** messages are keyed by the deployment's location, so moving the team's folder starts empty. Should `name` be the key instead?
- **Secrets:**
  - whether secrets can also be shared across a session;
  - how they're stored at rest (an OS keystore, systemd credentials, the macOS Keychain, or private files);
  - rotation;
  - which plugin or member may read which secret;
  - how a deployed team maps them to a cloud secret manager.
- **When renames land:** `prompt`, `message …` and the folded commands arrive in one follow-up change after the reworked pieces merge. Until then the commands under review keep today's names.
