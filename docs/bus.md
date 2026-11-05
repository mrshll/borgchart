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

**Storage rolls over by itself.** Internally a team's messages live in a *run*: storage with one message format and one configuration snapshot. When `brg up` starts a team whose configuration or message format changed, or whose storage has reached `max_age_days`, brg starts a new run and carries over the unfinished work (open tasks, unanswered questions, unread messages). Users never see or name runs; there is nothing to configure.

## Layout

Each deployment has a directory `<root>/<deployment-id>/`. Its `current` file names the run in use, and each run is a subdirectory (`r1/`, `r2/`, …; mode 0700, files 0600). Each area of a run has one owner:

| Path (in a run) | Owner | Contents |
| --- | --- | --- |
| `run.json` | lifecycle | The run manifest: ID, message format (`schema`), deployment, frozen topology, configuration snapshot, link to the run it continues. Immutable. |
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
Restart the team to move them over: brg down oncall && brg up oncall
```

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

## Lifecycle and runtime binding (planned)

- Every start validates fully before changing anything; lifecycle changes are serialized by `runtime/lifecycle.lock` (and, without a bus, a lock in brg's state directory).
- Starting one member into a running deployment reuses the deployment's original binary, base environment and plugin values rather than the caller's, and refuses a different binary or changed configuration without touching the running deployment.
- With a bus, `up` creates the team's storage on first start, and rolls it over (see Configuration) when the configuration or message format changed. Each member start gets a new incarnation: recorded before the window starts, then with its pane identity (`%N` pane ID, PID, window, socket) and vendor version once known. `down` marks the incarnations it stops as ended.
- `up` and `status` print the team as a tree, with each member's vendor, model and outcome or state. Messages talk about teams and members, never runs.

## Attention worker (planned)

Replaced by the team's *watcher*, a hidden tmux window described in [sessions.md](sessions.md#the-watcher). The rules that carry over: only a member listing its own inbox marks records as observed; observation is separate from acknowledgement; new records after an observation start a new episode; the watcher never creates a tmux session; and a wake left in flight by a crash is never retried.

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
