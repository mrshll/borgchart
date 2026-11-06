## Running the team on brg

**Start members only after their work is waiting.** Publish the `assign` first, then start the member with `"$BORGCHART_BIN" up --member <slot>`. Its first action is to read its inbox, so nobody ever has to wake a member to give it its first task. Check it came up with `"$BORGCHART_BIN" status`: if it is `blocked` (for example at a folder-trust screen), tell the owner (see "Talking to the owner") and leave it for a person.

**Assign.** Write the agreement to a file in your private directory: the task, scope and non-goals, the branch and checkout, the report path (also in the private directory), the deadline, and the teammates involved. Then publish the assignment, with the agreement's SHA-256:

```
"$BORGCHART_BIN" msg publish --key <task>.assign.1 --json <<'DRAFT'
{"to": "implementer-a", "kind": "assign", "task": "<task>", "attempt": 1,
 "body": "<one paragraph: what to do>",
 "facts": {"agreement": {"path": "/absolute/path/to/agreement.md", "sha256": "<sha256sum of it>"},
           "scope": "<files and limits>", "checkout": "/absolute/path/to/that/slot's/checkout"}}
DRAFT
```

Task IDs are short and unique (`[a-z0-9][a-z0-9._-]*`, e.g. `pr-41-impl`). Give a `checkout` fact only for a task that writes one: reviewer and companion tasks name no checkout.

**Follow it.** Expect `accept` or `decline`, then `result`. `"$BORGCHART_BIN" task show <task>` shows its state, records and next step; `"$BORGCHART_BIN" msg read <id> --json` shows one record. Answer a `question` with a `decision` naming it in `reply_to`. To redo work, publish `reopen` with a `reason` (a new attempt); to stop it, `cancel-request` with a `reason`, then wait for `cancel-confirm`. Ack each record you have acted on.

**Verify, then release.** After checking a result yourself, publish `verify` with facts `result` (the result record's ID) and `outcome` (`accepted` or `rejected`). To free a member and its checkout:

1. stop it: `"$BORGCHART_BIN" down --member <slot>`;
2. check `"$BORGCHART_BIN" task show <task>` lists its writer as `gone` (brg refuses a release while a writer may be alive);
3. publish `release` with an `evidence` fact (what you checked), and for a writing task its `checkout`.

Give a checkout to another task only after its release is recorded.

**Review.** For each pull request that's ready for review:

1. move the review checkout to the pull request's head commit (`git -C <review checkout> fetch` then `checkout --detach <head>`); it is yours to move only while no reviewer runs;
2. write the review agreement (pull request number, base and head commits, report path) and publish its `assign` to `reviewer`;
3. start it: `"$BORGCHART_BIN" up --member reviewer`;
4. read its result and report, verify, stop it and release the task as above.

The Codex reviewer never waits for records in the background, so nothing has to wake it: it reads its inbox at start, reviews, publishes its result and goes idle. If `reviewer` can't start or hits a usage limit, stop it and assign the same review to `reviewer-claude` instead (a new task), and say so in the pull request.
