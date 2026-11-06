# Concepts

**Status: proposal, revised after the owner's answers.** Nothing here is built yet. It drives the renames (#19), roles with copies (#24) and teams as members (#25). The last open questions are in PR #50.

## Definitions and what runs from them

Each definition noun has an active noun, created from it by `brg create`.

| Definition | Active | Created by | Examples |
| --- | --- | --- | --- |
| **Deployment**: a `.yaml` file saying how and where a team runs (folders, logins, values, message storage) | **Instance**: the team running from that deployment, with its members, messages and tasks | `brg create dev-team` | `dev-team-k3f9`, `dev-team-p7xm` |
| **Role**: a named part in a team (a role file, a vendor, a parent, whether it starts with the instance, how many copies may run) | **Member**: one running agent filling the role | `brg create dev-team-k3f9/role:implementer` | `implementer`, `implementer-2` |

- **Team:** the set of roles, a team folder with `team.yaml` and its role files. It's reusable and says nothing about one machine. A team runs only through a deployment, so it has no active noun of its own. Every run needs a deployment file.
- **Start:** one start of a member. Starts are numbered per member, and only a member's latest start may act as it.
- **Unchanged:**
  - **Plugins:** a *plugin* is a program the team declares, and a *plugin process* is one that runs alongside the members.
  - **Outside callers:** anyone outside the team, written `external:NAME`.

## Commands

There's no `up`. There are four verbs. `create` always makes something new; `start`, `stop` and `delete` are safe to repeat:

| Command | On an instance | On a member |
| --- | --- | --- |
| `brg create` | `create <deployment>` makes a new instance, starts it, and prints its ID and tree. Run it twice and you have two instances. | `create <instance>/role:<role>` makes a new member and starts it. It's refused past `max`. For a `max: 1` role whose member already exists, it points to `start`. |
| `brg start` | Brings the instance to what should be running. That's the `start: true` roles, the members that were running when it was last stopped, and every member holding an open task or checkout. Members already running are left alone. Changed files are applied only when the instance was fully stopped. | Starts that member with a new start. If it's already running, it says so and changes nothing. |
| `brg stop` | Stops every member. On a stopped instance, it says so and changes nothing. | Stops that member; the same on a stopped one. |
| `brg delete` | Removes a stopped instance: its messages, state and registration. It's refused while anything runs. If the instance holds open work, it lists that work and needs `--force`. | Removes a stopped member that holds no open work, freeing its place under `max`. Its number isn't reused. |

**Restarting** is `stop` then `start`. That's also how changed files are applied.

**Also:**
- `attach`, `logs` and `prompt` take a member address.
- `brg ls` lists instances by deployment, running or stopped.
- `brg status` shows the tree.

## Names and IDs

