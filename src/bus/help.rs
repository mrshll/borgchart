//! Long `--help` text for the bus commands.
//!
//! An agent in a team learns the bus from these pages, so the parts that
//! can drift are built from the code: the record kinds and their facts come
//! from [`kind_help`] (an exhaustive match over [`Kind`], whose required
//! facts a test checks against [`store::check_facts`]), and limits and exit
//! codes from the constants and [`Code`] the commands use.

use super::outcome::Code;
use super::record::{
    Kind, MAX_ARTIFACTS, MAX_BODY_BYTES, MAX_FACTS_BYTES, MAX_KEY_BYTES, MAX_LABEL_BYTES,
    MAX_RECORD_BYTES, MAX_REFS,
};
use super::store;

/// One fact a kind reads from `facts`.
pub struct Fact {
    pub name: &'static str,
    /// What the value is, as the help prints it.
    pub shape: &'static str,
    /// A valid value, as JSON (for tests and examples).
    pub sample: &'static str,
}

/// What the help says about one kind.
pub struct KindHelp {
    /// Who sends it to whom.
    pub route: &'static str,
    pub when: &'static str,
    pub required: &'static [Fact],
    pub optional: &'static [Fact],
}

/// Every record kind, in the order the help lists them.
pub const KINDS: [Kind; 12] = [
    Kind::Note,
    Kind::Question,
    Kind::Decision,
    Kind::Assign,
    Kind::Accept,
    Kind::Decline,
    Kind::Result,
    Kind::Verify,
    Kind::CancelRequest,
    Kind::CancelConfirm,
    Kind::Reopen,
    Kind::Release,
];

const REASON: Fact = Fact {
    name: "reason",
    shape: "text",
    sample: r#""out of scope""#,
};
const EVIDENCE: Fact = Fact {
    name: "evidence",
    shape: "text",
    sample: r#""stopped with brg down --member helper""#,
};
const CHECKS: Fact = Fact {
    name: "checks",
    shape: "[{name, outcome}]",
    sample: r#"[{"name": "tests", "outcome": "pass"}]"#,
};

/// The help for one kind. Required facts must be exactly what
/// [`store::check_facts`] requires (tested below).
pub fn kind_help(kind: Kind) -> KindHelp {
    match kind {
        Kind::Note => KindHelp {
            route: "",
            when: "a plain message.",
            required: &[],
            optional: &[],
        },
        Kind::Question => KindHelp {
            route: "",
            when: "ask; the body is the question, `task` optional. Open until a decision answers it.",
            required: &[],
            optional: &[],
        },
        Kind::Decision => KindHelp {
            route: "authority -> asker",
            when: "answer the open questions named in `reply_to`.",
            required: &[],
            optional: &[],
        },
        Kind::Assign => KindHelp {
            route: "authority -> assignee",
            when: "give a member a task; needs `attempt` (1, or the attempt a reopen started).",
            required: &[
                Fact {
                    name: "agreement",
                    shape: "{path: absolute file, sha256: its 64-hex digest}",
                    sample: r#"{"path": "/work/agreement.md", "sha256": "0000000000000000000000000000000000000000000000000000000000000000"}"#,
                },
                Fact {
                    name: "scope",
                    shape: "text",
                    sample: r#""the parser module""#,
                },
            ],
            optional: &[
                Fact {
                    name: "provenance",
                    shape: "text",
                    sample: r#""approved in planning""#,
                },
                Fact {
                    name: "limits",
                    shape: "text",
                    sample: r#""no new dependencies""#,
                },
                Fact {
                    name: "checkout",
                    shape: "absolute path the assignee writes",
                    sample: r#""/work/checkout""#,
                },
            ],
        },
        Kind::Accept => KindHelp {
            route: "assignee -> authority",
            when: "take the task (reply_to the assign).",
            required: &[],
            optional: &[],
        },
        Kind::Decline => KindHelp {
            route: "assignee -> authority",
            when: "refuse the task.",
            required: &[REASON],
            optional: &[],
        },
        Kind::Result => KindHelp {
            route: "assignee -> authority",
            when: "claim an outcome (a claim, not verification).",
            required: &[Fact {
                name: "status",
                shape: "complete|partial|blocked",
                sample: r#""complete""#,
            }],
            optional: &[CHECKS],
        },
        Kind::Verify => KindHelp {
            route: "authority -> assignee",
            when: "judge the current attempt's result.",
            required: &[
                Fact {
                    name: "result",
                    shape: "the result's record ID",
                    sample: r#""m3""#,
                },
                Fact {
                    name: "outcome",
                    shape: "accepted|rejected",
                    sample: r#""accepted""#,
                },
            ],
            optional: &[CHECKS],
        },
        Kind::CancelRequest => KindHelp {
            route: "authority -> assignee",
            when: "ask the assignee to stop.",
            required: &[REASON],
            optional: &[],
        },
        Kind::CancelConfirm => KindHelp {
            route: "assignee -> authority",
            when: "confirm it has stopped writing.",
            required: &[
                Fact {
                    name: "quiescent",
                    shape: "true",
                    sample: "true",
                },
                EVIDENCE,
            ],
            optional: &[],
        },
        Kind::Reopen => KindHelp {
            route: "authority -> assignee",
            when: "start the next attempt after verify, cancel or decline.",
            required: &[REASON],
            optional: &[],
        },
        Kind::Release => KindHelp {
            route: "authority -> assignee",
            when: "close the task, freeing its member and checkout. Refused (4) while a writer may still be running (see `task --help`).",
            required: &[EVIDENCE],
            optional: &[
                Fact {
                    name: "checkout",
                    shape: "must be the task's",
                    sample: r#""/work/checkout""#,
                },
                Fact {
                    name: "writer_incarnation",
                    shape: "another incarnation that may have written",
                    sample: r#""helper.20261105190000000.ab12""#,
                },
            ],
        },
    }
}

