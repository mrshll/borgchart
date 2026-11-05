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
  root: ~/borgchart-runs        # absolute (or ~/); holds one directory per run
  run: example-run-1            # this deployment's run ID: [a-z0-9][a-z0-9._-]{0,63}
  continues: example-run-0      # optional: the earlier run (same root) this one carries on
  alert: {plugin: notify, action: alert}   # optional: how brg alerts a person
  warn_age_days: 7              # optional (default 7): `bus check` warns from this age
  max_age_days: 14              # optional (default 14): rotate into a new run before this
```

`brg validate` checks that:

- `root` is absolute and has no whitespace, and is not inside the team directory or brg's state directory. A root inside a git checkout is a warning.
- `continues`, if set, is another valid run ID.
- `0 < warn_age_days < max_age_days`.
- `alert` names a plugin with a `path` and an action it declares.
- no member is `locked`: the bus doesn't support `locked` members yet.

Pick a `root` that every member's vendor configuration can already write without asking. If the only such place is under `/tmp`, keep `max_age_days` below your system's temporary-file cleanup age (and remember a reboot may clear it): brg never relies on reads refreshing a file's age.

**A run's configuration is frozen.** The first `brg up` of a run records a snapshot of its inputs: the SHA-256 of `borgchart.yaml`, `team.yaml`, each member file, and each member's instruction with includes expanded. Later starts in the same run refuse if any of these changed. To change configuration, start a new run (`run:`), optionally with `continues:` naming the old one. The snapshot hashes file bytes, not brg's internal types, so a compatible brg upgrade sees the same snapshot.

## Layout

Everything for one run lives in `<root>/<run>/` (mode 0700, files 0600). Each area has one owner:

| Path | Owner | Contents |
| --- | --- | --- |
| `run.json` | lifecycle | The run manifest: ID, schema, deployment, frozen topology, configuration snapshot, optional link to the run it continues. Immutable. |
| `runtime/lifecycle.lock` | lifecycle | Held while starting, stopping or rebinding anything in the run. |
| `runtime/generation.lock` | lifecycle | Taken shared by bus mutations and the attention worker, exclusive by a rebind. |
| `runtime/generations/<n>.json` | lifecycle | One per runtime generation: original binary path and SHA-256, the pinned copy, state directory, tmux socket, reason. Immutable. |
| `runtime/bin/<sha256>/brg` | lifecycle | The run's pinned copy of each binary it has used. |
| `runtime/incarnations/<member>.json` | lifecycle | The member's current incarnation; history in `runtime/incarnations/<member>/<id>.json`. |
| `bus/lock` | records | Held while a record is validated, numbered and written. |
| `bus/records/<seq>.json` | records | Immutable records, numbered from 1 with no gaps (`m1`, `m2`, …). |
| `bus/keys/<slot>/<sha256>.json` | records | Index of operation keys, per actor slot. |
| `bus/acks/<id>.json` | records | The recipient's acknowledgement of one record. |
| `attention/worker.lock`, `worker.json` | attention | The single attention worker and its state. |
| `attention/observed/<member>/<incarnation>.json` | attention | What `inbox` has returned to that incarnation. |
| `attention/inflight/<member>.json` | attention | A wake attempt whose intent is recorded and outcome not yet known. |
| `attention/alerts/<id>.json` | alerts | Open and resolved alerts. |

Before its first write into a run, every writer creates the run directory if needed and checks that the root and run directory are real directories (not symlinks), owned by you and not writable by group or others. This matters for a root under a shared directory such as `/tmp`.

Immutable files are written with a unique temporary sibling, flushed, then hard-linked to their final name, which fails if the name exists (`src/bus/fsx.rs`). A crash leaves at most a stray `.tmp-*` file; readers skip those.

**Lock order.** Locks are always taken in this order, and released in reverse: `runtime/lifecycle.lock` → `runtime/generation.lock` → `bus/lock` → `attention/worker.lock` → a member's send lock (in brg's state directory). Anything other than the worker itself only ever *tries* `attention/worker.lock` without waiting, to see whether a worker is running.

## Caller context

| Caller | How it is recognized | May |
| --- | --- | --- |
| Member | `BORGCHART_MEMBER`, `BORGCHART_RUN` and `BORGCHART_INCARNATION`, set at member start | publish as itself, read and ack its own inbox |
| External | `--external NAME`, with no member variables set | publish to a member, read and ack records addressed to `external:NAME` |
| Operator | neither | read and inspect only |

- `--external` inside a member is refused, and so is a partial member context.
- A member's `BORGCHART_RUN` must be the deployment's `bus.run`, and its `BORGCHART_INCARNATION` must be the member's current, not-ended incarnation; otherwise mutations are `fenced`. A run whose `run.json` names a different deployment is refused.
- Operation keys are scoped by slot: the member name, or `external.<name>`.

**Pinned binary.** Each runtime generation keeps its own copy of the `brg` binary under the run, and every window, shim and worker of that generation runs the copy, so rebuilding or reinstalling the original changes nothing. Members get `BORGCHART_BIN` (the pinned copy's absolute path) and a `brg` first on their `PATH` that runs it. Every mutating bus command checks that it *is* the current generation's binary (by SHA-256) and is `refused` otherwise; read-only commands work from any compatible `brg`. Outside callers should call the pinned path too; `brg run show` prints it.

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

Folds task records into per-task state: assigned, accepted or declined, result claimed, verified, released, cancel requested and confirmed, reopened as a new attempt, plus open questions and their decisions. Authority mutations (`assign`, `verify`, `release`, `cancel-request`, `reopen`, `decision`) must come from the recipient's parent's current incarnation, or from an external caller for a member with no parent; anything else is `fenced`. A member holds at most one unresolved task. Releasing never frees a checkout while its writer may be alive (`Liveness` other than `gone`). A run nearing `max_age_days`, or recovering from a corrupt store, continues in a new run (`continues:`) into which unresolved tasks, open questions and unacknowledged records are carried explicitly; the old run is left untouched.

## Lifecycle and runtime binding (planned)

- Every start validates fully before changing anything; lifecycle changes are serialized by `runtime/lifecycle.lock` (and, without a bus, a lock in brg's state directory).
- Starting one member into a running deployment reuses the deployment's original binary, base environment and plugin values rather than the caller's, and refuses a different binary or changed configuration without touching the running deployment.
- With a bus, `up` creates `run.json` on the run's first start and refuses a configuration that no longer matches it. It records a runtime generation (pinning the binary) and gives each member start a new incarnation: before the window starts, then with its pane identity (`%N` pane ID, PID, window, socket) and vendor version once known. `down` marks the incarnations it stops as ended.
- Moving the run to another binary is an explicit rebind, allowed only when the deployment is stopped, `runtime/generation.lock` can be taken exclusively (no worker or bus mutation running) and `attention/inflight/` is empty. Rollback is another rebind. The schema never changes within a run.
- `brg run show` prints the run, its current generation and pinned binary, and each member's incarnation and liveness.

## Attention worker (planned)

One on-demand worker per run watches for records nobody has seen. Only a member listing *its own* inbox (`inbox`, `inbox wait`) marks the IDs returned to that incarnation as observed; reads by anyone else, and `msg read`, never do. Observation is separate from acknowledgement. Records published after an observation start a new episode. For each episode the worker either wakes the member once (see below) or raises an alert. `publish`, `inbox`, `read` and `ack` start the worker if attention is pending and it isn't running; `brg bus service ensure` does so explicitly; `status`, `inspect` and `check` never do. The worker never creates a tmux session: publishing to a stopped deployment succeeds and reports `attention-unavailable`. A wake attempt left in flight by a crash is never retried automatically; `brg bus attention resolve <member>` clears it once a person has looked.

## Guarded wake (planned)

For a vendor, version and configuration that has passed qualification, the worker may paste one fixed reminder (never a payload) and press Enter. Before pasting it checks the pane's identity (against the incarnation), state, input box and dialogs; before Enter it checks the input box holds exactly the reminder and no dialog appeared. It never clears input, forces, answers a dialog or presses Enter after a crash. At most one paste per episode. A member at a prompt (`blocked`) gets nothing typed and an alert. A busy member, a draft in the input box or a transient uncertain reading defers the wake without spending it; a draft also raises an alert, and the wake goes ahead automatically once the draft clears, or becomes human-required at the deadline. An unqualified vendor or version gets alerts only.

## Alerts

When a person is needed, brg runs the deployment's `bus.alert` plugin action. An alert names a member, a reason and how many records are waiting, and nothing from any record.

```yaml
# team.yaml
plugins:
  notify:
    path: plugins/notify
    env: [NOTIFY_TOKEN]
    actions:
      alert: notify alert <id> <member> <reason> <pending>
