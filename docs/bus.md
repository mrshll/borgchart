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
| `bus/carried/<run>.json` | records | What `bus carry` brought into this run from an earlier one. Replaced on each carry. |
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

The stored record adds `schema`, `run` (the internal storage it was written in; commands never show it), `id` (`m<seq>`), `seq`, `at` (UTC, RFC 3339), `actor` (`{"type":"member","member":…,"incarnation":…}`, `{"type":"external","name":…}` or `{"type":"bus"}`) and `key`, and replaces each artifact with its measured `bytes` and `sha256`.

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

Every command takes the deployment as other commands do (`-f`, `$BORGCHART_FILE`, `./borgchart.yaml`); a team directory on its own has no bus, and a team whose storage `brg up` hasn't created yet ("team `oncall` hasn't been started yet") is `not-configured`; neither creates anything. Storage that has runs but no `current` is damaged, not new: every command reports it (`corrupt`) rather than starting over. Writers (`msg publish`, `msg ack`) follow a rollover as described under `msg publish`. Every command, reads included, refuses (`refused`, 4) a team whose messages are in another format, with the restart command shown under [Versions](#caller-context). Output names teams and members only: errors about damaged storage (an unreadable member session record, an unlistable record area, a storage folder others could write) give the reason without storage paths. With `--json`, each prints one JSON object `{ok, code, outcome, …}`; errors are `{ok: false, code, outcome, message}`. Exit codes are the [outcome codes](#outcome-codes).

**`msg publish`** reads a draft (from `--record FILE`, or stdin for `-` or no `--record`) and publishes it as the caller: a member from its environment, or `--external NAME`. An operator (neither) is `refused`. In order, it:

1. checks the key, the draft's shape, its kind's required facts and its recipient against the team's frozen topology (`invalid`), with an early check of the caller (below) to fail fast;
2. measures each artifact: a regular file (a symlink to one is followed) up to 256 MiB, recording `bytes` and `sha256` (`invalid` if missing, not a regular file, or too big; a FIFO or device is refused without opening it);
3. takes `bus/lock` exclusive (creating the store first if needed). If the storage was retired meanwhile (the team rolled over), it releases the lock, finds the team's current storage and locks that instead, so nothing lands in storage that has already been carried from. A draft with references (`reply_to`, a `verify`'s `facts.result`) is then refused (`fenced`: "team `oncall` was restarted, so `m1` may now be a different message; list the inbox again"), since IDs restart in the new storage;
4. holding it, checks the caller authoritatively (a member's membership, that it is from the team's current start, and its current, not-ended incarnation: `fenced`) and the recipient again. Incarnations change only under `bus/lock`, so this holds until the write;
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
              "key": "demo-1", "to": "lead", "kind": "note", "schema": 1,
              "body_bytes": 29, "acked": false}],
 "next": null, "unacked": 1, "total": 1, "store": "ok"}
