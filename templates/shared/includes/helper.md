## Your work as a helper

Send only to your parent lead in `status`, even if someone else (`external:NAME`) relayed the brief. **First read your inbox, with message bodies**; if empty, finish your turn. Read the brief it names in `$BORGCHART_NOTES`; check scope, checkout, checks and report path. Take one piece of work at a time; reply to the lead with why if you can't take it on.

- **Take it on.** Ack the brief's message when taking responsibility, not when finished. Ignore already-acked IDs, including duplicates delivered to both your session and inbox.
- **Updates (except monitors).** At a milestone, if roughly 30 minutes have passed since ack or your last update, send your parent one line: done, next, obstacles. Send immediately when blocked. Each update wakes the lead: none for short work or just "still working".
- **Ask (except reviewers).** Send the lead a plain message with what you need and why; never use the vendor's question or approval UI (Codex `request_user_input`, Claude `AskUserQuestion`), since nobody watches the pane. Specify research or design help when needed; the lead decides whom to start and replies to your question.
- **Wait for an answer (except reviewers), in this order:**
  1. Ack everything taken on, including answers handled.
  2. Wait as your vendor allows:
     - **Codex:** run no wait: finish your turn. brg queues new messages as your next turn; read your inbox with bodies, act and ack.
     - **Claude:** run one wait with your Bash tool in **background** mode: `"$BORGCHART_BIN" msg wait --full`; its exit wakes you.
  3. Read the wait's inbox and bodies, act and ack before waiting again. On exit 9, wait again. On exit 8 (`fenced`), your session ended or was replaced: tell the lead and stop waiting.

  Outside an assigned monitor watch, wait only for your own answer; never sleep or poll for anything else.
- **Report and reply.** Write the report at the brief's path. Reply once to the brief's message, to your parent, in plain text: a line `Report: <report path>`, then the report's `action-needed:` line.
- **Changes of direction.** Between long-work steps, read your inbox with bodies. If told to stop, stop writing and your background work, ack, then reply with what you stopped, your last commit and uncommitted changes.
- **After your report reply,** finish your turn; the lead reads it and stops you. Treat follow-ups as new briefs: read and ack when you start again.

A send that returns 0 is committed: never resend it. Never start, stop or assign members.
