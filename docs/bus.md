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

**Pinned binary.** Each runtime generation keeps its own copy of the `brg` binary under the run, and every window, shim and worker of that generation runs the copy, so rebuilding or reinstalling the original changes nothing. Members get `BORGCHART_BIN` (the pinned copy's absolute path) and `brg`/`borgchart` shims first on their `PATH` that run it. Every mutating bus command checks that it *is* the current generation's binary (by SHA-256) and is `refused` otherwise; read-only commands work from any compatible `brg`. Outside callers should call the pinned path too; `brg run show` prints it.

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

## Lifecycle and runtime binding

**Checks before changes.** `brg up` works out everything a start needs (plugin values, `auth: api_key` keys, the run's identity and generation, whether the deployment is already running) before it writes anything. A refused start leaves the state directory and the run directory as they were.

**Serialized.** `up`, `up --member`, `down`, `down --member` and `run rebind` take a lock in brg's state directory (`<state>/lifecycle.lock`) and then, with a bus, `runtime/lifecycle.lock`. `up` holds them while it writes files and starts windows, and releases them before it waits for `locked` gates and Claude's folder trust, so members can already use the bus and another `up --member` isn't held up. Anything that wait changes (the keys that accept folder trust, the trust marker, stopping a `locked` member that fails its check) takes the locks again and first checks that the window still runs the very pane (ID and PID) this `up` launched; a member stopped or replaced meanwhile is left alone (`replaced while starting`), and for a `locked` member that counts as a failed start. A `locked` member whose pane can't be read, at start or when it must be stopped, is stopped by its window name while the locks are held, so it never runs without its gate. Two `up --member helper` at once start it once; the other reports `already running`.

**Starting into a running deployment.** When the deployment's session exists and its `deployment.json` is there, `up` (with or without `--member`) joins it instead of starting afresh. It uses the binary, base environment and plugin values recorded at the first `up`, never your current shell's: nothing from your `PATH`, `TMPDIR` or other variables reaches the new member, and you don't need the plugin secrets set again. The one exception is `auth: api_key`: the vendor's key (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) is read from your environment at each start, for the members being started, and passed to them. It is not frozen and brg never stores it (it is in the member's launch file only until the window reads it); a start without it is refused before anything is written. It doesn't rewrite `deployment.json`, plugin value files or other members' files. It refuses, changing nothing, when:

- it is a different binary from the one that started the deployment (by SHA-256): the message names the binary to use instead;
- any configuration input changed since the deployment started (the same snapshot as `run.json`): the message names the changed inputs. Undo the change, or stop the team and start it again (with a bus, in a new run).

**Pinned runtime.** Every window, plugin shim, gate hook and plugin process runs one binary, and each member gets it as `BORGCHART_BIN` plus `brg` and `borgchart` shims first on its `PATH`. With a bus that is the generation's pinned copy; without one, the `brg` that started the deployment. A plugin command may not be called `brg` or `borgchart`.

**Run identity.** With a bus, the run's first `up` writes `run.json`: the deployment ID, schema, frozen topology and configuration snapshot, and with `continues:` a link `{run, root, reason: "continues"}` to the earlier run, whose `run.json` must exist under the same root. Later starts refuse a run whose `run.json` names another deployment, or whose configuration digest differs (start a new `run:`). The schema never changes within a run.

**Generations.** The first `up` also records generation 1 (`reason: start`): the binary's path and SHA-256, its pinned copy at `runtime/bin/<sha256>/brg` (0700, written once, hash checked after copying), the state directory and the tmux socket. A start from a binary, state directory or socket that differs from the latest generation's is refused: *rebind needed*.

**Incarnations.** Each member start is a new incarnation, with an ID `<member>.<generation>.<UTC yyyymmddhhmmssmmm>.<4 hex>` that is unique in the run. Its current file is written before the window starts, then again with the pane's identity (`%N` pane ID, PID, window, session, and the tmux server's absolute socket path) and the vendor version measured at this start; the same record then goes into the history (`runtime/incarnations/<member>/<id>.json`). The member gets `BORGCHART_RUN` and `BORGCHART_INCARNATION`. `down` and `down --member` mark the incarnations they stop ended (`ended_at`, `ended_reason: "stopped"`); a window tmux fails to create is ended as `start-failed`. Every change to an incarnation file is made holding `bus/lock` (briefly, after the lifecycle locks), so a bus mutation that checks a member's incarnation under that lock sees either the old record or the new one.

**Stopping.** `up` records the tmux socket name and the server's absolute socket path. `down` and `down --member` address that server directly, so a caller's `TMUX_TMPDIR` can't point them elsewhere, and refuse a caller whose `BORGCHART_TMUX_SOCKET` names another socket. They change nothing unless the server answers: an incarnation is marked ended, and secrets and launch files removed, only after brg stopped its pane or the server showed it absent. A `down` that stops the whole session (or finds it gone) records the confirmed stop in `deployment.json`, so a later `down` after the server has exited just says `not running`. If the server really is gone without a `down` (for example after a reboot), `brg up` and then `brg down` clear it.

**Liveness.** brg asks the incarnation's recorded tmux server (by its absolute socket path) for every pane. The incarnation is `alive` when that list has its pane with the same ID and PID, not dead; `gone` when the server answers and the pane is absent, dead or another process, or when the incarnation was ended by a confirmed stop; `unknown` otherwise: no pane recorded, tmux missing, or a server that can't be reached (not running, a removed or hidden socket, no permission). An unreachable server is never taken as proof that anything stopped; treat `unknown` as possibly alive.

**`brg run show [--json]`** prints the run, its configuration digest and link, the current generation with its pinned binary, the exact command that stops the deployment with that binary, and each member's incarnation and liveness. It never writes. `--json` adds every generation; `pinned` and `stop` are top-level fields.

**`brg run rebind --reason TEXT [--json]`** moves the run to the `brg` running it (and its state directory and tmux socket). It needs positive evidence that the run is stopped, and is `refused` otherwise: the deployment's recorded tmux server must answer without the deployment's session (or the last `down` must have recorded a confirmed stop), and every member's current incarnation must be `gone`; an incarnation that is `alive`, `unknown` or unreadable refuses. It is also refused while `runtime/generation.lock` is held (a bus command or the attention worker), or while `attention/inflight/` has anything in it. It writes generation n+1 (with `previous`) and pins the new binary; old pins, generations and every bus record stay. Rebinding to the same binary, state directory and socket changes nothing (`unchanged`). The run's identity and schema never change; a different configuration needs a new run.

**Upgrading brg.** Checkpoint the team (let members reach a stopping point), stop it with the run's own binary, rebind with the new one, and start again:

```
brg run show                          # prints "stop with: '<pinned>' --file … down"
'<pinned>' --file borgchart.yaml down
/path/to/new/brg run rebind --reason "upgrade"
/path/to/new/brg up
```

Rolling back is the same rebind run with the old binary; its pinned copy is still there.

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
