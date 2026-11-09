# The message bus

The bus keeps durable messages between members and outside callers. Publishing never types into a terminal. Message bodies and the paths they name are data to read, never text to execute or paste into a shell.

This is trusted, same-user operation: callers identify themselves through their environment, which anything running as you can set. Routing and fencing prevent mistakes; they don't isolate members or undo a shell's actions.

## Enable it

In `team.yaml`:

```yaml
bus: {}
# Optional alert action, validated but not called automatically:
# bus: {alert: {plugin: notify, action: alert}}
```

Bus members must use `vendor` posture. brg makes the instance's messages and notes writable for them; it grants no write access to its own files. Team files and launch settings are frozen while the instance is running. Apply changes with a full stop and start, after every member is positively stopped.

## Storage

One store lasts for the instance's whole life; message numbers continue across stops, starts and team-file changes. In `<state home>/<instance>/messages/`:

| Path | Contents |
| --- | --- |
| `store.json` | Format 3 and the instance ID |
| `lock` | Publication/acknowledgement lock |
| `records/<seq>.json` | Immutable messages, numbered from 1 without gaps; filenames use ten decimal digits |
| `acks/<id>.json` | Recipient acknowledgements, such as `m1.json` |

Empty `.agents`, `.aws`, `.codex` and `.git` directories (Codex's sandbox masks) are ignored; files, symlinks, non-empty directories with those names and other unexpected entries are refused.

Start records are in brg's own files outside messages: `members/<member>/incarnation.json`, with history in `members/<member>/incarnations/<id>.json`. Routing uses the member/parent entries in `start.json`. A stopped or replaced start cannot act for the member. Publication and acknowledgement recheck this under the message lock.

Directories must be real, owned by you and private (0700; files 0600). Immutable files use a flushed temporary sibling and a no-replace hard link; a crash may leave a `.tmp-*` file, which readers skip. Reads scan the store; there is no index or archiving.

## Callers and recipients

Inside a member use `"$BORGCHART_BIN"`, the brg that started it. Its instance, member and start ID come from `BORGCHART_INSTANCE`, `BORGCHART_MEMBER` and `BORGCHART_INCARNATION`. Outside a member every bus command requires `--instance ID`; inside one that option may only repeat its own instance.

- Members publish to their parent or children, by full member ID. Only a root may also publish to `external:NAME`.
- A copy's root members and their parent in the enclosing instance can also publish across their parent edge. Use the full `instance/member` address; a copy's roots receive their parent's address in `BORGCHART_PARENT`.
- Outside callers use `--external NAME` and may publish to any member. Their replies go to `external:NAME`. That flag is refused inside a member.
- An operator without an identity can read a named member's inbox and individual messages, and check the store, but cannot publish or ack.
- A member is addressable after its first start, including while stopped. A role name or another instance's member outside a parent edge is refused; there is no role queue.

## Draft and message

A draft has exactly these fields; `reply_to` is optional:

```json
{"to": "lead-q8m3", "body": "Report: notes/change-report.md\naction-needed: none", "reply_to": ["m1"]}
```

Every reference must name an existing message. The body is at most 65536 bytes, with newline and tab allowed but no other control characters. At most 32 references; the draft and stored message must fit within 131072 bytes. Unknown fields are refused.

brg adds identity and a UTC timestamp. For example:

```json
{"schema": 3, "id": "m2", "seq": 2, "at": "2026-11-08T00:00:00Z", "from": "implementer-x7k2", "to": "lead-q8m3", "reply_to": ["m1"], "body": "Report: notes/change-report.md"}
```

`from` is a member ID or `external:NAME`. Each publication makes a new message, even with an identical draft. Delegation, progress and verification belong in the pull request description and comments; briefs and reports can live in `$BORGCHART_NOTES`.

Across a parent edge, `from` and `to` are full addresses. Publication writes only the sender's store, after checking the edge. The recipient's next `inbox`, or each poll of `inbox wait`, reads the peer store under a read-only shared lock, releases it, and appends under its own lock. No command holds two stores' locks at once or writes a peer store.

An imported record gets its own local ID and an `origin`, for example `{"instance": "oncall-2a3b/dev-a7k2", "id": "m1"}`. The origin lives in the same immutable file as the message. Under the receiver's lock, imports skip origins already held, so concurrent readers and crash retries create one local record. Imported messages are never sent on again. A sender restart after publication does not invalidate its committed message.

Draft `reply_to` entries still name local messages. For a cross-edge publication, brg stores qualified references to their original instance and ID. On import, references map back to the receiver's own IDs where available; other references remain qualified. Same-instance messages keep bare member IDs and local references. Format 3 has no reader for older stores; create fresh instances when upgrading from format 2.

## Commands

`brg … --help` is the syntax authority. The examples below name a fictional instance; inside a member omit `--instance`. With `--json`, successes have `{ok: true, code: 0, outcome, …}`; errors have `{ok: false, code, outcome, message}`.

**Publish:** `brg msg publish [--record FILE|-] [--external NAME] [--json]` reads a draft from the file or, by default, stdin. It checks identity, routing, references and store health before writing.

```sh
brg msg publish --instance dev-team-k3f9 --external owner --record brief.json --json
```

```json
{"ok": true, "code": 0, "outcome": "committed", "id": "m1", "seq": 1, "at": "2026-11-08T00:00:00Z", "from": "external:owner", "to": "lead-q8m3"}
```

**Inbox:** `brg inbox [--member ID | --external NAME] [--full] [--json]` returns every unacked message, oldest first. A member defaults to itself; `--member` reads another member's inbox. Headers omit body and add `body_bytes` and `acked`; `--full` includes body instead of body_bytes. Reading never acknowledges.

```sh
brg inbox --instance dev-team-k3f9 --member lead-q8m3 --json
```

```json
{"ok": true, "code": 0, "outcome": "ok", "recipient": "lead-q8m3", "records": [{"schema": 3, "id": "m1", "seq": 1, "at": "2026-11-08T00:00:00Z", "from": "external:owner", "to": "lead-q8m3", "body_bytes": 5, "acked": false}], "unacked": 1, "total": 1, "store": "ok"}
```

**Read:** `brg msg read ID [--json]` shows one message in full, including an acknowledged one.

```sh
brg msg read m1 --instance dev-team-k3f9 --json
```

```json
{"ok": true, "code": 0, "outcome": "ok", "record": {"schema": 3, "id": "m1", "seq": 1, "at": "2026-11-08T00:00:00Z", "from": "external:owner", "to": "lead-q8m3", "body": "Hello", "acked": false}}
```

**Ack:** `brg msg ack ID… [--external NAME] [--json]` acknowledges only messages addressed to the caller. It validates every ID before writing any ack. Ack once you have taken responsibility, before waiting for more work.

```sh
"$BORGCHART_BIN" msg ack m1 --json
```

```json
{"ok": true, "code": 0, "outcome": "ok", "acked": [{"id": "m1", "outcome": "acked"}]}
```

Repeating an ack returns `already` for that ID.

**Wait:** `brg inbox wait [--external NAME] [--timeout SECONDS] [--full] [--json]` waits for your own inbox or the named outside recipient. Its success JSON is exactly the inbox object above.

```sh
"$BORGCHART_BIN" inbox wait --timeout 600 --full --json
```

- Timeout is 60–86400 seconds, default 86400; the 60 s floor matches the loop guard on every nonzero exit, including timeout. With no new message by the deadline it exits 9 (`timeout`).
- The highest sequence in the first readable scan is the start mark. A new unacked message to you above it returns at once.
- If unacked messages were already there, it returns after 60 seconds, only if some are still unacked. If all were acked meanwhile, it keeps waiting for a new one or the timeout.
- Errors, including missing identity, unreadable records and usage errors, return no sooner than 60 seconds after command start. Explicit help and signals are exempt.
- Each poll (250 ms) checks the current start, imports from parent-edge stores, and reads the next local message file, moving past other recipients. Peer lock waits are bounded. An existing next file that won't parse is an error, never skipped.
- A stopped or replaced member exits 8 (`fenced`), even without a publication. Stop waiting and tell the owner; never change identity variables to retry.

Ack everything you've taken on, then arm one wait: Claude with its Bash tool in background mode, Codex in the foreground (`--timeout 600`). On exit 9 wait again. Leaving messages unacked wakes you about once a minute.

**Check:** `brg bus check [--json]` reports gaps, unreadable files and stray temporaries, without writing.

```sh
brg bus check --instance dev-team-k3f9 --json
```

```json
{"ok": true, "code": 0, "outcome": "ok", "records": 2, "errors": [], "warnings": []}
```

## Damage and status

Absent or empty messages storage may be initialised. A foreign format/instance marker, unexpected layout, gap or unreadable record is damage: publication and acknowledgements refuse, and `bus check` reports why. Inbox can show readable messages with `store: incomplete`. Start refuses unreadable messages; stop and liveness still work without reading them. Delete needs `--force` for unacked local messages, undelivered cross-edge messages or unreadable messages, after every member is positively stopped. Checkouts are held only by running members or members whose stop is unknown; a stopped instance holds none.

A cross-edge message is delivered when the receiver's store holds its origin. The sender reads that receipt without writing anything; acknowledgements stay local to the receiver. Instance deletion lists all undelivered originals; member deletion lists that member's outgoing undelivered messages as well as its incoming unacked messages, including imports. A message to a deleted copy remains listed as "to a deleted copy" and needs `--force`. Busy, incomplete or unreadable receiving stores leave delivery unknown, with a note requiring `--force`; they never count as a receipt. Receipt reads release each peer lock before the sender takes its publication lock.

`brg status` and `-i` show positive unread (unacked) counts beside members. `--json` includes `unread`, including zero, on members with a readable bus. With no bus there are no counts; an unreadable store omits them and adds a short check note. Counts, newest sequence and lowest unacked sequence come from the same one-pass reader as inbox.

## Attention

The attention worker and watcher are not wired now; the lead nudges with `brg send`. By the brief's deadline it checks for a reply, checks status and nudges with `brg send <instance>/<member> 'check your inbox'`, or starts a stopped member.

The guarded typed wake exists in the library, uncalled. It uses a fixed payload-free reminder and vendor/version qualification, defers on drafts or uncertain screens, and rechecks the exact input before Enter. It never answers dialogs. It cannot eliminate a race with a person typing between the final check and Enter. See `src/wake.rs` and the opt-in live qualification tests.

## Outcome codes

These are separate from `brg send`'s codes.

| Code | Outcome | Meaning |
| --- | --- | --- |
| 0 | `ok`, `committed` | Success |
| 1 | `error` | I/O or unexpected error |
| 2 | `usage` | Bad arguments |
| 3 | `not-configured` | No enabled/initialised bus |
| 4 | `refused` | Caller context not allowed |
| 5 | `invalid` | Draft or request failed validation |
| 7 | `corrupt` | Unreadable or inconsistent store |
| 8 | `fenced` | Stopped or replaced member |
| 9 | `timeout` | Wait deadline passed |
| 10 | `not-found` | No such message |