fn facts_line(facts: &[Fact]) -> String {
    facts
        .iter()
        .map(|f| format!("{} ({})", f.name, f.shape))
        .collect::<Vec<_>>()
        .join("; ")
}

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

/// `brg --help`: how a member finds brg and its team.
pub fn root() -> String {
    "Inside a team:
  A member calls brg as \"$BORGCHART_BIN\" (the brg that started it, whatever
  is on PATH). Its deployment is already set in BORGCHART_FILE, so it passes no
  --file, and its identity comes from its environment. Read next:
    \"$BORGCHART_BIN\" msg publish --help   records: kinds, facts, keys, outcomes
    \"$BORGCHART_BIN\" inbox --help         your inbox and the --after cursor
    \"$BORGCHART_BIN\" inbox wait --help    waiting for new records
    \"$BORGCHART_BIN\" task --help          the task lifecycle
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
  kind       one of the kinds below
  task       task ID, [a-z0-9][a-z0-9._-]{{0,63}}; required for task kinds
  attempt    the task's attempt, from 1. Required for assign; otherwise it
             defaults to the current attempt (reopen: the next one)
  reply_to   IDs of records this answers or follows (m<number>), at most {MAX_REFS}
  body       text, at most {MAX_BODY_BYTES} bytes
  artifacts  [{{\"path\": \"/absolute/file\", \"label\": \"text\"}}], at most {MAX_ARTIFACTS}
             existing regular files of up to {} MiB (brg records bytes and
             sha256); `label` is optional, at most {MAX_LABEL_BYTES} bytes
  facts      an object of kind-specific details (below), at most {MAX_FACTS_BYTES} bytes;
             keys a kind doesn't name are kept and ignored
Text may contain newlines and tabs, no other control characters. The whole draft
is at most {MAX_RECORD_BYTES} bytes.
",
        store::MAX_ARTIFACT_BYTES / (1024 * 1024)
    ));
    let first_task = KINDS
        .iter()
        .find(|k| k.is_task_kind())
        .map_or("", |k| k.as_str());
    out.push_str(&format!(
        "
Kinds. From {first_task} on, a kind belongs to a task and needs `task`. The
authority is the assignee's parent, or an outside caller when the assignee is
the team's root.
"
    ));
    for kind in KINDS {
        let h = kind_help(kind);
        let head = if h.route.is_empty() {
            String::new()
        } else {
            format!("{}: ", h.route)
        };
        out.push_str(&hang(
            &format!("  {:<15} {head}{}", kind.as_str(), h.when),
            18,
        ));
        if !h.required.is_empty() {
            out.push_str(&hang(
                &format!("{:18}facts: {}", "", facts_line(h.required)),
                25,
            ));
        }
        if !h.optional.is_empty() {
            out.push_str(&hang(
                &format!("{:18}optional: {}", "", facts_line(h.optional)),
                28,
            ));
        }
    }
    out.push_str(&format!(
        "
Operation key (--key): 1-{MAX_KEY_BYTES} characters of [A-Za-z0-9._:/@#+=-], scoped
to the publisher (a member across its restarts, or an --external name). A key
must identify one intended record: reuse it only to retry that record.
  same key, same draft and artifact bytes: `duplicate`, the original record
  same key, anything different: `conflict` (6), nothing written
A new record needs a new key. After a crash or an unclear exit, publish the
same draft with the same key again.

Example (the assignee `helper` reports on task t-1 to its parent `lead`;
paste the lines between the rules as they are):
--------
\"$BORGCHART_BIN\" msg publish --key result-t-1-1 <<'EOF'
{{\"to\": \"lead\", \"kind\": \"result\", \"task\": \"t-1\", \"attempt\": 1,
 \"reply_to\": [\"m1\"],
 \"body\": \"Parser done; all tests pass.\",
 \"facts\": {{\"status\": \"complete\", \"checks\": [{{\"name\": \"tests\", \"outcome\": \"pass\"}}]}}}}
EOF
--------
Prints: committed: m3 (result to lead)

Outcomes (with --json: {{ok, code, outcome, ...}}; the exit status is the code):
"
    ));
    out.push_str(&outcomes(&[
        ("committed", Code::Ok, "published; prints the new record's ID"),
        (
            "duplicate",
            Code::Ok,
            "this key already published this draft; the original ID",
        ),
        (n(Code::Usage), Code::Usage, "a bad option, such as an --external name"),
        (
            n(Code::NotConfigured),
            Code::NotConfigured,
            "no bus in the deployment, or the team hasn't started",
        ),
        (
            n(Code::Refused),
            Code::Refused,
            "not from here: no identity, --external inside a member, another message format, a release while a writer may still be working",
        ),
        (
            n(Code::Invalid),
            Code::Invalid,
            "a bad draft, fact, recipient, reference or task step; nothing written",
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
            "stale: a replaced member, an old attempt, not the task's authority or assignee, or references from before a restart (list the inbox again)",
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
--external, --limit and --full are refused. New records return immediately;
every other exit, including usage errors and missing identity, waits until
60 seconds have passed. A restart or replaced session is always fenced:
stop re-arming. List your inbox, work, then re-arm one background wait after
each turn, with no cursor bookkeeping.

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
            "with --background: the session is over, stop. With --after: the team was restarted, so list the inbox again (`inbox`, no --after) and wait after what it shows. Or this member was replaced: stop",
        ),
        (
            n(Code::Usage),
            Code::Usage,
            "--timeout, --after or --limit out of range, no inbox named outside a member, or --background without own identity / with incompatible flags",
        ),
        (
            n(Code::NotConfigured),
            Code::NotConfigured,
            "no bus in the deployment, or the team hasn't started",
        ),
        (
            n(Code::Refused),
            Code::Refused,
            "--external inside a member, or another message format",
        ),
        (
            n(Code::Invalid),
            Code::Invalid,
            "--member names no member of the team",
        ),
        (
            n(Code::Corrupt),
            Code::Corrupt,
            "the team's storage is damaged (`brg bus check`)",
        ),
        (n(Code::Error), Code::Error, "an I/O or unexpected error"),
    ]));
    out
}

/// `brg task --help`.
pub fn task() -> String {
    "Tasks are folded, in order, from the team's records; `msg publish` checks each
task record against that fold. The assignee is the member an assign names;
the authority is its parent, or an outside caller when the assignee is the
team's root.

State (reached by) -> who acts next, with which kinds:
  assigned (assign)            assignee: accept or decline
  accepted (accept)            assignee: result; question/decision any time
  result-claimed (result)      authority: verify or cancel-request
  verified / rejected (verify outcome accepted / rejected)
                               authority: release or reopen
  cancel-requested (cancel-request)
                               assignee: cancel-confirm, or a result
  cancelled (cancel-confirm)   authority: release or reopen
  declined (decline)           nothing, if no attempt had writers; otherwise
                               authority: release or reopen
  reopened (reopen)            a new attempt (+1); assignee: accept or decline,
                               or the authority assigns it again to change it
  released (release)           nothing: the member and checkout are free
cancel-request applies to assigned, accepted, result-claimed or reopened.
Any other step is invalid (5); a record for an older attempt is fenced (8).
Acknowledging a record never changes a task; only these kinds do.

One task at a time: a member holds at most one unresolved task (not yet
released, or declined after an attempt had writers), and a checkout belongs
to at most one.

Writers: the assignee's incarnations (one per start) that accepted, sent a
result or confirmed a cancel, plus its current one unless it declined. A
release is refused (4) until every writer is `gone`, meaning brg saw it stop:
stop the assignee with `brg down --member <assignee>`, check that `task show`
lists each writer as gone, then release.

`task show` prints the task, its writers with liveness, its history and
`next: <who> (<kinds>)`: the member (or `external`) expected to publish next
and the kinds it may send; `next: nothing` once the task needs nobody.
"
    .to_string()
}

/// `brg task list --help`.
pub fn task_list() -> String {
    "List tasks and open questions, folded from the team's records.

One line per task: `<task>  <assignee>#<attempt>  <state>  -> <who>: <kinds>`,
who should publish next and which kinds, then each open question. `brg task
--help` explains the states."
        .to_string()
}

/// `brg task show --help`.
pub fn task_show() -> String {
    "Show one task: its state, agreement, records, writers, next step and open
questions about it.

Each `writer:` line is an incarnation that worked on the task, with its
liveness (alive, gone or unknown); a release needs every one gone.
`next: <who> (<kinds>)` is who should publish next and which kinds they may
send. `brg task --help` explains the states."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::record::Draft;
    use serde_json::{Map, Value, json};

    fn draft(kind: Kind, facts: &[&Fact]) -> Draft {
        let facts: Map<String, Value> = facts
            .iter()
            .map(|f| {
                (
                    f.name.to_string(),
                    serde_json::from_str(f.sample)
                        .unwrap_or_else(|e| panic!("{} sample for {}: {e}", f.name, kind.as_str())),
                )
            })
            .collect();
        let mut v = json!({"to": "lead", "kind": kind, "facts": facts});
        if kind.is_task_kind() {
            v["task"] = json!("t");
            v["attempt"] = json!(1);
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn task_kinds_come_last() {
        let first = KINDS.iter().position(|k| k.is_task_kind()).unwrap();
        assert!(KINDS[first..].iter().all(|k| k.is_task_kind()));
        assert!(!KINDS[..first].iter().any(|k| k.is_task_kind()));
    }

    /// The help's required facts are exactly the validator's: the required
    /// ones (plus any optional ones) pass, and leaving out any one fails.
    #[test]
    fn required_facts_match_the_validator() {
        for kind in KINDS {
            let h = kind_help(kind);
            let required: Vec<&Fact> = h.required.iter().collect();
            store::check_facts(&draft(kind, &required))
                .unwrap_or_else(|e| panic!("{}: required facts alone: {e}", kind.as_str()));
            let all: Vec<&Fact> = h.required.iter().chain(h.optional).collect();
            store::check_facts(&draft(kind, &all))
                .unwrap_or_else(|e| panic!("{}: with optional facts: {e}", kind.as_str()));
            for skip in h.required {
                let fewer: Vec<&Fact> = required
                    .iter()
                    .copied()
                    .filter(|f| f.name != skip.name)
                    .collect();
                assert!(
                    store::check_facts(&draft(kind, &fewer)).is_err(),
                    "{} without {} passes the validator, but the help says it is required",
                    kind.as_str(),
                    skip.name
                );
            }
        }
    }
}
