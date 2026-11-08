# Defining a brg team

Teams, instances, roles, copies up to `max` and the four verbs (`create`, `start`, `stop`, `delete`) work today. The department section below describes future work. [concepts.md](concepts.md) records the design; [format.md](format.md) describes the shipped format. Items marked **Open** are not decided yet. For command syntax, `brg … --help` is the authority; the tables below are a short summary.

## The files

A real team lives in [`dev-team/`](../dev-team/): the team this repository is developed with, plus [`dev-design-team/`](../dev-design-team/), the design panel its lead seats. The sketch below is a smaller version of it:

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

- **`models`:** pins a model, and optionally an effort, per vendor. Only the member's own vendor needs an entry; `dev-team/` names both, so moving a member to the other vendor is a one-line change to its `vendor:` in `team.yaml`.
- **Body:** the standing instruction. It's sent once, when the member starts.
- **Includes:** one level deep.

### Instances

```
$ brg create devteam ~/work/project
devteam-k3f9  team ~/.config/borgchart/teams/devteam · folder ~/work/project
└─ lead-q8m3            claude opus high          started
   ├─ implementer       not created (start: false)
   └─ reviewer          not created (start: false)
```

- **`create`** makes a new instance in that folder, with a suffixed ID; run it twice and you have two. The team's `bus: {}` turns messages on.
- **Per role** in `team.yaml`: `posture`, extra vendor `args` and an optional `cwd` inside the instance's folder. Standing rules go in the team's own files; context for one piece of work goes to the lead in a message.
- **Secrets:** brg stores none. They come from your environment by name when you run `brg create` or `brg start`; `brg secret devteam` lists which are set.

## Commands

A small top level for people. `brg --help` and `brg <command> --help` give the full syntax; inside a member, the help also covers plain message drafts, acknowledgement and the one inbox wait.

| Command | What it does |
| --- | --- |
| `brg validate <team>` | Check everything without starting anything |
| `brg create <team> <folder>`, `brg create <instance>/<role>` | Make a new instance, or a role's member, and start it. Prints the tree with each member's model and what happened. |
| `brg start <instance>[/<member>]`, `brg stop <instance>[/<member>]` | Bring it back, or stop it |
| `brg delete <instance>[/<member>] [--force]` | Remove a stopped instance or member. Unread messages require `--force`; running or unknown liveness is always refused. A deleted member's ID stays issued. |
| `brg status [instance] [--json \| -w \| -i]` | The live tree: who is idle, working or blocked, with positive unread message counts. JSON includes `member`, `role`, `parent` (a member ID), `start_id` (the current incarnation, when recorded), and `unread`, including zero with a readable bus. No bus means no counts; an unreadable store omits counts and adds a check note. |
| `brg ls` | Your teams, each with its instances |
| `brg attach <instance>/<member>`, `brg logs <instance>/<member>` | Watch a member |
| `brg send <instance>/<member> TEXT` | Type into a member now |
| `brg msg …`, `brg inbox …`, `brg bus check` | Durable messages and store health, below |

**Messages**, mostly used by members:

| Command | What it does |
| --- | --- |
| `brg msg publish [--record FILE\|-] [--external NAME] [--json]` | Publish a new message from `{to, body, reply_to?}`, on stdin by default |
| `brg inbox [--member ID \| --external NAME] [--full] [--json]` | List every unacked message, oldest first; --full includes bodies |
| `brg msg read ID [--json]` | Read one message, including an acknowledged one |
| `brg msg ack ID… [--external NAME] [--json]` | Acknowledge messages addressed to you |
| `brg inbox wait [--external NAME] [--timeout SECONDS] [--full] [--json]` | Wait for your inbox: new messages at once, existing ones after 60 seconds if still unacked; timeout 60–86400, default 86400. The 60 s floor matches the loop guard on every nonzero exit, including timeout |
| `brg bus check [--json]` | Report gaps and unreadable files without writing |

