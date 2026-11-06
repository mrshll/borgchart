# The coordination bus

**Status: in progress.** This document is the shared contract for the bus work. The configuration, layout, caller context, envelope and outcome codes below exist in code. Each later section says *planned* until the change implementing it lands.

The bus lets members, and callers outside the team, hand each other durable records: assignments, results, questions, decisions and plain notes. A record is a file. Publishing one never types anything into a terminal. A separate attention worker may type one fixed, payload-free reminder into an idle member, under strict checks, or raise an alert instead.

Limits that hold throughout:

- **Trusted, same-user operation.** Caller identity comes from environment variables that anything running as you can set. The checks prevent mistakes, not spoofing. Nothing here isolates members from each other, and fencing a stale actor can't undo what its shell already did.
- **Payloads are data.** No record body, fact or artifact path is ever typed into a terminal, passed to a shell or executed.
- **Best effort attention.** Records are durable whether or not anyone is woken. Waking a member can still race with a person typing.

## Configuration

The bus is enabled per deployment, in `borgchart.yaml`:

```yaml
schema: 1
bus:
  root: /tmp/brg-teams-1000     # absolute (or ~/); holds each team's message storage
  alert: {plugin: notify, action: alert}   # optional: how brg alerts a person
  warn_age_days: 7              # optional (default 7): `bus check` warns from this age
  max_age_days: 14              # optional (default 14): roll the storage over at this age
```

`brg validate` checks that:

- `root` is absolute and has no whitespace, and is not inside the team directory or brg's state directory. A root inside a git checkout is a warning.
- `0 < warn_age_days < max_age_days`.
- `alert` names a plugin with a `path` and an action it declares.
- no member is `locked`: the bus doesn't support `locked` members yet.

Pick a `root` that every member's vendor configuration can already write without asking. If the only such place is under `/tmp`, keep `max_age_days` below your system's temporary-file cleanup age (and remember a reboot may clear it): brg never relies on reads refreshing a file's age.

**A running team's configuration is frozen.** When a team starts, brg records a snapshot of its inputs: the SHA-256 of `borgchart.yaml`, `team.yaml`, each member file, and each member's instruction with includes expanded. Starting one member into the running team refuses if any of these changed. The change takes effect at the next `brg down`/`brg up`. The snapshot hashes file bytes, not brg's internal types, so a compatible brg upgrade sees the same snapshot.

**Storage rolls over by itself.** Internally a team's messages live in a *run*: storage with one message format and one configuration snapshot. Users never see or name runs; there is nothing to configure.

- **When:** a full `brg up` of a team that is positively stopped (every current member incarnation `gone` or ended), whose configuration changed or whose storage has reached `max_age_days`, starts a new run and carries over the unfinished work (open tasks, unanswered questions, unread messages). `up` of a running team with changed files refuses, like starting one member does.
- **Format changes:** a brg that can't read the old format can't carry from it. So a format change starts fresh storage and carries nothing. The old messages stay on disk untouched, and `up` says what was left behind.
- **No lost messages:** rollover takes the old run's `bus/lock`, writes its `retired` marker, carries, then switches `current`. Every writer re-checks `retired` after taking `bus/lock`, and if it's there, re-resolves `current` and writes to the new run instead.
- **Numbering:** new runs take the next number after the highest that exists (`r1`, `r2`, …), so a lost `current` file can't reuse old storage. A missing `current` beside existing runs is a storage problem, reported in team terms.
- **Where:** storage is found by the deployment's ID, which comes from its file's path. Moving the team's folder starts with empty storage, and the old storage stays where it was.

## Layout

Each deployment has a directory `<root>/<deployment-id>/`. Its `current` file names the run in use, and each run is a subdirectory (`r1/`, `r2/`, …; mode 0700, files 0600). Each area of a run has one owner:

| Path (in a run) | Owner | Contents |
| --- | --- | --- |
| `run.json` | lifecycle | The run manifest: ID, message format (`schema`), deployment, frozen topology, configuration snapshot, link to the run it continues. Immutable. |
| `retired` | lifecycle | Present once the team's storage has rolled over to a newer run; writers then go to `current` instead. |
| `runtime/lifecycle.lock` | lifecycle | Held while starting or stopping anything in the team. |
| `runtime/incarnations/<member>.json` | lifecycle | The member's current incarnation; history in `runtime/incarnations/<member>/<id>.json`. |
| `bus/lock` | records | Held while a record is validated, numbered and written. |
| `bus/records/<seq>.json` | records | Immutable records, numbered from 1 with no gaps (`m1`, `m2`, …). |
| `bus/keys/<slot>/<sha256>.json` | records | Index of operation keys, per actor slot. |
| `bus/acks/<id>.json` | records | The recipient's acknowledgement of one record. |
| `attention/worker.lock`, `worker.json` | watcher | The team's watcher and its state. |
| `attention/observed/<member>/<incarnation>.json` | watcher | What `inbox` has returned to that incarnation. |
| `attention/inflight/<member>.json` | watcher | A wake attempt whose intent is recorded and outcome not yet known. |
| `attention/alerts/<id>.json` | alerts | Open and resolved alerts. |