# borgchart.yaml
bus:
  alert: {plugin: notify, action: alert}
```

- **Arguments.** The program is `<plugin dir>/<command>` (a plugin whose commands come from `PATH` can't be an alert action). Its arguments are the action line's literal words after the command and before the first placeholder (above: `alert`), then exactly `<alert-id> <member> <reason> <pending-count>`. Alert IDs match `[a-z0-9][a-z0-9._-]{0,63}`; member names are checked too, so every argument is a plain word.
- **Reasons:** `blocked`, `draft`, `human-required`, `unqualified`, `overdue`, `unacked`, `worker-unavailable`, `test`.
- **Environment.** Built from scratch: the base environment recorded at `up`, that plugin's own values as written at `up` (secrets included), `BORGCHART_ACTOR=bus`, `BORGCHART_DEPLOYMENT`, `BORGCHART_RUN`, `BORGCHART_FILE`, `BORGCHART_BIN` and `BORGCHART_STATE_DIR` (the plugin's data directory). Never `BORGCHART_MEMBER`, `BORGCHART_PARENT` or `BORGCHART_INCARNATION` (even if a values file names them), nothing from the caller's environment and no other plugin's values. Changing your shell or a plugin's values in `borgchart.yaml` after `up` doesn't change what the action sees. The action line and the bus `root`, `run` and `alert` are recorded at `up` too. If the deployment file's `root`, `run` or `alert` no longer match them, the alert commands are `refused` (exit 4) and nothing is written or run, and the action itself is `unavailable`.
- **Bounds.** It runs in the plugin's directory with stdin closed, in its own process group. After 10 seconds the whole group is killed (`timed-out`). The last 4 KiB of its stdout and stderr are kept, with control characters other than newline and tab replaced and the plugin's own values (4 characters or longer) masked as `[redacted]`.
- **Outcomes.** `invoked` (exit zero: handed off, not proof anyone saw it), `failed` (non-zero exit or killed by a signal), `timed-out`, or `unavailable` when nothing could run: no `bus.alert`, the plugin or action wasn't recorded at `up`, the program is missing or not executable, or the plugin's values file is gone (after `brg down`).
- **Store.** Each alert is one file, `attention/alerts/<id>.json`: `id`, `member`, `reason`, `pending`, `opened_at`, `state` (`open` or `resolved`), `invocation_count`, the last 10 `invocations` (`at`, `outcome`, `status`, `message`, `output_tail`, `elapsed_ms`) and `resolved_at`/`resolved_by`. Updates are serialized by `attention/alerts/.lock`, which is never held while an action runs. An alert stays open, whatever its invocations' outcomes, until someone resolves it. An alert file that doesn't match what brg writes (a different ID, non-plain member, bad timestamp, unknown outcome) is `corrupt` rather than printed, and reading refuses root, run or alert directories others can write. `invocation_count` shows how often it was already sent, so the attention worker can bound reminders.
- With no `bus.alert`, attention is `attention-unavailable` and shows in status only.

```
brg bus alert test [--member ID] [--json]
brg bus alerts [--all] [--json]
brg bus alert resolve ID [--json]
```

- **test** opens an alert with reason `test` for the member (default: the team's root), runs the action once and records the invocation. Exit 0 `invoked`; 1 `alert-failed` or `alert-timed-out`; 3 `attention-unavailable` when there is no `bus.alert` (nothing is recorded) or the action is unavailable (the alert is recorded); 3 `not-configured` without `bus:` or before the first `brg up`; 5 `invalid` for an unknown member. 4 `refused` when the bus configuration changed since `up`, or a directory is unsafe (below). The alert stays open either way.
- **alerts** lists open alerts, oldest first (`--all` includes resolved ones). It never writes.
- **resolve** marks an alert resolved by `operator`. Resolving twice keeps the first resolution; an unknown ID is `not-found`. It updates only the requested ID's file, after checking that file names that same ID.
- Before reading or writing, every alert command checks the bus root, the run directory, `attention/` and `attention/alerts/` (those that exist) are real directories you own and others can't write; otherwise it is `refused`.
- `examples/desktop-alert` is a one-member deployment whose action appends a line to its `alerts.log` and, when `notify-send` is installed, shows a desktop notification.

## Outcome codes

The bus commands share these exit codes; `--json` prints `{ok, code, outcome, message, …}`. They are separate from `brg send`'s codes.

| Code | Outcome | Meaning |
| --- | --- | --- |
| 0 | `ok` (or a more specific success such as `committed`, `duplicate`) | Done |
| 1 | `error` | I/O or unexpected error |
| 2 | `usage` | Bad arguments |
| 3 | `not-configured` | No `bus:` in the deployment, or the run hasn't started |
| 4 | `refused` | Not allowed from this context (e.g. `--external` inside a member, or not the run's binary) |
| 5 | `invalid` | The record or request failed validation |
| 6 | `conflict` | The operation key was already used for different content |
| 7 | `corrupt` | The store is unreadable or has a gap; mutations are stopped |
| 8 | `fenced` | A stale run, incarnation or attempt, or not the recipient's authority |
| 9 | `timeout` | A bounded wait ended with nothing new |
| 10 | `not-found` | No such record |