**Addressing.** `devteam-k3f9/lead-q8m3`. Inside a member, a bare member ID means its own instance; everywhere else the instance is named in full ([concepts](concepts.md#names-and-ids)). Only `create` takes a role name. Member commands and messages refuse a role name, listing its kept members or naming `brg create <instance>/<role>` when it has none. Bus commands take `--instance ID` outside a member. An outside caller, for example a person's script or a coordinating agent, adds `--external NAME` (refused inside a member) and is answered at `external:NAME`.

## How members talk

- **`send`** types text into a member's input box right away, under checks: not mid-turn, not at a dialog. It's for people steering a member.
- **`msg publish`** writes a durable message. It is never typed into a terminal, survives restarts and is acknowledged explicitly. A member publishes to its parent or children; only the root also publishes to outside callers.
- **Work is prose.** The lead writes a brief file, sends a message naming it and starts the helper. The helper reads `inbox --full`, acks once it takes responsibility, writes its report atomically and replies naming it with `reply_to` the brief message. The lead verifies the report, diff and checks itself, stops the helper, then checks status shows it stopped before reusing the checkout. One writer per checkout and one piece of work per helper are team rules. Progress and delegation live in pull request descriptions and comments.

**Getting a member's attention.** Ack everything you have taken on, then arm one `inbox wait`: Claude with its Bash tool in background mode, Codex in the foreground with `--timeout 600`. New unacked messages return at once; messages already there wait 60 seconds and return only if still unacked. Errors wait at least 60 seconds too, except explicit help and signals. On exit 9 wait again; on exit 8 (`fenced`) stop waiting and tell the owner. A message left unacked wakes a member about once a minute.

The attention worker and watcher are not wired now; the lead nudges with `brg send`. By each brief's deadline it checks for a reply, then status, and sends `brg send <instance>/<member> 'check your inbox'` or restarts a stopped member with `brg start`. The guarded, vendor-qualified typed wake exists in the library, uncalled; see [the bus](bus.md#attention).

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
    cwd: copies/{member_id}            # optional; brg creates the empty leaf; copies/ must exist
```

- **Starting a copy.** Each `brg create <instance>/implementer` starts one new copy, with its own suffixed ID. Past `max` it's refused, listing all kept copies, stopped ones included. For `max: 1`, the refusal points to `start`. Each copy gets the same role file and its own name; its folder follows `cwd`.
- **The tree** shows every kept copy with its state and unread count. A `max > 1` role with no kept members gets one dim line:

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

- **Capacity and restarts.** `max` is optional, defaults to 1 and must be at least 1. `start: true` with `max > 1` makes one member at instance creation. `start` reuses kept members and never fills to `max`; a `start: true` role with none left gets one new member. Lowering `max` after a full stop keeps every member and refuses creation until enough are deleted. Deleted suffixes are never reused.
- **Parents.** `parent:` and a plugin's `delivers_to:` require a `max: 1` role. With a bus, only a person or the declared parent's latest running start may create, start, stop or delete a child member. Members cannot manage themselves or siblings, and replaced starts and members of another instance are refused. Creating a child requires a kept parent member. No-bus teams and whole-instance commands retain their existing caller rules.
- **Folders.** `{member_id}` in `cwd` is optional. Without it, copies share the role's folder. If used, it must be the last folder name, with no `..`. When `cwd` ends in `{member_id}`, brg makes only the missing empty leaf when issuing a new member (`create`, or a full `start` that gives a `start: true` role a new member), for any `max`; every ancestor must already exist, and an existing leaf is used as is. The parent puts a worktree in it before briefing the copy. Starting an existing member never creates its folder, and `delete` leaves them alone. Copies of a `{member_id}` role may not use equal, nested or symlinked aliases of each other's folders, even while stopped. Creation and every start check folders under the registry lock. Different roles may share a folder; the team's instructions still enforce one writer per checkout.
- **Taking work and attention** remain team rules. The lead chooses a member, gives it one brief, tracks its reply and checks its deadline. An ended copy remains addressable with unread counts until deleted; the lead can restart it with `brg start <instance>/<member>`.

## Teams as members (departments) (Open: design only, a later change)

A member can be a whole team. A **department** is just a team whose members include teams. It's a name for that shape, not a new kind of object.

```yaml
# coding/team.yaml
roles:
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
2. **One message store per department, not one per sub-team.** Copies are namespaced members (`ws-2/lead`) in the department's single store. A parent-to-child message is then an ordinary local record, with one sequence, one unread-count reader and one check. Separate stores would need delivery between stores.
3. **A sub-team copy can't write to outside callers.** Only the outermost team's top member answers `external:NAME`. Inside a department, a sub-team's top member writes to its parent. Otherwise every workstream could reach the person directly, around the department's lead.
4. **Per-copy folders come from the role's `cwd` with `{member_id}`,** inside the department's one working folder.
5. **The routing plugin starts as go/wait only.** Letting it change a copy's vendor or model would make `create` take overrides that member files currently pin. Add overrides only if go/wait proves too little.

Open questions:

- **Depth.** Cycles (a team containing itself, directly or not) are refused at `validate`. Is there also a depth limit? Proposed: 3 levels, raise it if needed.
- **Standalone use.** Can a sub-team's folder also run as an ordinary team? Proposed: yes, as its own instance.
- **Storage.** Shared with the department (proposed above) or one store per sub-team.
- **Stopping.** `brg stop <instance>/ws-2` stops the whole copy. The lead remains responsible for unfinished work and must verify each writer stopped before reusing a checkout.
- **Siblings.** Can `ws-1/lead` message `ws-2/lead` directly? Proposed: no, only through their common parent, as for any siblings.
- **Display.** Should `status` fold a busy sub-team into one line by default, with `--all` expanding it?

## What you never see

- **Storage.** One message store per instance for its whole life, with continuous numbering across stops, starts and team-file changes. Reads scan it; there is no index or archiving.
- **brg versions.** Members call `"$BORGCHART_BIN"`, the brg that started them. If an upgraded brg can't read a team's messages, it says exactly what to do:

  ```
  brg: instance `devteam-k3f9`'s message storage is damaged: store.json names another format; repair it by hand, or create a new instance (`brg create`)
  ```

  brg refuses to start such an instance, and points to `brg create` for a fresh one; its open work has to be sent again.

- **Configuration changes** take effect at the next full `brg stop` and `brg start`. Starting one member into a running instance with changed files says so, in instance and role terms.

## Limits

- **Trusted, same-user operation.** Members can reach each other's terminals; nothing isolates them.
- **A reboot stops every instance.** brg refuses a state folder under `/tmp`, so messages survive it.
- **`locked` members can't use messages yet.**

## Open

- **Teams as members** (above).
- **Sessions:** whether separate groups of teams (`--session`) are needed at all. They are kept minimal for now.
- **Secrets:** how a hosted brg would map them to a cloud secret manager.
- **When renames land:** `prompt`, `message …`, `rules:` and the folded commands arrive in one follow-up change (#19). Until then the commands keep today's names (`send`, `msg`, `inbox`, `bus check`).
