//! Long `--help` text for the bus commands.
//!
//! Limits and exit codes come from the constants the commands use.

use super::outcome::Code;
use super::record::{
    MAX_ARTIFACTS, MAX_BODY_BYTES, MAX_FACTS_BYTES, MAX_KEY_BYTES, MAX_LABEL_BYTES,
    MAX_RECORD_BYTES, MAX_REFS,
};
use super::store;

const WIDTH: usize = 80;

/// `line`, wrapped at [`WIDTH`] with later lines indented by `indent`.
fn hang(line: &str, indent: usize) -> String {
    let lead = line.len() - line.trim_start().len();
    let mut out = String::new();
    let mut cur = line[..lead].to_string();
    for word in line[lead..].split(' ') {
        let fresh = cur.trim().is_empty();
        if !fresh && cur.len() + 1 + word.len() > WIDTH {
            out.push_str(cur.trim_end());
            out.push('\n');
            cur = " ".repeat(indent);
        } else if !fresh {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    out.push_str(cur.trim_end());
    out.push('\n');
    out
}

/// `name (exit)  meaning` rows.
fn outcomes(rows: &[(&str, Code, &str)]) -> String {
    rows.iter()
        .map(|(name, code, meaning)| {
            let head = format!("{name} ({})", code.exit());
            hang(&format!("  {head:<18} {meaning}"), 21)
        })
        .collect()
}

fn n(code: Code) -> &'static str {
    code.as_str()
}

/// `brg --help`: teams and instances, and how a member finds brg and its
/// instance.
pub fn root() -> String {
    "Teams and instances:
  A team is a folder holding team.yaml; an instance is the team running in one
  working folder: `brg create <team> <folder>` makes one (`dev-team-k3f9`), and
  `start`, `stop` and `delete` act on it. Members are `<role>-<suffix>`
  (`lead-q8m3`), addressed as `<instance>/<member>`; a bare name is a role, and
  only `create` takes one. brg's configuration is ~/.config/borgchart/config.yaml
  (or $BORGCHART_CONFIG).

Inside a member:
  A member calls brg as \"$BORGCHART_BIN\" (the brg that started it, whatever
  is on PATH). Its instance is already set in BORGCHART_INSTANCE, so it names
  other members by ID alone and passes no --instance, and its identity comes
  from its environment. Read next:
    \"$BORGCHART_BIN\" msg publish --help   records: kinds, facts, keys, outcomes
    \"$BORGCHART_BIN\" inbox --help         your inbox and the --after cursor
    \"$BORGCHART_BIN\" inbox wait --help    waiting for new records
"
    .to_string()
}

