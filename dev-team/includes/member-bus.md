## Your work on brg

Your owner is your parent, `lead`; you publish only to it. Work arrives as records in your inbox, never as text typed into your terminal. Your work is published before you are started, so **your first action is to read your inbox**: `"$BORGCHART_BIN" inbox --json`. Read one record in full with `"$BORGCHART_BIN" msg read <id> --json`.

Publish a record by passing a JSON draft on stdin, with an operation key `<task>.<kind>.<attempt>.<n>`, where `<n>` counts records of that kind in that attempt (1, 2, …: your second question in attempt 1 is `<task>.question.1.2`). Reuse a key only to retry the very same record: the same key and draft return the same record (`duplicate`); the same key with a different draft is refused (`conflict`).

```
"$BORGCHART_BIN" msg publish --key <task>.accept.1.1 --json <<'DRAFT'
{"to": "lead", "kind": "accept", "task": "<task>", "attempt": 1, "reply_to": ["<assign id>"]}
DRAFT
```

- **Accept.** On an `assign`, read its agreement file and check it hashes to the `sha256` in the record (`sha256sum <path>`). Then publish `accept` (with `reply_to` the assign's ID), or `decline` with a `reason` fact if you can't do it. You hold at most one unresolved task: decline a second.
- **Ask.** For anything material that the agreement doesn't settle, publish a `question` to `lead`. Its answer is a `decision` that names your question in `reply_to`.
- **Result.** When done or stopped, write your report (see the rules below) at the path the agreement names, then publish one `result` for the task and attempt: facts `status` (`complete`, `partial` or `blocked`) and `checks` (`[{"name": …, "outcome": …}]`), the report as an artifact, and only its `action-needed:` line as the body:

```
{"to": "lead", "kind": "result", "task": "<task>", "attempt": 1, "reply_to": ["<assign id>"],
 "body": "action-needed: none",
 "artifacts": [{"path": "/absolute/path/to/report.md", "label": "report"}],
 "facts": {"status": "complete", "checks": [{"name": "cargo test", "outcome": "pass"}]}}
```

- **Cancel.** On a `cancel-request`, stop writing: make no further edits, stop anything you started in the background (except your inbox wait), and publish `cancel-confirm` with facts `quiescent: true` and `evidence` (what you stopped, your last commit, what is uncommitted).
- **After.** `verify`, `release` and `reopen` come from the lead. Ack them; on a `reopen`, read it and start the new attempt with an `accept`.
- **Ack** each record once you have acted on it: `"$BORGCHART_BIN" msg ack <id>…`. Reading never acknowledges.

Never start, stop or assign members, and never publish to anyone but `lead`.
