# Concepts

**Status: partly built.** Teams, instances, `roles:`, suffixed member IDs, copies up to `max`, member deletion and member status work today. Bus members may manage only their declared children, using their latest start ID. Teams as members (team roles whose members are child teams, #25) share one message store per runtime tree. The remaining command renames are planned.

## Definitions and what runs from them

Each definition noun has an active noun, created from it by `brg create`.

Definition folders and runtime working folders serve different purposes. A child
copy keeps its own instance and shares the tree's message store, with its working folder nested
under its parent instance, independently of where its team definition is stored.

| Definition | Active | Created by | Examples |
| --- | --- | --- | --- |
| **Team**: a folder you can share, holding `team.yaml` and role files, with local or catalog-shared includes and plugins | **Instance**: the team running in one working folder, with its members, messages and notes | `brg create dev-team ~/work/x` | `dev-team-k3f9`, `dev-team-p7xm` |
| **Role**: a named part in a team (a role file, a vendor, a parent, whether it starts with the instance, how many copies may run) | **Member**: one running agent filling the role | `brg create dev-team-k3f9/implementer` | `implementer-x7k2`, `lead-q8m3` |

- **Start:** one start of a member. It keeps today's unique start ID (incarnation); only a member's latest start may act as it.
- **Unchanged:**
  - **Plugins:** a *plugin* is a program the team declares, and a *plugin process* is one that runs alongside the members.
  - **Outside callers:** anyone outside the team, written `external:NAME`.

## Where things live

- **The team folder** holds its definition; sibling teams may use one catalog `shared/` folder for common includes and plugins:
  - the roles and their role files, includes and plugins;
  - whether the team uses the message bus, and its alert action (`bus:`);
  - each role's `posture`, its extra launch arguments (`args`, such as the researcher's `--search`), and whether each member works in its own git worktree (`worktree: true`);
  - plain plugin values, such as a channel ID.
- **The instance's working folder** is given at `create` (the current folder by default) and recorded on the instance for its whole life: usually the repository the team works on. Members start there, or in their own worktree for a `worktree: true` role. It can't be the same as, inside or around brg's state folder, so a member's own folder never exposes brg's files; brg's worktrees live beside the state folder, never in it.
- **The instance's state folder,** `~/.local/state/borgchart/<instance>/`, holds:
  - `messages/` at the tree root only, shared by all copies;
  - `notes/`, for members' reports and other files the members share (`$BORGCHART_NOTES`);
  - brg's own files.

  Codex `vendor` members run with full host access and no tool approvals, including access to brg's state files. Claude `vendor` members get the tree's `messages/` and their instance's `notes/` as added folders. Locked members retain their gate and restricted launch settings.

  `locked` members keep today's launch unchanged:
  - their gate;
  - no automatic trust for Codex;
  - writes limited to their granted plugins' data and their temp folder;
  - for Codex, a start in the temp folder unless the role sets `worktree: true`.

  A team with `bus:` still can't have `locked` members.
- **brg's global configuration,** `~/.config/borgchart/config.yaml`, holds what belongs to you rather than to a team:

  ```yaml
  schema: 1
  auth: login                   # login (default) | api_key, for every member
  auto_trust: true              # trust each instance's folder without a person
  teams:                        # where team names are looked up
    - ~/.config/borgchart/teams # the default
  plugins_dir: ~/borgchart-plugins   # where shared:<name> plugins live
  tmux: {user_config: true}     # load your own tmux config on brg's server (default: false)
  ```

  It's the same for every team, for now. `tmux.user_config` keeps today's meaning: it's read when brg's tmux server starts, and a change applies once every instance on that server is stopped. A hosted brg would use `api_key` only.
- **Secrets** are never stored by brg, and never written in a file you edit. A secret's value comes from the environment of the shell that runs `create` or `start` (for example, under your password manager's `run`), as today.
  - `brg secret <team>` lists the secret names the team's plugins need, and whether each is set in your environment. It stores and prints no values.
  - Members' windows start from a small base environment, so secrets don't reach them that way. The one exception is the vendor's own key: with `auth: api_key`, brg passes `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` to that vendor's members by name, and with `login` it passes neither. A plugin receives a secret only when its `env` names it: brg writes it to that plugin's private file (mode 0600), which the plugin's shims read, and removes the file at stop.
  - **0600 keeps other users out, not the members.** Members run as you. A member that isn't `locked` and has shell or file access could read that file while it exists. Only `locked` members are kept from it, by their gate.
