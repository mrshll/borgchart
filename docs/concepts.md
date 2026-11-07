# Concepts

**Status: agreed design, not built yet.** It drives the renames (#19), roles with copies (#24) and teams as members (#25). The old names go away outright, with no aliases or pointers.

## Definitions and what runs from them

Each definition noun has an active noun, created from it by `brg create`.

| Definition | Active | Created by | Examples |
| --- | --- | --- | --- |
| **Team**: a folder you can share, holding `team.yaml`, its role files, includes and plugins | **Instance**: the team running in one working folder, with its members, messages, notes and tasks | `brg create dev-team ~/work/x` | `dev-team-k3f9`, `dev-team-p7xm` |
| **Role**: a named part in a team (a role file, a vendor, a parent, whether it starts with the instance, how many copies may run) | **Member**: one running agent filling the role | `brg create dev-team-k3f9/implementer` | `implementer-x7k2`, `lead-q8m3` |

- **No deployment file.** What used to be in one is now in the team, in brg's global configuration, in the folder given at `create`, or worked out by brg for each instance (below).
- **Start:** one start of a member. Starts are numbered per member, and only a member's latest start may act as it.
- **Unchanged:**
  - **Plugins:** a *plugin* is a program the team declares, and a *plugin process* is one that runs alongside the members.
  - **Outside callers:** anyone outside the team, written `external:NAME`.

## Where things live

- **The team folder** holds everything that can be shared:
  - the roles and their role files, includes and plugins;
  - whether the team uses the message bus, and its alert action (`bus:`);
  - each role's `posture`, its extra launch arguments (`args`, such as the researcher's `--search`), and optionally a `cwd` inside the instance's folder;
  - plain plugin values, such as a channel ID.
- **The instance's working folder** is given at `create` and recorded on the instance for its whole life. Members start there, or in their role's `cwd` inside it. The folder is required: there's no default for now.
- **The instance's state folder,** `~/.local/state/borgchart/<instance>/`, holds:
  - `messages/`, its one message store;
  - `notes/`, for members' reports and other files the members share (`$BORGCHART_NOTES`);
  - brg's own files.

  brg makes `messages/` and `notes/` writable for every member (Codex's sandbox writable roots, Claude's added folders). It never does that for its own files: gate rules, shims, launch files and secrets.
- **brg's global configuration,** `~/.config/borgchart/config.yaml`, holds what belongs to you rather than to a team:

  ```yaml
  schema: 1
  auth: login                   # login (default) | api_key, for every member
  auto_trust: true              # trust each instance's folder without a person
  teams:                        # where team names are looked up
    - ~/.config/borgchart/teams # the default
  plugins_dir: ~/borgchart-plugins   # where shared:<name> plugins live
  ```

  It's the same for every team, for now. A hosted brg would use `api_key` only.
- **Secrets** are never stored by brg, and never written in a file you edit. A secret's value comes from the environment of the shell that runs `create` or `start` (for example, under your password manager's `run`), as today.
  - `brg secret <team>` lists the secret names the team's plugins need, and whether each is set in your environment. It stores and prints no values.
  - Members' windows start from a small base environment, so secrets don't reach them that way. A plugin receives a secret only when its `env` names it: brg writes it to that plugin's private file (mode 0600), which the plugin's shims read, and removes the file at stop.
  - **0600 keeps other users out, not the members.** Members run as you. A member that isn't `locked` and has shell or file access could read that file while it exists. Only `locked` members are kept from it, by their gate.
- **Prompts** come only from the team: each role file and the includes it names. The vendor's own instruction files load as normal (`~/.claude/CLAUDE.md`, the repo's `CLAUDE.md`, `AGENTS.md`). brg neither adds nor suppresses anything else.
  - **Standing rules** go in the team's own files, without repeating what the vendors' files already give every member. There's no separate overlay for local rules.
- **Context for one piece of work,** such as a tracking issue or a handover note, goes to the lead in a message. It isn't configuration.

## Commands

There's no `up`. There are four verbs. `create` always makes something new; `start`, `stop` and `delete` are safe to repeat:

