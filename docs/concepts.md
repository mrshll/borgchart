# Concepts

**Status: proposal, revised after the owner's first answers.** Nothing here is built yet. It drives the renames (#19), roles with copies (#24) and teams as members (#25). The questions still open are in PR #50.

## Definitions and what runs from them

Each definition noun has an active noun: the thing `brg up` creates from it.

| Definition | Active | Created by | Examples |
| --- | --- | --- | --- |
| **Role**: a named part in a team (a role file, a vendor, a parent, whether it starts with the team, how many copies may run) | **Member**: one running agent filling the role | `brg up --role implementer` | `implementer`, `implementer-2` |
| **Deployment**: a `.yaml` file that says how and where a team runs (folders, logins, values, message storage) | **Instance**: the team running from that deployment, with its members, messages and tasks | `brg up dev-team` | `dev-team`, `dev-team-2` |

**Team** is the set of roles: a team folder with `team.yaml` and its role files. It's reusable and says nothing about one machine. A team runs only through a deployment, so it has no active noun of its own. "Team" never means a running thing in commands, output or variables.

**Start**: one start of a member. Starts are numbered per member, and only a member's latest start may act as it.

Unchanged:
- **Plugins:** a *plugin* is a program the team declares, and a *plugin process* is one that runs alongside the members.
- **Outside callers:** anyone outside the team, written `external:NAME`.

## Names and IDs

| What | Name | Example |
| --- | --- | --- |
| Deployment | Its file name without `.yaml`. For a file literally named `borgchart.yaml`, its folder's name. There's no `name:` field. | `dev-team.yaml` is `dev-team` |
| Instance | The deployment's name. A second instance of the same deployment running at the same time takes `-2`, then `-3`. Numbers are never reused within a deployment. | `dev-team`, `dev-team-2` |
| Member | The role's name for a role with one member (`max: 1`, the default); `<role>-<n>` for copies. Copy numbers are never reused within the instance. | `lead`, `implementer-2` |
| Start | A number per member, from 1, never reused (below) | 3 |
| Member address | `[instance/]member`. The instance may be left out inside a member, in a folder holding exactly one deployment, or when one running instance has that member. A role name works when the role has exactly one member. | `dev-team/implementer-2` |
| Role address | `[instance/]role`, for sending a message only: it goes to the role's shared queue (#24). | `dev-team/implementer` |
| Message | `[instance/]m<seq>` | `dev-team/m42` |

**Uniqueness.** Deployment names are unique among your deployments: brg registers each by name and file path. A second file with the same name in another folder is refused, with a pointer to rename one of them.

**Parents are roles.** `parent: lead` names a role, and a member's parent is the member filling that role. So a parent role must have one member: `validate` refuses a `parent:` that names a role with `max > 1`.

