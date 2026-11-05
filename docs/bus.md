# The coordination bus

**Status: in progress.** This document is the shared contract for the bus work. The configuration, layout, caller context, envelope, record commands, task facts and outcome codes below exist in code. Each later section says *planned* until the change implementing it lands.

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
  root: /tmp/brg-runs-1000      # absolute (or ~/); holds one directory per run
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
| `bus/carried/<run>.json` | records | What `bus carry` brought into this run from an earlier one. Replaced on each carry. |
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

## Commands — records

```
brg msg publish --key KEY [--record FILE|-] [--external NAME] [--json]
brg inbox [--member ID | --external NAME] [--after ID] [--limit N] [--all] [--full] [--json]
brg inbox wait [--member ID | --external NAME] [--after ID] --timeout SECONDS [--limit N] [--full] [--json]
brg msg read ID [--json]
brg msg ack ID… [--external NAME] [--json]
brg bus inspect [--last N] [--json]
brg bus check [--json]
```

Every command takes the deployment as other commands do (`-f`, `$BORGCHART_FILE`, `./borgchart.yaml`); a team directory on its own has no bus (`not-configured`). With `--json`, each prints one JSON object `{ok, code, outcome, …}`; errors are `{ok: false, code, outcome, message}`. Exit codes are the [outcome codes](#outcome-codes).

**`msg publish`** reads a draft (from `--record FILE`, or stdin for `-` or no `--record`) and publishes it as the caller: a member from its environment, or `--external NAME`. An operator (neither) is `refused`. In order, it:

1. checks the key, the draft's shape, its kind's required facts and its recipient against the run's frozen topology (`invalid`), with an early check of the caller (below) to fail fast;
2. measures each artifact: a regular file (a symlink to one is followed) up to 256 MiB, recording `bytes` and `sha256` (`invalid` if missing, not a regular file, or too big; a FIFO or device is refused without opening it);
3. takes `runtime/generation.lock` shared (waiting up to 10 s, then `error`) and `bus/lock` exclusive;
4. holding both, checks the caller authoritatively: a member's run, membership and current, not-ended incarnation (`fenced`), and, once the run has a runtime generation, that this is the generation's binary (`refused`). Generations change only under `runtime/generation.lock` exclusive and incarnations only under `bus/lock`, so this holds until the write;
5. rescans the store (`corrupt` if anything is unreadable or missing);
6. if the caller's slot already used the key: the same draft and artifact contents return the original record (`duplicate`, repairing a missing key index entry), anything else is a `conflict`;
7. checks every `reply_to` ID (and a `verify`'s `facts.result`) names a record (`invalid`), and a task record or `decision` against the task fold ([below](#task-facts-and-recovery): `invalid`, `fenced` or `refused`), then writes the record and its key index entry.

```console
$ brg msg publish --external ops --key demo-1 --record demo.json --json
{"at":"2026-11-05T19:38:32.766Z","attention":"none","code":0,"id":"m1","key":"demo-1","kind":"note","ok":true,"outcome":"committed","seq":1,"to":"lead"}
$ brg msg publish --external ops --key demo-1 --record demo.json --json
{"at":"2026-11-05T19:38:32.766Z","attention":"none","code":0,"id":"m1","key":"demo-1","kind":"note","ok":true,"outcome":"duplicate","seq":1,"to":"lead"}
```

`attention` says whether anyone will be reminded; it is `none` until the attention worker exists.

**`inbox`** lists records addressed to one recipient, oldest first:

- With no `--member`/`--external`, a member lists its own inbox (its identity is checked as for publishing; a stale one is `fenced`). An operator must name one (`usage`).
- `--member ID` reads any member's inbox, read-only, from anyone. `--external NAME` reads `external:NAME`'s inbox, as that outside caller (refused inside a member).
- Unacknowledged records only, unless `--all`. Reading never acknowledges.
- `--limit` defaults to 20 (1–100). `next` is the ID to pass as `--after` for the next page, or `null` on the last page. Paging by ID is stable while records arrive.
- Records are headers (everything but `body` and `facts`, plus `body_bytes` and `acked`); `--full` includes the body and facts.

```json
{"ok": true, "code": 0, "outcome": "ok", "recipient": "lead",
 "records": [{"id": "m1", "seq": 1, "at": "…", "actor": {"type": "external", "name": "ops"},
              "key": "demo-1", "to": "lead", "kind": "note", "run": "demo-run-1", "schema": 1,
              "body_bytes": 29, "acked": false}],
 "next": null, "unacked": 1, "total": 1, "store": "ok"}
```

`store` is `incomplete` when the store has an unreadable record or a gap: the listing then shows what is readable.

**`inbox wait`** takes the same recipient options and checks every 250 ms until there is an unacknowledged record after `--after`, then prints the page as `inbox` does. `--timeout` (1–600 seconds) is required; when it passes with nothing new the command exits `timeout` (9).

**`msg read ID`** prints one record in full, with `acked`, to anyone. It never acknowledges, and never counts as the recipient having seen it. An unknown ID is `not-found`.

**`msg ack ID…`** records that the recipient has consumed records: a member for its own records, or `--external NAME` for `external:NAME`'s. Every ID is checked first (`not-found`; `refused` if any isn't addressed to the caller), then each gets `bus/acks/<id>.json`. Like publish, ack checks the caller again while holding both locks. Each result is `acked`, or `already` when it was acknowledged before, which changes nothing.

```json
{"ok": true, "code": 0, "outcome": "ok", "acked": [{"id": "m1", "outcome": "acked"}, {"id": "m2", "outcome": "already"}]}
```

**`bus inspect`** summarizes the run: run, root, deployment, `created_at`, `linked_from`, the frozen members, each recipient's `total` and `unacked`, the last `--last` (default 10) record headers as `recent`, and `store: {healthy, problems, strays}`.

**`bus check`** reports `errors` and `warnings` and the run's `age_days`:

- errors: an unreadable or misnamed record, key or ack; a record whose schema, run or number doesn't match; a gap; an ack for a missing record or by someone other than its recipient; a run older than `max_age_days`;
- warnings: stray `.tmp-*` files (left by a crash, safe to remove); a run older than `warn_age_days`; a run that continues another but hasn't carried from it yet, or whose carry was partial; the runs this one was carried into (also as `carried_into`), and any records added since the carry. Being past `max_age_days` is a warning rather than an error only when a complete carry (nothing skipped, a healthy store) covers every record the run has now.

It exits `ok` with warnings only, `corrupt` when the store has errors, and `error` when only the run's age does.

**Read-only.** `inbox`, `inbox wait`, `msg read`, `bus inspect` and `bus check` never create or change a file. Each read of the store holds `bus/lock` shared (opened read-only; with no lock file there is nothing to read yet), so it never sees a record without its key index entry or ack mid-write. An ack that is unreadable, has another schema or isn't by the record's recipient is a store problem and doesn't count as consumption.

**Corrupt store.** An unreadable record, key or ack, or a gap, stops every mutation (`corrupt`) until a person repairs the store or the work continues in a new run; reads and inspection still work and say the store is incomplete.

## Task facts and recovery

Task records are folded, in order, into one state per task (`src/bus/tasks.rs`). `brg msg publish` checks every task record and every `decision` against that fold under `bus/lock`, after the store scan and before the record is numbered, so a record is only ever checked against exactly the records it follows.

**Roles.** A task's *assignee* is the member its `assign` names; its *authority* is the assignee's parent, or any outside caller (`--external`) when the assignee is the team's root.

| From | Kinds | To |
| --- | --- | --- |
| the authority's current incarnation | `assign`, `verify`, `release`, `cancel-request`, `reopen` | the assignee |
| the assignee's current incarnation | `accept`, `decline`, `result`, `cancel-confirm` | the authority (its parent, or `external:<name>` for the root) |
| the recipient's authority | `decision` | the member that asked |

A task record from anyone else is `fenced`, and so is one from an incarnation that stopped being current while waiting for the lock. A record addressed to the wrong side is `invalid`. A `decision` can't go to `external:<name>` (an outside caller has no authority above it); answer an outside caller's question with a `note`.

**States.**

| State | Reached by | Next expected |
| --- | --- | --- |
| `assigned` | `assign` | assignee: `accept` or `decline` |
| `accepted` | `accept` | assignee: `result` |
| `declined` | `decline` | nothing for a first attempt nobody worked on; after an attempt with writers, authority: `release` or `reopen` |
| `result-claimed` | `result` (from `accepted` or `cancel-requested`) | authority: `verify` or `cancel-request` |
| `verified` / `rejected` | `verify` with `outcome` `accepted` / `rejected` | authority: `release` or `reopen` |
| `cancel-requested` | `cancel-request` (from `assigned`, `accepted`, `result-claimed`, `reopened`) | assignee: `cancel-confirm` (or a `result`) |
| `cancelled` | `cancel-confirm` | authority: `release` or `reopen` |
| `released` | `release` (from `verified`, `rejected`, `cancelled`, `declined`) | nothing |
| `reopened` | `reopen` (from `verified`, `rejected`, `cancelled`, `declined`): attempt + 1 | assignee: `accept` or `decline`; or the authority re-pins it with an `assign` of the new attempt |

Anything else is `invalid` (a `result` before an `accept`, a second `assign` of a task, a `release` of an accepted task, …). Further rules:

- **Attempts.** A new task's `assign` is attempt 1; `reopen` starts the next. A record whose `attempt` is older than the task's is `fenced`; a later one is `invalid`; a record without `attempt` means the current one (for `reopen`, the next).
- **Agreement.** `assign` needs `facts.agreement.path` to be a regular file that hashes to `facts.agreement.sha256` (`invalid` otherwise). The fold keeps the agreement, `scope`, `provenance`, `limits` and `checkout`.
- **One task at a time.** A member holds at most one *unresolved* task, and a checkout belongs to at most one. Resolved means `released`, or `declined` when no attempt of the task ever had a writer; a task declined after an earlier attempt was worked on keeps its member and checkout until a `release` finds its writers gone. An `assign` or `reopen` that would give the member a second unresolved task, or give a checkout to a second one, is `invalid`.
- **Re-pinning.** An `assign` of a reopened attempt may change or drop the task's `checkout` only once every incarnation that wrote for the task is `gone` (`refused` otherwise).
- **Verify** must name the current attempt's `result` record. A result is the assignee's claim; only a `verify` with `outcome: accepted` verifies it.
- **Release never frees a checkout while a writer may survive.** The incarnations that may be writing are every assignee incarnation that accepted, claimed a result or confirmed a cancellation for the task, the assignee's current incarnation (unless the task was declined), and `facts.writer_incarnation` if given. Each must be `gone`; `alive` or `unknown` is `refused`, whatever the records say. Until lifecycle's pane probe is wired in, an incarnation is `gone` only once its incarnation file records `ended_at` (it stopped through brg), and `unknown` otherwise. `facts.checkout`, if given, must be the task's.
- **Consumption is not acceptance.** Acknowledging an `assign` (`msg ack`) changes nothing; only `accept` does.
- **Questions.** A `question` is open until a `decision` names it in `reply_to`. A `decision` goes to the member that asked, from that member's authority, and must name at least one open question; naming an open question someone else asked is `invalid`. Open questions, like tasks, live in the records, so a replacement lead sees them.
- **Replacement.** State is per member, not per incarnation: a member's new incarnation carries on its predecessor's task (its old incarnation is `fenced`), and a new lead incarnation is the authority for tasks the old one assigned.
- **A damaged store.** Mutations stop (`corrupt`) as for any record. The fold reads the records from `m1` up to the first missing or unreadable one, so `task list` and `task show` still show what is known, marked `incomplete`. Records that don't apply (written by hand, or before these checks) are skipped and listed as `anomalies`.

## Commands — tasks

```
brg task list [--json]
brg task show TASK [--json]
brg bus carry --from RUN [--json]
```

**`task list`** and **`task show`** are read-only and need no identity. `list` gives every task (state, assignee, authority, attempt, agreement, scope, checkout, the record ID of each fact of the current attempt, writers with their liveness, and `next`) and every open question; `show` adds the task's full `history` and the open questions about it, and is `not-found` for an unknown task. Both report `through` (the last record folded) and `store` (`ok` or `incomplete`).

```console
$ brg task show t-1
task t-1: verified (attempt 1)
  assignee: helper
  authority: lead
  agreement: /work/agreement.md (sha256 901932a5…)
  scope: the config parser
  checkout: /work/checkout
  writer: helper-1 (unknown, current)
  m1  assign  #1  lead
  m2  accept  #1  helper
  m4  result  #1  helper
  m5  verify  #1  lead
  next: lead (release or reopen)
  open question m3 from helper
```

**`bus carry --from RUN`** continues work from an earlier run. Rotate *before* `max_age_days` (`bus check` warns from `warn_age_days`), or when a store is damaged: start a new run with `run: <new>` and `continues: <old>`, then run `brg bus carry --from <old>` against it. It:

1. requires `bus.continues` to be `RUN` (and the new `run.json`'s `linked_from`, if set, to name it) (`invalid` otherwise);
2. reads the old run read-only, under its `bus/lock` shared (as other reads do), writing nothing there. If its store is damaged it uses the records up to the first bad one and says so (outcome `partial`, `incomplete: true`, `through`);
3. publishes into the new run, as actor `bus`, one `note` per piece of pending work, to the original's recipient: the latest `assign` of each unresolved task, each open question, and each unacknowledged record not belonging to a resolved task. Each has `facts.carried_from: {run, id, sha256}` (the SHA-256 of the original record file), `facts.kind` and, for task records, `facts.task`. A recipient that isn't a member of the new run is `skipped`, which also makes the carry `partial` (`incomplete: true`);
4. uses the key `carry:<RUN>:<id>`, so running it again returns `duplicate` for everything already carried;
5. writes `bus/carried/<RUN>.json` in the new run.

Carried work is a pointer, not a transfer: the new run has no tasks until the authority assigns them again. `bus check` of the new run warns until a carry has run, and while the last one was partial. `bus check` of the old run (with the deployment pointed back at it) lists `carried_into` and warns about anything published there since; past `max_age_days` it stays an error unless a complete carry covers its latest record. Carrying again picks up the newer records (the ones already carried are `duplicate`).

```console
$ brg bus carry --from demo-run-1
m3 (open-question) -> lead: m1 committed
m7 (unacked) -> helper: m2 committed
carried 2 item(s) from `demo-run-1` into `demo-run-2`
```

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

## Alerts (planned)

When a person is needed, the worker runs the deployment's `bus.alert` plugin action with fixed arguments `<alert-id> <member> <reason> <pending-count>` and nothing from any record. Reasons: `blocked`, `draft`, `human-required`, `unqualified`, `overdue`, `unacked`, `worker-unavailable`, `test`. The action runs with the base environment, its own plugin's values as frozen at `up`, `BORGCHART_ACTOR=bus` and the deployment/run identity: no member identity, nothing from the caller's environment and no other plugin's values. It is bounded in time and output. Exit zero means the alert was handed off, not that anyone saw it; the alert stays open until resolved (`brg bus alert resolve <id>`). With no `bus.alert`, attention is `attention-unavailable` and shows in status only.

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
