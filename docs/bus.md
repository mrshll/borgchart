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
| 3 | `not-configured` | No `bus:` in the deployment, or the run hasn't started |
| 4 | `refused` | Not allowed from this context (e.g. `--external` inside a member, or not the run's binary) |
| 5 | `invalid` | The record or request failed validation |
| 6 | `conflict` | The operation key was already used for different content |
| 7 | `corrupt` | The store is unreadable or has a gap; mutations are stopped |
| 8 | `fenced` | A stale run, incarnation or attempt, or not the recipient's authority |
| 9 | `timeout` | A bounded wait ended with nothing new |
| 10 | `not-found` | No such record |
