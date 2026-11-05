//! `brg msg`, `brg inbox` and `brg bus`: the record commands.
//!
//! Every command prints `{ok, code, outcome, …}` with `--json`, and exits
//! with the bus outcome codes (`docs/bus.md`).

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::deploy;
use crate::diag::Report;
use crate::validate::Input;

use super::context::{self, Caller};
use super::outcome::{Code, Failure, ok_json};
use super::record::{Actor, MAX_RECORD_BYTES, Recipient, Record, parse_record_id};
use super::store::{self, Bus, Page};

#[derive(Subcommand)]
pub enum MsgCommand {
    /// Publish a record from a JSON draft (a file, or `-`/nothing for stdin).
    Publish {
        /// The operation key: a retry with the same key returns the same record.
        #[arg(long)]
        key: String,
        /// The draft (default: stdin).
        #[arg(long)]
        record: Option<PathBuf>,
        /// Publish as an outside caller with this name.
        #[arg(long)]
        external: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one record, with its body and facts. Never acknowledges it.
    Read {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Acknowledge records addressed to you (acking twice is a no-op).
    Ack {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Acknowledge as the outside caller with this name.
        #[arg(long)]
        external: Option<String>,
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
    /// Only records after this ID (the `next` of the previous page).
    #[arg(long)]
    pub after: Option<String>,
    #[arg(long, default_value_t = store::DEFAULT_PAGE)]
    pub limit: usize,
    /// Include acknowledged records.
    #[arg(long)]
    pub all: bool,
    /// Include bodies and facts.
    #[arg(long)]
    pub full: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Clone)]
pub enum InboxCommand {
    /// Wait until the inbox has an unacknowledged record after `--after`.
    Wait {
        #[command(flatten)]
        who: Who,
        #[arg(long)]
        after: Option<String>,
        /// Give up after this many seconds (1–600), exiting `timeout`.
        #[arg(long)]
        timeout: u64,
        #[arg(long, default_value_t = store::DEFAULT_PAGE)]
        limit: usize,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum BusCommand {
    /// Summarize the run: members, per-recipient counts, recent records, store health.
    Inspect {
        /// How many recent record headers to show.
        #[arg(long, default_value_t = 10)]
        last: usize,
        #[arg(long)]
        json: bool,
    },
    /// Check the store and the run's age. Never writes.
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Carry pending work from the run this one continues (read-only there).
    Carry {
        /// The earlier run: must be this run's `bus.continues`.
        #[arg(long)]
        from: String,
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
            }
            ExitCode::from(f.code.exit())
        }
    }
}

pub(crate) fn open_bus(input: &Input) -> Result<Bus, Failure> {
    let file = match input {
        Input::Team(_) => {
            return Err(Failure::new(
                Code::NotConfigured,
                "a team directory on its own has no bus; use a deployment file with `bus:`",
            ));
        }
        Input::Deployment(file) => file,
    };
    let mut report = Report::default();
    let deployment = deploy::load(file, &mut report).ok_or_else(|| {
        Failure::new(
            Code::Error,
            report
                .errors()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    store::open(&deployment)
}

fn parse_after(after: Option<&str>) -> Result<Option<u64>, Failure> {
    after
        .map(|a| {
            parse_record_id(a)
                .ok_or_else(|| Failure::new(Code::Usage, format!("`--after {a}`: not a record ID")))
        })
        .transpose()
}

fn check_limit(limit: usize) -> Result<usize, Failure> {
    if (1..=store::MAX_PAGE).contains(&limit) {
        Ok(limit)
    } else {
        Err(Failure::new(
            Code::Usage,
            format!("`--limit`: 1–{}", store::MAX_PAGE),
        ))
    }
}

fn read_draft(record: Option<&PathBuf>) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    let limit = MAX_RECORD_BYTES as u64 + 1;
    match record {
        Some(p) if p.as_os_str() != "-" => std::fs::File::open(p)
            .and_then(|f| f.take(limit).read_to_end(&mut bytes))
            .map_err(|e| Failure::io(&format!("cannot read {}", p.display()), e))?,
        _ => std::io::stdin()
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(|e| Failure::io("cannot read the draft from stdin", e))?,
    };
    Ok(bytes)
}

pub fn msg(input: &Input, command: MsgCommand) -> ExitCode {
    match command {
        MsgCommand::Publish {
            key,
            record,
            external,
            json,
        } => finish(
            json,
            (|| {
                let caller = context::caller(external.as_deref())?;
                let bus = open_bus(input)?;
                let draft = read_draft(record.as_ref())?;
                let p = store::publish(&bus, &caller, &key, &draft)?;
                let outcome = if p.duplicate {
                    "duplicate"
                } else {
                    "committed"
                };
                Ok(Output {
                    json: ok_json(
                        outcome,
                        json!({
                            "id": p.record.id,
                            "seq": p.record.seq,
                            "at": p.record.at,
                            "to": p.record.to,
                            "kind": p.record.kind,
                            "key": p.record.key,
                            "attention": p.attention.as_str(),
                        }),
                    ),
                    text: format!(
                        "{outcome}: {} ({} to {})\n",
                        p.record.id,
                        p.record.kind.as_str(),
                        p.record.to
                    ),
                })
            })(),
        ),
        MsgCommand::Read { id, json } => finish(
            json,
            (|| {
                let bus = open_bus(input)?;
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
                super::attention_hooks::ensure_if_pending(&bus.paths, &bus.deployment);
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
                let bus = open_bus(input)?;
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

/// Whose inbox, and whether that's the caller's own (which counts as observed).
struct Target {
    recipient: Recipient,
    own: Option<(String, String)>,
}

fn target(bus: &Bus, who: &Who) -> Result<Target, Failure> {
    if let Some(m) = &who.member {
        if !bus.manifest.members.contains_key(m) {
            return Err(Failure::new(
                Code::Invalid,
                format!("`{m}` is not a member of run `{}`", bus.config.run),
            ));
        }
        return Ok(Target {
            recipient: Recipient::Member(m.clone()),
            own: None,
        });
    }
    match context::caller(who.external.as_deref())? {
        Caller::External { name } => Ok(Target {
            recipient: Recipient::External(name),
            own: None,
        }),
        Caller::Operator => Err(Failure::new(
            Code::Usage,
            "name an inbox: `--member ID` or `--external NAME`",
        )),
        caller @ Caller::Member { .. } => {
            let Actor::Member {
                member,
                incarnation,
            } = store::actor_for(bus, &caller)?
            else {
                return Err(Failure::new(Code::Error, "a member caller is not a member"));
            };
            Ok(Target {
                recipient: Recipient::Member(member.clone()),
                own: Some((member, incarnation)),
            })
        }
    }
}

fn page_output(t: &Target, page: &Page, snap: &store::Snapshot, full: bool) -> Output {
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
        text.push_str("warning: the store is incomplete; `brg bus check` lists the problems\n");
    }
    for r in &page.records {
        text.push_str(&render(r, snap.is_acked(r.seq), full));
    }
    text.push_str(&format!(
        "{} unacknowledged of {} for {}{}\n",
        page.unacked,
        page.total,
        t.recipient,
        page.next
            .as_ref()
            .map(|n| format!("; more after {n} (--after {n})"))
            .unwrap_or_default()
    ));
    Output {
        json: ok_json(
            "ok",
            json!({
                "recipient": t.recipient.to_string(),
                "records": records,
                "next": page.next,
                "unacked": page.unacked,
                "total": page.total,
                "store": if snap.healthy() { "ok" } else { "incomplete" },
            }),
        ),
        text,
    }
}

fn render(r: &Record, acked: bool, full: bool) -> String {
    let task = match (&r.task, r.attempt) {
        (Some(t), Some(a)) => format!("  {t}#{a}"),
        (Some(t), None) => format!("  {t}"),
        _ => String::new(),
    };
    let mut out = format!(
        "{}  {}  {} -> {}  {}{task}{}\n",
        r.id,
        r.at,
        store::actor_label(&r.actor),
        r.to,
        r.kind.as_str(),
        if acked { "  (acked)" } else { "" }
    );
    if full {
        if !r.reply_to.is_empty() {
            out.push_str(&format!("  reply_to: {}\n", r.reply_to.join(", ")));
        }
        for a in &r.artifacts {
            out.push_str(&format!(
                "  artifact: {} ({} bytes, sha256 {})\n",
                a.path.display(),
                a.bytes,
                a.sha256
            ));
        }
        if !r.facts.is_empty() {
            out.push_str(&format!(
                "  facts: {}\n",
                serde_json::to_string(&r.facts).unwrap_or_default()
            ));
        }
        for line in r.body.lines() {
            out.push_str(&format!("  | {line}\n"));
        }
    }
    out
}

pub fn inbox(input: &Input, args: InboxArgs) -> ExitCode {
    match args.command {
        None => finish(
            args.json,
            (|| {
                let limit = check_limit(args.limit)?;
                let after = parse_after(args.after.as_deref())?;
                let bus = open_bus(input)?;
                let t = target(&bus, &args.who)?;
                let snap = store::snapshot(&bus)?;
                let page = store::list(&snap, &t.recipient, after, limit, args.all);
                if let Some((m, i)) = &t.own {
                    store::observed(&bus, m, i, &page);
                } else {
                    super::attention_hooks::ensure_if_pending(&bus.paths, &bus.deployment);
                }
                Ok(page_output(&t, &page, &snap, args.full))
            })(),
        ),
        Some(InboxCommand::Wait {
            who,
            after,
            timeout,
            limit,
            full,
            json,
        }) => finish(
            json,
            (|| {
                if !(1..=store::MAX_WAIT_SECS).contains(&timeout) {
                    return Err(Failure::new(
                        Code::Usage,
                        format!("`--timeout`: 1–{} seconds", store::MAX_WAIT_SECS),
                    ));
                }
                let limit = check_limit(limit)?;
                let after = parse_after(after.as_deref())?;
                let bus = open_bus(input)?;
                let t = target(&bus, &who)?;
                let deadline = Instant::now() + Duration::from_secs(timeout);
                loop {
                    let snap = store::snapshot(&bus)?;
                    let page = store::list(&snap, &t.recipient, after, limit, false);
                    if !page.records.is_empty() {
                        if let Some((m, i)) = &t.own {
                            store::observed(&bus, m, i, &page);
                        }
                        return Ok(page_output(&t, &page, &snap, full));
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(Failure::new(
                            Code::Timeout,
                            format!("nothing new for {} after {timeout}s", t.recipient),
                        ));
                    }
                    std::thread::sleep(store::POLL.min(deadline - now));
                }
            })(),
        ),
    }
}

pub fn bus(input: &Input, command: BusCommand) -> ExitCode {
    match command {
        BusCommand::Inspect { last, json } => finish(
            json,
            (|| {
                let bus = open_bus(input)?;
                let snap = store::snapshot(&bus)?;
                let v = store::inspect(&bus, &snap, last);
                let mut text = format!(
                    "run {} ({}), deployment {}, created {}\n",
                    bus.config.run,
                    bus.paths.root.display(),
                    bus.manifest.deployment,
                    bus.manifest.created_at
                );
                text.push_str("members:\n");
                for (id, m) in &bus.manifest.members {
                    text.push_str(&format!(
                        "  {id} ({}){}\n",
                        m.vendor,
                        m.parent
                            .as_ref()
                            .map(|p| format!(", reports to {p}"))
                            .unwrap_or_default()
                    ));
                }
                text.push_str("inboxes:\n");
                for r in v["recipients"].as_array().into_iter().flatten() {
                    text.push_str(&format!(
                        "  {}: {} unacknowledged of {}\n",
                        r["to"].as_str().unwrap_or(""),
                        r["unacked"],
                        r["total"]
                    ));
                }
                text.push_str(&format!("records: {}\n", snap.records.len()));
                for r in snap.records.values().rev().take(last).rev() {
                    text.push_str(&render(r, snap.is_acked(r.seq), false));
                }
                if snap.healthy() {
                    text.push_str("store: ok\n");
                } else {
                    text.push_str("store: incomplete\n");
                    for p in &snap.problems {
                        text.push_str(&format!("  {p}\n"));
                    }
                }
                Ok(Output {
                    json: ok_json("ok", v),
                    text,
                })
            })(),
        ),
        BusCommand::Check { json } => {
            let result = open_bus(input).and_then(|bus| store::check(&bus));
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
                        "age_days": report.age_days,
                        "errors": report.errors,
                        "warnings": report.warnings,
                        "carried_into": report.carried_into,
                    })
                );
            } else {
                for e in &report.errors {
                    eprintln!("error: {e}");
                }
                for w in &report.warnings {
                    eprintln!("warning: {w}");
                }
                println!(
                    "{}: {} record(s){}",
                    code.as_str(),
                    report.records,
                    report
                        .age_days
                        .map(|a| format!(", run age {a} days"))
                        .unwrap_or_default()
                );
            }
            ExitCode::from(code.exit())
        }
        BusCommand::Carry { from, json } => super::task_cli::carry(input, &from, json),
    }
}
