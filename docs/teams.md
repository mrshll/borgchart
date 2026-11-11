# Defining a brg team

Teams, instances, roles, copies up to `max`, teams as members and the four verbs (`create`, `start`, `stop`, `delete`) work today. [concepts.md](concepts.md) records the design; [format.md](format.md) describes the shipped format. Items marked **Open** are not decided yet. For command syntax, `brg … --help` is the authority; the tables below are a short summary.

## The files

A real team lives in [`dev-team/`](../teams/dev-team/): the team this repository is developed with, plus [`dev-design-team/`](../teams/dev-design-team/), a design panel. They are siblings of [`engineering/`](../teams/engineering/), whose root lead seats both, through roles referring to them by name (see "Teams as members"). The sketch below is a smaller version of it:

```
devteam/
├── team.yaml          # who is on the team, and which plugins they may use
├── orchestrator.md    # one file per role: models and standing instruction
├── implementer.md
├── reviewer.md
└── includes/base.md   # shared text, pulled in with {{includes/base}}
```

The team folder is shareable: it says nothing about one machine or one project. What belongs to you (auth, trust, where teams are found) is in brg's configuration, `~/.config/borgchart/config.yaml`; the folder an instance works in is given at `brg create devteam <folder>`.

### `team.yaml`

```yaml
default: claude                        # vendor for members that don't name one

roles:
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

- **Roles** have a role file (`prompt`), an optional `vendor`, a `parent`, and `start`. The key under `roles:` names the role; frontmatter `role:` and the old `members:` key are rejected. `start: false` means its members are created on demand. Each role keeps up to `max` members (default 1), running or stopped. Roles also have a `posture` (`vendor`, the default, or `locked` for members that read untrusted text) and plugin grants.
- **`parent`** makes the org chart, and messages follow it: a member writes to its parent or its children. The top member can also write to an outside caller (`external:NAME`).
- **Plugins** are plain programs, declared under `plugins:` (a folder in the team, or `shared:<name>`, with the variables they receive and the actions members may be granted). brg runs only what `team.yaml` lists. A member can use only the actions it's granted; each granted action appears in its instruction with an absolute path. Secrets reach the plugin through a private file, never the agent's environment.
  - A plugin may also declare a long-running `process` (a watcher of some outside source) with `delivers_to: <role>`.
  - An action that brg runs itself, like a bus `alert` action, gets fixed arguments and never any message text. The alert setting is validated, but neither the watcher nor alert dispatch is wired now; see "Getting a member's attention".

### Role files

```markdown
---
models: {claude: opus medium, codex: gpt-6.1-sol high}
---
You are the implementer on a development team. The lead assigns you one bounded task at a time, in your own checkout.

{{includes/base}}
```

- **`models`:** pins a model, and optionally an effort, per vendor. Only the member's own vendor needs an entry; `teams/dev-team/` names both, so moving a member to the other vendor is a one-line change to its `vendor:` in `team.yaml`.
- **Body:** the standing instruction. It's sent once, when the member starts.
- **Includes:** one level deep.
- **Another team's role file:** a line `{{teams/<team>/<role>}}` takes that role's instruction body wholesale, so a private team can reuse a shared lead and add its own lines ([format](format.md#reusing-a-shared-team)).

### Instances

```
$ brg create devteam ~/work/project
devteam-k3f9  team ~/.config/borgchart/teams/devteam · folder ~/work/project
└─ lead-q8m3            claude opus high          started
   ├─ implementer       not created (start: false)
   └─ reviewer          not created (start: false)
