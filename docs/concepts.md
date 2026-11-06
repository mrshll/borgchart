# Concepts

**Status: proposal.** Nothing here is built yet. Once the names are agreed they drive the renames (#19), roles with copies (#24) and teams as members (#25). The open naming questions and their alternatives are in PR #50.

## The nouns

| Noun | What it is | Example |
| --- | --- | --- |
| **Team** | A definition: a team folder with `team.yaml` and its role files. Reusable and shareable; it says nothing about one machine. | `dev-team/` |
| **Deployment** | How and where a team runs: a `borgchart.yaml` file (folders, logins, values, message storage). Kept with the user, not with the team. | `~/.config/borgchart/borgchart-dev.yaml` |
| **Team instance** | A team seated from a deployment: its members, messages and tasks. It has an ID. In commands and output it's simply "the team" (`brg up borgchart-dev`). | `borgchart-dev` |
| **Role** | A named part in a team: a role file, a vendor, a parent, whether it starts with the team, and how many copies may run. | `lead`, `implementer` |
| **Member** | One running agent filling a role in a team instance. Copies of a role are separate members. | `lead`, `implementer-2` |
| **Start** | One start of a member: `brg up` starts it, and only its latest start may act as the member. | `implementer-2` start 3 |

Unchanged:
- **Plugins:** a *plugin* is a program the team declares, and a *plugin process* is one that runs alongside the members.
- **Outside callers:** anyone outside the team, written `external:NAME`.
- **Sessions:** a *session* (`--session`, planned) is a separate tmux server for a group of teams. That's not the tmux session brg makes for each team, which stays an implementation detail.

**A team instance lives from its first `brg up` until you remove it.**
- **Restarts:** `brg down` then `brg up` restarts the same instance, with the same ID, messages and open tasks.
- **What comes back:** a full `up` starts the `start: true` roles plus any member that still holds an open task or checkout. The rest are listed as stopped.

**Members:**
- **A member keeps its ID across restarts.** `implementer-2` stopped and started again is still `implementer-2`, with its inbox and its task.
- **A role with one member (`max: 1`, the default) always has exactly one member, named after the role**, running or not. Copies are numbered `implementer-1`, `implementer-2`, and the numbers are never reused within the instance.

**Parents are roles.** `parent: lead` names a role, and a member's parent is the member filling that role. So a parent role must have one member: `validate` refuses a `parent:` that names a role with `max > 1`.

**A department (#25) is one team instance.** A role can be a team. Its members (`ws-1`, `ws-2`) are members of the department that have their own sub-members (`coding/ws-2/lead`), in the department's one message store. They aren't separate team instances.

## No run, no incarnation

Today's code has two more nouns: a *run* (a hidden storage generation, rolled over when files, the message format or the storage's age change, carrying open work across) and an *incarnation* (one start of a member, used for fencing). The proposal removes both from code, storage and records, not just from output:

- **The team instance is the unit of storage.** It keeps one message store for its whole life, so message numbers never restart.
  - **Configuration** is frozen from a full `up` to the full `down`. It's replaced only when every member is positively stopped.
  - **Orphaned work:** a full `up` refuses while the new configuration orphans open work (a removed role holding a task or checkout, say) and says what to cancel or restore.
  - **A message-format change** is refused until `brg down <team> --reset` archives the old messages. Losing open work is then a decision, and message and start numbers continue from the archive.
  - **There's no age-based rollover.**
    - `validate` refuses a message root under `/tmp` or any other path cleaned by age.
    - An index keeps reads cheap as the store grows.
  - **The upgrade:** the switch to this layout converts a team's current storage once, so open work survives it.
- **Starts replace incarnations.** Start numbers are per member and never reused, even across `--reset`. If the record of them is lost while other traces of the team remain, `up` refuses rather than start counting again. A start keeps every job an incarnation does:
  - fencing a replaced process ("this start of `implementer-2` has been replaced");
  - the recorded tmux pane that decides whether the start is still running;
  - the task writers that must be gone before a checkout is freed;
  - what the background wait has already shown.

## IDs and addresses

| What | Format | Example |
| --- | --- | --- |
| Team instance | `[a-z][a-z0-9_-]{0,31}`: `name:` in the deployment, or by default its file name without `.yaml` (the folder's name for `borgchart.yaml`). Unique among your teams in a session, running or stopped; a duplicate is refused, with a pointer to `name:`. | `borgchart-dev` |
| Member | The role name for a `max: 1` role; `<role>-<n>` for copies. `validate` refuses a role name that could be another role's copy ID. | `lead`, `implementer-2` |
| Start | A number per member, from 1 | 3 |
| Member address | `[team/]member`, for `attach`, `logs`, `prompt` and `down`. The team may be left out inside a member, in a folder holding `borgchart.yaml`, or when exactly one running team has that member. A role name works when it has exactly one member. | `borgchart-dev/implementer-2` |
| Role address | `[team/]role`, for sending a message only: it goes to the role's shared queue (#24) | `borgchart-dev/implementer` |
| Nested member | `<team>/<member>/<sub-member>` | `coding/ws-2/lead` |
| Message | `[team/]m<seq>` | `borgchart-dev/m42` |

## `brg status`

```
$ brg status
borgchart-dev                       team dev-team/ · ~/.config/borgchart/borgchart-dev.yaml
└─ lead              claude opus high          working
   ├─ implementer-1  codex gpt-6.1-sol high    working
   ├─ implementer-2  codex gpt-6.1-sol high    idle
   ├─ reviewer       claude opus high          not started (start: false)
   └─ researcher     on demand (none running)
```

- **Header line:** the team instance, then its team folder and deployment file.
- **Tree lines:** one line per member. A role with copies and none running (here `researcher`, `max: 2`) gets one dim line. Department members nest.
- **`--json`:** each member row has `team`, `member`, `role`, `parent` (a member ID), `start`, and the existing fields. Plugin-process rows keep `kind: plugin` and their name, with no member or role.

## From today's names

| Where | Today | Proposed |
| --- | --- | --- |
| `team.yaml` | `members:` | `roles:` |
| | member file (`prompt: implementer.md`) | role file (same key) |
| | `parent:`, `delivers_to:` name a member | name a role |
| | frontmatter `role:` (a required, unused label) | dropped |
| `borgchart.yaml` | `members:` (per-member values) | `roles:` |
| | `team: .` (the team folder) | unchanged |
| | team name from the deployment's folder | `name:`, default from the file name |
| CLI | `--team DIR` (a team folder alone) | `--team-dir DIR` |
| | `--member ID` | a member ID, or a role name with exactly one member |
| | `brg up --member reviewer` starts that member | unchanged. `--role implementer` starts the next copy of a `max > 1` role, and `--member implementer` is refused there, listing the copies. |
| | `attach`, `logs`, `send <member>`, `--from MEMBER` | member addresses (above) |
| | `brg bus carry --from RUN` | removed |
| Output | the team name in trees and errors | the team instance (`borgchart-dev/implementer-2`) |
| | `status --json` `name`, `kind` | `member`, `role`, `team`, `start`; `kind` stays |
| Records | `actor: {type: member, member, incarnation}`, task fact `writer_incarnation`, the internal `run` field | `actor: {type: member, member, start}`, `writer_start`, no `run`: one message-format change, together with the single store |
| On disk | `<bus.root>/<name>-<hash>/current`, `r1/…`, `runtime/incarnations/<member>.json` | `<bus.root>/<team>-<hash>/…`, `members/<member>/starts/<n>.json` |
| | brg state `$XDG_STATE_HOME/borgchart/<name>-<hash>/` (also inside `BORGCHART_STATE_DIR`) | `…/borgchart/<team>-<hash>/` |
| | tmux session `brg-<name>-<hash>` | `brg-<team>` |
| Docs | "member" for both the declared part and the running agent, "run", "incarnation" | "role" and "member", "start". Covers `format.md` (and its older-shape steps), `teams.md`, `sessions.md`, `bus.md`, `cli.md`, the `brg … --help` text, and the `dev-team/` role files. |

**Upgrading running teams.** New default names change the storage folder, the state folder and the tmux session, so stop every team before upgrading. brg refuses `up` while a tmux session under the old name exists for that deployment file, rather than start a second set of members on the same checkouts.

## Environment variables

Every `BORGCHART_*` variable that a member, plugin or test can see:

| Today | Seen by | Proposed | Meaning |
| --- | --- | --- | --- |
| `BORGCHART_MEMBER` | members, plugin actions | unchanged | the member ID (`implementer-2`) |
| — | members, plugin actions | `BORGCHART_ROLE` | the role it fills (`implementer`) |
| `BORGCHART_DEPLOYMENT` | members, plugins, alert actions | `BORGCHART_TEAM` | the team instance. Today's value is a storage key. |
| `BORGCHART_RUN`, `BORGCHART_INCARNATION` | members | `BORGCHART_START` | the member's start number; brg refuses an earlier start |
| `BORGCHART_FILE` | members, plugins, the caller | unchanged | the deployment file |
| `BORGCHART_BIN` | members, plugins | unchanged | the brg that started them |
| `BORGCHART_PARENT` | plugin actions | unchanged | the calling member's parent (a member ID) |
| `BORGCHART_DELIVER_TO` | plugin processes | unchanged | the role address a process hands work to (its queue, for a role with copies) |
| `BORGCHART_STATE_DIR` | plugins | unchanged | the plugin's private folder |
| `BORGCHART_ACTOR` | alert actions | unchanged | `bus`: an alert action can't act as a member |
| `BORGCHART_TMUX_SOCKET` | members, plugins, tests | unchanged for now; `BORGCHART_SESSION` if sessions land | which tmux server the team runs on |
| `BORGCHART_TMUX_TMPDIR` (#47) | members, plugins, tests | unchanged for now; `BORGCHART_SESSION_DIR` if sessions land | where that server keeps its socket; keeps test servers private |
| `BORGCHART_STATE_HOME` | the caller, tests | unchanged | brg's state folder |
| `BORGCHART_GATE_WAIT_MS` | the caller, tests | unchanged; a tuning knob | how long `up` waits for a `locked` member's gate |
| `BORGCHART_WAIT_MIN_MS`, `BORGCHART_WAIT_TIMEOUT_MS` | tests (test builds only) | unchanged | shorten the background wait's minimum and timeout |
| `BORGCHART_QUALIFY`, `…_VENDORS`, `…_SUBMIT` | the manual vendor qualification test | unchanged | turn that test on and pick its vendors |

## Effects on #24 and #25

Their acceptance text says `brg up --member implementer` starts the next copy. With this proposal, `brg up --role implementer` does that. #25's departments become members whose role is a team, not separate team instances. Update both issues once the names are agreed.