| Command | On an instance | On a member |
| --- | --- | --- |
| `brg create` | `create <team> <folder>` makes a new instance working in that folder (required), starts it, and prints its ID and tree. `<team>` is a team's name or a path to its folder. Run it twice and you have two instances. | `create <instance>/<role>` makes a new member of that role and starts it. `create` takes only a team or a role, never a member. It's refused past `max`; for a `max: 1` role whose member already exists, it points to `start`. |
| `brg start` | Brings the instance to what should be running. That's the `start: true` roles, the members that were running when it was last stopped, and every member holding an open task or checkout. Members already running are left alone. Changed files are applied only when the instance was fully stopped. It takes no folder. | Starts that member with a new start. If it's already running, it says so and changes nothing. |
| `brg stop` | Stops every member. On a stopped instance, it says so and changes nothing. | Stops that member; the same on a stopped one. |
| `brg delete` | Removes a stopped instance: its messages, notes, state and registration. It's refused while anything runs. If the instance holds open work, it lists that work and needs `--force`. | Removes a stopped member that holds no open work, freeing its place under `max`. Its number isn't reused. |

**Restarting** is `stop` then `start`. That's also how changed files are applied.

**A team, a path or a role.** A path starts with `/`, `./`, `../` or `~/`. Any other argument to `create` is a team's name when it has no `/`, and `<instance>/<role>` when it has one. So `brg create ./dev-team ~/work/x` runs the folder `./dev-team`, and `brg create teams/dev-team` is refused ("no instance `teams`; for a folder, write `./teams/dev-team`").

**Also:**
- `attach`, `logs` and `prompt` take a member address.
- `brg ls` lists the teams in the search paths, each with its instances, running or stopped.
- `brg status` shows the tree.
- `brg validate <team>` checks the team folder and the global configuration.

## Names and IDs

| What | Name | Example |
| --- | --- | --- |
| Team | Its folder's name. There's no `name:` field. | `~/.config/borgchart/teams/dev-team/` is `dev-team` |
| Instance | `<team>-<suffix>`. The suffix is four characters from `23456789abcdefghjkmnpqrstuvwxyz` (no `0 1 i l o`), random, checked against every instance brg has registered, and never reused. | `dev-team-k3f9` |
| Member | `<role>-<suffix>`, with the same kind of suffix as instances, never reused within the instance | `lead-q8m3`, `implementer-x7k2` |
| Start | A number per member, from 1 | 3 |
| Member address | `[instance/]member` | `dev-team-k3f9/implementer-x7k2` |
| Role address | `[instance/]<role>`, the bare role name. Only `create` takes it. Members always carry a suffix, so a bare name is never a member's ID. There's no role queue: messages go to members. | `dev-team-k3f9/implementer` |
| Message | `[instance/]m<seq>` | `dev-team-k3f9/m42` |

**Finding a team by name.** brg looks in every folder under `teams:`. A team is a subfolder holding a `team.yaml`.
- One match: that's the team.
- Two or more: brg lists their paths and asks for one. It never picks the first.
- A path always works, in or out of the search paths.

Two folders with the same name can both run. Their instances are told apart by their suffixes, and `brg ls` shows each one's folder.

**Leaving the instance out.** Inside a member, an address without an instance means that member's own instance, so a lead writes `implementer-x7k2`. Everywhere else the full instance ID is needed (`dev-team-k3f9/implementer-x7k2`, `brg start dev-team-k3f9`). There are no other shortcuts for now: brg never infers an instance from a team's name or from which instances are running. Message, inbox and task commands take the instance as `--instance`; inside a member it's always the member's own.

**Members are always named in full.** Commands that act on a member (`attach`, `prompt`, `stop`), and messages, take its full ID (`lead-q8m3`). A role name there is refused, listing the role's members. There are no shortcuts for now.

**Parents are roles.** `parent: lead` names a role, and a member's parent is the member filling that role. So a parent role must have one member: `validate` refuses a `parent:` that names a role with `max > 1`. The same goes for a plugin's `delivers_to:`.

