//! The bus help pages use the implementation's limits and exit codes.

pub fn root() -> String {
    "Templates and teams:
  `brg create <template> <folder>` starts a team. `start`, `stop`, `delete`
  and `status` act on it; members are addressed as <team>/<member>.

Inside a member:
  Call the brg that started you as \"$BORGCHART_BIN\". Your team and
  identity come from your environment. Read next:
    \"$BORGCHART_BIN\" msg send --help
    \"$BORGCHART_BIN\" msg list --help
    \"$BORGCHART_BIN\" msg wait --help
"
    .into()
}

pub fn list() -> String {
    "Lists every unacknowledged message to you, oldest first. --full includes
bodies. --member ID reads another member's inbox; --external NAME reads that
outside recipient's inbox. Without team context (BORGCHART_TEAM,
BORGCHART_MEMBER, BORGCHART_INCARNATION, BORGCHART_ROLE, BORGCHART_NOTES and
BORGCHART_PARENT all unset or empty), the inbox defaults to external:owner.
A plugin process with only BORGCHART_TEAM set must name an inbox.
Outside a member, pass --team ID.

The tree shares one store and one set of message IDs. --member accepts a full
address anywhere in the tree, or a bare ID in your own team or the one --team
names.
Reading never acknowledges. Use `msg read ID` for an older message and
`msg ack ID...` once you have taken messages on, then `msg wait` (Claude). A
Codex member runs no wait: it finishes its turn, and brg delivers each new
message into its session.
Progress and reports belong in your notes and GitHub.

A Claude member's loop:
  \"$BORGCHART_BIN\" msg list --full
  \"$BORGCHART_BIN\" msg ack m4 m5
  \"$BORGCHART_BIN\" msg wait --timeout 600
"
    .into()
}

pub fn wait() -> String {
    "Waits for your own inbox, or external:NAME with --external NAME.
Without team context (BORGCHART_TEAM, BORGCHART_MEMBER,
BORGCHART_INCARNATION, BORGCHART_ROLE, BORGCHART_NOTES and BORGCHART_PARENT
all unset or empty), defaults to external:owner. A plugin process with only
BORGCHART_TEAM set must name --external NAME.
--timeout is 60-86400 seconds, default 86400. --full includes message bodies;
--json prints the same inbox object as `brg msg list --json`.
The 60 s floor applies to nonzero exits except fenced, timeout included.

The first readable scan marks the newest message. A new unacked message to
you returns at once. Messages already present return after 60 seconds only if
still unacked; if all were acked meanwhile, waiting continues. A new message
can return sooner. Ack everything you have taken on before waiting.
Each later poll reads the next message in the tree store and checks your
current session. Store lock attempts are bounded by the timeout.
Errors, including usage errors, wait at least 60 seconds too; fenced, explicit
help and signals may end immediately.

Outcomes: ok (0) prints the inbox; timeout (9) means nothing new in time.
fenced (8) means your session ended or was replaced: stop. Member liveness is
checked about once a second; an unknown result keeps waiting. Other outcomes are
usage (2), not-configured (3), refused (4), invalid (5), corrupt (7),
not-found (10) and error (1).
"
    .into()
}

pub fn send() -> String {
    format!(
        "Without team context (BORGCHART_TEAM, BORGCHART_MEMBER,
BORGCHART_INCARNATION, BORGCHART_ROLE, BORGCHART_NOTES and BORGCHART_PARENT
all unset or empty), outside callers default to external:owner; --external NAME
overrides it. A plugin process with only BORGCHART_TEAM set must name
--external NAME. Members use their launch identity and can send to any addressable
member of their tree. If either member is restricted, the route must connect
a parent and child. Use the full team/member address to reach another team
in the tree. A child team's roots find their parent's address in
BORGCHART_PARENT. The tree shares one store and message IDs. Queue, typing and
restricted delivery depend on the recipient. Only root members of the tree's
root team may address external:NAME. --from asserts a parent route within a team,
not authorship. --reply-to takes plain existing IDs such as m1.
This command takes no --team; name the target as <team>/<member>.

Parentless unrestricted members of different top-level teams may send to each
other by full address. Child-team roots, non-root or restricted members, --from
and --allow-unknown cannot use this route. One record belongs to the recipient
tree; the sender keeps its printed ID and store. --reply-to m1 names a message
from that peer to you in your own store, saved as <your-tree>:m1. The sender
cannot read or ack the other tree's records; reply when taking a request on.
The envelope identifies another tree's root member, not the owner. These checks
prevent mistakes, not hostile processes running as the same user. This adds no
other-tree --team, lifecycle, read, list, ack or secrets access; status, logs
and attach already reach every tree.

Ordinary messages commit before delivery. Exit 0 means committed even when
stored, blocked or uncertain; never automatically republish. Every Codex
recipient gets its next turn through `codex queue`, after any current turn,
using its recorded base environment and current session. Interactive and
--allow-unknown have no effect for Codex. Stopped or replaced starts are
never queued; queue never gets --resume. A start under 30 s old gets up to 5
s for its session. A nonzero queue exit leaves ordinary messages stored; no
answer within 10 s is uncertain: may have been queued; do not resend. Queue
leaves UI pickers unanswered and drafts alone. A recognized blocked pane
reports blocked with its rule and an attach command: its queued turn waits
for a person.

Interactive Claude recipients are inbox-only, even with --allow-unknown.
Other Claude recipients get guarded typing, possibly during the current turn.
Delivery never acknowledges: use `msg ack ID` when taking responsibility,
and ignore an already-acked duplicate from `msg wait`.

Restricted Codex needs a live start with a pane, active gate marker and current
session before recording. Restricted Claude requires a live, guarded, empty
composer and the typing budget. Restricted records need no ack and have no inbox
recovery. Postcommit failures are recorded-not-delivered or uncertain, return
nonzero and include the ID; retry may duplicate recorded or submitted input.

TEXT of - reads stdin. Body limit: {} bytes; repeat --reply-to for up to 32
existing record IDs. Controls other than newline/tab and leading / or ! refuse.
Claude typing uses at most 3 envelope lines and 800 bytes, plus a screen-row
budget. Larger ordinary Claude messages get a one-line pointer to `msg read ID`;
restricted Claude messages outside that budget refuse before committing. Drafts,
dialogs, copy mode and unverified pasted text never get Enter or clearing.

Outcomes: committed (0); usage (2); unaddressable/restricted not running
(3); restricted Claude dialog/copy mode (4); validation, fence, storage or
restricted input failure (5). --json includes ok, code, message, and a
committed id, store, delivery and reason when present. Delivery: typed,
queued, stored, blocked, uncertain, recorded-not-delivered. Blocked means
committed but waiting for a person; do not resend. Status keeps the first
blocked or uncertain event until its message is acked or delivery succeeds.",
        crate::runtime::MAX_SEND_BYTES
    )
}
