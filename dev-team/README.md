# A development team

This folder defines a brg team for developing a software project: a lead and four helper roles it starts on demand. `../dev-design-team/` is its design panel, a separate team the lead seats before costly-to-reverse design choices. Both folders are generic: everything about one machine (the checkout, brg's configuration, your vendor logins) stays on it, and what one piece of work needs (the tracking issue, a handover note) goes to the lead in a message.

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

brg finds teams by name in the search paths of its configuration, `~/.config/borgchart/config.yaml`:

```yaml
schema: 1
auth: login
teams:
  - /path/to/teams   # holds a pinned copy of dev-team/ and dev-design-team/
```

Keep the teams in the search path as a **pinned copy** of these folders, taken at the commit of the brg you run, not the checkout the team works in. A running instance's files are frozen: if the implementer switched branches under them, the instance would refuse to create or start members until a full stop.

## Run it

1. **Use a stable brg,** installed outside any checkout the team changes: every member runs the brg that started it (`$BORGCHART_BIN`).
2. **Check the teams:** `brg validate dev-team` and `brg validate dev-design-team`.
3. **Create the instance** in the shared checkout: `brg create dev-team <checkout>`. It prints the instance's ID (`dev-team-k3f9`) and its tree: the lead `started`, the implementer `on demand (none created)`, other helpers `not created (start: false)`. The lead makes `<checkout>/copies/` once before creating an implementer copy; brg creates each copy's empty `copies/{member_id}` folder. Before each brief, the lead prepares its worktree (`git worktree add <checkout>/copies/<member ID> <branch>` when empty, or `--detach <base>`) and names it in the brief.
4. **Tell the lead what this work is:** write `brief.json` with `{"to": "<lead member>", "body": "Tracking issue: <issue>; handover: <note path>."}`, then `brg msg publish --external owner --instance dev-team-k3f9 --record brief.json`. `brg status dev-team-k3f9` gives the full member ID. Messages are durable; if the lead misses one, nudge it with `brg send dev-team-k3f9/<lead member> 'check your inbox'`.
5. **Answer the folder-trust screens,** once per folder: when `brg status dev-team-k3f9` shows a member `blocked`, run `brg attach dev-team-k3f9/<member>`, answer, and detach. brg makes the instance's messages and notes writable for every member, and adds nothing else.
6. **Work with the lead** in its session (`brg attach dev-team-k3f9/<lead member>`). It reaches you on GitHub: review requests on the draft pull requests it hands off, and comments when it needs you.
7. **Stop it:** `brg stop dev-team-k3f9`; `brg start dev-team-k3f9` brings back what was running. `brg delete dev-team-k3f9` removes its messages and notes once its messages are acknowledged (unacked messages need `--force`); the checkout is left as it is.

## Working rules

- **The owner and reviewer is @mrshll; the lead opens draft pull requests and never merges.** It requests the owner's review on each one it hands off; the owner merges every pull request, unless he says otherwise for a specific one.
- **One writer per checkout, one piece of work per copy.** Implementers work only in their own `copies/{member_id}` worktree, never the shared checkout. The lead may do git work and push from the shared checkout while copies work. The reviewer reads the shared checkout at the pull request's head after the push; the reviewer, researcher and monitor are read-only and write reports only.
- **Parallel implementers** are approved, up to two copies, each in its own worktree. Push a copy's branch only while it is between pieces of work, from its worktree so pre-push checks run there. A kept copy may retain its worktree across briefs; remove it with `git worktree remove` before deleting the copy.
- **Helpers get a brief message before starting work.** They read `inbox --full`, ack once they take it on, write their report atomically, then reply naming the report and its `action-needed:` line. The lead verifies, stops them and checks `status` shows them stopped before reusing their checkout. It follows up by each brief's deadline with `status`, a `send` nudge, or `start` for a stopped helper. Progress and delegation live in the pull request description and comments.
- **Heavy builds are memory-capped,** at most two cargo builds across the team at a time; checks run through `.githooks/checks`, never with `--no-verify`.
- **Waiting:** ack everything you have taken on, then run `inbox wait`: Claude with its Bash tool in background mode, Codex in the foreground with `--timeout 600`. Exit 9 means wait again; exit 8 (`fenced`) means stop waiting and tell the owner. An unacked message wakes you about once a minute. The monitor's watch remains background for Claude, foreground for Codex in commands of at most 10 minutes.
- **Change the team's files only when no work is open.** A changed file takes effect at the next full `brg stop` and `brg start`.
- **One fresh review per pull request,** followed by one fix round.
