//! `brg msg`, `brg inbox` and `brg bus`: the record commands.
//!
//! Every command prints `{ok, code, outcome, …}` with `--json`, and exits
//! with the bus outcome codes (`docs/bus.md`).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use super::context::{self, Actor, Caller};
use super::outcome::{Code, Failure, ok_json};
use super::record::{Recipient, Record, parse_record_id};
use super::store::{self, Bus, Inbox};

#[derive(Subcommand)]
pub enum MsgCommand {
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
        /// Acknowledge as the outside caller with this name.
        #[arg(long)]
        external: Option<String>,
        /// Print each ID's outcome (`acked` or `already`) as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Clone)]
pub struct Who {
    /// Another member's inbox, read-only.
    #[arg(long, conflicts_with = "external")]
    pub member: Option<String>,
    /// The inbox of `external:NAME`, as that outside caller.
    #[arg(long)]
    pub external: Option<String>,
}

#[derive(Args, Clone)]
pub struct InboxArgs {
    #[command(subcommand)]
    pub command: Option<InboxCommand>,
    #[command(flatten)]
    pub who: Who,
    /// Include message bodies.
    #[arg(long)]
    pub full: bool,
    /// Print the inbox as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Clone)]
pub enum InboxCommand {
    /// Wait for new messages, with a minimum delay for messages already present.
    #[command(after_long_help = super::help::wait())]
    Wait {
        /// Wait as an outside recipient.
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
}

#[derive(Subcommand)]
pub enum BusCommand {
    /// Check the instance's message storage. Never writes.
    Check {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
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
                for note in &f.notes {
                    eprintln!("note: {note}");
                }
            }
            ExitCode::from(f.code.exit())
        }
    }
}

/// Open the message storage of the instance the caller names (`--instance`)
/// or, inside a member, its own.
pub(crate) fn open_bus(instance: Option<&str>) -> Result<Bus, Failure> {
    let id = context::instance(instance)?;
    let inst = crate::address::instance(&id).map_err(|e| Failure::new(Code::NotFound, e))?;
    store::open(&inst.state)
}

pub fn msg(instance: Option<&str>, command: MsgCommand) -> ExitCode {
    match command {
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
        if !bus.meta.members.contains_key(m) {
            return Err(Failure::new(
                Code::Invalid,
                store::not_a_member(&bus.meta, m),
            ));
        }
        return Ok(Target {
            recipient: Recipient::Member(m.clone()),
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
    for note in &snap.notes {
        text.push_str(&format!("note: {note}\n"));
    }
    if !snap.healthy() {
        text.push_str("warning: the store is incomplete; `brg bus check` lists the problems\n");
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
                "notes": snap.notes,
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

pub fn inbox(instance: Option<&str>, args: InboxArgs, started: Instant) -> ExitCode {
    match args.command {
        None => finish(
            args.json,
            (|| {
                let bus = open_bus(instance)?;
                let t = target(&bus, &args.who)?;
                let caller = if args.who.member.is_some() {
                    Caller::Operator
                } else {
                    context::caller(args.who.external.as_deref())?
                };
                let deadline = Instant::now() + Duration::from_secs(1);
                let snap = store::import(&bus, &caller, &t.recipient, deadline)?;
                let inbox = store::list(&bus, &snap, &t.recipient);
                Ok(inbox_output(&t, &inbox, &snap, args.full))
            })(),
        ),
        Some(InboxCommand::Wait {
            external,
            timeout,
            full,
            json,
        }) => finish(
            json,
            (|| {
                if args.who.member.is_some()
                    || args.who.external.is_some()
                    || args.full
                    || args.json
                {
                    return Err(Failure::new(
                        Code::Usage,
                        "put wait's options after `inbox wait`",
                    ));
                }
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
                let inbox = store::list(&bus, &snap, &t.recipient);
                Ok(inbox_output(&t, &inbox, &snap, full))
            })(),
        ),
    }
}

pub fn bus(instance: Option<&str>, command: BusCommand) -> ExitCode {
    match command {
        BusCommand::Check { json } => {
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
    }
}
