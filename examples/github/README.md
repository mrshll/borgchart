# GitHub intake

`brg github` polls GitHub notifications using a configured GitHub CLI and sends verified, pointer-only JSON as `external:github`. Messages carry `untrusted: true`; consumers fetch any needed content themselves and apply their own instruction boundaries. Titles, bodies and raw API URLs never enter the bus message.

Copy `config.example.json` to private local configuration and replace its fake identities and CLI path. The configured CLI must already authenticate as the configured agent. No tokens are read by this integration. Set `GITHUB_INTAKE_CONFIG` to the absolute config path. Initialize with an explicit UTC cutoff:

```sh
brg github --config /absolute/local/github.json init --since 2026-01-01T00:00:00Z
```

State lives at `<brg state home>/github/<agent-id>-<owner-id>/state.json`. It is private, bounded and explicitly initialized. Existing state, corrupt state and changed authority scope are refused; nothing resets automatically. Keep this reserved directory on durable storage. A scope change requires an operator to archive the old directory and initialize deliberately; that changes the dedup boundary.

Run continuously through brg's existing plugin-process lifecycle. Copy this folder's `watch` launcher into a shared plugin directory and configure a single delivery role:

```yaml
plugins:
  github:
    path: shared:github
    process: [./watch]
    delivers_to: lead
    env:
      GITHUB_INTAKE_CONFIG: /absolute/local/github.json
```

brg supplies `BORGCHART_BIN`, `BORGCHART_INSTANCE`, `BORGCHART_DELIVER_TO` and the plugin health directory. The target stays fixed for that process start; changing `delivers_to` requires a full stop/start. The watcher catches failed ticks and retries; a competing watcher loses the per-cycle lock without exiting. Stop the plugin through the normal instance lifecycle. `tick` runs one bounded cycle with the same plugin target environment. `status` and `retry` need only the config.

Notification pages nominate items. Numeric repository identity, owner, item parent, actor provenance and source timestamps are rechecked before enqueueing and again before sending. Human assignment, returned review feedback, conservative explicit mentions and merge news preserve their own cutoffs. App initiators are limited to enrolled repositories and their own bot's first assignment; reviewer enrollment grants only requested review feedback. Excluded names pin numeric repository IDs. `snapshot` policy admits only `initial_repositories`; `include_new` uses the initialization cutoff for newly discovered repositories.

A tick has one deadline. Every CLI child has capped output and process-group cleanup. The bus sender invokes the current brg executable with a 15-second bound. Exit 0 means committed even without JSON; unavailable routes don't consume attempts. Unconfirmed sends get at most three durable attempts, paced by the poll interval, then a sanitized drop log. Duplicates or misses after uncertain results are accepted; no consumer claim, receipt or ACK protocol is added.

`reactions: true` enables eyes only after a committed send is durably retired. Numeric reaction routes and parent identity are rechecked; merge news and whole reviews have no reaction. Attempts are reserved before POST and never replayed after uncertainty. Writes are serial, spaced from completion, bounded and share primary/secondary read cooldowns. Validation-only write backoff does not freeze intake.

Inspect local counts and recover a deterministic hold without erasing seen state:

```sh
brg github --config /absolute/local/github.json status
brg github --config /absolute/local/github.json retry
brg github --config /absolute/local/github.json retry --item 104:7
```

The first retry clears feed holds; the second releases one held item. Fix the underlying cause first. Authentication, transport and rate-limit failures retry automatically. Health includes retry and per-tick drop counts; ordinary pacing and delivery retries remain healthy, while unavailable routes and deterministic holds are unhealthy. No old Python state import or compatibility format is supported. Nothing here deploys a watcher automatically.

Eyes reactions are best-effort: a busy tick may use its deadline delivering the backlog and skip reactions. A skipped reaction never delays or replays bus delivery.