- **Prompts** come only from the team: each role file and the includes it names. The vendor's own instruction files load as normal (`~/.claude/CLAUDE.md`, the repo's `CLAUDE.md`, `AGENTS.md`). brg neither adds nor suppresses anything else.
  - **Standing rules** go in the team's own files, without repeating what the vendors' files already give every member. There's no separate overlay for local rules.
- **Context for one piece of work,** such as a tracking issue or a handover note, goes to the lead in a message. It isn't configuration.

## Commands

There's no `up`. There are four verbs. `create` always makes something new; `start`, `stop` and `delete` are safe to repeat:

| Command | On an instance | On a member |
| --- | --- | --- |
| `brg create` | `create <team> <folder>` makes a new instance working in that folder (required), starts it, and prints its ID and tree. `<team>` is a team's name or a path to its folder. Run it twice and you have two instances. | `create <instance>/<role>` makes a new member of that role and starts it. `create` takes only a team or a role, never a member. It's refused past `max`; for a `max: 1` role whose member already exists, it points to `start`. |
| `brg start` | Brings the instance to what should be running. That's the `start: true` roles and the members that were running when it was last stopped. A `start: true` role whose member was deleted gets a fresh member. Members already running are left alone. Changed files are applied only when the instance was fully stopped. It takes no folder. | Starts that member with a new start. If it's already running, it says so and changes nothing. A deleted ID is refused. |
| `brg stop` | Stops every member. On a stopped instance, it says so and changes nothing. | Stops that member; the same on a stopped one. |
| `brg delete` | Removes a stopped instance: its notes, state and registration. Copy messages stay as history in the tree store; root deletion removes that store. It's refused while anything runs. Unacked messages, or messages it cannot read, are listed and need `--force`. | Removes a stopped member. Unacked messages are listed with a count and require `--force`; running or unknown liveness is always refused. Kept children, running or stopped, also prevent deletion, even with `--force`: their full addresses are listed; delete them first. Its ID stays issued. Immutable messages remain as history; unread ones to a deleted member stop counting as open work. |

**Restarting** is `stop` then `start`. That's also how changed files are applied.

**Copies.** `max` defaults to 1 and counts running and stopped members. A `start: true` role gets one member at instance creation; `start` reuses kept members and never fills to `max`. Lowering `max` after a full stop keeps all members and refuses further creation until enough are deleted. With a bus, member lifecycle commands require a person or the declared parent's latest running start. A replaced start, a sibling, the member itself or another instance's member is refused. Creating a child requires a kept parent member. No-bus teams and whole-instance commands keep their existing caller rules.

**A team, a path or a role.** A path starts with `/`, `./`, `../` or `~/`. Any other argument to `create` is a team's name when it has no `/`, and `<instance>/<role>` when it has one. So `brg create ./dev-team ~/work/x` runs the folder `./dev-team`, and `brg create teams/dev-team` is refused ("no instance `teams`; for a folder, write `./teams/dev-team`").

**Also:**
- `attach`, `logs` and `prompt` take a member address.
- `brg ls` lists the teams in the search paths, each with its instances, running or stopped.
- `brg status` shows the tree and each member's unread (unacked) count, including in `-i` and `--json`.
- `brg validate <team>` checks the team folder and the global configuration.

## Names and IDs

| What | Name | Example |
| --- | --- | --- |
| Team | Its folder's name. There's no `name:` field. | `~/.config/borgchart/teams/dev-team/` is `dev-team` |
| Instance | `<team>-<suffix>`. The suffix is four characters from `23456789abcdefghjkmnpqrstuvwxyz` (no `0 1 i l o`), random, checked against every instance brg has registered, and never reused. | `dev-team-k3f9` |
| Member | `<role>-<suffix>`, with the same kind of suffix as instances, never reused within the instance | `lead-q8m3`, `implementer-x7k2` |
| Start | A unique ID for one start of a member (an incarnation), as today | `implementer-x7k2.20261108000000000.ab12` |
| Member address | `[instance/]member` | `dev-team-k3f9/implementer-x7k2` |
| Role address | `[instance/]<role>`, the bare role name. Only `create` takes it. Members always carry a suffix, so a bare name is never a member's ID. There's no role queue: messages go to members. | `dev-team-k3f9/implementer` |
| Message | `m<seq>`, unique within the tree | `m42` |

