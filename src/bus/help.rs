//! The bus help pages use the implementation's limits and exit codes.

use super::record::{MAX_BODY_BYTES, MAX_RECORD_BYTES, MAX_REFS};

pub fn root() -> String {
    "Teams and instances:
  `brg create <team> <folder>` starts an instance. `start`, `stop`, `delete`
  and `status` act on it; members are addressed as <instance>/<member>.

Inside a member:
  Call the brg that started you as \"$BORGCHART_BIN\". Your instance and
  identity come from your environment. Read next:
    \"$BORGCHART_BIN\" msg publish --help
    \"$BORGCHART_BIN\" inbox --help
    \"$BORGCHART_BIN\" inbox wait --help
"
    .into()
}

pub fn publish() -> String {
    format!(
        "A member publishes as itself to its parent or children. A root may also send
outside the instance to external:<name>. An outside caller uses --external
NAME and may send to any member.

Draft (one JSON object; other fields are refused):
  to         a member ID, or external:<name>
  body       text, at most {MAX_BODY_BYTES} bytes
  reply_to   optional IDs this answers or follows, at most {MAX_REFS}
Every reference must name an existing message. Text may contain newlines and
tabs, with no other control characters. The entire draft is at most
{MAX_RECORD_BYTES} bytes. Each publish creates a new message.

Example (a helper writes to its parent lead):
--------
\"$BORGCHART_BIN\" msg publish <<'EOF'
{{\"to\": \"lead\", \"body\": \"The parser is ready.\"}}
EOF
--------
Prints: committed: m1 (to lead)

Outcomes (also reported by --json): committed (0), usage (2),
not-configured (3), refused (4), invalid (5), corrupt (7), fenced (8),
not-found (10), error (1). Fenced means this member was stopped or replaced."
    )
}

pub fn inbox() -> String {
    "Lists every unacknowledged message to you, oldest first. --full includes
bodies. --member ID reads another member's inbox; --external NAME reads that
outside recipient's inbox. Outside a member, pass --instance ID.

Reading never acknowledges. Use `msg read ID` for an older message and
`msg ack ID...` once you have taken messages on, then `inbox wait`.
Progress and reports belong in your notes and GitHub.

A member's loop:
  \"$BORGCHART_BIN\" inbox --full
  \"$BORGCHART_BIN\" msg ack m4 m5
  \"$BORGCHART_BIN\" inbox wait --timeout 600
"
    .into()
}

pub fn wait() -> String {
    "Waits for your own inbox, or external:NAME with --external NAME.
--timeout is 60-86400 seconds, default 86400. --full includes message bodies;
--json prints the same inbox object as `brg inbox --json`.
The 60 s floor applies to nonzero exits except fenced, timeout included.

The first readable scan marks the newest message. A new unacked message to
you returns at once. Messages already present return after 60 seconds only if
still unacked; if all were acked meanwhile, waiting continues. A new message
can return sooner. Ack everything you have taken on before waiting.
Each later poll reads only the next message and checks your current session.
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
