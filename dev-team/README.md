# A development team

This folder defines a brg team for developing a software project: a lead and four helpers it starts on demand. `../dev-design-team/` is its design panel, a separate team the lead seats before costly-to-reverse design choices. Both folders are generic: everything about one machine (the checkout, brg's configuration, your vendor logins) stays on it, and what one piece of work needs (the tracking issue, a handover note) goes to the lead in a message.

| Member | Default vendor and model | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the team | plans, assigns, verifies, pushes, opens and hands off pull requests |
| `implementer` | Codex gpt-6.1-sol high | per task | one task at a time, the only writer of the shared checkout; commits locally |
| `reviewer` | Claude opus high | per review | one review pass, then stopped |
| `researcher` | Codex gpt-6.1-terra medium | per question | researches the web and the code, with evidence |
| `monitor` | Claude sonnet low | per watch | observes and reports; never acts |
| `designer-claude`, `designer-codex` (design panel) | Claude opus high, Codex gpt-6-astra medium | per design | questions, then one independent proposal each from a blind brief |

Each member file names both vendors' models; `vendor:` in `team.yaml` picks one. The lead must run as Claude until brg can wake a Codex member: it wakes itself with a background wait, which only Claude's tools support.

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
3. **Create the instance** in the shared checkout: `brg create dev-team <checkout>`. It prints the instance's ID (`dev-team-k3f9`) and its tree: the lead `started`, every other role `on demand (none created)`. The lead creates and starts helpers as their work arrives.
4. **Tell the lead what this work is:** publish a note to its member with the owner's GitHub handle, the tracking issue and any handover note (for example `brg msg publish --external owner --instance dev-team-k3f9 --key start-1` with the note on stdin).
5. **Answer the folder-trust screens,** once per folder: when `brg status dev-team-k3f9` shows a member `blocked`, run `brg attach dev-team-k3f9/<member>`, answer, and detach. brg makes the instance's messages and notes writable for every member, and adds nothing else.
6. **Work with the lead** in its session (`brg attach dev-team-k3f9/<lead member>`). It reaches you on GitHub: review requests on the draft pull requests it hands off, and comments when it needs you.
7. **Stop it:** `brg stop dev-team-k3f9`; `brg start dev-team-k3f9` brings back what was running. `brg delete dev-team-k3f9` removes its messages and notes once no work is open; the checkout is left as it is.

## Working rules

- **The lead never merges.** It opens draft pull requests and requests the owner's review; the owner merges every pull request, unless they say otherwise for a specific one.
- **One shared checkout, one writer:** the implementer, with at most one open task. Nobody edits the checkout while it holds a task; the lead does git work only between its tasks, and starts the reviewer after the push, at the pull request's head. The reviewer, researcher and monitor are read-only and write reports only.
- **One writer at a time** until one pull request has gone cleanly through the whole team. A second writer needs the owner's OK on the tracking issue, and works in a worktree next to the checkout, removed once pushed.
- **Helpers are started with their work** and stopped when done. They wait only for the answer to their own question: Codex in the foreground, Claude in the background. The monitor runs its watch the same way: Claude in the background, Codex in the foreground in commands of at most 10 minutes.
- **Heavy builds are memory-capped,** at most two cargo builds across the team at a time; checks run through `.githooks/checks`, never with `--no-verify`.
- **Change the team's files only when no work is open.** A changed file takes effect at the next full `brg stop` and `brg start`.
- **One fresh review per pull request,** followed by one fix round.