```

- **`create`** makes a new instance in that folder, with a suffixed ID; run it twice and you have two. Every team has durable messages.
- **Per role** in `team.yaml`: `posture`, extra vendor `args` and `worktree: true` for members that each need their own checkout. Standing rules go in the team's own files; context for one piece of work goes to the lead in a message.
- **Secrets:** brg stores none. They come from your environment by name when you run `brg create` or `brg start`; `brg secret devteam` lists which are set.

## Commands

A small top level for people. `brg --help` and `brg <command> --help` give the full syntax; inside a member, the help also covers plain message drafts, acknowledgement and the one `msg wait`.

| Command | What it does |
| --- | --- |
| `brg validate <team>` | Check everything without starting anything |
| `brg create <team> <folder>`, `brg create <instance>/<role>` | Make a new instance, or a role's member, and start it. Prints the tree with each member's model and what happened. |
| `brg start <instance>[/<member>]`, `brg stop <instance>[/<member>]` | Bring it back, or stop it |
| `brg delete <instance>[/<member>] [--force]` | Remove a stopped instance or member. Unread messages require `--force`; running or unknown liveness, or a member with kept children, is always refused. A deleted member's ID stays issued. |
| `brg status [instance] [--json \| -w \| -i]` | The live tree: who is idle, working or blocked, with positive unacked message counts. JSON includes `member`, `role`, `parent` (a member ID), `start_id` (the current incarnation, when recorded), and `unacked`, including zero with a readable bus. An unreadable store omits counts and adds a check note. |
| `brg ls` | Your teams, each with its instances |
| `brg attach <instance>/<member>`, `brg logs <instance>/<member>` | Watch a member |
| `brg msg send [--external NAME] TARGET TEXT [--reply-to ID]` | Record a message, then optionally type it; outside callers must name an external sender; `-` reads stdin |
| `brg msg …` | Durable messages and store health, below |

**Messages**, mostly used by members:

| Command | What it does |
| --- | --- |
| `brg msg list [--member ID \| --external NAME] [--full] [--json]` | List every unacked message, oldest first; --full includes bodies |
| `brg msg read ID [--json]` | Read one message, including an acknowledged one |
| `brg msg ack ID… [--external NAME] [--json]` | Acknowledge messages addressed to you |
| `brg msg wait [--external NAME] [--timeout SECONDS] [--full] [--json]` | Wait for your inbox: new messages at once, existing ones after 60 seconds if still unacked; timeout 60–86400, default 86400. The 60 s floor matches the loop guard on every nonzero exit except fenced, including timeout |
| `brg msg check [--json]` | Report gaps and unreadable files without writing |

**Addressing.** `devteam-k3f9/lead-q8m3`. Inside a member, a bare member ID means its own instance; everywhere else the instance is named in full ([concepts](concepts.md#names-and-ids)). Only `create` takes a role name. Member commands and messages refuse a role name, listing its kept members or naming `brg create <instance>/<role>` when it has none. Bus commands take `--instance ID` outside a member. An outside caller, for example a person's script or a coordinating agent, adds `--external NAME` (refused inside a member) and is answered at `external:NAME`.

## How members talk

- **`msg send`** writes a durable message, then may type its tagged envelope, including mid-turn. Members send to their parent or children; only root members of the tree's root instance also send to outside callers. Outside callers must pass `--external NAME`. Across a copy's parent edge, members use full `instance/member` addresses; the tree shares one store and message IDs, with the same typed and locked delivery rules across edges. Other cross-instance sends, including between sibling copies, are refused.
- **`interactive: true`** roles are inbox-only even with `--allow-unknown`; false is the default. The setting is frozen per start and invalid on locked or nested team roles. Ordinary recipients explicitly ack when taking responsibility; typing does not ack. Treat a duplicate ID from typed delivery and msg wait as the same work. Locked records require no ack and give no new permissions or inbox recovery.
- **Stored success is success.** An ordinary send returns 0 once committed, even when input is suppressed or uncertain. Never republish it automatically. Locked postcommit typing failure returns nonzero with its ID; retries may duplicate history and input.
- **Work is prose.** The lead writes a brief file, sends a message naming it and starts the helper. The helper reads `msg list --full`, acks once it takes responsibility, writes its report atomically and replies naming it with `reply_to` the brief message. The lead verifies the report, diff and checks itself, stops the helper, then checks status shows it stopped before reusing the checkout. One writer per checkout and one piece of work per helper are team rules. Progress and delegation live in pull request descriptions and comments.

**Getting a member's attention.** Ack everything you have taken on, then arm one `msg wait`: Claude with its Bash tool in background mode, Codex in the foreground with `--timeout 600`. New unacked messages return at once; messages already there wait 60 seconds and return only if still unacked. Errors wait at least 60 seconds too, except fenced, explicit help and signals. On exit 9 wait again; on exit 8 (`fenced`) stop waiting and tell the owner. A message left unacked wakes a member about once a minute.

By each brief's existing deadline, check ordinary untyped records' ack/status. Start a stopped recipient; if idle and unacked, send one follow-up referencing the original ID with `--reply-to`; otherwise escalate. Do not busy-poll, republish stored successes or routinely force-delete pending work. See [the bus](bus.md#attention).

## Secrets

Decided: brg stores no secrets. A plugin's secrets come from the environment `brg create` or `brg start` runs in (for example under your password manager's `run`), go only to that plugin's 0600 file, and are removed at `brg stop`. `brg secret <team>` lists the names the plugins need and whether each is set, without values. 0600 keeps other users out, not the members: a member that isn't `locked` could read the file while it exists.

## Roles with several copies

Some roles are needed as work appears, sometimes several at once: three implementers on three tasks:

```yaml
roles:
  implementer:
    prompt: implementer.md
    parent: lead
    start: false
    max: 3                             # up to three kept copies, running or stopped
    worktree: true                     # optional; each copy gets its own git worktree
