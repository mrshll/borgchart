//! The bus help pages use the implementation's limits and exit codes.

pub fn root() -> String {
    "Teams and instances:
  `brg create <team> <folder>` starts an instance. `start`, `stop`, `delete`
  and `status` act on it; members are addressed as <instance>/<member>.

Inside a member:
  Call the brg that started you as \"$BORGCHART_BIN\". Your instance and
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
outside recipient's inbox. Outside a member, pass --instance ID.

The tree shares one store and one set of message IDs. --member accepts a full
address anywhere in the tree, or a bare ID in your own or --instance instance.
Reading never acknowledges. Use `msg read ID` for an older message and
`msg ack ID...` once you have taken messages on, then `msg wait`.
Progress and reports belong in your notes and GitHub.

A member's loop:
  \"$BORGCHART_BIN\" msg list --full
  \"$BORGCHART_BIN\" msg ack m4 m5
  \"$BORGCHART_BIN\" msg wait --timeout 600
"
    .into()
}

pub fn wait() -> String {
    "Waits for your own inbox, or external:NAME with --external NAME.
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
        "Outside callers must name --external NAME. Members use their launch identity
and may send only to their parent or children. Across a copy's parent edge,
name the full instance/member address (a copy's roots find their parent's in
BORGCHART_PARENT). The tree shares one store and message IDs; typed and locked
delivery work across edges too. Only root members of the tree's root instance
may address external:NAME. --from asserts a parent route within one instance,
not authorship. --reply-to takes plain existing IDs such as m1.
This command takes no --instance; name the target as <instance>/<member>.

Ordinary messages commit before optional typing. Exit 0 means committed even
when delivery is stored or uncertain; never automatically republish. Interactive
recipients are inbox-only even with --allow-unknown. Working recipients may
absorb input in the current turn. Typing never acknowledges: use `msg ack ID`
when taking responsibility, and ignore an already-acked duplicate from `msg wait`.

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