```

`store` is `incomplete` when the store has an unreadable record or a gap: the listing then shows what is readable.

**`inbox wait`** takes the same recipient options and checks every 250 ms until there is an unacknowledged record after `--after`, then prints the page as `inbox` does. `--timeout` (1–600 seconds) is required; when it passes with nothing new the command exits `timeout` (9), even while a writer holds `bus/lock` (it never waits for the lock past its deadline). Each check finds the team's current storage again and re-checks the inbox, the caller and the message format against it, so a restart while waiting is followed. If the team was restarted and `--after` was given, the cursor no longer applies: the wait is `fenced` and asks you to list the inbox again.

**`msg read ID`** prints one record in full, with `acked`, to anyone. It never acknowledges, and never counts as the recipient having seen it. An unknown ID is `not-found`.

**`msg ack ID…`** records that the recipient has consumed records: a member for its own records, or `--external NAME` for `external:NAME`'s. Every ID is checked first (`not-found`; `refused` if any isn't addressed to the caller), then each gets `bus/acks/<id>.json`. Like publish, ack checks the caller again while holding `bus/lock`. An ack that had to follow a restart to get the lock is `fenced` ("list the inbox again") rather than acknowledging whatever now has that ID. Each result is `acked`, or `already` when it was acknowledged before, which changes nothing.

```json
{"ok": true, "code": 0, "outcome": "ok", "acked": [{"id": "m1", "outcome": "acked"}, {"id": "m2", "outcome": "already"}]}
```

**`bus inspect`** summarizes the team's messages: `team`, `deployment`, `created_at` (when the current storage was created), the frozen members, each recipient's `total` and `unacked`, the last `--last` (default 10) record headers as `recent`, and `store: {healthy, problems, strays}`.

**`bus check`** reports `errors` and `warnings` and the storage's `age_days`:

- errors: an unreadable or misnamed record, key or ack; a record whose schema, storage or number doesn't match; a gap; an ack for a missing record or by someone other than its recipient; storage older than `max_age_days`;
- warnings: stray `.tmp-*` files (left by a crash, safe to remove; shown relative to the storage); storage older than `warn_age_days`. Both age messages give the restart command that rolls the storage over, in a form that works from where you are. Also: storage linked from an earlier run that hasn't carried from it yet, or whose carry was partial; the runs this one was carried into (also as `carried_into`), and any records added since the carry.

It exits `ok` with warnings only, `corrupt` when the store has errors, and `error` when only the storage's age does.

```console
$ brg bus check
warning: team `oncall`'s message storage is 8.0 days old (warn_age_days 7); it rolls over at the first restart after 14 days: brg down && brg up
ok: 2 record(s), storage 8.0 days old
```

**Read-only.** `inbox`, `inbox wait`, `msg read`, `bus inspect` and `bus check` never create or change a file. Each read of the store holds `bus/lock` shared (opened read-only; with no lock file there is nothing to read yet), so it never sees a record without its key index entry or ack mid-write. An ack that is unreadable, has another schema or isn't by the record's recipient is a store problem and doesn't count as consumption.

**Corrupt store.** An unreadable record, key or ack, or a gap, stops every mutation (`corrupt`) until a person repairs the store; reads and inspection still work and say the store is incomplete.

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

**`bus carry --from RUN`** continues work from the earlier run the team's current storage is linked from (its `run.json`'s `linked_from`, written when the storage rolls over). The lifecycle calls the same carry (`carry::carry_at_rollover`) while it rolls over, holding the old run's `bus/lock`; until that is wired in, a rollover leaves the work behind and this command carries it. It:

1. requires the current run's `linked_from` to name `RUN` (`invalid` otherwise, including when it is linked from nothing);
2. reads the old run read-only, under its `bus/lock` shared (as other reads do), writing nothing there. If its store is damaged it uses the records up to the first bad one and says so (outcome `partial`, `incomplete: true`, `through`);
3. publishes into the new run, as actor `bus`, one `note` per piece of pending work, to the original's recipient: the latest `assign` of each unresolved task, each open question, and each unacknowledged record not belonging to a resolved task. Each has `facts.carried_from: {run, id, sha256}` (the SHA-256 of the original record file), `facts.kind` and, for task records, `facts.task`. A recipient that isn't a member of the new run is `skipped`, which also makes the carry `partial` (`incomplete: true`);
4. uses the key `carry:<RUN>:<id>`, so running it again returns `duplicate` for everything already carried;
5. writes `bus/carried/<RUN>.json` in the new run.

Carried work is a pointer, not a transfer: the new run has no tasks until the authority assigns them again. `bus check` of the new run warns until a carry has run, and while the last one was partial. (`bus check` only reads the current run, so the old run's `carried_into` warnings and its past-`max_age_days` exemption apply only if it were current again.) Carrying again picks up the newer records (the ones already carried are `duplicate`).

```console
$ brg bus carry --from demo-run-1
m3 (open-question) -> lead: m1 committed
m7 (unacked) -> helper: m2 committed
carried 2 item(s) from `demo-run-1` into `demo-run-2`
```

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

`status` prints the same tree with each member's live state and details, then the plugin processes. A member with no window is `not started` when it has never run since the team's storage began (with a bus: no incarnation in the current storage, so every member of a team that never started, or just rolled over, is `not started`; without one: no start recorded since the team last started afresh; state from an earlier brg recorded none, so its members read `stopped`), printed as `up` prints it: `not started (start: false)` for a `start: false` member. One that ran and ended (or whose start record can't be read) is `stopped`. While the team is running (at least one member has a live window), a `stopped` `start: true` member is an alert: printed `stopped (should be running)`, with `"expected_running": true` in `--json`, and shown like `exited` in `status -i`. After a whole-team `down`, ended members are plain `stopped`. `status --json` is the same rows as before, in tree order, with `parent`, `model`, `expected_running` when set and, with a bus, `liveness`.

```
$ brg down --member helper
helper: stopped
$ brg status
oncall
└─ lead        claude opus high      idle
   ├─ helper   codex example-model   stopped (should be running)
   └─ reviewer claude sonnet medium  not started (start: false)