**Finding a team by name.** brg looks in every folder under `teams:`. A team is a subfolder holding a `team.yaml`.
- One match: that's the team.
- Two or more: brg lists their paths and asks for one. It never picks the first.
- A path always works, in or out of the search paths.

Two folders with the same name can both run. Their instances are told apart by their suffixes, and `brg ls` shows each one's folder.

**Leaving the instance out.** Inside a member, an address without an instance means that member's own instance, so a lead writes `implementer-x7k2`. Everywhere else the full instance ID is needed (`dev-team-k3f9/implementer-x7k2`, `brg start dev-team-k3f9`). There are no other shortcuts for now: brg never infers an instance from a team's name or from which instances are running. Message, inbox and bus commands take the instance as `--instance`; inside a member it's always the member's own.

**Members are always named in full.** Commands that act on a member (`attach`, `prompt`, `stop`), and messages, take its full ID (`lead-q8m3`). A role name there is refused, listing the role's members. There are no shortcuts for now.

**Parents are agent roles.** `parent: lead` names an agent role, and a member's parent is the member filling that role. So a parent role must have one member: `validate` refuses a `parent:` that names a team role or a role with `max > 1`. The same goes for a plugin's `delivers_to:`.

**Recipients.** A message goes to one kept, started member along a parent/child edge, or from a root member of the tree's root instance to `external:NAME`. An instance isn't a recipient: name a member in it. Across a copy edge, use its full member address. Siblings, cousins and independent roots cannot exchange messages. Every record carries full member addresses and a tree-wide message ID; replies reference plain IDs such as `m1`.

