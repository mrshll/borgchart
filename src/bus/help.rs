//! The bus help pages use the implementation's limits and exit codes.

pub fn root() -> String {
    "Teams and instances:
  `brg create <team> <folder>` starts an instance. `start`, `stop`, `delete`
  and `status` act on it; members are addressed as <instance>/<member>.

Inside a member:
  Call the brg that started you as \"$BORGCHART_BIN\". Your instance and
  identity come from your environment. Read next:
    \"$BORGCHART_BIN\" send --help
    \"$BORGCHART_BIN\" inbox --help
    \"$BORGCHART_BIN\" inbox wait --help
"
    .into()
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
The 60 s floor matches the loop guard on every nonzero exit, timeout included.

The first readable scan marks the newest message. A new unacked message to
you returns at once. Messages already present return after 60 seconds only if
still unacked; if all were acked meanwhile, waiting continues. A new message
can return sooner. Ack everything you have taken on before waiting.
Each later poll reads only the next message and checks your current session.
Errors, including usage errors, wait at least 60 seconds too; explicit help
and signals may end immediately.

Outcomes: ok (0) prints the inbox; timeout (9) means nothing new in time.
fenced (8) means your session ended or was replaced: stop. Other outcomes are
usage (2), not-configured (3), refused (4), invalid (5), corrupt (7),
not-found (10) and error (1).
"
    .into()
}

pub fn send() -> String {
    format!(
        "Outside callers must name --external NAME. Members use their launch identity
and may send along parent/child routes in their own instance. Only roots may
address external:NAME. --from asserts the sender's parent route, not authorship.

Ordinary messages commit before optional typing. Exit 0 means committed even
when delivery is stored or uncertain; never automatically republish. Interactive
recipients are inbox-only even with --force. Working recipients may absorb input
in the current turn. Typing never acknowledges: use `msg ack ID` when taking
responsibility, and ignore an already-acked duplicate ID from an inbox wait.

Locked recipients require a live, guarded, empty composer before committing.
Their records do not require ack and have no inbox recovery. Failures after
commit include the ID; retry may duplicate recorded or submitted input.

TEXT of - reads stdin. Body limit: {} bytes; repeat --reply-to for up to 32
existing record IDs. Controls other than newline/tab and leading / or ! refuse.
Typing uses a smaller conservative screen-verification budget: 1024 envelope
bytes and at most ten estimated lines. Larger ordinary messages remain stored;
locked messages outside that budget refuse before committing. Drafts, dialogs,
copy mode and unverified pasted text never get an automatic Enter or clearing.

Outcomes: committed (0); usage (2); unaddressable/locked not running (3);
locked dialog or copy mode (4); validation, fence, storage or locked input
failure (5). --json includes ok, code, message, and a committed id plus delivery
and reason when present. Delivery: typed, stored, uncertain, recorded-not-typed.",
        crate::runtime::MAX_SEND_BYTES
    )
}
