//! `brg task list|show` and `brg bus carry`.

use std::process::ExitCode;

use clap::Subcommand;
use serde_json::{Value, json};

use crate::validate::Input;

use super::cli::{Output, finish, open_bus};
use super::outcome::{Code, Failure, ok_json};
use super::{carry as carrying, store, tasks};

#[derive(Subcommand)]
pub enum TaskCommand {
    /// List tasks and open questions, folded from the run's records.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one task: its state, agreement, records, writers, next step
    /// and open questions about it.
    Show {
        task: String,
        #[arg(long)]
        json: bool,
    },
}

const INCOMPLETE: &str = "warning: the store is incomplete; tasks are shown as of the last readable record (`brg bus check` lists the problems)\n";

fn store_label(complete: bool) -> &'static str {
    if complete { "ok" } else { "incomplete" }
}

pub fn task(input: &Input, command: TaskCommand) -> ExitCode {
    let (json_out, result) = match command {
        TaskCommand::List { json } => (json, list(input)),
        TaskCommand::Show { task, json } => (json, show(input, &task)),
    };
    finish(json_out, result)
}

fn list(input: &Input) -> Result<Output, Failure> {
    let bus = open_bus(input)?;
    let snap = store::snapshot(&bus)?;
    let fold = tasks::fold(&bus.manifest, &snap);
    let probe = |m: &str, i: &str| tasks::recorded_liveness(&bus.paths, m, i);
    let mut text = String::new();
    if !fold.complete {
        text.push_str(INCOMPLETE);
    }
    let mut rows = Vec::new();
    for t in fold.tasks.values() {
        let mut v = tasks::task_json(&bus.paths, t, &probe);
        if let Some(map) = v.as_object_mut() {
            map.remove("history");
            map.insert("next".into(), tasks::next_json(t));
        }
        rows.push(v);
        text.push_str(&format!(
            "{}  {}#{}  {}  -> {}{}\n",
            t.task,
            t.assignee,
            t.attempt,
            t.state.as_str(),
            t.next()
                .map(|(a, k)| format!("{a}: {}", k.join("/")))
                .unwrap_or_else(|| "nothing".into()),
            t.checkout
                .as_ref()
                .map(|c| format!("  checkout {c}"))
                .unwrap_or_default()
        ));
    }
    if fold.tasks.is_empty() {
        text.push_str("no tasks\n");
    }
    for q in &fold.questions {
        text.push_str(&format!(
            "open question {}  {} -> {}{}\n",
            q.id,
            q.from,
            q.to,
            q.task
                .as_ref()
                .map(|t| format!("  ({t})"))
                .unwrap_or_default()
        ));
    }
    Ok(Output {
        json: ok_json(
            "ok",
            json!({
                "tasks": rows,
                "questions": fold.questions,
                "through": fold.through,
                "store": store_label(fold.complete),
                "anomalies": fold.anomalies,
            }),
        ),
        text,
    })
}

fn show(input: &Input, id: &str) -> Result<Output, Failure> {
    let bus = open_bus(input)?;
    let snap = store::snapshot(&bus)?;
    let fold = tasks::fold(&bus.manifest, &snap);
    let t = fold.tasks.get(id).ok_or_else(|| {
        Failure::new(
            Code::NotFound,
            format!(
                "no task `{id}`{}",
                if fold.complete {
                    ""
                } else {
                    " in the readable records"
                }
            ),
        )
    })?;
    let probe = |m: &str, i: &str| tasks::recorded_liveness(&bus.paths, m, i);
    let questions: Vec<&tasks::Question> = fold
        .questions
        .iter()
        .filter(|q| q.task.as_deref() == Some(id))
        .collect();
    let task_json = tasks::task_json(&bus.paths, t, &probe);
    let mut text = String::new();
    if !fold.complete {
        text.push_str(INCOMPLETE);
    }
    text.push_str(&format!(
        "task {}: {} (attempt {})\n  assignee: {}\n  authority: {}\n  agreement: {} (sha256 {})\n  scope: {}\n",
        t.task,
        t.state.as_str(),
        t.attempt,
        t.assignee,
        t.authority.as_deref().unwrap_or("external"),
        t.agreement.path,
        t.agreement.sha256,
        t.scope
    ));
    for (label, v) in [
        ("provenance", &t.provenance),
        ("limits", &t.limits),
        ("checkout", &t.checkout),
    ] {
        if let Some(v) = v {
            text.push_str(&format!("  {label}: {v}\n"));
        }
    }
    for w in task_json["writers"].as_array().into_iter().flatten() {
        text.push_str(&format!(
            "  writer: {} ({}{})\n",
            w["incarnation"].as_str().unwrap_or(""),
            w["liveness"].as_str().unwrap_or(""),
            if w["current"] == Value::Bool(true) {
                ", current"
            } else {
                ""
            }
        ));
    }
    for s in &t.history {
        text.push_str(&format!(
            "  {}  {}  #{}  {}\n",
            s.id,
            s.kind.as_str(),
            s.attempt,
            s.actor
        ));
    }
    match t.next() {
        Some((actor, kinds)) => {
            text.push_str(&format!("  next: {actor} ({})\n", kinds.join(" or ")))
        }
        None => text.push_str("  next: nothing\n"),
    }
    for q in &questions {
        text.push_str(&format!("  open question {} from {}\n", q.id, q.from));
    }
    Ok(Output {
        json: ok_json(
            "ok",
            json!({
                "task": task_json,
                "next": tasks::next_json(t),
                "questions": questions,
                "through": fold.through,
                "store": store_label(fold.complete),
            }),
        ),
        text,
    })
}

pub fn carry(input: &Input, from: &str, json_out: bool) -> ExitCode {
    let result = (|| {
        if !super::is_valid_run_id(from) {
            return Err(Failure::new(
                Code::Usage,
                format!("`--from {from}`: not a run ID"),
            ));
        }
        let bus = open_bus(input)?;
        let report = carrying::carry(&bus, from)?;
        if let Some(f) = report.failure() {
            return Err(f);
        }
        if report.incomplete && !json_out {
            eprintln!(
                "warning: run `{from}`'s store is incomplete; carried only through {} ({})",
                report.through.as_deref().unwrap_or("nothing"),
                report.problems.join("; ")
            );
        }
        let mut text = String::new();
        for i in &report.carried {
            text.push_str(&format!(
                "{} ({}) -> {}: {}{}\n",
                i.source,
                i.reason,
                i.to,
                i.id.as_ref().map(|id| format!("{id} ")).unwrap_or_default(),
                i.outcome
            ));
        }
        text.push_str(&format!(
            "carried {} item(s) from `{from}` into `{}`\n",
            report.carried.iter().filter(|i| i.id.is_some()).count(),
            report.into
        ));
        let outcome = if report.incomplete {
            "partial"
        } else {
            "carried"
        };
        Ok(Output {
            json: ok_json(
                outcome,
                serde_json::to_value(&report).unwrap_or(Value::Null),
            ),
            text,
        })
    })();
    finish(json_out, result)
}
