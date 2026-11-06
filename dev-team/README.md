# The borgchart development team

This folder defines the brg team that develops borgchart: a lead, an on-demand companion, two implementer slots and a reviewer. It is public and generic. Everything about one machine (checkouts, message storage, which GitHub identity to use, build limits) goes in a deployment file you keep outside git, made from `borgchart.example.yaml`.

| Member | Vendor and model | Starts | Does |
| --- | --- | --- | --- |
| `lead` | Claude opus high | with the team | plans, assigns, verifies, pushes, opens and hands off pull requests |
| `companion` | Claude opus high | on demand | challenges the lead's plans, designs and results |
| `implementer-a`, `implementer-b` | Claude opus medium | per task | one task each, in its own checkout; commits locally |
| `reviewer` | Codex, the CLI's default model | per review | one review pass, then stopped |
| `reviewer-claude` | Claude opus high | per review | the fallback when Codex is unavailable |

`brg validate` warns that `reviewer.md` has no `models.codex` entry: that is deliberate (Codex uses its default model).

## Run it

1. **Install a stable brg,** separate from any checkout the team changes. Every member runs the brg that started it (as `$BORGCHART_BIN`), so a team started from a build in a checkout it is changing would have its own tools rebuilt, or removed by `cargo clean`, under it.

   ```sh
   git clone <this repository URL> ~/src/brg-stable && git -C ~/src/brg-stable checkout <commit>
   cargo install --locked --path ~/src/brg-stable --root ~/opt/brg-stable
   ```

   Upgrade the same way, at a new known commit, while no work is open.

2. **Create the four worktrees** from your clone of this repository (one writer each; the companion reads the lead's):

   ```sh
   git -C <your clone> fetch origin
   for w in lead impl-a impl-b review; do git -C <your clone> worktree add --detach ~/borgchart-dev/$w origin/main; done
   mkdir -p ~/borgchart-dev/notes
   ```

   `notes/` is the team's private directory (agreements, reports, review notes). It is outside every checkout.

3. **Write your deployment file:** copy `borgchart.example.yaml` outside the repository, e.g. to `~/.config/borgchart/dev-team.yaml`, and replace every `/path/to/…` and `<…>` value. `team:` must be absolute. Put local rules in each member's `task:` text: the GitHub identity and wrapper to use, the owner's handle and the tracking issue (lead only), build limits, paths never to touch. Check it:

   ```sh
   ~/opt/brg-stable/bin/brg validate --file ~/.config/borgchart/dev-team.yaml
   ```

4. **Answer the folder-trust screens** once per checkout. `auto_trust` is false, so a person answers them: when a member first starts in a checkout and `brg status` shows it `blocked`, run `brg attach <member>`, answer, and detach (your tmux prefix, then `d`). Or run `claude` (and `codex` in the review checkout) there once beforehand.

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

## Working rules

- **The lead never merges.** The owner merges every pull request, unless they say otherwise for a specific one.
- **Codex members are started with their work** and stopped when done; nothing has to wake them.
- **Claude members wake themselves** with a background `brg inbox wait` (see `includes/self-wake.md`).
- **Change the team's configuration only when no work is open.** A changed team or deployment file takes effect at the next `brg down` / `brg up`, which starts fresh message storage without carrying open work over.
- **Run the team from a stable brg install,** never from a build in a checkout the team is changing.
- **One writer per checkout,** and at most one open task per implementer slot.
- **One fresh review per pull request,** then one fix round, with no automatic second review.
