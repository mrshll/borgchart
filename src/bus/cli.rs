//! `brg msg`: sending, reading and checking messages.
//!
//! Commands print `{ok, code, outcome, …}` with `--json`. Sending keeps
//! its delivery outcome codes; the others use bus outcomes (`docs/bus.md`).

use std::io::Read;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::runtime;

use super::context::{self, Actor, Caller};
use super::outcome::{Code, Failure, ok_json};
use super::record::{Recipient, Record, parse_record_id};
use super::store::{self, Bus, Inbox};

#[derive(Subcommand)]
pub enum MsgCommand {
    /// Store a message for its recipient, and type it in when safe.
    ///
    /// It is never typed into an interactive Claude recipient.
    #[command(after_long_help = crate::bus::help::send())]
    Send {
        /// The member to type into: `<team>/<member>`.
        #[arg(add = crate::complete::members())]
        member: String,
        /// The text (several words are joined with spaces).
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Send even when the member's state is unknown.
        #[arg(long)]
        allow_unknown: bool,
        /// The sending member, by ID (for plugins relaying a member's
        /// message): brg checks that the target is that member's parent.
        #[arg(long)]
        from: Option<String>,
        /// Name an outside author (default: owner without team context);
        /// members use their own fenced identity.
        #[arg(long)]
        external: Option<String>,
        /// Reply to an existing record, by its ID (repeatable).
        #[arg(long)]
        reply_to: Vec<String>,
        /// Print {ok, code, message, id, delivery, reason} as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List unacked messages addressed to you, oldest first.
    #[command(after_long_help = super::help::list())]
    List {
        #[command(flatten)]
        args: InboxArgs,
    },
    /// Show one record, with its body. Never acknowledges it.
    Read {
        /// The record ID (`m<number>`).
        id: String,
        /// Print the record as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Acknowledge records addressed to you (acking twice is a no-op).
    Ack {
        /// Record IDs (`m<number>`), all addressed to you.
        #[arg(required = true)]
        ids: Vec<String>,
        /// Acknowledge as this outside caller (default: owner without team context).
        #[arg(long)]
        external: Option<String>,
        /// Print each ID's outcome (`acked` or `already`) as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Wait for new messages, with a minimum delay for messages already present.
    #[command(after_long_help = super::help::wait())]
    Wait {
        /// Wait as this outside recipient (default: owner without team context).
        #[arg(long)]
        external: Option<String>,
        /// Seconds until timeout (60-86400; default 86400).
        #[arg(long, default_value_t = store::MAX_WAIT_SECS)]
        timeout: u64,
        /// Include message bodies.
        #[arg(long)]
        full: bool,
        /// Print the inbox as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check the team's message storage. Never writes.
    IntegrityCheck {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Clone)]
pub struct Who {
    /// Another member's inbox, read-only.
    #[arg(long, conflicts_with = "external")]
    pub member: Option<String>,
    /// The inbox of `external:NAME` (default: owner without team context).
    #[arg(long)]
    pub external: Option<String>,
}

#[derive(Args, Clone)]
pub struct InboxArgs {
    #[command(flatten)]
    pub who: Who,
    /// Include message bodies.
    #[arg(long)]
    pub full: bool,
    /// Print the inbox as JSON.
    #[arg(long)]
    pub json: bool,
}

/// What a command produced: JSON for `--json`, text otherwise.
pub(crate) struct Output {
    pub(crate) json: Value,
    pub(crate) text: String,
}

pub(crate) fn finish(json: bool, result: Result<Output, Failure>) -> ExitCode {
    match result {
        Ok(out) => {
            if json {
                println!("{}", out.json);
            } else {
                print!("{}", out.text);
            }
            ExitCode::SUCCESS
        }
        Err(f) => {
            if json {
                println!("{}", f.to_json());
            } else {
                eprintln!("brg: {}", f.message);
            }
            ExitCode::from(f.code.exit())
        }
    }
}

/// Open the message storage of the team the caller names (`--team`)
/// or, inside a member, its own.
pub(crate) fn open_bus(instance: Option<&str>) -> Result<Bus, Failure> {
    let id = context::instance(instance)?;
    let inst = crate::address::instance(&id).map_err(|e| Failure::new(Code::NotFound, e))?;
    store::open(&inst.state)
}

pub fn msg(instance: Option<&str>, command: MsgCommand, started: Instant) -> ExitCode {
    match command {
        MsgCommand::Send {
            member,
            text,
            allow_unknown,
            from,
            external,
            reply_to,
            json,
        } => {
            let result = (|| {
                if instance.is_some() {
                    return Err(runtime::SendError::Usage(
                        "`msg send` takes no --team; name the target as <team>/<member>".into(),
                    ));
                }
                let mut text = text.join(" ");
                if text == "-" {
                    text.clear();
                    std::io::stdin()
                        .take((runtime::MAX_SEND_BYTES + 1) as u64)
                        .read_to_string(&mut text)
                        .map_err(|e| {
                            runtime::SendError::Usage(format!("cannot read stdin: {e}"))
                        })?;
                }
                let (inst, name) = crate::cli::send_target(&member)?;
                runtime::send(
                    &inst,
                    &name,
                    &text,
                    allow_unknown,
                    from.as_deref(),
                    external.as_deref(),
                    &reply_to,
                )
            })();
            match result {
                Ok(out) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({"ok": true, "code": 0, "outcome": "committed", "message": out.message, "id": out.id, "delivery": out.delivery, "reason": out.reason})
                        );
                    } else {
                        println!("{}: {} ({})", out.message, out.delivery, out.reason);
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    if json {
                        let outcome = match error.code() {
                            2 => "usage",
                            3 => "not-alive",
                            4 => "blocked",
                            _ => "refused",
                        };
                        let mut value = serde_json::json!({"ok": false, "code": error.code(), "outcome": outcome, "message": error.message()});
                        if let runtime::SendError::Committed { id, delivery, .. } = &error {
                            value["id"] = id.clone().into();
                            value["delivery"] = (*delivery).into();
                            value["reason"] = error.message().into();
                        }
                        println!("{value}");
                    } else {
                        eprintln!(
                            "brg: {}{}",
                            error.message(),
                            match &error {
                                runtime::SendError::Committed { id, .. } =>
                                    format!(" (committed {id})"),
                                _ => String::new(),
                            }
                        );
                    }
                    ExitCode::from(error.code())
                }
            }
        }
        MsgCommand::List { args } => list(instance, args),
        MsgCommand::Wait {
            external,
            timeout,
            full,
            json,
        } => wait(instance, external, timeout, full, json, started),
        MsgCommand::IntegrityCheck { json } => check(instance, json),
        MsgCommand::Read { id, json } => finish(
            json,
            (|| {
                let bus = open_bus(instance)?;
                if parse_record_id(&id).is_none() {
                    return Err(Failure::new(
                        Code::Usage,
                        format!("`{id}` is not a record ID"),
                    ));
                }
                let snap = store::snapshot(&bus)?;
                let rec = snap
                    .get(&id)
                    .ok_or_else(|| Failure::new(Code::NotFound, format!("no record `{id}`")))?;
                let acked = snap.is_acked(rec.seq);
                Ok(Output {
                    json: ok_json("ok", json!({"record": store::full(rec, acked)})),
                    text: render(rec, acked, true),
                })
            })(),
        ),
        MsgCommand::Ack {
            ids,
            external,
            json,
        } => finish(
            json,
            (|| {
                let caller = context::caller(external.as_deref())?;
                let bus = open_bus(instance)?;
                let results = store::ack(&bus, &caller, &ids)?;
                Ok(Output {
                    json: ok_json(
                        "ok",
                        json!({"acked": results.iter().map(|(id, o)| json!({"id": id, "outcome": o.as_str()})).collect::<Vec<_>>()}),
                    ),
                    text: results
                        .iter()
                        .map(|(id, o)| format!("{id}: {}\n", o.as_str()))
                        .collect(),
                })
            })(),
        ),
    }
}

/// The recipient selected by the caller.
struct Target {
    recipient: Recipient,
}

fn target(bus: &Bus, who: &Who) -> Result<Target, Failure> {
    if let Some(m) = &who.member {
        let (instance, member) = bus.member(m).map_err(|e| Failure::new(Code::Invalid, e))?;
        return Ok(Target {
            recipient: Recipient::Member(format!("{}/{member}", instance.instance)),
        });
    }
    match context::caller(who.external.as_deref())? {
        Caller::External { name } => Ok(Target {
            recipient: Recipient::External(name),
        }),
        Caller::Operator => Err(Failure::new(
            Code::Usage,
            "name an inbox: `--member ID` or `--external NAME`",
        )),
        caller @ Caller::Member { .. } => {
            let Actor::Member { member, .. } = store::actor_for(bus, &caller)? else {
                return Err(Failure::new(Code::Error, "a member caller is not a member"));
            };
            Ok(Target {
                recipient: Recipient::Member(member.clone()),
            })
        }
    }
}

fn inbox_output(t: &Target, page: &Inbox, snap: &store::Snapshot, full: bool) -> Output {
    let records: Vec<Value> = page
        .records
        .iter()
        .map(|r| {
            let acked = snap.is_acked(r.seq);
            if full {
                store::full(r, acked)
            } else {
                store::header(r, acked)
            }
        })
        .collect();
    let mut text = String::new();
    if !snap.healthy() {
        text.push_str(
            "warning: the store is incomplete; `brg msg integrity-check` lists the problems\n",
        );
    }
    for r in &page.records {
        text.push_str(&render(r, snap.is_acked(r.seq), full));
    }
    text.push_str(&format!(
        "{} unacknowledged of {} for {}\n",
        page.unacked, page.total, t.recipient,
    ));
    Output {
        json: ok_json(
            "ok",
            json!({
                "recipient": t.recipient.to_string(),
                "records": records,
                "unacked": page.unacked,
                "total": page.total,
                "store": if snap.healthy() { "ok" } else { "incomplete" },
            }),
        ),
        text,
    }
}

fn render(r: &Record, acked: bool, full: bool) -> String {
    let mut out = format!(
        "{}  {}  {} -> {}{}\n",
        r.id,
        r.at,
        r.from,
        r.to,
        if !r.ack_required {
            "  (ack not required)"
        } else if acked {
            "  (acked)"
        } else {
            ""
        }
    );
    if full {
        if !r.reply_to.is_empty() {
            out.push_str(&format!("  reply_to: {}\n", r.reply_to.join(", ")));
        }
        for line in r.body.lines() {
            out.push_str(&format!("  | {line}\n"));
        }
    }
    out
}

fn list(instance: Option<&str>, args: InboxArgs) -> ExitCode {
    finish(
        args.json,
        (|| {
            let bus = open_bus(instance)?;
            let t = target(&bus, &args.who)?;
            let caller = if args.who.member.is_some() {
                Caller::Operator
            } else {
                context::caller(args.who.external.as_deref())?
            };
            if matches!(caller, Caller::Member { .. }) {
                store::actor_for(&bus, &caller)?;
            }
            let snap = match store::wait_snapshot(&bus, Instant::now() + Duration::from_secs(1))? {
                store::WaitSnapshot::Ready(snap) => snap,
                store::WaitSnapshot::TimedOut => {
                    return Err(Failure::new(
                        Code::Timeout,
                        "message store is busy; try the inbox again",
                    ));
                }
            };
            let inbox = store::list(&snap, &t.recipient);
            Ok(inbox_output(&t, &inbox, &snap, args.full))
        })(),
    )
}

fn wait(
    instance: Option<&str>,
    external: Option<String>,
    timeout: u64,
    full: bool,
    json: bool,
    started: Instant,
) -> ExitCode {
    finish(
        json,
        (|| {
            if !(store::MIN_WAIT_SECS..=store::MAX_WAIT_SECS).contains(&timeout) {
                return Err(Failure::new(Code::Usage, "`--timeout`: 60-86400 seconds"));
            }
            let caller = context::caller(external.as_deref())?;
            let bus = open_bus(instance)?;
            let t = target(
                &bus,
                &Who {
                    member: None,
                    external,
                },
            )?;
            let duration = Duration::from_secs(timeout);
            #[cfg(feature = "test-support")]
            let duration = std::env::var("BORGCHART_WAIT_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_millis)
                .unwrap_or(duration);
            let snap = store::wait(&bus, &caller, &t.recipient, started, duration)?;
            let inbox = store::list(&snap, &t.recipient);
            Ok(inbox_output(&t, &inbox, &snap, full))
        })(),
    )
}

fn check(instance: Option<&str>, json: bool) -> ExitCode {
    let result = open_bus(instance).and_then(|bus| store::check(&bus));
    let report = match result {
        Ok(r) => r,
        Err(f) => return finish(json, Err(f)),
    };
    let code = if report.corrupt {
        Code::Corrupt
    } else if !report.errors.is_empty() {
        Code::Error
    } else {
        Code::Ok
    };
    if json {
        println!(
            "{}",
            json!({
                "ok": code == Code::Ok,
                "code": code.exit(),
                "outcome": code.as_str(),
                "records": report.records,
                "errors": report.errors,
                "warnings": report.warnings,
            })
        );
    } else {
        for e in &report.errors {
            eprintln!("error: {e}");
        }
        for w in &report.warnings {
            eprintln!("warning: {w}");
        }
        println!("{}: {} record(s)", code.as_str(), report.records);
    }
    ExitCode::from(code.exit())
}
