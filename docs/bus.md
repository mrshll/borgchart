# The coordination bus

**Status: in progress.** This document is the shared contract for the bus work. The configuration, layout, caller context, envelope and outcome codes below exist in code; each later section is marked with the piece that implements it and changes from *planned* to *done* as it lands.

The bus lets members, and callers outside the team, hand each other durable records: assignments, results, questions, decisions and plain notes. A record is a file. Publishing one never types anything into a terminal. A separate attention worker may later type one fixed, payload-free reminder into an idle member, under strict checks, or raise an alert instead.

Limits that hold throughout:

- **Trusted, same-user operation.** Caller identity comes from environment variables that anything running as you can set. The checks prevent mistakes, not spoofing. Nothing here isolates members from each other.
- **Payloads are data.** No record body, fact or artifact path is ever typed into a terminal, passed to a shell or executed.
- **Best effort attention.** Records are durable whether or not anyone is woken. Waking a member can still race with a person typing.

## Configuration

The bus is enabled per deployment, in `borgchart.yaml`:

```yaml
schema: 1
bus:
  root: ~/borgchart-runs        # absolute (or ~/); holds one directory per run
  run: example-run-1            # this deployment's run ID: [a-z0-9][a-z0-9._-]{0,63}
  alert: {plugin: notify, action: alert}   # optional: how brg alerts a person
  warn_age_days: 7              # optional (default 7): `bus check` warns from this age
  max_age_days: 14              # optional (default 14): rotate into a linked run before this
```

`brg validate` checks that:

- `root` is absolute and has no whitespace, and is not inside the team directory or brg's state directory. A root inside a git checkout is a warning.
- `0 < warn_age_days < max_age_days`.
- `alert` names a plugin with a `path` and an action it declares.
- no member is `locked`: the bus doesn't support `locked` members yet.

Pick a `root` that every member's vendor configuration can already write without asking. If the only such place is under `/tmp`, keep `max_age_days` below your system's temporary-file cleanup age: brg never relies on reads refreshing a file's age.

## Layout

Everything for one run lives in `<root>/<run>/` (mode 0700, files 0600). Each area has one owner:

| Path | Owner | Contents |
| --- | --- | --- |
| `run.json` | lifecycle | The run manifest: ID, schema, deployment, frozen topology, configuration digest, optional link to an earlier run. Immutable. |
| `runtime/lifecycle.lock` | lifecycle | Held while starting, stopping or rebinding anything in the run. |
| `runtime/generations/<n>.json` | lifecycle | One per runtime generation: binary path and SHA-256, state directory, tmux socket, reason. Immutable. |
| `runtime/incarnations/<member>.json` | lifecycle | The member's current incarnation (replaced at each start); history in `runtime/incarnations/<member>/<id>.json`. |
| `bus/lock` | records | Held while a record is validated, numbered and written. |
| `bus/records/<seq>.json` | records | Immutable records, numbered from 1 with no gaps (`m1`, `m2`, …). |
| `bus/keys/<slot>/<sha256>.json` | records | Operation keys, per actor slot. |
| `bus/acks/<id>.json` | records | The recipient's acknowledgement of one record. |
| `attention/worker.lock`, `worker.json` | attention | The single attention worker and its state. |
| `attention/observed/<member>/<incarnation>.json` | attention | What `inbox` has returned to that incarnation. |
| `attention/inflight/<member>.json` | attention | A wake attempt whose intent is recorded and outcome not yet known. |
| `attention/alerts/<id>.json` | alerts | Open and resolved alerts. |

Before its first write into a run, every writer creates the run directory if needed and checks that the root and run directory are real directories (not symlinks), owned by you and not writable by group or others. This matters for a root under a shared directory such as `/tmp`.

Immutable files are written with a unique temporary sibling, flushed, then hard-linked to their final name, which fails if the name exists (`src/bus/fsx.rs`). A crash leaves at most a stray `.tmp-*` file; readers skip those.

## Caller context

| Caller | How it is recognized | May |
| --- | --- | --- |
| Member | `BORGCHART_MEMBER`, `BORGCHART_RUN` and `BORGCHART_INCARNATION`, set at member start | publish as itself, read and ack its own inbox |
| External | `--external NAME`, with no member variables set | publish to a member, read and inspect |
| Operator | neither | read and inspect only |

`--external` inside a member is refused, and so is a partial member context. Operation keys are scoped by slot: the member name, or `external.<name>`.

Members also get `BORGCHART_BIN`, the absolute path of the `brg` that started the run, so their control calls use that binary rather than whatever `brg` is first on `PATH`.

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

Kinds: `note`, `question`, `decision`, and the task kinds `assign`, `accept`, `decline`, `result`, `verify`, `release`, `cancel-request`, `cancel-confirm`, `reopen` (which need `task`).

Shape limits (checked before anything is written): unknown fields are refused; `to` is a member name; task IDs match `[a-z0-9][a-z0-9._-]{0,63}`; `attempt` starts at 1; at most 32 `reply_to` IDs and 32 artifacts, with absolute paths; body up to 64 KiB and facts up to 16 KiB, with no control characters other than newline and tab; whole record up to 128 KiB. Keys are 1–128 characters of `[A-Za-z0-9._:/@#+=-]`.

