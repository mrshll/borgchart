# A development team

This folder defines a brg team for developing a software project: a lead and four helper roles it starts on demand. `teams/dev-design-team/` is its design panel, the nested team of its `design` role, which the lead creates before costly-to-reverse design choices. The team runs on its own, as below, or as the `dev` role of the department that holds it (`borgchart-engineering/`), where its lead's parent is the department lead. Both folders share the department's approved GitHub account and practices; everything about one machine (the checkout, brg's configuration, your vendor logins) stays on it, and what one piece of work needs (the tracking issue, a handover note) goes to the lead in a message.

| Member | Default vendor and model | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the team | plans, assigns, verifies, pushes, opens and hands off pull requests |
| `implementer` | Codex gpt-6.1-sol high | per task, up to two copies | one task per copy, writes only its own worktree; commits locally |
| `reviewer` | Claude opus high | per review | one review pass, then stopped |
| `researcher` | Codex gpt-6.1-terra medium | per question | researches the web and the code, with evidence |
| `monitor` | Claude sonnet low | per watch | observes and reports; never acts |
| `designer-claude`, `designer-codex` (design panel) | Claude opus high, Codex gpt-6-astra medium | per design | questions, then one independent proposal each from a blind brief |

Each member file names both vendors' models; `vendor:` in `team.yaml` picks one. The default lead runs as Claude and wakes itself with a Bash background wait. Codex runs the same inbox wait in the foreground with `--timeout 600`.

## Setup

brg runs the team by its folder's path. Keep a **pinned copy** of the repository, taken at the commit of the brg you run, not the checkout the team works in, and run brg from its root. A running instance's files are frozen: if the team's own work changed them under it, the instance would refuse to create or start members until a full stop.

The lead checks seating with the `seat` plugin, `shared:seat`, so brg's configuration, `~/.config/borgchart/config.yaml`, names the folder that holds it:

```yaml
schema: 1
auth: login
plugins_dir: /path/to/plugins   # holds seat/: a copy of examples/seat/ with this machine's usage source
```

[`examples/seat/`](../../../examples/seat/) is the plugin, with a README on installing a real usage source. Without one, every seating check answers `wait`.

Install `cargo-slot` on every member's `PATH` before starting; see the [department setup](../../README.md#run-it) for the cap and shared lock requirements. All concurrent teams must use the same two slots, including during a supervisor cutover.

## Run it

1. **Use a stable brg,** installed outside any checkout the team changes: every member runs the brg that started it (`$BORGCHART_BIN`).
2. **Check the teams:** `brg validate ./borgchart-engineering/teams/dev-team` and `brg validate ./borgchart-engineering/teams/dev-team/teams/dev-design-team`.
3. **Prepare the instance's folder and create the instance.** The folder (`<folder>`) holds the shared checkout at `<folder>/repo`, a clone or a git worktree, where the lead, reviewer, researcher and monitor start. Set the repository hooks path and run `agent-gh --setup` in that checkout before its first commit. Then run `brg create ./borgchart-engineering/teams/dev-team <folder>`. It prints the instance's ID (`dev-team-k3f9`) and its tree: the lead `started`, the implementer `on demand (none created)`, other helpers `not created (start: false)`. Implementer copies work beside the checkout, in `<folder>/copies/<member ID>`: the lead makes `<folder>/copies/` once, brg creates each copy's empty folder, and the lead puts a worktree there before each brief (`git -C <folder>/repo worktree add <folder>/copies/<member ID> <branch>`). The design panel's copies go in `<folder>/design/<copy>/`. `orchestrator.md` has the lead's steps.
4. **Tell the lead what this work is:** write `brief.json` with `{"to": "<lead member>", "body": "Tracking issue: <issue>; handover: <note path>."}`, then `brg msg publish --external owner --instance dev-team-k3f9 --record brief.json`. `brg status dev-team-k3f9` gives the full member ID. Messages are durable; if the lead misses one, nudge it with `brg send dev-team-k3f9/<lead member> 'check your inbox'`.
5. **Answer the folder-trust screens,** once per folder: when `brg status dev-team-k3f9` shows a member `blocked`, run `brg attach dev-team-k3f9/<member>`, answer, and detach. brg makes the instance's messages and notes writable for every member, and adds nothing else. A copy's git writes go to the shared checkout's `.git`, so sandboxed Codex copies need approval for each commit unless the owner's Codex config allows that folder; a separate clone per copy is the alternative.
6. **Work with the lead** in its session (`brg attach dev-team-k3f9/<lead member>`). It reaches you on GitHub: review requests on the pull requests it marks ready at handoff, and comments when it needs you.
7. **Stop it:** `brg stop dev-team-k3f9`; `brg start dev-team-k3f9` brings back what was running. `brg delete dev-team-k3f9` removes its messages and notes once its messages are acknowledged (unacked messages need `--force`); the checkout is left as it is.

## Working rules

- **The owner and reviewer is @mrshll; the lead opens draft pull requests and never merges.** It requests the owner's review on each one it hands off; the department lead also has its limited approved-head merge permission when this team is nested. The shared [git rules](includes/git.md) cover identity, assignments and review requests.
- **One writer per checkout, one piece of work per copy.** Implementers work only in their own `copies/{member_id}` worktree, never the shared checkout. The lead must not change the shared checkout (edits, checkouts, resets) while a reviewer, researcher or monitor works there; do other git work in a copy's worktree between pieces, or wait. The reviewer reads the pull request's head after the push; these helpers are read-only and write reports only.
- **Parallel implementers** are approved, up to two copies, each in its own worktree. Push a copy's branch through `agent-gh` only between its pieces of work, from its worktree so pre-push checks run there; check `git -C <copy> config core.hooksPath` and correct missing hooks first. A kept copy may retain its worktree across briefs; remove it with `git worktree remove` before deleting the copy. If removal refuses uncommitted or untracked work, commit or report it first; never use `--force`.
- **Helpers get a brief message before starting work.** They read `inbox --full`, ack once they take it on, write their report atomically, then reply naming the report and its `action-needed:` line. The lead verifies, stops them and checks `status` shows them stopped before reusing their checkout. It follows up by each brief's deadline with `status`, a `send` nudge, or `start` for a stopped helper. Progress and delegation live in the pull request description and comments.
- **Heavy builds are memory-capped,** at most two builds across all teams on the machine at a time, through `cargo-slot`; lead verification and pre-push checks use the same slots. Each copy's brief grants full checks now or asks it to report `ready for checks` and wait. Run `.githooks/checks` and pushes with `CHECKS_WRAP=cargo-slot`; never also wrap the checks script itself. Never use `--no-verify`.
- **Waiting:** ack everything you have taken on, then run `inbox wait`: Claude with its Bash tool in background mode, Codex in the foreground with `--timeout 600`. Exit 9 means wait again; exit 8 (`fenced`) means stop waiting and tell the owner. An unacked message wakes you about once a minute. The monitor's watch remains background for Claude, foreground for Codex in commands of at most 10 minutes.
- **Change the team's files only when no work is open.** A changed file takes effect at the next full `brg stop` and `brg start`.
- **One fresh review per pull request,** followed by one fix round.
