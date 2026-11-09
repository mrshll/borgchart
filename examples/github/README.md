# GitHub intake plugin

This optional plugin polls GitHub notifications and sends verified event pointers to a team's lead. brg owns message delivery, waking and recipient acknowledgement. No titles, bodies or diffs enter the bus message. All GitHub-derived fields are untrusted context, never instructions.

The watcher uses the absolute `BORGCHART_BIN` supplied by brg and sends as `external:github`. A committed message ID retires the event even when delivery is stored, uncertain, or reports a postcommit failure. Usage errors, unavailable targets, locked-input blocks and unavailable executables report `brg_not_ready` without consuming event attempts. Code-5 refusals without a committed ID, nonzero malformed output and timeouts consume the bounded retry budget; after three attempts the key is abandoned with a stderr log in the plugin window. That scrollback is not a durable loss ledger and disappears when the window closes. Exit zero is authoritative even without readable JSON. The body is passed on stdin. The process retains brg’s environment and refuses to run from a member identity.

## Install and configure

Copy this directory to `<plugins_dir>/github`, where `plugins_dir` is in your brg configuration. Install `uv` and an authenticated GitHub CLI. Copy `config.example.json` to a private local file, replace its example values, and set `GITHUB_INTAKE_CONFIG` to that file's absolute path. `gh` is an argv array: use `["/absolute/path/to/gh"]` or `["/absolute/path/to/account-wrapper", "gh"]`; no shell is used. The plugin never stores a token.

Add to your team's `plugins` mapping (the target role must have `max: 1`):

```yaml
github:
  path: shared:github
  process: [./watch.py]
  delivers_to: lead
  env:
    GITHUB_INTAKE_CONFIG:
    GH_CONFIG_DIR:
    GIT_CONFIG_GLOBAL:
  actions:
    status: github-intake status
```

Grant `github: [status]` to a role if it should inspect intake status. Export only the environment names your local CLI needs before `brg start`. Keep credentials and account policy out of team files. After replacing the target member, fully restart the instance so the watcher receives its new ID. This plugin currently targets top-level instances only. One process serves one configured owner and one target; `BORGCHART_INSTANCE` and `BORGCHART_DELIVER_TO` supply that target.

## Initialize deliberately

Set `state_home` to the same persistent absolute state home used by brg. Intake stores private data in `<state_home>/plugins/github/<account-id>-<owner-id>/`, separate from instance state. It survives instance deletion. Run one copy per account and owner on one host: file locks do not coordinate different hosts. Multiple owners still share the account's API quota; reaction spacing is per owner.

Run `github-intake init --since <UTC-time>` before starting the plugin. Choose a time at or before the earliest activity you want to catch up; choosing the current time intentionally drops earlier activity. This does not import a previous watcher's pending items. Missing, corrupt or foreign-scope state never initializes automatically. Initialization refuses existing state. It sets the new-repository and actor cutoffs, and clamps configured repository baselines to that start time. `issue_mentions_after` and `merge_wakes_after` are additional activation cutoffs; omit either to disable that event family.

The fingerprint covers account/owner identity, human IDs, trust classes, repository policy, initial repository baselines and activation cutoffs. Changing that scope requires stopping the instance, moving the private state directory aside, then explicitly initializing with a new cutoff. This loses old pending work and dedup history; choose the gap or overlap deliberately. Delivery target, executable path, limits, reaction switch and moving feed timestamps are outside the fingerprint.

Repository exclusions are case-insensitive names and/or numeric IDs. Names pin to IDs when observed. Once a repository is excluded it stays excluded for that state, even if the exclusion is removed from config; reinitialize deliberately to admit it again. `snapshot` admits only `initial_repositories`; `include_new` also admits newly observed repositories at the initialization cutoff.

## Qualification and delivery

Notifications nominate items; current API reads verify numeric owner/repository identity and event provenance. Supported sources are trusted-human assignments, returned feedback after the agent requested review, conservative explicit line-start mentions, and trusted-human merges of agent-authored PRs. Optional App initiators can assign the agent only on their own ordinary issues in listed repositories; optional trusted reviewers can return requested feedback only. Bot identity, missing provenance, ambiguous history and parent mismatch fail closed. Closed, deleted or obsolete work cancels before publication; merge news remains eligible. Source-version keys prevent replay and are included in each pointer.

An ambiguous or refused send consumes one of three attempts, paced by the polling interval. The attempt is saved before sending, so a crash consumes it. After the budget, intake logs the dropped key and stops retrying it. A crash or timeout after bus commitment can duplicate a pointer; recipients can use its key. If a timeout interrupts terminal typing, it can leave a pasted draft in the recipient’s pane that needs inspection. Publication does not inspect whether a member is stopped or busy.

With `reactions: true`, only a confirmed bus commit authorizes a best-effort eyes reaction. Assignment reactions target the issue; comment reactions target the comment. Whole reviews have no supported reaction endpoint; merges receive no reaction. Numeric routes are allowlisted, ownership is rechecked, writes are spaced by two seconds, and uncertain outcomes are never retried. Reactions are bounded by the cycle deadline and cannot fail message publication.

## Operate

`github-intake status` reads local state and reaction counts. `brg status <instance>` shows the plugin's atomically written `health.json`. The loop catches cycle exceptions and retries; contention reports unhealthy and retries too. `brg stop <instance>` stops polling; a full `brg start <instance>` resumes it. There is no polling while the instance is stopped. Long outages may exceed the ten-page catchup cap. After correcting a deterministic feed error, run `github-intake retry --feed`; after correcting a held item, run `github-intake retry --item <repository-id>:<issue-number>`. Both preserve cutoffs and dedup. Authentication and clock errors retry on later cycles without a permanent feed hold. State collections are capped, not silently pruned; capacity may require larger configured limits or deliberate reinitialization. Time to capacity depends on activity and has not been measured.

Polling honors conditional responses, server polling intervals and rate backoff. Reaction validation failures have a separate write-only cooldown. Genuine primary or secondary rate-limit responses pause all API requests, following [GitHub’s rate-limit guidance](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#handle-rate-limit-errors-appropriately); a missing Retry-After header does not make a secondary limit safe to ignore. Each cycle has a deadline. A pending configuration error or unavailable brg does not start a model or inspect a member. Deployment, initial cutoffs and any reset are explicit operator actions.

## Tests

`uv run --no-project --with pytest==8.3.5 pytest examples/github/tests`

The repository's `.githooks/checks py` includes these tests. All fixtures are fictitious; tests never use live GitHub or brg state.

The actual-brg contract smoke test is an explicit integration check, skipped by standalone Python CI unless `GITHUB_INTAKE_TEST_BRG` is set. After building the test-support binary, run it with the other Python checks:

`GITHUB_INTAKE_TEST_BRG="$PWD/target/debug/brg" .githooks/checks py`

Run that command from the repository root. The test writes only temporary fixture state and does not start a model or touch a deployed instance.