Before its first write into a run, every writer creates the run directory if needed and checks that the root, the team's directory and the run directory are real directories (not symlinks), owned by you and not writable by group or others. This matters for a root under a shared directory such as `/tmp`.

Immutable files are written with a unique temporary sibling, flushed, then hard-linked to their final name, which fails if the name exists (`src/bus/fsx.rs`). A crash leaves at most a stray `.tmp-*` file; readers skip those.

**Lock order.** Locks are always taken in this order, and released in reverse: `runtime/lifecycle.lock` → `bus/lock` → `attention/worker.lock` → a member's send lock (in brg's state directory). Anything other than the watcher itself only ever *tries* `attention/worker.lock` without waiting, to see whether one is running.

## Caller context

| Caller | How it is recognized | May |
| --- | --- | --- |
| Member | `BORGCHART_MEMBER`, `BORGCHART_RUN` and `BORGCHART_INCARNATION`, set at member start | publish as itself, read and ack its own inbox |
| External | `--external NAME`, with no member variables set | publish to a member, read and ack records addressed to `external:NAME` |
| Operator | neither | read and inspect only |

- `--external` inside a member is refused, and so is a partial member context.
- A member's `BORGCHART_RUN` (internal) must be the team's current run, and its `BORGCHART_INCARNATION` must be the member's current, not-ended incarnation; otherwise mutations are `fenced`. Storage whose `run.json` names a different deployment is refused.
- Operation keys are scoped by slot: the member name, or `external.<name>`.

**Versions.** brg isn't pinned: every call uses the installed `brg`. A brg that can't read a team's message format refuses before reading or writing anything, and says how to fix it:

```
brg: team `oncall`'s messages use format 1; this brg uses format 2.
Restart the team to start fresh storage in the new format: brg down && brg up
(Open work in the old format isn't carried over; the old messages stay on disk.)
```

The restart command is given in a form that works from where you are: the bare commands in the team's folder, or with `--file` from elsewhere.

## Records

A caller writes a **draft**; brg adds identity and stores a **record**.

```json
{
  "to": "helper",
  "kind": "assign",
  "task": "t-1",
  "attempt": 1,
  "reply_to": ["m3"],
  "body": "Free text, up to 64 KiB.",
  "artifacts": [{"path": "/abs/path/report.md", "label": "report"}],
  "facts": {"agreement": {"path": "/abs/path/agreement.md", "sha256": "…"}}
}
```

The stored record adds `schema`, `run`, `id` (`m<seq>`), `seq`, `at` (UTC, RFC 3339), `actor` (`{"type":"member","member":…,"incarnation":…}`, `{"type":"external","name":…}` or `{"type":"bus"}`) and `key`, and replaces each artifact with its measured `bytes` and `sha256`.

