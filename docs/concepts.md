# Concepts

**Status: proposal.** Nothing here is built yet. Once the names are agreed they drive the renames (#19), roles with copies (#24) and teams as members (#25). Today's names are in the mapping table below.

## The nouns

| Noun | What it is | Example |
| --- | --- | --- |
| **Team** | A definition: `team.yaml` plus its role files. Reusable and shareable; it says nothing about one machine. | `dev-team/` |
| **Deployment** | How and where a team runs: `borgchart.yaml` (folders, logins, values, message storage). Kept with the user, not with the team. | `~/.config/borgchart/dev-team.yaml` |
| **Team instance** | A team seated from a deployment: its members, messages and tasks. It has an ID. | `dev-team` |
| **Role** | A named part in a team: a role file, a vendor, a parent, whether it starts with the team, and how many copies may run. | `lead`, `implementer` |
| **Member** | One running agent filling a role in a team instance. Copies of a role are separate members. | `lead`, `implementer-2` |

**What else stays as it is:** a *plugin* is a program the team declares, and a *plugin process* is one that runs alongside the members. An *outside caller* is anyone outside the team, written `external:NAME`.

**A team instance lives from its first `brg up` until it's removed.**
- **Restarts:** `brg down` then `brg up` restarts the same instance, with the same ID, messages and open tasks.
- **Storage:** one message store for the instance's whole life (see "No hidden storage generations" below).
- **Several instances of one team:** today one deployment seats one instance. Departments (#25), and a deployment file that declares several teams (#34), seat several instances of a team.

**A member keeps its ID across restarts.** `implementer-2` stopped and started again is still `implementer-2`, with its inbox and its task.

**Parents are roles.** In `team.yaml`, `parent: lead` names a role. At run time a member's parent is the member filling that role.

### Starts

**A member's *start* is one `brg up` of that member,** numbered 1, 2, 3, … within the team instance. Only a member's latest start may act as it. A process left over from an earlier start (one that ignored its hang-up, say) is refused with "this start of `implementer-2` has been replaced". brg also records each start's terminal pane, which decides whether that start is still running before brg frees a checkout it wrote to.

### No hidden storage generations

**The team instance is also the unit of storage:** one message store for its whole life, so message numbers never restart. Today brg rolls the storage over internally (a *run*) and carries open work across. This proposal drops that:
- **Configuration:** recorded at a full `brg up` and frozen until the full `brg down`.
- **New message format:** brg refuses until `brg reset <instance>` archives the old messages.
- **Age:** there's no age-based rollover.

Today's *run* and *incarnation* disappear from code, storage and records as well as from output.

## IDs

| ID | Format | Example |
| --- | --- | --- |
| Team instance | `[a-z][a-z0-9_-]{0,31}`, unique among the team instances on one tmux server. It's `name:` in the deployment, or by default the deployment file's name without `.yaml` (or, for a file named `borgchart.yaml`, its folder's name). A duplicate is refused, with a pointer to `name:`. | `dev-team` |
| Team instance inside a department | `<department>/<member>` | `coding/ws-2` |
| Member | The role name for a role with one member. `<role>-<n>` for copies, numbered from 1 and never reused within the instance. | `lead`, `implementer-2` |
| Address | `[instance/]member`. The instance may be left out inside a member, in the deployment's folder, or when one running instance has that member. | `dev-team/implementer-2` |
| Message | `[instance/]m<seq>` | `dev-team/m42` |
| Member start | A number per member, from 1 | `implementer-2` start 3 |

## `brg status`

```
$ brg status
dev-team                            team dev-team · ~/.config/borgchart/dev-team.yaml
└─ lead              claude opus high          working
   ├─ implementer-1  codex gpt-6.1-sol high    working
   ├─ implementer-2  codex gpt-6.1-sol high    idle
   ├─ reviewer       claude opus high          not started (start: false)
   └─ researcher     on demand (none running)
```

- **Header line:** the instance ID, then the team and the deployment file.
- **Tree lines:**
  - one line per member;
  - a role with copies and none running gets one dim line;
  - a team instance inside a department nests under its member.
- **`--json` rows:** `team` (instance ID), `member`, `role`, `parent` (a member ID), the existing fields, and `start` (the start number).

## From today's names

| Where | Today | Proposed |
| --- | --- | --- |
| `team.yaml` | `members:` | `roles:` |
| | member file (`prompt: implementer.md`) | role file (same key) |
| | `parent:`, `delivers_to:` name a member | name a role |
| | frontmatter `role:` (a free-form label) | `description:` (open, below) |
| `borgchart.yaml` | `members:` (per-member values) | `roles:` |
| | team name from the deployment's folder | `name:`, default from the file (above) |
| CLI | `--member ID` | a member ID. A role name works when exactly one member fills it. |
| | `brg up --member implementer` starts a copy | `brg up --role implementer` starts a new member; `--member implementer-2` restarts that one |
| | `attach`, `logs`, `send <member>`, `--from MEMBER` | member IDs, `[instance/]member` |
| Output | the team name in trees and errors | the instance ID (`dev-team/implementer-2`) |
| | `status --json` `name` | `member`, plus `role` and `team` |
| Environment | every variable | see the next table |
| Records | `actor: {type: member, member, incarnation}`, task fact `writer_incarnation`, the internal `run` field | `actor: {type: member, member, start}`, `writer_start`; no `run` (one change, with the new storage) |
| On disk | `<bus.root>/<name>-<hash>/current`, `r1/…`, `runtime/incarnations/<member>.json`; tmux session `brg-<name>-<hash>` | `<root>/<instance>-<hash>/…`, `members/<member>/starts/<n>.json`; session `brg-<instance>` |
| CLI | `brg bus carry --from RUN` | removed (no rollover) |
| Docs | "member" for both the declared part and the running agent | "role" for the part, "member" for the agent |

## Environment variables

Every `BORGCHART_*` variable a member, plugin or test can see:

| Today | Seen by | Proposed | Meaning |
| --- | --- | --- | --- |
| `BORGCHART_MEMBER` | members, plugin actions | `BORGCHART_MEMBER` | the member ID (`implementer-2`) |
| — | members, plugin actions | `BORGCHART_ROLE` | the role it fills (`implementer`) |
| `BORGCHART_DEPLOYMENT` | members, plugins | `BORGCHART_TEAM` | the team instance ID. Today's value is a storage key (`<name>-<hash>`). |
| `BORGCHART_RUN`, `BORGCHART_INCARNATION` | members | `BORGCHART_START` | the member's start number; brg refuses an earlier start |
| `BORGCHART_FILE` | members, plugins, the caller | unchanged | the deployment file |
| `BORGCHART_BIN` | members, plugins | unchanged | the brg that started them |
| `BORGCHART_PARENT` | plugin actions | unchanged | the member ID of the calling member's parent |
| `BORGCHART_DELIVER_TO` | plugin processes | unchanged | the member ID a process hands work to (resolved from its role) |
| `BORGCHART_STATE_DIR` | plugins | unchanged | the plugin's own private folder |
| `BORGCHART_ACTOR` | bus alert actions | internal | set to `bus` so an alert action can't act as a member |
| `BORGCHART_TMUX_SOCKET` | members, plugins, tests | unchanged for now; `BORGCHART_SESSION` if sessions land | which tmux server the team runs on |
| `BORGCHART_TMUX_TMPDIR` (#47) | members, plugins, tests | unchanged for now; `BORGCHART_SESSION_DIR` if sessions land | where that tmux server keeps its socket; used to keep test servers private |
| `BORGCHART_STATE_HOME` | the caller, tests | unchanged | brg's state folder |
| `BORGCHART_GATE_WAIT_MS` | the caller, tests | unchanged, marked as a tuning knob | how long `up` waits for a `locked` member's gate |
| `BORGCHART_WAIT_MIN_MS`, `BORGCHART_WAIT_TIMEOUT_MS` | tests only (test builds) | unchanged | shorten the background wait's minimum and timeout |
| `BORGCHART_QUALIFY`, `…_VENDORS`, `…_SUBMIT` | the manual vendor qualification test | unchanged | turn that test on and pick its vendors |

## Effects on the planned work

- **#19 (renames)** applies the table: `roles:`, `--role`, the environment variables and the output names, alongside the command renames.
- **#24 (copies)** is the role/member split: `max: N` on a role, members `implementer-1`, `implementer-2`.
- **#25 (departments)** is a role whose members are team instances: `ws` in `coding` runs members `ws-1` and `ws-2`, each a team instance `coding/ws-1`.

## Open

The proposal above takes the recommended answer to each question; the alternatives are in the pull request.

1. **Instance IDs:** a plain name, or always numbered (`dev-team-1`).
2. **New instances:** only a new deployment or department copy makes one, or every `up` after a full `down` does.
3. **Default name:** the deployment file's name, the folder's name, or a required `name:`.
4. **The role-file label `role:`:** renamed `description:`, or dropped.
5. **Flags:** `--role` to start a new member and `--member` to name one, or a single `--member` for both.
6. **Runs:** no hidden storage generations (above), a new team instance per rollover, or runs kept internally under another name.
7. **Incarnations:** numbered member starts (above), fencing by the caller's tmux pane alone, or incarnation IDs kept internally.
8. **Old names:** refused with a pointer, or accepted with a warning for one release.
9. **tmux variables:** keep `BORGCHART_TMUX_SOCKET` and `BORGCHART_TMUX_TMPDIR` until sessions are decided, or rename them now.
