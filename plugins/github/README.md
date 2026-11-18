# GitHub intake — `brg-github`

Wake the owning lead for four actions GitHub's API attributes to the owner:
an unedited mention of the machine account; assignment of an issue to that
account; a submitted review on a pull request assigned to that account; or a
merge of a pull request assigned to that account. Mentions cover issue and pull
request bodies, comments, reviews and owner inline review comments. Other
accounts' actions and edits that add a mention do not wake anyone.

`make install` installs this Rust workspace plugin alongside `brg`. It needs
`gh` on PATH, authenticated as a separate machine account. Numeric account IDs
must differ from the owner's; an unsafe or changed identity stops discovery.

The private YAML file `GITHUB_CONFIG` defaults to `github.yaml`, beside brg's
configuration file (`~/.config/borgchart/config.yaml`, or `$BORGCHART_CONFIG`).
A relative name resolves against that configuration folder; an absolute name
is used as given. Create it to turn intake on:

```yaml
owner: example-owner
gh_config_dir: ~/.config/gh-example-bot
repositories:
  - example-org/widgets
  - example-org/*
```

`gh_config_dir` is required and `~/` expands to HOME. The plugin pins `gh` to
that profile and clears token, host, repository and debug overrides. Repository
entries are `owner/name` or `org/*`; a user wildcard is refused. Keep this file
private and out of the repository. An empty `repositories: []` watches nothing.

The engineering template runs one poller and grants its lead `read`. The
dev-team template grants its lead `read` without running another poller. After
updating templates for a running tree, run `brg start <root>` once to register
the plugin; members stay running and get the new action at their next start.
Private templates can use the same declarations:

```yaml
plugins:
  github:
    process: [brg-github, run]
    delivers_to: lead
    env: {GITHUB_CONFIG: github.yaml}
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

Use `brg-github read <id>` for each notice item. It rechecks identity and
authorship, prints approval state and commit where applicable, and frames body
lines. Owner text is marked unedited, owner-edited or `NOT AUTHORITATIVE` when
another or unknown editor changed it. Only the root lead merges, after checking
the owner's approving review against the current head on GitHub and green CI.
A comment never grants merge approval. Repeated IDs already taken can be skipped.

Health appears under `brg status`. No file means ok `off: no <path>`, with no
GitHub calls; `read` exits 1 saying intake is not configured. Creating or removing
the file, editing repositories, or changing owner/profile takes effect at the
next poll without restart. Identity changes rerun the guard. Invalid files stop
discovery and mark health not ok; repair the file. Rate limits pause API calls
for ten minutes. Failed delivery and pending events also mark health not ok.

The private state directory keeps `intake.json`, `health.json` and a lock.
Removing configuration keeps the journal. Restart skips activity before process
start but retains admitted events; turning intake on skips activity before it.
Installing a binary leaves running processes on the old binary: end the GitHub
plugin process and run `brg start <root>` to replace it. Updated role text and
action grants need each member's next start.

Known limits: shared branch names across repositories can misroute a notice;
indented code and HTML comments can count as mentions; only the first 50 inline
review comments are scanned. Two trees listing the same repository both deliver.
A process running as the same user can forge a notice, so merge approval and
head must be confirmed on GitHub. Bodies are bounded; `read` points to GitHub
for omitted text. `brg-github run --once` runs one poll for local checks.