| What | Name | Example |
| --- | --- | --- |
| Deployment | Its file name without `.yaml`; for a file named `borgchart.yaml`, its folder's name. There's no `name:` field. | `dev-team.yaml` is `dev-team` |
| Instance | `<deployment>-<suffix>`. The suffix is four characters from `23456789abcdefghjkmnpqrstuvwxyz` (no `0 1 i l o`), random, checked against every instance brg has registered, and never reused. | `dev-team-k3f9` |
| Member | The role's name for its first member, then `<role>-2`, `<role>-3`; numbers are never reused within the instance | `lead`, `implementer-2` |
| Start | A number per member, from 1 | 3 |
| Member address | `[instance/]member` | `dev-team-k3f9/implementer-2` |
| Role address | `[instance/]role:<role>`. It's used by `create`, and for a message to the role's shared queue (#24). It's spelled like `external:NAME`, so it can't be confused with the role's first member. | `dev-team-k3f9/role:implementer` |
| Message | `[instance/]m<seq>` | `dev-team-k3f9/m42` |

**Leaving the instance out.** An address may omit the instance:
- inside a member, where it means that member's own instance;
- when exactly one running instance has that member;
- when the deployment's name is given and that deployment has exactly one instance (`brg start dev-team`).

Otherwise brg lists the candidates and asks for the ID. It never picks by order.

**Parents are roles.** `parent: lead` names a role, and a member's parent is the member filling that role. So a parent role must have one member: `validate` refuses a `parent:` that names a role with `max > 1`.

**Departments (#25).** A role can be a team. Its members (`ws`, `ws-2`) have their own sub-members (`dev-k3f9/ws-2/lead`), inside the department's instance and its one message store.

## Lifetimes

**An instance lives from `create` until `delete`.**
- **Stopping and starting:** `stop` and `start` keep its ID, messages and open tasks, and its members keep their IDs.
- **Members are kept:** a member exists from `create` until `delete`, running or stopped. `max` counts those members. A stopped member stays addressable and can be started again.
- **Its deployment file:** the instance records the file's path. If the file moves, `start <instance> --file <new path>` points it there.
- **Message store:** one per instance for its whole life, keyed by the instance ID, so message numbers never restart.
- **Frozen configuration:** the configuration is frozen from a start to the next full stop. `start` applies changes only after a full `stop`, once every member is positively stopped.
- **Orphaned work:** `start` refuses while the new configuration orphans open work (a removed role that holds a task or checkout, say), and says what to cancel or restore.
- **Checkouts:** every operation that starts a member (`create`, `start`, `start --file`) checks its resolved working folder. It refuses if that folder is the same as, inside, or around a folder that another instance's member holds, running or stopped with an open checkout. The check and the reservation happen together, under one lock, so two starts can't race.
- **A new message format:** brg refuses to start an instance whose messages it can't read, and points to `brg create` for a fresh instance (its open work has to be sent again). `stop` and liveness never parse messages, so they still work on the old instance. Its checkouts stay reserved, and `delete` of it needs `--force` ("its contents can't be inspected"). No storage generations are needed.
- **No age-based rollover:** `validate` refuses a message root under `/tmp` or any other path cleaned by age. An index keeps reads cheap as the store grows.
- **The upgrade:** the switch to this layout converts each team's current storage once, into an instance, so open work survives it.

**Starts** do the jobs of today's incarnations:
- fencing a replaced process ("this start of `implementer-2` has been replaced");
- the recorded tmux pane that decides whether the start is still running;
- the task writers that must be gone before a checkout is freed;
- what the background wait has already shown.

A start is identified by its instance, member and number. Instance IDs are never reused, so a process left over from a deleted instance can't pass as a member of a new one. Within an instance, start numbers never repeat. If brg's record of them is lost while the instance's messages remain, `restart` refuses rather than start counting again.

Today's *run* (a hidden storage generation) and *incarnation* are gone from code, storage and records.

## `brg status`

```
$ brg status
dev-team-k3f9                       deployment ~/.config/borgchart/dev-team.yaml · team dev-team/
└─ lead              claude opus high          working
   ├─ implementer    codex gpt-6.1-sol high    working
   ├─ implementer-2  codex gpt-6.1-sol high    idle
   ├─ reviewer       claude opus high          not created (start: false)
   └─ researcher     on demand (none running)
```

- **Header line:** the instance, then its deployment file and team folder.
- **Tree lines:**
  - one line per member;
  - a role with copies and none running (here `researcher`, `max: 2`) gets one dim line;
  - department members nest.
- **`--json` member rows:** `instance`, `deployment`, `member`, `role`, `parent` (a member ID), `start`, and the existing fields. Plugin-process rows keep `kind: plugin` and their name.

## Every use of "team" today

| Today | Means | Proposed |
| --- | --- | --- |
| `brg up` / `down` / `status` "the team" (help, output) | the running thing | instance |
| `brg ls` "your teams" | deployments and what runs from them | instances, by deployment |
| `[team/]member` (sessions design) | instance | `[instance/]member` |
| team name from the deployment's folder | instance | `<deployment>-<suffix>` |
| errors: "team `x` was restarted", "no team named `x`" | instance | "instance `x`" |
| `BORGCHART_DEPLOYMENT` (a storage key), `BORGCHART_TEAM` (earlier proposal) | instance | `BORGCHART_INSTANCE` |
| JSON `team` (earlier proposal) | instance | `instance`, plus `deployment` |
| `--team DIR`, or a lone `./team.yaml`: a team folder run without a deployment | the definition | removed: every run needs a deployment file |
| `team:` in `borgchart.yaml` | the team folder | unchanged |
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
| `borgchart.yaml` | `members:` (per-member values) | `roles:` |
| | — | `{instance}` in `cwd` for parallel instances, `{n}` for copies |
| CLI | `brg up` | `create` (new instance or member) and `start` (existing ones) |
| | `brg up --member X`, `brg down --member X` | `create <instance>/role:X`, then `start` or `stop <instance>/X` |
| | `brg down` | `brg stop` |
| | `--forget` (sessions design) | `delete` |
| | `brg bus carry --from RUN` | removed |
| Output | `status --json` `name`, `kind` | `member`, `role`, `instance`, `deployment`, `start`; `kind` stays |
| | `not started` | `not created` |
| Records | `actor: {type: member, member, incarnation}`, `writer_incarnation`, the internal `run` field | `actor: {type: member, member, start}`, `writer_start`, no `run`: one message-format change, with the single store |
| On disk | `<bus.root>/<name>-<hash>/current`, `r1/…`, `runtime/incarnations/…` | `<bus.root>/<instance>/…`, `members/<member>/starts/<n>.json` |
| | brg state `…/borgchart/<name>-<hash>/` | `…/borgchart/<instance>/`, plus the registry of instances |
| Docs | "member" for both the part and the agent, "run", "incarnation", "team" for the running thing, `up` | "role", "member", "start", "instance", the four verbs. Covers `format.md` (and its older-shape steps), `teams.md`, `sessions.md`, `bus.md`, `cli.md`, the `--help` text, and the `dev-team/` role files. |

**Upgrading.** Stop every team before upgrading. The new brg converts each team's storage into an instance once, and refuses to start anything while a tmux session under an old name exists for that deployment.

## Environment variables

Every `BORGCHART_*` variable that a member, plugin or test can see:

| Today | Seen by | Proposed | Meaning |
| --- | --- | --- | --- |
| `BORGCHART_DEPLOYMENT` | members, plugins, alert actions | `BORGCHART_INSTANCE` | the instance (`dev-team-k3f9`). Today's value is a storage key. |
| — | members, plugin actions | `BORGCHART_ROLE` | the role (`implementer`) |
| `BORGCHART_MEMBER` | members, plugin actions | unchanged | the member (`implementer-2`) |
| `BORGCHART_RUN`, `BORGCHART_INCARNATION` | members | `BORGCHART_START` | the member's start number; brg refuses an earlier start |
| `BORGCHART_FILE` | members, plugins, the caller | unchanged | the deployment file |
| `BORGCHART_BIN` | members, plugins | unchanged | the brg that started them |
| `BORGCHART_PARENT` | plugin actions | unchanged | the calling member's parent (a member) |
| `BORGCHART_DELIVER_TO` | plugin processes | unchanged | the role address a process hands work to (its queue, for a role with copies) |
| `BORGCHART_STATE_DIR` | plugins | unchanged | the plugin's private folder |
| `BORGCHART_ACTOR` | alert actions | unchanged | `bus`: an alert action can't act as a member |
| `BORGCHART_TMUX_SOCKET` | members, plugins, tests | unchanged (decided) | which tmux server runs the instance |
| `BORGCHART_TMUX_TMPDIR` (#47) | members, plugins, tests | unchanged (decided) | where that server keeps its socket; keeps test servers private |
| `BORGCHART_STATE_HOME` | the caller, tests | unchanged | brg's state folder |
| `BORGCHART_GATE_WAIT_MS` | the caller, tests | unchanged; a tuning knob | how long a start waits for a `locked` member's gate |
| `BORGCHART_WAIT_MIN_MS`, `BORGCHART_WAIT_TIMEOUT_MS` | tests (test builds only) | unchanged | shorten the background wait's minimum and timeout |
| `BORGCHART_QUALIFY`, `…_VENDORS`, `…_SUBMIT` | the manual vendor qualification test | unchanged | turn that test on and pick its vendors |

## Effects on #24 and #25

Their acceptance text says `brg up --member implementer` starts the next copy; that becomes `brg create <instance>/role:implementer`. #25's departments are members whose role is a team, inside one instance. Update both issues once the names are agreed.