/// `brg msg publish --help`.
pub fn publish() -> String {
    let mut out = String::new();
    out.push_str(
        "A member publishes as itself; an outside caller names itself with --external.
A member may write to its parent or its children; the team's root may also write
to external:<name>; an outside caller may write to any member.

Draft (one JSON object; any other field is refused):
",
    );
    out.push_str(&format!(
        "  to         a member, or external:<name>
  kind       note (a plain message)
  reply_to   IDs of records this answers or follows (m<number>), at most {MAX_REFS}
  body       text, at most {MAX_BODY_BYTES} bytes
  artifacts  [{{\"path\": \"/absolute/file\", \"label\": \"text\"}}], at most {MAX_ARTIFACTS}
             existing regular files of up to {} MiB (brg records bytes and
             sha256); `label` is optional, at most {MAX_LABEL_BYTES} bytes
  facts      an object of structured details, at most {MAX_FACTS_BYTES} bytes
Text may contain newlines and tabs, no other control characters. The whole draft
is at most {MAX_RECORD_BYTES} bytes.
",
        store::MAX_ARTIFACT_BYTES / (1024 * 1024)
    ));
    out.push_str(&format!(
        "
Kinds:
  note            a plain message.

Operation key (--key): 1-{MAX_KEY_BYTES} characters of [A-Za-z0-9._:/@#+=-], scoped
to the publisher (a member across its restarts, or an --external name). A key
must identify one intended record: reuse it only to retry that record.
  same key, same draft and artifact bytes: `duplicate`, the original record
  same key, anything different: `conflict` (6), nothing written
A new record needs a new key. After a crash or an unclear exit, publish the
same draft with the same key again.

Example (a member `helper` reports to its parent `lead`;
paste the lines between the rules as they are):
--------
\"$BORGCHART_BIN\" msg publish --key parser-report-1 <<'EOF'
{{\"to\": \"lead\", \"kind\": \"note\",
 \"reply_to\": [\"m1\"],
 \"body\": \"Parser done; all tests pass.\",
 \"facts\": {{\"status\": \"complete\", \"checks\": [{{\"name\": \"tests\", \"outcome\": \"pass\"}}]}}}}
EOF
--------
Prints: committed: m3 (note to lead)

Outcomes (with --json: {{ok, code, outcome, ...}}; the exit status is the code):
"
    ));
    out.push_str(&outcomes(&[
        (
            "committed",
            Code::Ok,
            "published; prints the new record's ID",
        ),
        (
            "duplicate",
            Code::Ok,
            "this key already published this draft; the original ID",
        ),
        (
            n(Code::Usage),
            Code::Usage,
            "a bad option, such as an --external name",
        ),
        (
            n(Code::NotConfigured),
            Code::NotConfigured,
            "no bus in the team, or the instance hasn't started with one",
        ),
        (
            n(Code::Refused),
            Code::Refused,
            "not from here: no identity, --external inside a member, another message format",
        ),
        (
            n(Code::Invalid),
            Code::Invalid,
            "a bad draft, recipient or reference; nothing written",
        ),
        (
            n(Code::Conflict),
            Code::Conflict,
            "the key was used for a different draft",
        ),
        (
            n(Code::Corrupt),
            Code::Corrupt,
            "the store is damaged; nothing can be published (`brg bus check`)",
        ),
        (
            n(Code::Fenced),
            Code::Fenced,
            "stale: a stopped or replaced member",
        ),
        (n(Code::Error), Code::Error, "an I/O or unexpected error"),
    ]));
    out
}

/// `brg inbox --help`.
pub fn inbox() -> String {
    format!(
        "Lists one inbox, oldest first: your own as a member, unacknowledged records
only (--all for every record). Reading never acknowledges; `msg ack ID...`
does, and only the recipient may. Acknowledged records stay readable (--all,
msg read).

Cursor: record IDs are m<number>, in publish order, compared as numbers (m10
comes after m9). --after ID lists only records after it. A page holds --limit
records (default {}, at most {}); `next` in --json, or \"more after ID\" in
text, is the --after for the next page (null on the last page).

A member's loop:
  \"$BORGCHART_BIN\" inbox --full                        read what is waiting
  \"$BORGCHART_BIN\" msg ack m4 m5                       once handled
  \"$BORGCHART_BIN\" inbox wait --after m5 --timeout 600  then wait for more

Outcomes are as for `inbox wait --help`, without timeout.
",
        store::DEFAULT_PAGE,
        store::MAX_PAGE
    )
}

/// `brg inbox wait --help`.
pub fn wait() -> String {
    let mut out = format!(
        "Checks every {} ms until the inbox has an unacknowledged record after --after
(without --after: any unacknowledged record), then prints that page as `inbox`
does. It returns at once if one is already there, so acknowledge what you
have handled or pass the last ID you have seen as --after. Waiting never
acknowledges. --timeout (1-{} seconds) is required.

With --background, only your own member session may wait. It compares records
with what your own `inbox` listings have shown this session, and prints only
the unseen, unacknowledged IDs (space-separated), or {{ok, code, outcome, ids,
count}} with --json. It never writes, marks records as shown, or acknowledges.
--timeout is optional (60-86400 seconds, default 86400). --after, --member,
--external, --limit and --full are refused. The highest sequence in the first
readable snapshot is the start mark. An unseen record above it returns at
once, including every unseen ID, old and new, in the output. If unseen records
exist only at or below the mark, keep polling and return them at 60 seconds
since command start; a later arrival can return sooner. Errors, including
usage errors and missing identity, also wait for that minimum. This applies
to normal outcomes: explicit --help/-h and signals (SIGTERM, SIGKILL) can end
the command immediately. Damaged or missing observations cannot bypass the
minimum for old records. A replaced session is always fenced:
stop re-arming. List your inbox and follow next with `inbox --after <next>`
until it is null, work, then re-arm one background wait after each turn.
Earlier unacknowledged records are fine; the wait needs no cursor bookkeeping.

Outcomes (with --json: {{ok, code, outcome, ...}}; the exit status is the code):
",
        store::POLL.as_millis(),
        store::MAX_WAIT_SECS
    );
    out.push_str(&outcomes(&[
        (
            n(Code::Ok),
            Code::Ok,
            "new records; read them, then wait --after the last one (with --background: list your inbox, work, then re-arm)",
        ),
        (
            n(Code::Timeout),
            Code::Timeout,
            "nothing new in time; wait again with the same --after (with --background: re-arm without a cursor)",
        ),
        (
            n(Code::Fenced),
            Code::Fenced,
            "this member was stopped or replaced: stop",
        ),
        (
            n(Code::Usage),
            Code::Usage,
            "--timeout, --after or --limit out of range, no inbox named outside a member, or --background without own identity / with incompatible flags",
        ),
        (
            n(Code::NotConfigured),
            Code::NotConfigured,
            "no bus in the team, or the instance hasn't started with one",
        ),
        (
            n(Code::Refused),
            Code::Refused,
            "--external inside a member, or another message format",
        ),
        (
            n(Code::Invalid),
            Code::Invalid,
            "--member names no member of the instance",
        ),
        (
            n(Code::Corrupt),
            Code::Corrupt,
            "the instance's storage is damaged (`brg bus check`)",
        ),
        (n(Code::Error), Code::Error, "an I/O or unexpected error"),
    ]));
    out
}
