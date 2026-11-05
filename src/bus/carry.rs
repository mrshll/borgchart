//! Carrying work into a linked run.
//!
//! A run that is getting old, or whose store is damaged, is continued in a
//! new run (`bus.continues: <old>`). `brg bus carry --from <old>`, run
//! against the new run, reads the old one without changing anything and
//! publishes, as `bus`, one `note` per piece of pending work: each
//! unresolved task (its assignment), each open question and each
//! unacknowledged record, to the original's recipient, with
//! `facts.carried_from: {run, id, sha256}` naming the original exactly. Keys
//! derive from the source IDs, so carrying again changes nothing. A marker
//! in the new run (`bus/carried/<old>.json`) lets `bus check` of either
//! run say what happened.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::layout::RunPaths;
use super::manifest;
use super::outcome::{Code, Failure};
use super::record::{Actor, Kind, Recipient, Record};
use super::store::{self, Bus};
use super::{fsx, tasks, time};

/// What a carry left in the new run: `bus/carried/<from>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub schema: u32,
    pub from: String,
    pub into: String,
    pub at: String,
    /// False when the old store had problems (only its readable prefix was
    /// carried) or some pending work was skipped.
    pub complete: bool,
    /// The last old record considered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
    /// Old record ID → the new record carrying it.
    pub carried: Vec<(String, String)>,
    /// Old record IDs not carried, with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<(String, String)>,
}

pub fn marker_path(run: &RunPaths, from: &str) -> PathBuf {
    run.bus_dir().join("carried").join(format!("{from}.json"))
}

/// One piece of pending work and what happened to it.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub source: String,
    pub reason: &'static str,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub from: String,
    pub into: String,
    /// The old store had problems, or some pending work was skipped.
    pub incomplete: bool,
    pub through: Option<String>,
    pub problems: Vec<String>,
    pub carried: Vec<Item>,
}

impl Report {
    pub fn failure(&self) -> Option<Failure> {
        self.carried
            .iter()
            .find(|i| i.outcome == "conflict" || i.outcome == "error")
            .map(|i| {
                Failure::new(
                    if i.outcome == "conflict" {
                        Code::Conflict
                    } else {
                        Code::Error
                    },
                    format!("{}: {}", i.source, i.message.clone().unwrap_or_default()),
                )
            })
    }
}

