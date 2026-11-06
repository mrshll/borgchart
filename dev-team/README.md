# A development team

This folder defines a brg team for developing a software project: a lead and four helpers it starts on demand. `../dev-design-team/` is its design panel, a separate team the lead seats before costly-to-reverse design choices. Both folders are generic. Everything about one machine and one project (checkouts, message storage, the GitHub identity, the project's checks and conventions) goes in deployment files you keep outside git.

| Member | Default vendor and model | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the team | plans, assigns, verifies, pushes, opens and hands off pull requests |
| `implementer` | Codex gpt-6.1-sol high | per task | one task at a time, in its own checkout; commits locally |
| `reviewer` | Claude opus high | per review | one review pass, then stopped |
| `researcher` | Codex gpt-6.1-terra medium | per question | researches the web and the code, with evidence |
| `monitor` | Claude sonnet low | per watch | observes and reports; never acts |
| `designer-claude`, `designer-codex` (design panel) | Claude opus high, Codex gpt-6-astra medium | per design | questions, then one independent proposal each from a blind brief |

Each member file names both vendors' models; `vendor:` in `team.yaml` picks one. The lead must run as Claude until brg can wake a Codex member: it wakes itself with a background wait, which only Claude's tools support.

## A deployment

```yaml
schema: 1
team: /path/to/repo/dev-team
auth: login
bus:
  root: ~/.local/state/brg-teams
members:
  lead:
    cwd: /path/to/work/lead
    task: "Local rules: act on GitHub as @example-bot; private notes in /path/to/work/notes; the checks are `make check`."
  implementer:
    cwd: /path/to/work/impl
    task: "Local rules: commit as @example-bot; run `make check` before reporting."
```

`team:` is absolute. Give each member that writes its own checkout (one writer per checkout); the researcher and monitor can read the lead's. `task:` holds each member's standing local rules: the lead's also name the owner's handle, the tracking issue, the review checkout and the design panel (its deployment file, checkout, design directory and deadline).

The panel's deployment has the same shape with no `bus:`. Both designers share one read-only design checkout, and each designer's `task:` names the brief and the design directory.

## Run it

1. **Use a stable brg,** installed outside any checkout the team changes: every member runs the brg that started it (`$BORGCHART_BIN`).
2. **Create the checkouts** (`git worktree add --detach …`) and a private notes directory outside them, then check your files: `brg validate --file <deployment>` for each.
3. **Start the team:** `brg up --file <deployment>`. The tree shows `lead … started` and every other member `not started (start: false)`; the lead starts them as their work arrives.
4. **Answer the folder-trust screens,** once per checkout: when `brg status --file <deployment>` shows a member `blocked`, run `brg attach <member> --file <deployment>`, answer, and detach. This can happen again the first time an on-demand member starts in a new checkout. If your Codex sandbox stops a member at the message storage or the notes directory, you can add exactly those to that member's `args` (`-c sandbox_workspace_write.writable_roots=[…]`); that widens the sandbox, so it's your choice.
5. **Work with the lead** in its session (`brg attach lead --file <deployment>`). It reaches you on GitHub: review requests on pull requests it hands off, and comments when it needs you.
6. **Stop the team:** `brg down --file <deployment>`.

## Working rules

- **The lead never merges.** The owner merges every pull request, unless they say otherwise for a specific one.
- **Helpers are started with their work** and stopped when done. They wait only for the answer to their own question: Codex in the foreground, Claude in the background. The monitor runs its watch the same way: Claude in the background, Codex in the foreground in commands of at most 10 minutes.
- **Change the team's configuration only when no work is open.** A changed team or deployment file takes effect at the next `brg down` / `brg up`, which starts fresh message storage.
- **One writer per checkout,** at most one open task for the implementer, and one fresh review per pull request followed by one fix round.