```

- **Starting a copy.** Each `brg create <instance>/implementer` starts one new copy, with its own suffixed ID. Past `max` it's refused, listing all kept copies, stopped ones included. For `max: 1`, the refusal points to `start`. Each copy gets the same role file and its own name; with `worktree: true`, its own worktree too.
- **The tree** shows every kept copy with its state and unacked count. A `max > 1` role with no kept members gets one dim line:

  ```
  devteam-k3f9
  └─ lead-q8m3          claude opus high      working
     ├─ implementer-x7k2 claude opus medium   working
     ├─ implementer-p7xm claude opus medium   idle
     └─ reviewer        not created (start: false)

  devteam-k3f9
  └─ lead-q8m3       claude opus high      idle
     └─ implementer  on demand (none created)
  ```

  Ended copies stay in the tree as stopped members until deleted.
- **Addressing.** `devteam-k3f9/implementer-x7k2` reaches one copy. A bare `implementer` is always refused where a member is expected, listing the kept members or naming `create` when it has none.
- **Messages go to a member.** The lead chooses a full member ID; there is no role queue.

- **Parents.** `parent:` and a plugin's `delivers_to:` require a `max: 1` agent role. A person may manage anything; a member at its latest running start may create, start, stop or delete anything below it along recorded parent edges, including members and copies at every depth. The caller's start is checked in its own instance under the registry lock. Members cannot manage themselves, ancestors, siblings or independent instances, and a copy's roots cannot manage their own copy or each other. Stopped, replaced and incomplete starts are refused. Creating a child requires a kept parent member.
- **Folders.** Members work in the instance's folder, usually the repository. With `worktree: true`, brg adds each new member or copy its own git worktree from that folder when it issues it (`create`, or a full `start` that gives a `start: true` role a new member), detached at the folder's `HEAD`, or on a new branch with `brg create <instance>/<role> --branch <name>`. Worktrees live beside brg's state folder, at `~/.local/state/borgchart-worktrees/<address>` (with `/` written as `.`); the command that adds one names it, and `status --json` gives it as `worktree`. `delete` removes a clean one, refuses one with uncommitted work or commits on no remote, and with `--force` leaves it as it is. Members without a worktree share the folder; the team's instructions still enforce one writer per checkout.
- **Taking work and attention** remain team rules. The lead chooses a member, gives it one brief, tracks its reply and checks its deadline. An ended copy remains addressable with unacked counts until deleted; the lead can restart it with `brg start <instance>/<member>`.

## Teams as members

A role can be a whole team, so teams form a tree: a team's roles can be teams, and every member has a parent (except the root), children and a subtree. The member with no parent is the **root**; the lead at the top of a tree is its **root lead**. [`engineering/`](../teams/engineering/) is the real tree: its root lead runs dev teams, design panels, researchers and monitors for this repository's engineering.

```yaml
# engineering/team.yaml, shortened
roles:
  lead:   {prompt: lead.md, start: true, interactive: true}
  dev:    {team: dev-team, parent: lead, start: false, max: 4, worktree: true}
  design: {team: dev-design-team, parent: lead, start: false, max: 2}