/// The pending work in a run's readable prefix, oldest first: unresolved
/// tasks (by their latest `assign`), open questions, and unacknowledged
/// records that don't belong to a resolved task. One reason per record.
pub fn pending(old: &manifest::RunManifest, snap: &store::Snapshot) -> Vec<(&'static str, Record)> {
    let records = tasks::prefix(snap);
    let fold = tasks::fold_records(old, &records);
    let mut out: Vec<(&'static str, Record)> = Vec::new();
    for r in &records {
        let task = r.task.as_deref().and_then(|t| fold.tasks.get(t));
        let reason = if r.kind == Kind::Assign
            && task.is_some_and(|t| !t.is_resolved() && t.records.get("assign") == Some(&r.id))
        {
            Some("unresolved-task")
        } else if r.kind == Kind::Question && fold.questions.iter().any(|q| q.id == r.id) {
            Some("open-question")
        } else if !snap.is_acked(r.seq)
            && !(r.kind.is_task_kind() && task.is_none_or(|t| t.is_resolved()))
        {
            Some("unacked")
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push((reason, (*r).clone()));
        }
    }
    out
}

/// Carry pending work from run `from` (under the same root) into `bus`'s run.
pub fn carry(bus: &Bus, from: &str) -> Result<Report, Failure> {
    let invalid = |m: String| Failure::new(Code::Invalid, m);
    if bus.config.continues.as_deref() != Some(from) {
        return Err(invalid(format!(
            "run `{}` doesn't continue `{from}`: set `bus.continues: {from}` in {}",
            bus.config.run,
            bus.deployment.file.display()
        )));
    }
    if let Some(link) = &bus.manifest.linked_from
        && link.run != from
    {
        return Err(invalid(format!(
            "run `{}` is linked from `{}`, not `{from}`",
            bus.config.run, link.run
        )));
    }
    let old_paths = RunPaths::from_root(&bus.config.root.join(from));
    let old = manifest::load_manifest(&old_paths)
        .map_err(|e| Failure::new(Code::Error, e))?
        .ok_or_else(|| {
            Failure::new(
                Code::NotFound,
                format!("no run `{from}` under {}", bus.config.root.display()),
            )
        })?;
    let snap = store::snapshot_of(&old_paths, from)?;
    let through = tasks::prefix(&snap).last().map(|r| r.id.clone());

    let mut items = Vec::new();
    for (reason, r) in pending(&old, &snap) {
        let mut item = Item {
            source: r.id.clone(),
            reason,
            to: r.to.clone(),
            id: None,
            outcome: String::new(),
            message: None,
        };
        if let Some(Recipient::Member(m)) = Recipient::parse(&r.to)
            && !bus.manifest.members.contains_key(&m)
        {
            item.outcome = "skipped".into();
            item.message = Some(format!("`{m}` is not a member of run `{}`", bus.config.run));
            items.push(item);
            continue;
        }
        let original = old_paths.record(r.seq);
        let bytes = std::fs::read(&original)
            .map_err(|e| Failure::io(&format!("cannot read {}", original.display()), e))?;
        let draft = carry_draft(from, &r, &manifest::sha256_hex(&bytes), &original);
        let key = format!("carry:{from}:{}", r.id);
        let draft_bytes =
            serde_json::to_vec(&draft).map_err(|e| Failure::new(Code::Error, e.to_string()))?;
        match store::publish_as(bus, Actor::Bus, &key, &draft_bytes) {
            Ok(p) => {
                item.id = Some(p.record.id.clone());
                item.outcome = if p.duplicate {
                    "duplicate"
                } else {
                    "committed"
                }
                .into();
            }
            Err(f) if f.code == Code::Conflict => {
                item.outcome = "conflict".into();
                item.message = Some(f.message);
            }
            Err(f) => return Err(f),
        }
        items.push(item);
    }
    let skipped = items.iter().any(|i| i.outcome == "skipped");
    let report = Report {
        from: from.to_string(),
        into: bus.config.run.clone(),
        incomplete: !snap.healthy() || skipped,
        through,
        problems: snap.problems.clone(),
        carried: items,
    };
    if report.failure().is_none() {
        write_marker(bus, &report)?;
    }
    Ok(report)
}

fn carry_draft(from: &str, r: &Record, sha256: &str, original: &Path) -> Value {
    let what = match (r.kind, &r.task) {
        (Kind::Assign, Some(t)) => format!("the assignment of unresolved task `{t}`"),
        (Kind::Question, _) => "an open question".to_string(),
        _ => "an unacknowledged record".to_string(),
    };
    let body = format!(
        "Carried from run `{from}`: {what}, {} ({} from {} to {}, {}). The original is unchanged at {}. Work carried here needs re-authorizing in this run.",
        r.id,
        r.kind.as_str(),
        store::actor_label(&r.actor),
        r.to,
        r.at,
        original.display()
    );
    let mut facts = json!({
        "carried_from": {"run": from, "id": r.id, "sha256": sha256},
        "kind": r.kind.as_str(),
    });
    if let Some(t) = &r.task {
        facts["task"] = json!(t);
    }
    json!({"to": r.to, "kind": "note", "body": body, "facts": facts})
}

fn write_marker(bus: &Bus, report: &Report) -> Result<(), Failure> {
    let marker = Marker {
        schema: super::SCHEMA,
        from: report.from.clone(),
        into: report.into.clone(),
        at: time::utc_now(),
        complete: !report.incomplete,
        through: report.through.clone(),
        carried: report
            .carried
            .iter()
            .filter_map(|i| i.id.clone().map(|id| (i.source.clone(), id)))
            .collect(),
        skipped: report
            .carried
            .iter()
            .filter(|i| i.outcome == "skipped")
            .map(|i| (i.source.clone(), i.message.clone().unwrap_or_default()))
            .collect(),
    };
    let bytes =
        serde_json::to_vec_pretty(&marker).map_err(|e| Failure::new(Code::Error, e.to_string()))?;
    fsx::replace(&marker_path(&bus.paths, &report.from), &bytes)
        .map_err(|e| Failure::io("cannot write the carry marker", e))
}

/// Read a marker, if present and readable.
pub fn read_marker(path: &Path) -> Option<Marker> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// The runs under `root` that `run` was carried into. Reads only.
pub fn carried_into(root: &Path, run: &str) -> Vec<Marker> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<Marker> = entries
        .flatten()
        .filter_map(|e| {
            let into = e.file_name().to_string_lossy().to_string();
            let m = read_marker(&marker_path(&RunPaths::from_root(&e.path()), run))?;
            (m.into == into && m.from == run).then_some(m)
        })
        .collect();
    out.sort_by(|a, b| a.into.cmp(&b.into));
    out
}

/// What `bus check` says about carries, for and from this run.
pub struct CheckNotes {
    pub warnings: Vec<String>,
    /// A complete carry covers every record this run has now: past
    /// `max_age_days`, it is retired (a warning) rather than overdue.
    pub retired: bool,
}

/// `into` are the markers naming this run (from [`carried_into`]).
pub fn check_notes(bus: &Bus, snap: &store::Snapshot, into: &[Marker]) -> CheckNotes {
    let run = &bus.config.run;
    let mut warnings = Vec::new();
    let last = snap
        .records
        .keys()
        .next_back()
        .map(|seq| super::record::record_id(*seq));
    let mut retired = false;
    for m in into {
        let covers = snap.healthy() && m.through == last;
        warnings.push(format!(
            "run `{run}` was carried into `{}` at {}{}",
            m.into,
            m.at,
            match (m.complete, covers) {
                (true, true) => String::new(),
                (false, _) => format!(
                    " (partially: {} skipped or unreadable; reconcile, then `brg bus carry --from {run}` there again)",
                    if m.skipped.is_empty() {
                        "records".to_string()
                    } else {
                        m.skipped.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(", ")
                    }
                ),
                (true, false) => format!(
                    " through {}, but it now has records up to {} that weren't carried: `brg bus carry --from {run}` there again",
                    m.through.as_deref().unwrap_or("nothing"),
                    last.as_deref().unwrap_or("nothing")
                ),
            }
        ));
        retired |= m.complete && covers;
    }
    if let Some(prev) = &bus.config.continues {
        match read_marker(&marker_path(&bus.paths, prev)) {
            None => warnings.push(format!(
                "this run continues `{prev}`, but nothing has been carried from it yet: `brg bus carry --from {prev}`"
            )),
            Some(m) if !m.complete => warnings.push(format!(
                "the carry from `{prev}` was partial ({} skipped{}): reconcile, then `brg bus carry --from {prev}` again",
                m.skipped.len(),
                if m.through.is_some() { "" } else { ", nothing readable" }
            )),
            Some(_) => {}
        }
    }
    CheckNotes { warnings, retired }
}
