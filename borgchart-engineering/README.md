# The engineering department

This folder defines `borgchart-engineering`, the team this repository's engineering runs under. A lead runs the effort for the owner. Its other roles are nested teams and helpers it starts as work arrives.

| Role | What it is | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the department | takes requests, seats and briefs the teams, watches them, relays, merges approved pull requests |
| `dev` | a copy of [`teams/dev-team/`](teams/dev-team/), up to four | one per workstream | plans, builds, reviews and hands off its pull requests |
| `design` | a copy of [`teams/dev-team/teams/dev-design-team/`](teams/dev-team/teams/dev-design-team/), up to two | per cross-team design choice | two independent proposals from one blind brief |
| `researcher` | the dev team's researcher, up to three | per question | researches the web and the code, with evidence |
| `monitor` | the dev team's monitor, up to three | per watch | observes and reports; never acts |

The dev team still runs on its own, as any team folder does: see [its README](teams/dev-team/README.md).

## Folders

```
<folder>/                    the department's working folder
├─ lead/                     a checkout of the repository: lead, researchers, monitors
├─ dev/<copy>/               a dev team copy
│  ├─ repo/                  its worktree: the dev lead, reviewer, researcher, monitor
│  ├─ copies/<member>/       an implementer's worktree
│  └─ design/<copy>/work/    the dev team's own design panel
└─ design/<copy>/work/       a department-level design panel
```

`create` of a team role (`dev`, `design`) makes a copy and its folders, stopped. Its parent adds the worktree, checks seating, then starts it. `create` of an ordinary role (`researcher`, `monitor`) starts its new member, as in any team. Work folders never overlap, so brg's checkout check holds at every level.

## Run it

1. **Use a stable brg and a pinned copy of this folder,** installed outside any checkout the teams change. Every member runs the brg that started it.
2. **Install the seating plugin** as `<plugins_dir>/seat`, with a real usage source for this machine: see [`examples/seat/`](../examples/seat/). Set `plugins_dir` in brg's configuration.
3. **Check it:** `brg validate <pinned copy>`.
4. **Make the lead's checkout:** `git clone <repository> <folder>/lead`, or add a worktree there from a checkout you already have. Set its hooks path as the repository's README says.
5. **Create the department:** `brg create <pinned copy> <folder>`. It prints the instance ID and its tree: the lead started, every other role not yet created.
6. **Answer the folder-trust screens,** once per folder. When `brg status` shows a member `blocked`, run `brg attach <member address>`, answer, and detach.
7. **Tell the lead what to work on.** Attach to it and say so, or publish a message to it as the dev team's README shows (`brg msg publish --external owner --instance <instance> --record brief.json`). It reaches you on GitHub: review requests from the dev leads, and comments when it needs you.
8. **Stop it:** `brg stop <instance>` stops every copy, deepest first, then the lead. `brg start <instance>` brings back what was running.

## Working rules

- **The owner reviews and approves every pull request.** Each dev lead hands off its own pull requests. The department lead may merge one only after the owner's approving review on its current head, with CI green, by a merge commit.
- **Seating is checked before every launch,** through the `seat` plugin's `route` action. `wait` or an unknown answer means wait, never another vendor or model.
- **Machine-specific things stay off this folder:** the usage source goes in the plugins folder, and the GitHub identity in the owner's own vendor instructions.
