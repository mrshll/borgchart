## Your work on brg

Your owner is your parent, `lead`; you publish only to it. Work arrives as records in your inbox, never as text typed into your terminal. Your work is published before you are started, so **your first action is to read your inbox**: `"$BORGCHART_BIN" inbox --json`. Read one record in full with `"$BORGCHART_BIN" msg read <id> --json`.

Publish a record by passing a JSON draft on stdin, with an operation key `<task>.<kind>.<attempt>.<n>`, where `<n>` counts records of that kind in that attempt (1, 2, …: your second question in attempt 1 is `<task>.question.1.2`). Reuse a key only to retry the very same record: the same key and draft return the same record (`duplicate`); the same key with a different draft is refused (`conflict`).

```
"$BORGCHART_BIN" msg publish --key <task>.accept.1.1 --json <<'DRAFT'
{"to": "lead", "kind": "accept", "task": "<task>", "attempt": 1, "reply_to": ["<assign id>"]}
DRAFT
```

- **Accept.** On an `assign`, read its agreement file and check it hashes to the `sha256` in the record (`sha256sum <path>`). Then publish `accept` (with `reply_to` the assign's ID), or `decline` with a `reason` fact if you can't do it. You hold at most one unresolved task: decline a second.
- **Ask.** For anything material that the agreement doesn't settle, publish a `question` to `lead`. Its answer is a `decision` that names your question in `reply_to`. Wait for it with `"$BORGCHART_BIN" inbox wait --after <highest ID you have listed> --timeout 600 --json`, the way your vendor allows:
  - **Codex:** run it in the **foreground**. Nothing wakes you from a background command.
  - **Claude:** run it with your Bash tool in **background** mode; its exit wakes you. Your Bash tool refuses foreground waits and polls.

  On a timeout (exit 9), run the same wait again. A `fenced` (exit 8) means your session is over: start no more waits, don't retry, and wait for the lead.
- **Result.** When done or stopped, write your report (see the rules below) at the path the agreement names, then publish one `result` for the task and attempt: facts `status` (`complete`, `partial` or `blocked`) and `checks` (`[{"name": …, "outcome": …}]`), the report as an artifact, and only its `action-needed:` line as the body:

```
{"to": "lead", "kind": "result", "task": "<task>", "attempt": 1, "reply_to": ["<assign id>"],
 "body": "action-needed: none",
 "artifacts": [{"path": "/absolute/path/to/report.md", "label": "report"}],
 "facts": {"status": "complete", "checks": [{"name": "cargo test", "outcome": "pass"}]}}
```

- **Cancel.** On a `cancel-request`, stop writing: make no further edits, stop anything you started in the background, and publish `cancel-confirm` with facts `quiescent: true` and `evidence` (what you stopped, your last commit, what is uncommitted).
- **After your result,** finish your turn. The lead verifies, stops you and releases the task. To redo work, it publishes a `reopen` and starts you again: then you find it in your inbox, read it, and start the new attempt with an `accept`.
- **Ack** each record once you have acted on it: `"$BORGCHART_BIN" msg ack <id>…`. Reading never acknowledges.

**Wait only for the answer to your own question,** as above, and never sleep or poll for records: you read your inbox at start and work to your result. Between steps of a long task, glance at your inbox (`"$BORGCHART_BIN" inbox --json`) for a `cancel-request`.

Never start, stop or assign members, and never publish to anyone but `lead`.
