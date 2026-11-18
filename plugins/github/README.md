# GitHub intake — `brg-github`

Wake the owning lead for four actions GitHub's API attributes to the owner:
an unedited mention of the machine account; assignment of an issue to that
account; a submitted review on a pull request assigned to that account; or a
merge of a pull request assigned to that account. Mentions cover issue and pull
request bodies, comments, reviews and owner inline review comments, including
replies in existing threads. Other accounts' actions and edits that add a mention
do not wake anyone.

`make install` installs this Rust workspace plugin alongside `brg`. It needs
`gh` on PATH, authenticated as a separate machine account. Numeric account IDs
must differ from the owner's; an unsafe or changed identity stops discovery.

Add a `github:` section to brg's private configuration file
(`~/.config/borgchart/config.yaml`, or `$BORGCHART_CONFIG`) to turn intake on:

```yaml
schema: 1
github:
  owner: example-owner
  gh_config_dir: ~/.config/gh-example-bot
```

`owner` and `gh_config_dir` are required. `~/` expands to HOME; a relative
profile path resolves against the configuration file's folder. The plugin pins
`gh` to that profile and clears token, host, repository and debug overrides.
Keep owner and profile in this private configuration, out of the repository.
Move `owner`, `gh_config_dir` and any `repositories` list from `github.yaml`
under `github:` in `config.yaml`, then replace the running poller as described
below; the separate file is no longer read.

In `template.yaml`, list repositories in this plugin's
`env.GITHUB_REPOSITORIES` (below): `owner/name` or `org/*` separated by spaces
or newlines. User wildcards are refused. The poller reads from its working
directory, the template folder, so run `brg-github` from PATH, as shipped. A
non-empty template list overrides `github.repositories`; health says so. The
lists are never combined. An absent entry or key, or a blank string (`""`),
uses the section's list. `GITHUB_REPOSITORIES:` without a value is an
error: brg reads the start environment, which the poller ignores. Invalid
lists stop discovery, even an ignored one. With neither list, health is ok
`no repositories listed` and no GitHub call is made. Engineering ships a
commented example. Move any list from `github-repositories.yaml` here and
delete that file.

The engineering template runs one poller and grants its lead `read`. The
dev-team template grants its lead `read` without running another poller. After
updating templates for a running tree, run `brg start <root>` once to register
the plugin; members stay running and get the new action at their next start.
Private templates can use the same declarations:

```yaml
plugins:
  github:
    process: [brg-github, run]
    # This team's lead. brg-github sends PR notices to the dev team's lead whose
    # checkout holds the branch or commit; this lead gets everything else,
    # a copy of each approval, and intake-gap notices.
    delivers_to: lead
    env:
      GITHUB_REPOSITORIES: example-org/widgets example-org/*
    actions:
      read: brg-github read <id>
roles:
  lead:
    plugins: {github: [read]}
```

Polling runs every 60 seconds, at most 20 changed items per poll. A branch or
commit held by one live lead routes there; issues use their linked pull request
heads. No match, several matches, a stopped lead or unavailable routing falls
back to the root lead, with the reason in the notice. Approving reviews routed
to a child lead also get a root copy. Notices contain IDs, not GitHub bodies;
failed sends stay journaled and can repeat after an interrupted send.
After a notice commits to the bus, comments and reviews receive an eyes reaction
meaning received; at most one per poll, with failures in health and no retry
except after a rate-limit pause. A root copy gets none: its owning lead's notice
does. Bodies, assignments and merges get no reaction.

Use `brg-github read <id>` for each notice item. It rechecks identity and
authorship, prints approval state and commit where applicable, and frames body
lines. Owner text is marked unedited, owner-edited or `NOT AUTHORITATIVE` when
another or unknown editor changed it. Only the root lead merges, after checking
the owner's approving review against the current head on GitHub and green CI.
A comment never grants merge approval. Repeated IDs already taken can be skipped.

Health appears under `brg status`. No `github:` section means ok
`off: no github section in <path>`, regardless of the template list, with no
GitHub calls; `read` exits 1 saying intake is not configured. Both brg's
configuration and the template list are reread every poll: adding or removing
the section, editing either list, or changing owner/profile takes effect at the
next poll without restart. The copy of `GITHUB_REPOSITORIES` in the plugin's
environment is set at registration and not used. Identity changes rerun the
guard. An invalid brg configuration (including unrelated sections) or template
list stops discovery and marks health not ok; repair the file. Rate limits pause
API calls for ten minutes. Failed delivery and pending events also mark health not ok.
While off or with no repositories listed, pending events are still delivered.

The private state directory keeps `intake.json`, `health.json` and a lock.
Removing configuration keeps the journal. Restarting or turning intake back on
resumes saved cursors, replaying up to 72 hours of downtime. A longer gap sends
one cut-off notice to the root lead through the journal; check GitHub for earlier
actions. New repository entries start when intake starts or turns on, or at
the next poll when added while running. Listing a first repository after none
skips activity before it. Admitted event IDs stay for four
days to suppress replay duplicates; pending deliveries carry over. Health reports
the earliest replay floor, before which activity is not delivered.
Installing a binary leaves running processes on the old binary: end the GitHub
plugin process and run `brg start <root>` to replace it. Updated role text and
action grants need each member's next start.

Known limits: shared branch names across repositories can misroute a notice;
child teams are found by a role named `lead`;
indented code and HTML comments can count as mentions; only a pull request's
100 most recently started reviews (each thread reply is one) and each review's
first 50 inline comments are checked. Two trees listing the same repository both
deliver.
Every tree started with the same brg configuration reads the same `github:`
section, so give each tree its own scope in its template's
`template.yaml`; a tree whose template lists nothing watches the
section's list. A process running as the same user can forge a notice, so merge approval and
head must be confirmed on GitHub. Bodies are bounded; `read` points to GitHub
for omitted text. `brg-github run --once` runs one poll for local checks.
