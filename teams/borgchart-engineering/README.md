# borgchart-engineering

This folder defines `borgchart-engineering`, the team this repository's engineering runs under, at the root of a tree of teams. Its lead, the root lead, runs the effort for the owner. Its other roles are child teams and helpers it starts as work arrives. The definitions are sibling folders under `teams/`; only the running instances and their working folders nest.

| Role | What it is | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the team | takes requests, seats and briefs the teams, watches them, relays, merges approved pull requests |
| `dev` | a copy of [`../dev-team/`](../dev-team/), up to four | one per workstream | plans, builds, reviews and hands off its pull requests |
| `design` | a copy of [`../dev-design-team/`](../dev-design-team/), up to two | per cross-team design choice | two independent proposals from one blind brief |
| `researcher` | the dev team's researcher, up to three | per question | researches the web and the code, with evidence |
| `monitor` | the dev team's monitor, up to three | per watch | observes and reports; never acts |

The dev team still runs on its own, as any team folder does: see [its README](../dev-team/README.md).

## Folders

```
<folder>/                    the team's working folder
├─ lead/                     a checkout of the repository: lead, researchers, monitors
├─ dev/<copy>/               a dev team copy
│  ├─ repo/                  its worktree: the dev lead, reviewer, researcher, monitor
│  ├─ copies/<member>/       an implementer's worktree
│  └─ design/<copy>/work/    the dev team's own design panel
└─ design/<copy>/work/       the root lead's own design panel
```

`create` of a team role (`dev`, `design`) makes a copy and its folders, stopped. Its parent adds the worktree, checks seating, then starts it. `create` of an ordinary role (`researcher`, `monitor`) starts its new member, as in any team. Work folders never overlap, so brg's checkout check holds at every level.

## Run it

These are the cutover steps. They work once nesting (D1) and delivery between a parent team and its child teams (D2) are in, and the live smoke check has passed (#80, "Cutover"). Until then the root lead can't brief a copy's lead, or hear back from it.

1. **Use a stable brg and a pinned copy of the complete sibling catalog,** installed as `brg-<sha>/teams/` outside any checkout the teams change (see [installation](../README.md)). Every member runs the brg that started it.
2. **Make the quota reader available:** the bundled [seating plugin](../shared/plugins/seat/README.md) uses `usage-route` on `PATH`, or the executable named by its one local configuration setting. The reader remains external; moving it into this repository is a later step. Missing or unknown usage means wait.
3. **Check the team:** `brg validate borgchart-engineering`. The repository's checks already use `tools/cargo-slot`; no wrapper installation or `PATH` entry is needed. During supervisor cutover, drain existing builds and move all callers to the same new lock domain before starting more builds.
4. **Make the lead's checkout:** `git clone <repository> <folder>/lead`, or add a worktree there from a checkout you already have. Set its hooks path as the repository's README says, and run `agent-gh --setup` there before the first commit.
5. **Create the team:** `brg create borgchart-engineering <folder>`. It prints the instance ID and its tree: the lead started, every other role not yet created.
6. **Answer the folder-trust screens,** once per folder. When `brg status` shows a member `blocked`, run `brg attach <member address>`, answer, and detach.
7. **Tell the lead what to work on.** Attach to it and say so, or send a message to its inbox (`brg send --external owner <instance>/<lead-member> 'Read <brief path>'`). It reaches you on GitHub: review requests from the dev leads, and comments when it needs you.
8. **Stop it:** `brg stop <instance>` stops every copy, deepest first, then the lead. `brg start <instance>` brings back what was running.

## Working rules

- **The owner reviews and approves every pull request.** Each dev lead hands off its own pull requests. Only the root lead may merge one, and only after the owner's approving review on its current head, with CI green, by a merge commit.
- **Seating is checked before every launch,** through the `seat` plugin's `route` action. `wait` or an unknown answer means wait, never another vendor or model.
- **Git and GitHub practice** lives in [`../shared/includes/git.md`](../shared/includes/git.md), shared by the root lead, child-team lead and implementer, with writes limited by role. The approved public account is `mrshll-agent`. Vendor instructions still apply.
- **Machine-specific configuration stays local:** quota-reader paths and login/cache access do not belong in this folder. There is no private policy overlay.

Before cutover, smoke-test seating, the build wrapper and `agent-gh` from the actual member environment: a GitHub read and an authorized test push whose pre-push checks use the bundled throttle automatically. Normal tool approvals still apply; team instructions do not override vendor approval settings. Arrange how GitHub reviews and comments reach the lead: this definition waits on the brg inbox and does not install a GitHub event watcher.

The root and dev leads are `interactive: true`: sends store messages without typing, even with `--force`. Helpers use guarded typing by default. Messages between instances and nested workstream operation remain gated on D2 and the cutover smoke check above. Do not bypass this boundary with member identity overrides.
