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
- **Storage:** brg rolls its storage over by itself, and that stays inside the instance too.
- **Several instances of one team:** today one deployment seats one instance. Departments (#25), and a deployment file that declares several teams (#34), seat several instances of a team.

**A member keeps its ID across restarts.** `implementer-2` stopped and started again is still `implementer-2`, with its inbox and its task.

**Parents are roles.** In `team.yaml`, `parent: lead` names a role. At run time a member's parent is the member filling that role.

### Internal, never shown as concepts

- **Storage generation** (today a *run*, `r1`, `r2`, …): the message storage of one configuration and message format. brg starts a new one by itself and carries open work across. It's never named in commands, output or errors.
- **Member start** (today an *incarnation*): one start of a member, used to fence a stale process after a restart ("this session is over"). It appears only as an opaque value: one environment variable and a field in `--json`.

## IDs

| ID | Format | Example |
| --- | --- | --- |
| Team instance | `[a-z][a-z0-9_-]{0,31}`, unique among the team instances on one tmux server. It's `name:` in the deployment, or by default the deployment file's name without `.yaml` (or, for a file named `borgchart.yaml`, its folder's name). A duplicate is refused, with a pointer to `name:`. | `dev-team` |
| Team instance inside a department | `<department>/<member>` | `coding/ws-2` |
| Member | The role name for a role with one member. `<role>-<n>` for copies, numbered from 1 and never reused within the instance. | `lead`, `implementer-2` |
| Address | `[instance/]member`. The instance may be left out inside a member, in the deployment's folder, or when one running instance has that member. | `dev-team/implementer-2` |
| Message | `[instance/]m<seq>` | `dev-team/m42` |
| Member start (internal) | Opaque | — |

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
- **`--json` rows:** `team` (instance ID), `member`, `role`, `parent` (a member ID), the existing fields, and an opaque `start`.

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
| Environment | `BORGCHART_MEMBER` (the member's name) | `BORGCHART_MEMBER` (member ID) |
| | — | `BORGCHART_ROLE`, `BORGCHART_TEAM` (instance ID) |
| | `BORGCHART_DEPLOYMENT` (an internal storage key) | removed; `BORGCHART_TEAM` replaces it |
| | `BORGCHART_RUN`, `BORGCHART_INCARNATION` | one opaque internal variable (open, below) |
| | `BORGCHART_PARENT`, `BORGCHART_DELIVER_TO` | member IDs |
| | `BORGCHART_FILE`, `BORGCHART_BIN`, `BORGCHART_STATE_DIR` | unchanged |
| Records | `actor: {type: member, member, incarnation}` | `member` is a member ID; `incarnation` (open, below) |
| On disk | `<bus.root>/<name>-<hash>/r1/…`, tmux session `brg-<name>-<hash>` | `<bus.root>/<instance>-<hash>/…`, session `brg-<instance>`; generations stay internal |
| Docs | "member" for both the declared part and the running agent | "role" for the part, "member" for the agent |

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
6. **The internal start identity:** one opaque `BORGCHART_START`, or today's two variables kept as internal.
7. **`incarnation` in records and task facts:** kept as internal storage names, or renamed at a message-format change.
8. **Old names:** refused with a pointer, or accepted with a warning for one release.