**Departments (#25).** A role can be a team, named as any team is. Its members (`ws-a7k2`) have their own sub-members (`dev-k3f9/ws-a7k2/lead-q8m3`), inside the department's instance, its one message store and its working folder.

## Lifetimes

**An instance lives from `create` until `delete`.**
- **Stopping and starting:** `stop` and `start` keep its ID, messages, notes and open tasks, and its members keep their IDs.
- **Members are kept:** a member exists from `create` until `delete`, running or stopped. `max` counts those members. A stopped member stays addressable and can be started again.
- **Its team folder:** the instance records the folder's path, not its name, so a same-named team elsewhere never takes its place. If the folder moves, `start` refuses, and `start <instance> --team <new path>` points it there. That's a change of files like any other: it applies only from a full stop, and the orphaned-work check applies.
- **Its working folder** is fixed at `create`. If it's gone, `start` refuses; create a new instance.
- **Message store:** one per instance for its whole life, in its state folder, so message numbers never restart.
- **Frozen configuration:** the team's files, with includes expanded, and the global settings that reach members (`auth`, `auto_trust`) are frozen from a start to the next full stop. `start` applies changes only after a full `stop`, once every member is positively stopped.
- **Orphaned work:** `start` refuses while the new configuration orphans open work (a removed role that holds a task or checkout, say), and says what to cancel or restore.
- **Checkouts:** every operation that starts a member (`create`, `start`, `start --team`) checks its resolved working folder. It refuses if that folder is the same as, inside, or around a folder that another instance's member holds, running or stopped with an open checkout. Members of one instance may share a folder. The check and the reservation happen together, under one lock, so two starts can't race.
- **A new message format:** brg refuses to start an instance whose messages it can't read, and points to `brg create` for a fresh instance (its open work has to be sent again). `stop` and liveness never parse messages, so they still work on the old instance. Its checkouts stay reserved, and `delete` of it needs `--force` ("its contents can't be inspected"). No storage generations are needed.
- **No age-based rollover:** brg refuses a state folder under `/tmp` or any other path cleaned by age. An index keeps reads cheap as the store grows.
- **The upgrade:** today's storage isn't converted. It stays on disk, unread. Stop every team before upgrading: brg refuses to start anything while a tmux session under an old name is alive.

**Starts** do the jobs of today's incarnations:
- fencing a replaced process ("this start of `implementer-x7k2` has been replaced");
- the recorded tmux pane that decides whether the start is still running;
- the task writers that must be gone before a checkout is freed;
- what the background wait has already shown.

A start is identified by its instance, member and number. Instance IDs are never reused, so a process left over from a deleted instance can't pass as a member of a new one. Within an instance, start numbers never repeat. If brg's record of them is lost while the instance's messages remain, `start` refuses rather than start counting again.

Today's *run* (a hidden storage generation) and *incarnation* are gone from code, storage and records.

## `brg status`

```
$ brg status
dev-team-k3f9                       team ~/.config/borgchart/teams/dev-team/ · folder ~/work/x
└─ lead-q8m3            claude opus high          working
   ├─ implementer-x7k2  codex gpt-6.1-sol high    working
   ├─ implementer-c4nd  codex gpt-6.1-sol high    idle
   ├─ reviewer          not created (start: false)
   └─ researcher     on demand (none running)
```

- **Header line:** the instance, then its team folder and working folder.
- **Tree lines:**
  - one line per member;
  - a role with copies and none running (here `researcher`, `max: 2`) gets one dim line;
  - department members nest.
- **`--json` member rows:** `instance`, `team`, `member`, `role`, `parent` (a member ID), `start`, and the existing fields. Plugin-process rows keep `kind: plugin` and their name.

## Every use of "team" today

| Today | Means | Proposed |
| --- | --- | --- |
| `brg up` / `down` / `status` "the team" (help, output) | the running thing | instance |
| `brg ls` "your teams" | deployments and what runs from them | teams, each with its instances |
| `[team/]member` (sessions design) | instance | `[instance/]member` |
| team name from the deployment's folder | instance | `<team>-<suffix>`, where the team's name is its folder's |
| errors: "team `x` was restarted", "no team named `x`" | instance | "instance `x`" |
| `BORGCHART_DEPLOYMENT` (a storage key), `BORGCHART_TEAM` (earlier proposal) | instance | `BORGCHART_INSTANCE` |
| JSON `team` (earlier proposal) | instance | `instance`, plus `team` (the team's name) |
| `--team DIR`, or a lone `./team.yaml`: a team run without a deployment | the definition | every team runs this way now, as `brg create <team>`. There's no discovery from the current folder. `--team` remains only on `start`, to re-point a moved team. |
| `team:` in `borgchart.yaml` | the team folder | removed, with the file |
| "team directory", `team.yaml` | the definition | unchanged ("team folder") |
| tmux session `brg-<name>-<hash>` | instance | `brg-<instance>` |
| docs: "team" for the running thing | instance | "instance"; "team" only for the definition |

## Other names

| Where | Today | Proposed |
| --- | --- | --- |
| `team.yaml` | `members:` | `roles:` |
| | member file (`prompt: implementer.md`) | role file (same key) |
| | `parent:`, `delivers_to:` name a member | name a role |
| | frontmatter `role:` (required, unused) | dropped |
| | plugin `env:` lists names | names, or `NAME: value` for a plain value; a name without a value is a secret |
| `borgchart.yaml` | the whole file | removed. `auth`, `auto_trust` and `plugins_dir` go to `config.yaml`; `bus:` (without `root` or the ages), each member's `args` and `posture`, and plugin values go to `team.yaml`; `cwd` becomes the folder given at `create`; `task:` becomes the team's includes, or a message to the lead; `bus.root` becomes the instance's state folder. |
| | `{instance}` in `cwd` | not needed: each instance has its own folder. `{member}` stays, for copies. |
| CLI | `brg up` | `create` (new instance or member) and `start` (existing ones) |
| | `brg up --member X`, `brg down --member X` | `create <instance>/X` (a role), then `start` or `stop <instance>/<member>` |
| | `brg down` | `brg stop` |
| | `--forget` (sessions design) | `delete` |
| | `--file`/`-f`, `$BORGCHART_FILE`, `./borgchart.yaml` | removed |
| | secrets from the environment `brg up` runs in | unchanged: the environment `create` or `start` runs in; `brg secret <team>` checks which are set |
| | `brg bus carry --from RUN` | removed |
| Output | `status --json` `name`, `kind` | `member`, `role`, `instance`, `team`, `start`; `kind` stays |
| | `not started` | `not created` |
| Records | `actor: {type: member, member, incarnation}`, `writer_incarnation`, the internal `run` field | `actor: {type: member, member, start}`, `writer_start`, no `run`: one message-format change, with the single store |
| On disk | `<bus.root>/<name>-<hash>/current`, `r1/…`, `runtime/incarnations/…` | `…/borgchart/<instance>/messages/…`, `members/<member>/starts/<n>.json` |
| | brg state `…/borgchart/<name>-<hash>/` | `…/borgchart/<instance>/`, with `messages/` and `notes/`, plus the registry of instances |
| Docs | "member" for both the part and the agent, "run", "incarnation", "deployment", "team" for the running thing, `up` | "role", "member", "start", "instance", the four verbs. Covers `format.md` (its `borgchart.yaml` section becomes `config.yaml`'s, and its older-shape steps), `teams.md`, `sessions.md`, `bus.md`, `cli.md`, the `--help` text, the example, and the `dev-team/` role files. |

## Environment variables

Every `BORGCHART_*` variable that a member, plugin or test can see:

| Today | Seen by | Proposed | Meaning |
| --- | --- | --- | --- |
| `BORGCHART_DEPLOYMENT` | members, plugins, alert actions | `BORGCHART_INSTANCE` | the instance (`dev-team-k3f9`). Today's value is a storage key. |
| — | members, plugin actions | `BORGCHART_ROLE` | the role (`implementer`) |
| `BORGCHART_MEMBER` | members, plugin actions | unchanged | the member (`implementer-x7k2`) |
| `BORGCHART_RUN`, `BORGCHART_INCARNATION` | members | `BORGCHART_START` | the member's start number; brg refuses an earlier start |
| — | members | `BORGCHART_NOTES` | the instance's notes folder |
| `BORGCHART_FILE` | members, plugins, the caller | removed | commands find the instance from `BORGCHART_INSTANCE` or `--instance` |
| — | the caller, tests | `BORGCHART_CONFIG` | the global configuration file (default `~/.config/borgchart/config.yaml`) |
| `BORGCHART_BIN` | members, plugins | unchanged | the brg that started them |
| `BORGCHART_PARENT` | plugin actions | unchanged | the calling member's parent (a member) |
| `BORGCHART_DELIVER_TO` | plugin processes | unchanged | the member a process hands work to (its `delivers_to` role's only member) |
| `BORGCHART_STATE_DIR` | plugins | unchanged | the plugin's private folder |
| `BORGCHART_ACTOR` | alert actions | unchanged | `bus`: an alert action can't act as a member |
| `BORGCHART_TMUX_SOCKET` | members, plugins, tests | unchanged (decided) | which tmux server runs the instance |
| `BORGCHART_TMUX_TMPDIR` (#47) | members, plugins, tests | unchanged (decided) | where that server keeps its socket; keeps test servers private |
| `BORGCHART_STATE_HOME` | the caller, tests | unchanged | brg's state folder, holding every instance's state folder |
| `BORGCHART_GATE_WAIT_MS` | the caller, tests | unchanged; a tuning knob | how long a start waits for a `locked` member's gate |
| `BORGCHART_WAIT_MIN_MS`, `BORGCHART_WAIT_TIMEOUT_MS` | tests (test builds only) | unchanged | shorten the background wait's minimum and timeout |
| `BORGCHART_QUALIFY`, `…_VENDORS`, `…_SUBMIT` | the manual vendor qualification test | unchanged | turn that test on and pick its vendors |

## Effects on #24 and #25

Their acceptance text says `brg up --member implementer` starts the next copy; that becomes `brg create <instance>/implementer`, and copies are named `implementer-<suffix>`. #25's departments are members whose role is a team, inside one instance and its working folder. Update both issues once the names are agreed.