```

- **A team role's member is a copy:** a nested instance of the sub-team, with its own members, notes and state, working in its own worktree (`worktree: true`) or in its parent's folder. Members that start read the copy's current team files.
- **`team:` names another definition:** `team: dev-team` resolves through the configured team directories, exactly like `brg create dev-team`. Missing or ambiguous names fail validation. There is no same-installation restriction or nested-path compatibility. Definitions can be siblings while runtime working folders and addresses still nest. A definition also runs independently with `brg create dev-team` in a checkout.
- **New copies use the current catalog.** Existing copies retain their recorded source folders. Missing referenced definitions remain tolerable for observing/stopping running instances, but validation, copy creation and a fresh full start refuse them.
- **Copies are created stopped.** `brg create <instance>/dev` makes the copy and, for `worktree: true`, its worktree; it prints its ID and folder and starts nothing. `brg start <instance>/<copy>` starts the copy's `start: true` roles. A tree's instances may share folders, so the checkout check is between trees.
- **Addresses nest:** `<instance>/<copy>/<member>`, for example `engineering-h4k2/dev-a7k2/lead-c4nd`. A copy's instance ID is its full path. A target resolves by the longest registered instance prefix, then a member, a role or nothing. A copy's top members (those with no parent role) get their parent's full address in `BORGCHART_PARENT`.
- **Escalation follows `parent:` one level at a time.** A copy's top members hang under the member of the team role's `parent`. Only the root writes to `external:NAME`; a child team's lead writes to its parent. Siblings don't message each other: their parent carries what one needs from another.
- **Authority is the whole subtree.** A member, at its latest running start, may create, start, stop or delete anything below it along recorded parent edges, at any depth: its child members, the copies under it, and their members and copies. Never itself, its ancestors, its siblings or their subtrees, so a copy's top member can't stop or delete its own copy. A person may act on anything.
- **Stopping cascades.** `stop` of a copy or of the root's instance stops members deepest first, marking the copy `stopping` so nothing new is created or started inside it meanwhile; a later `stop` finishes an interrupted one. `start` brings back the copies that were running. `delete` of an instance with kept copies is refused, listing them.
- **Cycles are refused,** a team containing itself directly or not, at `validate` and again at `create` and a full restart. There's no depth limit. Every runtime tree shares one message store; no bus opt-in is needed.
- **Check seating before each launch.** The lead lists the launches caused by each `create` or `start` and runs `brg-seat route <vendor> <model>` for each. The [seating plugin](../plugins/seat/README.md) answers `go` or `wait` from subscription quota. Unknown or failed usage is wait, never another vendor or model. The launch commands do not run this check automatically.
- **One tree store:** messages across copy edges use full member addresses and plain tree-wide reply IDs. Copy deletion preserves history and checks unacked messages to its members under the tree lock; deleted recipients no longer count as pending work.

## What you never see

- **Storage.** One message store per team tree at its root, with continuous numbering across stops, starts and team-file changes. Reads scan it; there is no index or archiving.
- **brg versions.** Members call `"$BORGCHART_BIN"`, the brg that started them. If an upgraded brg can't read a team's messages, it says exactly what to do:

  ```
  brg: the tree's message store at `devteam-k3f9` uses an earlier format; follow the cutover in docs/bus.md#storage (move the old store aside, leave an empty private `messages/` folder, then start the root)
  ```

  brg refuses to start any instance in that tree, names the root that owns the store, and points to the [cutover steps](bus.md#storage).

- **Configuration changes** reach members when they next start, without stopping their running siblings.

## Limits

- **Trusted, same-user operation.** Members can reach each other's terminals; nothing isolates them.
- **A reboot stops every instance.** brg refuses a state folder under `/tmp`, so messages survive it.
- **`locked` members can't use messages yet.**

## Open

- **Sessions:** whether separate groups of teams (`--session`) are needed at all. They are kept minimal for now.
- **Secrets:** how a hosted brg would map them to a cloud secret manager.