**Teams as members (#25).** A role can be a team, referenced by name through the configured team directories (`team: dev-team`). Its members are copies (`dev-a7k2`): nested instances, each with its own members, notes, state and frozen configuration, and a working folder inside its parent's. Teams form a tree: the member with no parent is the root. Addresses nest (`borgchart-engineering-h4k2/dev-a7k2/lead-c4nd`), and a copy's instance ID is its full path. Copies are created stopped, prepared by their parent and then started. A member may act on its whole subtree, and `stop` cascades deepest first. [teams.md](teams.md#teams-as-members) has the rules, and [`borgchart-engineering/`](../teams/borgchart-engineering/) defines the runtime tree from sibling definitions under `teams/`.

## Lifetimes

**An instance lives from `create` until `delete`.**
- **Stopping and starting:** `stop` and `start` keep its ID, messages and notes, and its members keep their IDs.
- **Members are kept:** a member exists from `create` until `delete`, running or stopped. `max` counts those members. A stopped member stays addressable and can be started again.
- **Its team folder:** the instance records the folder's path, not its name, so a same-named team elsewhere never takes its place. If the folder moves, `start` refuses, and `start <instance> --team <new path>` points it there. That's a change of files like any other: it applies only from a full stop, once every member is positively stopped.
- **Its working folder** is fixed at `create`. If it's gone, `start` refuses; create a new instance.
- **Message store:** one per team tree, in the root instance's state folder. Message numbers continue across stops, starts and copy deletion. Earlier formats require the [manual cutover](bus.md#storage).
- **Frozen configuration:** the team's files, with includes expanded, and the global settings that reach members (`auth`, `auto_trust`, and where `plugins_dir` puts each shared plugin) are frozen from a start to the next full stop. `start` applies changes only after a full `stop`, once every member is positively stopped.
- **Checkouts:** every operation that starts a member (`create`, `start`, `start --team`) checks its resolved working folder. It refuses if that folder is the same as, inside, or around a folder that another instance's member holds while running or while brg cannot tell it stopped. Members of one tree (an instance and its copies) may share a folder: a copy without `worktree: true` works in its parent's. Members work in the instance's folder unless their role sets `worktree: true`: then each member or copy gets its own git worktree from the team's folder, which brg adds when it issues it (`create`, or a full `start` that gives a `start: true` role a new member), detached at the folder's `HEAD` or on `create --branch <name>`. It lives beside brg's state folder, at `~/.local/state/borgchart-worktrees/<address>` (with `/` written as `.`); `create` prints it, and `status --json` gives it as `worktree`. `delete` removes it when it is clean, refuses while it holds uncommitted work or commits on no remote, and with `--force` leaves it as it is. Starting an existing member never adds its worktree; one that is gone refuses the start. The checks, creation and reservation happen under one registry lock.
- **A new message format:** brg refuses to start any instance whose tree store uses an earlier format, names the root that owns it, and points to the [cutover steps](bus.md#storage). `stop` and liveness never parse messages, so they still work on the old instance. A stopped instance holds no checkout; `delete` of unreadable messages needs `--force` ("its contents can't be inspected"). No storage generations are needed.
- **Durability:** brg refuses a state folder under `/tmp` or any other path cleaned by age. Reads scan the store; there is no index or archiving.

**Starts** keep today's IDs (incarnations), for fencing a replaced process, identifying its recorded tmux pane, liveness and stop. `BORGCHART_INCARNATION` is the current member start's ID, not a per-member number. Start records live in brg's own files outside messages. If their state is missing or unreadable, brg refuses to guess whether a member is stopped. Instance IDs are never reused, so a process left over from a deleted instance cannot act as a member of a new one.

## `brg status`

```
$ brg status
dev-team-k3f9                       team ~/.config/borgchart/teams/dev-team/ · folder ~/work/x
└─ lead-q8m3            claude opus high          working
   ├─ implementer-x7k2  codex gpt-6.1-sol high    working
   ├─ implementer-c4nd  codex gpt-6.1-sol high    idle
   ├─ reviewer          not created (start: false)
   └─ researcher        not created (start: false)
```

- **Header line:** the instance, then its team folder and working folder.
- **Tree lines:**
  - one line per member;
  - a role with no kept member gets `not created (start: false)` or `not created (start: true)`, following its `start` setting, or a dim `on demand (none created)` for `max > 1`. A created member that isn't running shows on its own line as `stopped`;
  - positive unread message counts appear beside members, also in `-i`; a zero adds nothing to the tree;
  - copies nest under their parent member, each with its full instance path.
- **`--json` member rows:** `instance`, `team`, `member`, `role`, `parent` (a member ID), `start_id` (the current incarnation, when recorded), and the existing fields. A stop retains `start_id`; restarting changes it. With a readable bus, `unread` gives each kept member's unacked count, including zero. Without a bus it is absent; if the store cannot be read, counts are absent and status adds a short note. Deleted members have no rows. Plugin-process rows keep `kind: plugin` and their name.

## Environment variables

Every `BORGCHART_*` variable that a member, plugin or test can see:

| Name | Seen by | Meaning |
| --- | --- | --- |
| `BORGCHART_INSTANCE` | members, plugins, alert actions | the instance (`dev-team-k3f9`) |
| `BORGCHART_ROLE` | members, plugin actions | the role (`implementer`) |
| `BORGCHART_MEMBER` | members, plugin actions | the member (`implementer-x7k2`) |
| `BORGCHART_INCARNATION` | members | the member's current start ID; brg refuses a stopped or replaced start |
| `BORGCHART_NOTES` | members | the instance's notes folder |
| `BORGCHART_CONFIG` | the caller, members, tests | the global configuration file (default `~/.config/borgchart/config.yaml`); members get the caller's, so their own `brg create` finds the same teams |
| `BORGCHART_BIN` | members, plugins | the brg that started them |
| `BORGCHART_PARENT` | plugin actions; the members of a copy that have no parent role | the calling member's parent (a member); for a copy's top members, the full address of the member the copy belongs to |
| `BORGCHART_DELIVER_TO` | plugin processes | the member a process hands work to (its `delivers_to` role's only member) |
| `BORGCHART_STATE_DIR` | plugins | the plugin's private folder |
| `BORGCHART_TMUX_SOCKET` | members, plugins, tests | which tmux server runs the instance |
| `BORGCHART_TMUX_TMPDIR` | members, plugins, tests | where that server keeps its socket; keeps test servers private |
| `BORGCHART_STATE_HOME` | the caller, tests | brg's state folder, holding every instance's state folder |
| `BORGCHART_GATE_WAIT_MS` | the caller, tests | how long a start waits for a `locked` member's gate |
| `BORGCHART_WAIT_MIN_MS`, `BORGCHART_WAIT_TIMEOUT_MS` | tests (test builds only) | shorten the inbox wait's minimum and timeout |
| `BORGCHART_QUALIFY`, `…_VENDORS`, `…_SUBMIT` | the manual vendor qualification test | turn that test on and pick its vendors |