```

**Starting into a running team.** When the team's session exists and its `deployment.json` is there, `up` (with or without `--member`) joins it instead of starting afresh. It uses the base environment and plugin values recorded at the first `up`, never your current shell's: nothing from your `PATH`, `TMPDIR` or other variables reaches the new member, and you don't need the plugin secrets set again. The one exception is `auth: api_key`: the vendor's key (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) is read from your environment at each start, for the members being started, and passed to them. It is not frozen and brg never stores it (it is in the member's launch file only until the window reads it); a start without it is refused before anything is written. It doesn't rewrite plugin value files or other members' files. With a bus it doesn't rewrite `deployment.json` either; without one, `deployment.json` records each member's first start since the team started afresh (its recorded environment and plugin values stay as they were). If any configuration input changed since the team started (see Configuration), it refuses, changing nothing, and names what changed in team and member terms:

```
brg: oncall/helper's instruction file changed since the team started; restart the team to apply it: brg down && brg up
```

The restart command is the bare form in the team's folder and names `--file` elsewhere. A full `up` of a running team with changed files refuses the same way.

**brg isn't pinned.** Every window, plugin shim, gate hook and plugin process runs the `brg` that started it (`_exec`, `_run` and `_gate` use the running binary, as in v0): a member started later with `up --member` uses the `brg` that ran that command. Each member gets that brg's absolute path as `BORGCHART_BIN`, so its own brg calls (`"$BORGCHART_BIN" inbox`, …) don't depend on what its shell puts first on `PATH`; a bare `brg` is whatever is installed there. Plugin programs get `BORGCHART_BIN` too: the `brg` that started the team. So start a team from a stable install, not a build that `cargo clean` can remove. A plugin command may not be called `brg` or `borgchart`: a granted command's shim comes first on the member's `PATH`, so it would shadow the installed brg and the member's message commands would call the plugin.

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

## Guarded wake

For a vendor, version and configuration that has passed qualification, the worker may paste one fixed reminder and press Enter:

```text
brg: new records in your inbox. Run `brg inbox` at your next stopping point.
```

Nothing from any record is ever typed. One attempt (`src/wake.rs`) holds the member's send lock (waiting at most 2 s, otherwise it defers as `locked`) and goes:

1. **Identity.** The pane is looked up on the tmux server the incarnation recorded (its socket path, or a socket name) and must be the incarnation's own: same session, window, `pane_id` and `pane_pid`, and not dead. Otherwise `not-running`, nothing typed.
2. **State.** The state rules `status` uses run on the same capture. A prompt gives `blocked:<rule>` (the same rule ID `status` shows) and nothing is typed. A running turn defers as `busy`.
3. **Input box.** The input box must read as empty (below), and the pane must not be in tmux copy mode. A draft defers as `draft`; anything unreadable defers as `uncertain`.
4. **Intent.** The worker records the attempt; if that fails nothing is typed.
5. **Paste.** The reminder goes in as one bracketed paste.
6. **Before Enter.** The pane is re-read, for up to 3 s, until no prompt is showing and the box holds exactly the reminder: the same characters and spaces on each line, the cursor right after the last character (so a typed trailing space shows), and a line break only where the vendor had to wrap (the next word could not have fitted in the pane's width). Otherwise `human-required` and Enter is not pressed.
7. **Enter**, then, for up to 3 s, a check that the box reads as empty. Only an empty box, seen, counts as `submitted`; the reminder still there, other text, a pane that is gone or a screen that can't be read is `human-required`.

It never clears input, forces, answers a dialog, presses Enter after a crash or tries a second time: `human-required` always waits for a person. Attached clients don't matter; a resize that leaves the screen mid-redraw just defers. A busy member, a draft or an uncertain reading defers without spending the episode's one paste; a draft also raises an alert, the wake goes ahead once the draft clears, and the worker makes it human-required at the deadline. A member at a prompt gets an alert. An unqualified vendor, version or configuration gets alerts only.

**Reading the input box** (`src/composer.rs`) works on a styled capture (`capture-pane -e`) and anchors on each vendor's layout at the bottom of the screen:

- *Claude Code:* the lines between the last two horizontal rules, starting with `❯`; later lines are indented. A grey `❯` means a turn is running (the window title doesn't change during a turn in this version). The footer shows `-- INSERT --` in vim insert mode. Claude shows no placeholder, and typed spaces look like nothing, so a blank box reads as empty only in insert mode with the cursor right after the `❯`. In vim normal mode a box holding one space looks exactly like an empty one, so Claude in normal mode always defers as uncertain.
- *Codex:* the lines above one blank line and the status line ending in `Vim: Insert` or `Vim: Normal`, starting with a bold `›`. Codex draws its dim placeholder (`Ask Codex to do anything`) only while the box is empty, so the placeholder reads as empty and a blank box without it (typed spaces) as a draft.

Any other text in the box is a draft: typed text, recalled history, a pasted image's tag, a suggestion. So is a box of more than one line, even if its visible lines are blank (the rest may be scrolled out of view). A pane under 20 rows, or a screen that doesn't match the layout (a popup, a dialog, a resize in progress), reads as uncertain.

**Qualification.** Typing is allowed only for these exact combinations, each tested with the real CLI by `tests/qualify.rs` (empty, single-line, multiline, wrapped and scrolled drafts; vim insert and normal mode; history recall; a draft that clears and one that stays; a short pane; whitespace-only drafts; the Enter check on a pasted, never-submitted reminder, plain, wrapped at 60 columns, with a typed trailing space and with a space removed; a resize and two attached clients, one in control mode, during attempts; a submission in each vim mode, where Claude in normal mode now defers):

| Vendor | `--version` | Input mode | Other settings |
| --- | --- | --- | --- |
| Claude Code | `2.1.318 (Claude Code)` | `editorMode: vim` | `promptSuggestionEnabled: false` |
| Codex | `codex-cli 0.165.0` | `[tui] vim_mode_default = true` | |

The input mode and settings are measured from the files the vendor reads (Claude: `~/.claude.json`, `~/.claude/settings.json`, the project's `.claude/settings{,.local}.json`, a `--settings` argument and managed settings; Codex: `$CODEX_HOME/config.toml` or `~/.codex/config.toml`, the project's `.codex/config.toml` and `-c tui.…` arguments). A missing settings file counts as unset. One that exists but can't be read or parsed, a value of the wrong type, a setting written in a form the reader doesn't follow (an inline `tui = {…}` table, a `-c` override other than `tui.vim_mode_default=true|false`, the key under another table), a `--profile`, or anything not in the table means alerts only. To qualify another version, run the harness (it needs a folder both CLIs already trust, and makes two real submissions per vendor) and add a row only if every case passes:

```text
BORGCHART_QUALIFY=1 cargo test --locked --test qualify -- --ignored --nocapture --test-threads=1
```

**What is not guaranteed.** A person typing into the member between the last check and Enter can have their text submitted along with the reminder; nothing here can close that gap. A typed newline exactly where the vendor would have wrapped, or extra spaces at a wrap point, can't be seen in a capture and would pass the Enter check. There is no warning before typing.

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
