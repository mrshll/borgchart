# The borgchart development team

This folder defines the brg team that develops borgchart: a lead and four helpers it starts on demand. `../dev-design-team/` is its design panel, a separate team the lead seats before costly-to-reverse design choices. Both are public and generic. Everything about one machine (checkouts, message storage, which GitHub identity to use, build limits) goes in deployment files you keep outside git, made from each folder's `borgchart.example.yaml`.

| Member | Default vendor and model | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the team | plans, assigns, verifies, pushes, opens and hands off pull requests |
| `implementer` | Codex gpt-6-sol high | per task | one task at a time, in its own checkout; commits locally |
| `reviewer` | Claude opus high | per review | one review pass, then stopped |
| `researcher` | Codex gpt-6-terra medium | per question | answers from primary sources, with evidence |
| `monitor` | Claude sonnet low | per watch | observes and reports; never acts |
| `designer-claude`, `designer-codex` (design panel) | Claude opus high, Codex gpt-6-astra medium | per design | one independent proposal each from a blind brief |

Each member file names both vendors' models; `vendor:` in `team.yaml` picks one. A Codex lead isn't supported until a wake mechanism exists (the watcher, #20, or a wake plugin): the lead wakes itself with a background wait, which only Claude's tools support.

## Run it

1. **Install a stable brg,** separate from any checkout the team changes. Every member runs the brg that started it (as `$BORGCHART_BIN`), so a team started from a build in a checkout it is changing would have its own tools rebuilt, or removed by `cargo clean`, under it.

   ```sh
   git clone <this repository URL> ~/src/brg-stable && git -C ~/src/brg-stable checkout <commit>
   cargo install --locked --path ~/src/brg-stable --root ~/opt/brg-stable
   ```

   Upgrade the same way, at a new known commit, while no work is open.

2. **Create the four worktrees** from your clone of this repository (one writer each; the researcher and monitor read the lead's, the designers read `design`):

   ```sh
   git -C <your clone> fetch origin
   for w in lead impl review design; do git -C <your clone> worktree add --detach ~/borgchart-dev/$w origin/main; done
   mkdir -p ~/borgchart-dev/notes/design/current
   ```

   `notes/` is the team's private directory (agreements, reports, review notes, design briefs and proposals). It is outside every checkout.

3. **Write your deployment files:** copy `borgchart.example.yaml` here and in `../dev-design-team/` outside the repository, e.g. to `~/.config/borgchart/dev-team.yaml` and `~/.config/borgchart/dev-design-team.yaml`, and replace every `/path/to/…` and `<…>` value. `team:` must be absolute. Put local rules in each member's `task:` text: the GitHub identity and wrapper to use, the owner's handle and the tracking issue (lead only), the design panel's deployment file, checkout, directory and deadline (lead only), build limits, paths never to touch. Check both:

   ```sh
   for t in dev-team dev-design-team; do ~/opt/brg-stable/bin/brg validate --file ~/.config/borgchart/$t.yaml; done
   ```

4. **Answer the folder-trust screens** once per checkout. `auto_trust` is false, so a person answers them: when a member first starts in a checkout and `brg status` shows it `blocked`, run `brg attach <member>`, answer, and detach (your tmux prefix, then `d`). Or run `claude` and `codex` there once beforehand, in every checkout whose members use them (the design checkout has both).

5. **Start the team:**

   ```sh
   ~/opt/brg-stable/bin/brg up --file ~/.config/borgchart/dev-team.yaml
   ```

   The tree shows `lead … started` and every other member `not started (start: false)`. The lead starts the others as their work arrives.

6. **Work with the lead** by attaching to it: `~/opt/brg-stable/bin/brg attach lead --file ~/.config/borgchart/dev-team.yaml`. Watch, answer and give it work there; detach when you're done. `brg status --file …` shows the whole team.

7. **The lead reaches you on GitHub,** with the identity its deployment task names: it requests your review on each pull request it hands off, and comments on the pull request, issue or tracking issue when it needs you, mentioning your handle only when it really needs your attention.

8. **Stop the team:**

   ```sh
   ~/opt/brg-stable/bin/brg down --file ~/.config/borgchart/dev-team.yaml
   ```

## The design panel

Before a choice that is costly to reverse, the lead writes a blind brief (goals, constraints, facts, unknowns, an exact baseline commit and a file allowlist, but none of its own ideas) to the design directory, detaches the design checkout at the baseline, and runs `brg up --file <design deployment>`. Each designer reads the brief, writes `proposal-<its name>.md` there and goes idle. When both exist, the lead runs `brg down --file <design deployment>`, files the brief and proposals in a dated folder of its notes, and synthesizes them, recording where each choice came from. The designers run with no bus and never see each other's proposal. They don't replace code review.

## Working rules

- **The lead never merges.** The owner merges every pull request, unless they say otherwise for a specific one.
- **Helpers are started with their work** and stopped when done; nothing has to wake them. They never wait in the background, only in the foreground for the answer to their own question.
- **The lead wakes itself** with a background `brg inbox wait` (see `orchestrator.md`).
- **Change the team's configuration only when no work is open.** A changed team or deployment file takes effect at the next `brg down` / `brg up`, which starts fresh message storage without carrying open work over.
- **Run the team from a stable brg install,** never from a build in a checkout the team is changing.
- **One writer per checkout,** and at most one open task for the implementer.
- **One fresh review per pull request,** then one fix round, with no automatic second review.