**Recipients.** `to` is a member, or `external:<name>` for an outside caller. A member may publish to its parent or its children. A member with no parent (the team's root) may also publish to `external:<name>`, for questions and results that leave the team. An external caller may publish to any member.

**Kinds.** `note`, `question`, `decision`, and the task kinds `assign`, `accept`, `decline`, `result`, `verify`, `release`, `cancel-request`, `cancel-confirm`, `reopen`, which need `task`.

**Shape limits** (checked before anything is written): unknown fields are refused; task IDs match `[a-z0-9][a-z0-9._-]{0,63}`; `attempt` starts at 1; at most 32 `reply_to` IDs and 32 artifacts, with absolute paths; body up to 64 KiB and facts up to 16 KiB, with no control characters other than newline and tab; whole record up to 128 KiB. Keys are 1–128 characters of `[A-Za-z0-9._:/@#+=-]`.

**Extending.** Within schema 1 the envelope is closed: an unknown top-level field makes a record unreadable, and an unreadable record stops mutations. `facts` is the only extension point. Readers ignore fact keys they don't know; each kind's required facts are below.

| Kind | Required facts | Notes |
| --- | --- | --- |
| `assign` | `agreement: {path, sha256}`, `scope` (text) | Optional `provenance` (text), `limits` (text), `checkout` (absolute path the assignee will write). `attempt` required. |
| `accept` | — | Answers an `assign` (`reply_to`). |
| `decline` | `reason` | |
| `result` | `status`: `complete`, `partial` or `blocked` | The assignee's claim, not verification. Optional `checks: [{name, outcome}]`. |
| `verify` | `result` (the `result` record ID), `outcome`: `accepted` or `rejected` | Optional `checks`. |
| `cancel-request` | `reason` | |
| `cancel-confirm` | `quiescent: true`, `evidence` (text) | The assignee confirms it has stopped writing. |
| `release` | `evidence` (text) | Optional `checkout`, `writer_incarnation`. Never frees a checkout whose writer may be alive. |
| `reopen` | `reason` | Starts `attempt` + 1. |
| `question` | — | Body is the question. |
| `decision` | — | Answers questions named in `reply_to`. Authorization stays prose, with provenance. |

**Retries.** The record is the commit point: a crash before its final name exists means nothing was published, and after it means it was. A retry with the same slot and key and the same draft (artifact contents included) returns the original record (`duplicate`); with any difference, including changed artifact bytes, it fails (`conflict`). Sequence numbers come from the records themselves, so a crash can't leave a gap; a gap means a record was removed, which is `corrupt`.

## Commands — records (planned)

```
brg msg publish --key KEY [--record FILE|-] [--external NAME] [--json]
brg inbox [--member ID | --external NAME] [--after ID] [--limit N] [--all] [--json]
brg inbox wait [--member ID | --external NAME] [--after ID] --timeout SECONDS [--json]
brg msg read ID [--json]
brg msg ack ID… [--external NAME] [--json]
brg bus inspect [--json]
brg bus check [--json]
```

- **publish** validates the draft against the run's frozen topology and the rules above, checks `reply_to` IDs exist, measures artifacts, then numbers and writes the record under `bus/lock`.
- **inbox** lists records addressed to one recipient that it hasn't acknowledged (`--all` includes acknowledged ones), oldest first, a page at a time, with a cursor for the next page. Reading never acknowledges. A member lists its own inbox; anyone may name another with `--member`, read-only.
- **inbox wait** returns as soon as the inbox has something after `--after`, or exits `timeout`. The timeout is required and capped.
- **ack** records consumption by the recipient (a member's current incarnation, or the named external caller). Acking twice is a no-op.
- **inspect** and **check** never write. `check` reports the first unreadable record or gap, stray temporary files, and the run's age against `warn_age_days`/`max_age_days`.
- An unreadable record, a gap or an unreadable key/ack stops every mutation (`corrupt`); reads and inspection still work and say the store is incomplete.

## Task facts and recovery (planned)

Folds task records into per-task state: assigned, accepted or declined, result claimed, verified, released, cancel requested and confirmed, reopened as a new attempt, plus open questions and their decisions. Authority mutations (`assign`, `verify`, `release`, `cancel-request`, `reopen`, `decision`) must come from the recipient's parent's current incarnation, or from an external caller for a member with no parent; anything else is `fenced`. A member holds at most one unresolved task. Releasing never frees a checkout while its writer may be alive (`Liveness` other than `gone`). When storage rolls over (see Configuration), unresolved tasks, open questions and unacknowledged records are carried into the new run; the old run is left untouched.

## Lifecycle and runtime binding

**Checks before changes.** `brg up` works out everything a start needs (plugin values, `auth: api_key` keys, which storage the team's messages go to, whether the team is already running) before it writes anything. A refused start leaves brg's state directory and the team's message storage as they were.

**Serialized.** `up`, `up --member`, `down` and `down --member` take a lock in brg's state directory (`<state>/lifecycle.lock`) and then, with a bus, `runtime/lifecycle.lock`. `up` holds them while it writes files and starts windows, and releases them before it waits for `locked` gates and Claude's folder trust, so members can already use the bus and another `up --member` isn't held up. Anything that wait changes (the keys that accept folder trust, the trust marker, stopping a `locked` member that fails its check) takes the locks again and first checks that the window still runs the very pane (ID and PID) this `up` launched; a member stopped or replaced meanwhile is left alone (`replaced while starting`), and for a `locked` member that counts as a failed start. A `locked` member whose pane can't be read, at start or when it must be stopped, is stopped by its window name while the locks are held, so it never works without its gate. Two `up --member helper` at once start it once; the other reports `already running`.

**What `up` and `status` print.** `up` prints the team as a tree (members under their `parent`, siblings by name), each member with its vendor, model and what happened: `started`, `already running`, `not started (start: false)`, `failed: …`, or a `locked` member's `stopped: …`. Plugin processes follow the tree. `up --member` leaves the other members' outcome blank.

```
$ brg up
oncall
└─ lead      claude opus high     started
   └─ helper codex example-model  not started (start: false)
```

`status` prints the same tree with each member's live state and details, then the plugin processes. A member with no window is `not started` when it has never run since the team's storage began (with a bus: no incarnation in the current storage, so every member of a team that never started, or just rolled over, is `not started`; without one: no start since the team last started afresh), printed as `up` prints it: `not started (start: false)` for a `start: false` member. One that ran and ended (or whose start record can't be read) is `stopped`; for a `start: true` member that is an alert, printed `stopped (should be running)`, with `"expected_running": true` in `--json` and shown like `exited` in `status -i`. `status --json` is the same rows as before, in tree order, with `parent`, `model`, `expected_running` when set and, with a bus, `liveness`.

```
$ brg down --member helper
helper: stopped
$ brg status
oncall
└─ lead        claude opus high      idle
   ├─ helper   codex example-model   stopped (should be running)
   └─ reviewer claude sonnet medium  not started (start: false)
```

**Starting into a running team.** When the team's session exists and its `deployment.json` is there, `up` (with or without `--member`) joins it instead of starting afresh. It uses the base environment and plugin values recorded at the first `up`, never your current shell's: nothing from your `PATH`, `TMPDIR` or other variables reaches the new member, and you don't need the plugin secrets set again. The one exception is `auth: api_key`: the vendor's key (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) is read from your environment at each start, for the members being started, and passed to them. It is not frozen and brg never stores it (it is in the member's launch file only until the window reads it); a start without it is refused before anything is written. It doesn't rewrite `deployment.json`, plugin value files or other members' files. If any configuration input changed since the team started (see Configuration), it refuses, changing nothing, and names what changed in team and member terms:

```
brg: oncall/helper's instruction file changed since the team started; restart the team to apply it: brg down && brg up
```

The restart command is the bare form in the team's folder and names `--file` elsewhere. A full `up` of a running team with changed files refuses the same way.

**brg isn't pinned.** Every window, plugin shim, gate hook and plugin process runs the `brg` that started it (`_exec`, `_run` and `_gate` use the running binary, as in v0): a member started later with `up --member` uses the `brg` that ran that command, and members find whatever `brg` is installed on their `PATH`; members get no `BORGCHART_BIN` (plugin programs still do: the `brg` that started the team). So start a team from a stable install, not a build that `cargo clean` can remove. A plugin command may not be called `brg` or `borgchart`: a granted command's shim comes first on the member's `PATH`, so it would shadow the installed brg and the member's message commands would call the plugin.

**Message storage at `up`.** With a bus, the team's first `up` creates its storage (internally, run `r1`, named in the team's `current` file) and writes `run.json`: the deployment ID, message format, frozen topology and configuration snapshot. Later starts reuse it, except that a full `up` of a stopped team rolls over to a new run (see Configuration) when the configuration or message format changed or the storage reached `max_age_days`:

- The new run takes the next number after the highest that exists, and its `run.json` links to the old one (`linked_from: {run, root, reason}`, with reason `config`, `format` or `age`).
- Rollover needs every member positively stopped: each current incarnation recorded in the old run (including members the new configuration no longer has) must be ended or `gone`. One that is `alive`, `unknown`, unreadable or malformed refuses (`brg can't tell that oncall/helper has stopped …`), changing nothing, whatever the reason for the rollover.
- It takes the old run's `bus/lock`, writes the old run's `retired` marker, carries unfinished work (configuration and age rollovers only), then switches `current`. Carrying is a hook (`carry_into`) that task facts fill in; until it is wired in, a configuration or age rollover leaves unfinished work behind in the old run.
- A format rollover carries nothing (this brg can't read the old format) and `up` says how many messages were left on disk.
- `up --member` never rolls over: with changed files or another format it refuses with the restart command. A team that is running, or whose storage is missing its `current` file while runs exist, or whose current run is missing its `run.json`, is never rolled over or started either; those refuse as a storage problem before writing anything.
- Nothing ever starts into retired storage. If a rollover was interrupted after retiring the old run but before switching `current`, the next full `up` finishes it (switches to the run the marker names, carrying again, so carrying must be idempotent) when that run clearly is its successor (exists, belongs to this team, links back), then decides as usual; otherwise it refuses. `up --member` refuses and asks for a full start.
- Lifecycle storage errors are reported in team terms, without storage paths or run IDs.

The old run is never changed beyond its `retired` marker.

**Incarnations.** Each member start is a new incarnation, with an ID `<member>.<UTC yyyymmddhhmmssmmm>.<4 hex>` that is unique in the run. Its current file is written before the window starts, then again with the pane's identity (`%N` pane ID, PID, window, session, and the tmux server's absolute socket path) and the vendor version measured at this start; the same record then goes into the history (`runtime/incarnations/<member>/<id>.json`). The member gets `BORGCHART_RUN` (internal) and `BORGCHART_INCARNATION`. `down` and `down --member` mark the incarnations they stop ended (`ended_at`, `ended_reason: "stopped"`); a window tmux fails to create is ended as `start-failed`. Every change to an incarnation file is made holding `bus/lock` (briefly, after the lifecycle locks), so a bus mutation that checks a member's incarnation under that lock sees either the old record or the new one.

**Stopping.** `up` records the tmux socket name and the server's absolute socket path. `down` and `down --member` address that server directly, so a caller's `TMUX_TMPDIR` can't point them elsewhere, and refuse a caller whose `BORGCHART_TMUX_SOCKET` names another socket. They change nothing unless the server answers: an incarnation is marked ended, and secrets and launch files removed, only after brg stopped its pane or the server showed it absent. A `down` that stops the whole session (or finds it gone) records the confirmed stop in `deployment.json`, so a later `down` after the server has exited just says `not running`. If the server really is gone without a `down` (for example after a reboot), `brg up` and then `brg down` clear it.

**Liveness.** brg asks the incarnation's recorded tmux server (by its absolute socket path) for every pane. The incarnation is `alive` when that list has its pane with the same ID and PID, not dead; `gone` when the server answers and the pane is absent, dead or another process, or when the incarnation was ended by a confirmed stop; `unknown` otherwise: no pane recorded, tmux missing, or a server that can't be reached (not running, a removed or hidden socket, no permission). An unreachable server is never taken as proof that anything stopped; treat `unknown` as possibly alive.

## Attention worker (planned)

Replaced by the team's *watcher*, a hidden tmux window described in the sessions design (`docs/sessions.md`, proposed in its own change). The rules that carry over: only a member listing its own inbox marks records as observed; observation is separate from acknowledgement; new records after an observation start a new episode; the watcher never creates a tmux session; and a wake left in flight by a crash is never retried.

## Guarded wake (planned)

For a vendor, version and configuration that has passed qualification, the worker may paste one fixed reminder (never a payload) and press Enter. Before pasting it checks the pane's identity (against the incarnation), state, input box and dialogs; before Enter it checks the input box holds exactly the reminder and no dialog appeared. It never clears input, forces, answers a dialog or presses Enter after a crash. At most one paste per episode. A member at a prompt (`blocked`) gets nothing typed and an alert. A busy member, a draft in the input box or a transient uncertain reading defers the wake without spending it; a draft also raises an alert, and the wake goes ahead automatically once the draft clears, or becomes human-required at the deadline. An unqualified vendor or version gets alerts only.

## Alerts (planned)

When a person is needed, the worker runs the deployment's `bus.alert` plugin action with fixed arguments `<alert-id> <member> <reason> <pending-count>` and nothing from any record. Reasons: `blocked`, `draft`, `human-required`, `unqualified`, `overdue`, `unacked`, `worker-unavailable`, `test`. The action runs with the base environment, its own plugin's values as frozen at `up`, `BORGCHART_ACTOR=bus` and the deployment/run identity: no member identity, nothing from the caller's environment and no other plugin's values. It is bounded in time and output. Exit zero means the alert was handed off, not that anyone saw it; the alert stays open until resolved (`brg bus alert resolve <id>`). With no `bus.alert`, attention is `attention-unavailable` and shows in status only.

## Outcome codes

The bus commands share these exit codes; `--json` prints `{ok, code, outcome, message, …}`. They are separate from `brg send`'s codes.

| Code | Outcome | Meaning |
| --- | --- | --- |
| 0 | `ok` (or a more specific success such as `committed`, `duplicate`) | Done |
| 1 | `error` | I/O or unexpected error |
| 2 | `usage` | Bad arguments |
| 3 | `not-configured` | No `bus:` in the deployment, or the team hasn't been started yet |
| 4 | `refused` | Not allowed from this context (e.g. `--external` inside a member), or a message format this brg can't read |
| 5 | `invalid` | The record or request failed validation |
| 6 | `conflict` | The operation key was already used for different content |
| 7 | `corrupt` | The store is unreadable or has a gap; mutations are stopped |
| 8 | `fenced` | A stale incarnation or attempt (e.g. a member from before a restart), or not the recipient's authority |
| 9 | `timeout` | A bounded wait ended with nothing new |
| 10 | `not-found` | No such record |