**Departments (#25).** A role can be a team. Its members (`ws-1`, `ws-2`) have their own sub-members (`dev/ws-2/lead`), inside the department's instance and its one message store.

## Commands: create from the definition, act on the active noun

| Command | What it does |
| --- | --- |
| `brg up [deployment]` | Creates the deployment's instance, or continues it: it starts its `start: true` roles, plus any member that still holds an open task or checkout. If the instance is already running, it starts what's missing and says so. |
| `brg up [deployment] --new` | Creates the next concurrent instance (`dev-team-2`). Each instance needs its own checkouts, so this is refused unless the deployment's `cwd` values use `{instance}`. |
| `brg up --role R [instance]` | Creates a member for role R: the next copy, or, for a `max: 1` role, its single member if it isn't running. |
| `brg down [instance]`, `brg down instance/member` | Stops an instance or one member. |
| `brg restart instance/member`, `brg restart instance` | Starts an existing member, or instance, again with a new start. A stopped copy that holds a task resumes it. |
| `attach`, `logs`, `prompt` | A member address |
| `brg ls` | Your deployments and their instances, running or stopped |

## Lifetimes

**An instance lives from its first `up` until you remove it.**
- **Restarts:** `down` then `up`, or `restart`, keep its name, messages and open tasks. Its members keep their IDs.
- **Message store:** one per instance, for its whole life, so message numbers never restart.
- **Frozen configuration:** the configuration is frozen from a full `up` to the full `down`, and replaced only once every member is positively stopped.
- **Orphaned work:** a full `up` refuses while the new configuration orphans open work (for example, a removed role that holds a task or checkout). It says what to cancel or restore.
- **Message-format change:** refused until `brg down <instance> --reset` archives the old messages. Message and start numbers then continue from the archive.
- **No age-based rollover:** `validate` refuses a message root under `/tmp` or any other path cleaned by age, and an index keeps reads cheap as the store grows.
- **The upgrade:** the switch to this layout converts each instance's current storage once, so open work survives it.

**Starts** do the jobs of today's incarnations:
- fencing a replaced process ("this start of `implementer-2` has been replaced");
- the recorded tmux pane that decides whether the start is still running;
- the task writers that must be gone before a checkout is freed;
- what the background wait has already shown.

Start numbers are never reused, even across `--reset`. They live in brg's registry, outside the archivable store. If that record is lost while other traces of the instance remain, `up` refuses rather than start counting again.

Today's *run* (a hidden storage generation) and *incarnation* disappear from code, storage and records.

## `brg status`

```
$ brg status
dev-team                            deployment ~/.config/borgchart/dev-team.yaml · team dev-team/
└─ lead              claude opus high          working
   ├─ implementer-1  codex gpt-6.1-sol high    working
   ├─ implementer-2  codex gpt-6.1-sol high    idle
   ├─ reviewer       claude opus high          not started (start: false)
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
| `brg ls` "your teams" | deployments and what runs from them | deployments and their instances |
| `[team/]member` (sessions design) | instance | `[instance/]member` |
| team name derived from the deployment's folder | instance | the deployment file's name |
| errors: "team `x` was restarted", "no team named `x`" | instance | "instance `x`" |
| `BORGCHART_DEPLOYMENT` (a storage key), `BORGCHART_TEAM` (earlier proposal) | instance | `BORGCHART_INSTANCE` |
| JSON `team` (earlier proposal) | instance | `instance`, plus `deployment` |
| `--team DIR`: a team folder used on its own | the definition, with an implicit deployment | `--team-dir DIR`, an implicit deployment named after the folder |
| `./team.yaml` on its own (no deployment file) | the same | the same implicit deployment |
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
| | — | `{instance}` in `cwd` for concurrent instances, `{n}` for copies |
| CLI | `brg up --member X` | `brg up --role X` (create); `restart`/`down` with a member address |
| | `brg bus carry --from RUN` | removed |
| Output | `status --json` `name`, `kind` | `member`, `role`, `instance`, `deployment`, `start`; `kind` stays |
| Records | `actor: {type: member, member, incarnation}`, `writer_incarnation`, the internal `run` field | `actor: {type: member, member, start}`, `writer_start`, no `run`: one message-format change, with the single store |
| On disk | `<bus.root>/<name>-<hash>/current`, `r1/…`, `runtime/incarnations/…` | `<bus.root>/<instance>/…`, `members/<member>/starts/<n>.json` |
| | brg state `…/borgchart/<name>-<hash>/` | `…/borgchart/<instance>/`, plus the registry |
| Docs | "member" for both the part and the agent, "run", "incarnation", "team" for the running thing | "role", "member", "start", "instance". Covers `format.md` (and its older-shape steps), `teams.md`, `sessions.md`, `bus.md`, `cli.md`, the `--help` text, and the `dev-team/` role files. |

**Upgrading running instances.** The new names change the storage folder, the state folder and the tmux session, so stop everything before upgrading. brg refuses `up` while a tmux session under an old name exists for that deployment, rather than start a second set of members on the same checkouts.

## Environment variables

Every `BORGCHART_*` variable that a member, plugin or test can see:

| Today | Seen by | Proposed | Meaning |
| --- | --- | --- | --- |
| `BORGCHART_DEPLOYMENT` | members, plugins, alert actions | `BORGCHART_INSTANCE` | the instance (`dev-team-2`). Today's value is a storage key. |
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
| `BORGCHART_GATE_WAIT_MS` | the caller, tests | unchanged; a tuning knob | how long `up` waits for a `locked` member's gate |
| `BORGCHART_WAIT_MIN_MS`, `BORGCHART_WAIT_TIMEOUT_MS` | tests (test builds only) | unchanged | shorten the background wait's minimum and timeout |
| `BORGCHART_QUALIFY`, `…_VENDORS`, `…_SUBMIT` | the manual vendor qualification test | unchanged | turn that test on and pick its vendors |

## Effects on #24 and #25

Their acceptance text says `brg up --member implementer` starts the next copy; that becomes `brg up --role implementer`. #25's departments are members whose role is a team, inside one instance. Update both issues once the names are agreed.