## Commands — records (planned)

```
brg msg publish --key KEY [--record FILE|-] [--external NAME] [--json]
brg inbox [--member ID] [--after ID] [--limit N] [--all] [--json]
brg inbox wait [--member ID] [--after ID] --timeout SECONDS [--json]
brg msg read ID [--json]
brg msg ack ID… [--json]
brg bus inspect [--json]
brg bus check [--json]
```

- **publish** validates the draft against the run's frozen topology (a member may publish to its parent or its children; an external caller to any member), checks `reply_to` IDs exist, measures artifacts, then numbers and writes the record under `bus/lock`. The same slot and key with the same draft returns the original record (`duplicate`); with a different draft it fails (`conflict`).
- **inbox** lists records addressed to a member that it hasn't acknowledged (`--all` includes acknowledged ones), oldest first, a page at a time, with a cursor for the next page. Reading never acknowledges.
- **inbox wait** returns as soon as the inbox has something after `--after`, or exits `timeout`. The timeout is required and capped.
- **ack** records consumption by the recipient's current incarnation. Acking twice is a no-op.
- **inspect** and **check** never write. `check` reports the first unreadable record or gap, stray temporary files and the run's age against `warn_age_days`/`max_age_days`.
- An unreadable record, a gap or an unreadable key/ack stops every mutation (`corrupt`); reads and inspection still work and say the store is incomplete.

## Task facts and recovery (planned)

Folds task records into per-task state: assigned, accepted or declined, result claimed, verified, released, cancel requested and confirmed, reopened as a new attempt, plus open questions and their decisions. Authority mutations (assign, verify, release, cancel-request, reopen, decision) must come from the recipient's parent's current incarnation, or from an external caller for a member with no parent; anything else is `fenced`. Releasing a task never frees a checkout while its writer may still be running. Recovery from a corrupt store preserves the original and continues in an explicitly reconciled linked run.

## Lifecycle and runtime binding (planned)

Every start validates fully before changing anything, and lifecycle changes are serialized by `runtime/lifecycle.lock`. Starting one member into a running deployment reuses the deployment's original binary, base environment and plugin values rather than the caller's, and refuses a different binary or changed configuration without touching the running deployment. With a bus, `up` creates `run.json` on first start, refuses a configuration that no longer matches it, records the runtime generation and gives each member start a new incarnation. Moving the run to another `brg` binary is an explicit rebind, allowed only when the run is stopped, no attention worker is running and `attention/inflight/` is empty; rollback is another rebind. The schema never changes within a run.

## Attention worker (planned)

One on-demand worker per run (`attention/worker.lock`) watches for records nobody has seen. An incarnation's `inbox` listing marks exactly the IDs it returned as observed; that is separate from acknowledgement. Records published after an observation start a new episode. For each episode the worker either wakes the member once (see below) or raises an alert. `publish`, `inbox`, `read` and `ack` start the worker if attention is pending and it isn't running; `brg bus service ensure` does so explicitly; `status`, `inspect` and `check` never do. The worker never creates a tmux session: publishing to a stopped deployment succeeds and reports `attention-unavailable`.

## Guarded wake (planned)

For a vendor, version and configuration that has passed qualification, the worker may paste one fixed reminder (never a payload) and press Enter. Before pasting it checks the pane's identity, state, input box and dialogs; before Enter it checks the input box holds exactly the reminder and no dialog appeared. It never clears input, forces, answers a dialog or presses Enter after a crash. At most one paste per episode. A busy member, a draft in the input box or a transient uncertain reading defers the wake without spending it; a draft also raises an alert, and the wake goes ahead automatically once the draft clears, or becomes human-required at the deadline. An unqualified vendor gets alerts only.

## Alerts (planned)

When a person is needed, the worker runs the deployment's `bus.alert` plugin action with fixed arguments `<alert-id> <member> <reason> <pending-count>` and nothing from any record. The action runs with the base environment, its own plugin's values, `BORGCHART_ACTOR=bus` and the deployment/run identity: no member identity and no other plugin's values. It is bounded in time and output. Exit zero means the alert was handed off, not that anyone saw it; the alert stays open until resolved. With no `bus.alert`, attention is `attention-unavailable` and shows in status only.

## Outcome codes

The bus commands share these exit codes; `--json` prints `{ok, code, outcome, message, …}`. They are separate from `brg send`'s codes.

| Code | Outcome | Meaning |
| --- | --- | --- |
| 0 | `ok` (or a more specific success such as `committed`, `duplicate`) | Done |
| 1 | `error` | I/O or unexpected error |
| 2 | `usage` | Bad arguments |
| 3 | `not-configured` | No `bus:` in the deployment, or the run hasn't started |
| 4 | `refused` | Not allowed from this context (e.g. `--external` inside a member) |
| 5 | `invalid` | The record or request failed validation |
| 6 | `conflict` | The operation key was already used for different content |
| 7 | `corrupt` | The store is unreadable or has a gap; mutations are stopped |
| 8 | `fenced` | A stale actor, incarnation or attempt |
| 9 | `timeout` | A bounded wait ended with nothing new |
| 10 | `not-found` | No such record |
