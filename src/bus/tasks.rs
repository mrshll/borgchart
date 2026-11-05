//! Task facts: folding task records into per-task state, and the checks a
//! task record must pass before it is published.
//!
//! The fold is a pure function of the ordered records. It reads only the
//! contiguous prefix of the store (a gap or an unreadable record ends it),
//! so a damaged store still shows what is known up to the damage.
//!
//! Roles: a task's **assignee** is the member its `assign` names; its
//! **authority** is the assignee's parent, or any outside caller when the
//! assignee is the team's root. Authority kinds (`assign`, `verify`,
//! `release`, `cancel-request`, `reopen`, and `decision` for questions)
//! come from the authority's current incarnation; assignee kinds (`accept`,
//! `decline`, `result`, `cancel-confirm`) from the assignee's current
//! incarnation. Acknowledging a record (consumption) never changes a task.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::layout::RunPaths;
use super::manifest::{self, Liveness, RunManifest};
use super::outcome::{Code, Failure};
use super::record::{Actor, Draft, Kind, Recipient, Record};
use super::store::{self, Bus, Snapshot};

/// Where a task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Assigned,
    Accepted,
    Declined,
    ResultClaimed,
    Verified,
    Rejected,
    CancelRequested,
    Cancelled,
    Released,
    Reopened,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Assigned => "assigned",
            State::Accepted => "accepted",
            State::Declined => "declined",
            State::ResultClaimed => "result-claimed",
            State::Verified => "verified",
            State::Rejected => "rejected",
            State::CancelRequested => "cancel-requested",
            State::Cancelled => "cancelled",
            State::Released => "released",
            State::Reopened => "reopened",
        }
    }

    /// Resolved tasks hold nothing: no checkout, no claim on the assignee.
    pub fn is_resolved(self) -> bool {
        matches!(self, State::Released | State::Declined)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Agreement {
    pub path: String,
    pub sha256: String,
}

/// One record that touched a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub id: String,
    pub kind: Kind,
    pub attempt: u32,
    pub actor: String,
    pub at: String,
}

/// A task as the records leave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub task: String,
    pub assignee: String,
    /// The assignee's parent, or `None` when an outside caller is its authority.
    #[serde(serialize_with = "authority_ser")]
    pub authority: Option<String>,
    pub attempt: u32,
    pub state: State,
    pub agreement: Agreement,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<String>,
    /// The assignee's incarnations that accepted, claimed a result or
    /// confirmed a cancellation for this task, in first-seen order.
    #[serde(skip)]
    pub writers: Vec<String>,
    /// The record ID of each fact of the current attempt (`assign`, `accept`,
    /// `result`, `verify`, …; the latest of each kind).
    pub records: BTreeMap<String, String>,
    pub history: Vec<Step>,
}

fn authority_ser<S: serde::Serializer>(a: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(a.as_deref().unwrap_or("external"))
}

impl Task {
    /// The current attempt's `result` record, if one was claimed.
    pub fn result_id(&self) -> Option<&str> {
        self.records.get("result").map(String::as_str)
    }

    /// Who is expected to act next, and with which kinds.
    pub fn next(&self) -> Option<(String, &'static [&'static str])> {
        let authority = self.authority.clone().unwrap_or_else(|| "external".into());
        let assignee = self.assignee.clone();
        Some(match self.state {
            State::Assigned | State::Reopened => (assignee, &["accept", "decline"]),
            State::Accepted => (assignee, &["result"]),
            State::CancelRequested => (assignee, &["cancel-confirm", "result"]),
            State::ResultClaimed => (authority, &["verify", "cancel-request"]),
            State::Verified | State::Rejected | State::Cancelled => {
                (authority, &["release", "reopen"])
            }
            State::Declined | State::Released => return None,
        })
    }
}

/// A question nobody has answered with a `decision` yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub at: String,
}

/// Everything the records say about tasks and questions.
#[derive(Debug, Clone, Default)]
pub struct Fold {
    pub tasks: BTreeMap<String, Task>,
    pub questions: Vec<Question>,
    /// The last record folded (`None` for an empty store).
    pub through: Option<String>,
    /// False when the store has problems: the fold stops at the first one.
    pub complete: bool,
    /// Records that did not apply (written before these checks existed, or
    /// by hand); they are skipped.
    pub anomalies: Vec<String>,
}

impl Fold {
    /// The task, if any, the assignee holds unresolved (other than `except`).
    pub fn unresolved_for(&self, member: &str, except: &str) -> Option<&Task> {
        self.tasks
            .values()
            .find(|t| t.assignee == member && t.task != except && !t.state.is_resolved())
    }
}

/// The records from 1 up to the first missing one.
pub fn prefix(snap: &Snapshot) -> Vec<&Record> {
    let mut out = Vec::new();
    for (i, (seq, r)) in snap.records.iter().enumerate() {
        if *seq != i as u64 + 1 {
            break;
        }
        out.push(r);
    }
    out
}

/// Fold the store's readable prefix.
pub fn fold(manifest: &RunManifest, snap: &Snapshot) -> Fold {
    let records = prefix(snap);
    let mut f = fold_records(manifest, &records);
    // A problem with a record ends the prefix; problems with keys or acks
    // don't change what the records say, but the store is still incomplete.
    f.complete = snap.healthy();
    f
}

pub fn fold_records(manifest: &RunManifest, records: &[&Record]) -> Fold {
    let mut f = Fold {
        complete: true,
        ..Fold::default()
    };
    let mut questions: BTreeMap<String, Question> = BTreeMap::new();
    for r in records {
        f.through = Some(r.id.clone());
        let fact = Fact::of_record(r);
        match r.kind {
            Kind::Note => {}
            Kind::Question => {
                questions.insert(
                    r.id.clone(),
                    Question {
                        id: r.id.clone(),
                        from: store::actor_label(&r.actor),
                        to: r.to.clone(),
                        task: r.task.clone(),
                        at: r.at.clone(),
                    },
                );
            }
            Kind::Decision => match check_decision(manifest, &fact, &questions) {
                Ok(answered) => {
                    for id in &answered {
                        questions.remove(id);
                    }
                }
                Err(e) => f.anomalies.push(format!("{}: {}", r.id, e.message)),
            },
            _ => match transition(manifest, &f, &fact) {
                Ok(task) => {
                    f.tasks.insert(task.task.clone(), task);
                }
                Err(e) => f.anomalies.push(format!("{}: {}", r.id, e.message)),
            },
        }
    }
    f.questions = questions.into_values().collect();
    f
}

/// The parts of a draft or record the task rules look at.
struct Fact<'a> {
    id: Option<&'a str>,
    at: &'a str,
    actor: &'a Actor,
    to: &'a str,
    kind: Kind,
    task: Option<&'a str>,
    attempt: Option<u32>,
    reply_to: &'a [String],
    facts: &'a Map<String, Value>,
}

impl<'a> Fact<'a> {
    fn of_record(r: &'a Record) -> Self {
        Fact {
            id: Some(&r.id),
            at: &r.at,
            actor: &r.actor,
            to: &r.to,
            kind: r.kind,
            task: r.task.as_deref(),
            attempt: r.attempt,
            reply_to: &r.reply_to,
            facts: &r.facts,
        }
    }

    fn of_draft(d: &'a Draft, actor: &'a Actor) -> Self {
        Fact {
            id: None,
            at: "",
            actor,
            to: &d.to,
            kind: d.kind,
            task: d.task.as_deref(),
            attempt: d.attempt,
            reply_to: &d.reply_to,
            facts: &d.facts,
        }
    }
}

fn fail<T>(code: Code, message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::new(code, message))
}

fn text(facts: &Map<String, Value>, name: &str) -> Option<String> {
    facts.get(name).and_then(Value::as_str).map(str::to_string)
}

/// The authority over `member`: its parent, or `None` (outside callers).
pub fn authority_of(manifest: &RunManifest, member: &str) -> Option<String> {
    manifest.members.get(member).and_then(|m| m.parent.clone())
}

/// Whether `actor` is the authority over the recipient `to` (by name; the
/// incarnation is checked at publication).
fn check_authority(
    manifest: &RunManifest,
    actor: &Actor,
    to: &str,
    kind: Kind,
) -> Result<(), Failure> {
    let member = match Recipient::parse(to) {
        Some(Recipient::Member(m)) => m,
        _ => {
            return fail(
                Code::Fenced,
                format!(
                    "a `{}` can only go to a member, from its authority",
                    kind.as_str()
                ),
            );
        }
    };
    let ok = match (authority_of(manifest, &member), actor) {
        (Some(parent), Actor::Member { member: m, .. }) => *m == parent,
        (None, Actor::External { .. }) => true,
        _ => false,
    };
    if ok {
        return Ok(());
    }
    let who = authority_of(manifest, &member)
        .map(|p| format!("its parent `{p}`"))
        .unwrap_or_else(|| "an outside caller (`--external`)".into());
    fail(
        Code::Fenced,
        format!(
            "a `{}` for `{member}` must come from {who}, not {}",
            kind.as_str(),
            store::actor_label(actor)
        ),
    )
}

/// Check a `decision` and return the open questions it answers: each must
/// have been asked by the decision's recipient, whose authority decides.
fn check_decision(
    manifest: &RunManifest,
    f: &Fact,
    open: &BTreeMap<String, Question>,
) -> Result<Vec<String>, Failure> {
    check_authority(manifest, f.actor, f.to, f.kind)?;
    let mut answered = Vec::new();
    for id in f.reply_to {
        let Some(q) = open.get(id) else { continue };
        if q.from != f.to {
            return fail(
                Code::Invalid,
                format!(
                    "{id} was asked by {}; a `decision` answering it goes to them, not `{}`",
                    q.from, f.to
                ),
            );
        }
        answered.push(id.clone());
    }
    if answered.is_empty() {
        return fail(
            Code::Invalid,
            "a `decision` must answer an open question named in `reply_to`",
        );
    }
    Ok(answered)
}

fn is_assignee_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Accept | Kind::Decline | Kind::Result | Kind::CancelConfirm
    )
}

/// Apply one task record to the fold, returning the task's new state, or
/// why it can't apply. Checks roles by name, attempts and states; the
/// checks that depend on the moment of publication (current incarnations,
/// the agreement file, liveness, one task per assignee) are in
/// [`check_publish`].
fn transition(manifest: &RunManifest, fold: &Fold, f: &Fact) -> Result<Task, Failure> {
    let Some(task_id) = f.task else {
        return fail(
            Code::Invalid,
            format!("a `{}` needs `task`", f.kind.as_str()),
        );
    };
    let existing = fold.tasks.get(task_id);
    let step = |attempt: u32| Step {
        id: f.id.unwrap_or("(new)").to_string(),
        kind: f.kind,
        attempt,
        actor: store::actor_label(f.actor),
        at: f.at.to_string(),
    };

    if f.kind == Kind::Assign {
        let Some(Recipient::Member(assignee)) = Recipient::parse(f.to) else {
            return fail(Code::Invalid, "a task can only be assigned to a member");
        };
        check_authority(manifest, f.actor, f.to, f.kind)?;
        let attempt = f.attempt.unwrap_or(1);
        let mut history = Vec::new();
        match existing {
            None if attempt != 1 => {
                return fail(
                    Code::Invalid,
                    format!("task `{task_id}` is new: its first `assign` is attempt 1"),
                );
            }
            None => {}
            Some(t) if t.state == State::Reopened => {
                if t.assignee != assignee {
                    return fail(
                        Code::Invalid,
                        format!(
                            "task `{task_id}` belongs to `{}`; a new attempt can't move it",
                            t.assignee
                        ),
                    );
                }
                check_attempt(t, attempt, t.attempt)?;
                history = t.history.clone();
            }
            Some(t) => {
                return fail(
                    Code::Invalid,
                    format!(
                        "task `{task_id}` already exists ({}); reopen it for a new attempt",
                        t.state.as_str()
                    ),
                );
            }
        }
        let agreement = f.facts.get("agreement");
        let field = |k: &str| {
            agreement
                .and_then(|a| a.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        history.push(step(attempt));
        let mut records = BTreeMap::new();
        records.insert("assign".to_string(), f.id.unwrap_or("(new)").to_string());
        return Ok(Task {
            task: task_id.to_string(),
            authority: authority_of(manifest, &assignee),
            assignee,
            attempt,
            state: State::Assigned,
            agreement: Agreement {
                path: field("path"),
                sha256: field("sha256"),
            },
            scope: text(f.facts, "scope").unwrap_or_default(),
            provenance: text(f.facts, "provenance"),
            limits: text(f.facts, "limits"),
            checkout: text(f.facts, "checkout"),
            writers: existing.map(|t| t.writers.clone()).unwrap_or_default(),
            records,
            history,
        });
    }

    let Some(t) = existing else {
        return fail(Code::Invalid, format!("there is no task `{task_id}`"));
    };
    let mut t = t.clone();

    // Roles.
    if is_assignee_kind(f.kind) {
        match f.actor {
            Actor::Member { member, .. } if *member == t.assignee => {}
            other => {
                return fail(
                    Code::Fenced,
                    format!(
                        "only `{}`, the assignee of `{task_id}`, may {} it, not {}",
                        t.assignee,
                        f.kind.as_str(),
                        store::actor_label(other)
                    ),
                );
            }
        }
        let to_authority = match (&t.authority, Recipient::parse(f.to)) {
            (Some(p), Some(Recipient::Member(m))) => *p == m,
            (None, Some(Recipient::External(_))) => true,
            _ => false,
        };
        if !to_authority {
            return fail(
                Code::Invalid,
                format!(
                    "a `{}` for `{task_id}` goes to {}",
                    f.kind.as_str(),
                    t.authority
                        .as_ref()
                        .map(|p| format!("`{p}`"))
                        .unwrap_or_else(|| "`external:<name>`".into())
                ),
            );
        }
    } else {
        if f.to != t.assignee {
            return fail(
                Code::Invalid,
                format!(
                    "a `{}` for `{task_id}` goes to its assignee `{}`",
                    f.kind.as_str(),
                    t.assignee
                ),
            );
        }
        check_authority(manifest, f.actor, f.to, f.kind)?;
    }

    // Attempts.
    let expected = if f.kind == Kind::Reopen {
        t.attempt + 1
    } else {
        t.attempt
    };
    check_attempt(&t, f.attempt.unwrap_or(expected), expected)?;

    // States.
    use State::*;
    let next = match (f.kind, t.state) {
        (Kind::Accept, Assigned | Reopened) => Accepted,
        (Kind::Decline, Assigned | Reopened) => Declined,
        (Kind::Result, Accepted | CancelRequested) => ResultClaimed,
        (Kind::Verify, ResultClaimed) => {
            let named = f.facts.get("result").and_then(Value::as_str);
            if named.is_none() || named != t.result_id() {
                return fail(
                    Code::Invalid,
                    format!(
                        "a `verify` of `{task_id}` attempt {} must name its result ({})",
                        t.attempt,
                        t.result_id().unwrap_or("none")
                    ),
                );
            }
            match f.facts.get("outcome").and_then(Value::as_str) {
                Some("accepted") => Verified,
                _ => Rejected,
            }
        }
        (Kind::CancelRequest, Assigned | Accepted | ResultClaimed | Reopened) => CancelRequested,
        (Kind::CancelConfirm, CancelRequested) => Cancelled,
        (Kind::Release, Verified | Rejected | Cancelled | Declined) => Released,
        (Kind::Reopen, Verified | Rejected | Cancelled | Declined) => Reopened,
        (kind, state) => {
            return fail(
                Code::Invalid,
                format!(
                    "task `{task_id}` is {}: a `{}` doesn't apply",
                    state.as_str(),
                    kind.as_str()
                ),
            );
        }
    };
    if f.kind == Kind::Release
        && let (Some(named), Some(held)) = (text(f.facts, "checkout"), &t.checkout)
        && named != *held
    {
        return fail(
            Code::Invalid,
            format!("task `{task_id}` holds checkout {held}, not {named}"),
        );
    }

    if f.kind == Kind::Reopen {
        t.attempt = expected;
        t.records.retain(|k, _| k == "assign");
    }
    if matches!(f.kind, Kind::Accept | Kind::Result | Kind::CancelConfirm)
        && let Actor::Member { incarnation, .. } = f.actor
        && !t.writers.contains(incarnation)
    {
        t.writers.push(incarnation.clone());
    }
    t.state = next;
    t.records.insert(
        f.kind.as_str().to_string(),
        f.id.unwrap_or("(new)").to_string(),
    );
    t.history.push(step(t.attempt));
    Ok(t)
}

fn check_attempt(t: &Task, given: u32, expected: u32) -> Result<(), Failure> {
    if given < expected {
        return fail(
            Code::Fenced,
            format!(
                "attempt {given} of `{}` is stale: the task is on attempt {}",
                t.task, t.attempt
            ),
        );
    }
    if given > expected {
        return fail(
            Code::Invalid,
            format!("`{}` has no attempt {given} (expected {expected})", t.task),
        );
    }
    Ok(())
}

/// Tells whether an incarnation's process may still be running.
pub type Probe<'a> = &'a dyn Fn(&str, &str) -> Liveness;

/// Liveness from what the run has recorded: `gone` once the incarnation
/// ended through brg, `unknown` otherwise. Never `alive`, so it never
/// frees a checkout on a guess. (Lifecycle's tmux-based probe refines this.)
pub fn recorded_liveness(paths: &RunPaths, member: &str, incarnation: &str) -> Liveness {
    let ended = |inc: Option<manifest::Incarnation>| {
        inc.is_some_and(|i| i.incarnation == incarnation && i.ended_at.is_some())
    };
    let history = std::fs::read(paths.incarnation_history(member, incarnation))
        .ok()
        .and_then(|b| serde_json::from_slice::<manifest::Incarnation>(&b).ok());
    if ended(history) || ended(manifest::load_incarnation(paths, member).ok().flatten()) {
        Liveness::Gone
    } else {
        Liveness::Unknown
    }
}

/// The incarnations that might be writing `t`'s checkout: every one that
/// worked on it, the assignee's current one (unless the task was declined),
/// and any the releaser names.
pub fn possible_writers(paths: &RunPaths, t: &Task, named: Option<&str>) -> Vec<String> {
    let mut out = t.writers.clone();
    let mut add = |inc: String| {
        if !out.contains(&inc) {
            out.push(inc);
        }
    };
    if t.state != State::Declined
        && let Ok(Some(cur)) = manifest::load_incarnation(paths, &t.assignee)
    {
        add(cur.incarnation);
    }
    if let Some(n) = named {
        add(n.to_string());
    }
    out
}

/// The task checks for a draft about to be published as `actor`. Called
/// by the store holding `runtime/generation.lock` and `bus/lock`, after it
/// has rechecked the caller's incarnation and scanned a healthy store and
/// before numbering, so the fold is exactly what the new record follows.
pub fn check_publish(
    bus: &Bus,
    snap: &Snapshot,
    actor: &Actor,
    draft: &Draft,
    liveness: Probe,
) -> Result<(), Failure> {
    if !draft.kind.is_task_kind() && draft.kind != Kind::Decision {
        return Ok(());
    }
    let manifest = &bus.manifest;
    let fold = fold(manifest, snap);
    let fact = Fact::of_draft(draft, actor);

    if draft.kind == Kind::Decision {
        let open: BTreeMap<String, Question> = fold
            .questions
            .iter()
            .map(|q| (q.id.clone(), q.clone()))
            .collect();
        return check_decision(manifest, &fact, &open).map(|_| ());
    }

    let task = transition(manifest, &fold, &fact)?;
    match draft.kind {
        Kind::Assign => {
            if let Some(other) = fold.unresolved_for(&task.assignee, &task.task) {
                return fail(
                    Code::Invalid,
                    format!(
                        "`{}` already holds unresolved task `{}` ({})",
                        task.assignee,
                        other.task,
                        other.state.as_str()
                    ),
                );
            }
            if let Some(checkout) = &task.checkout
                && let Some(other) = fold.tasks.values().find(|t| {
                    t.task != task.task
                        && !t.state.is_resolved()
                        && t.checkout.as_ref() == Some(checkout)
                })
            {
                return fail(
                    Code::Invalid,
                    format!(
                        "checkout {checkout} is held by unresolved task `{}` ({})",
                        other.task, other.assignee
                    ),
                );
            }
            check_agreement(&task.agreement)?;
        }
        Kind::Reopen => {
            if let Some(other) = fold.unresolved_for(&task.assignee, &task.task) {
                return fail(
                    Code::Invalid,
                    format!(
                        "`{}` already holds unresolved task `{}`",
                        task.assignee, other.task
                    ),
                );
            }
        }
        Kind::Release => {
            let before = &fold.tasks[&task.task];
            let named = text(&draft.facts, "writer_incarnation");
            for inc in possible_writers(&bus.paths, before, named.as_deref()) {
                let l = liveness(&before.assignee, &inc);
                if l != Liveness::Gone {
                    return fail(
                        Code::Refused,
                        format!(
                            "`{}` incarnation `{inc}` may still be writing (liveness: {}); release `{}` once it has stopped",
                            before.assignee,
                            liveness_str(l),
                            task.task
                        ),
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn liveness_str(l: Liveness) -> &'static str {
    match l {
        Liveness::Alive => "alive",
        Liveness::Gone => "gone",
        Liveness::Unknown => "unknown",
    }
}

fn check_agreement(a: &Agreement) -> Result<(), Failure> {
    // This runs under `bus/lock`: refuse anything but a regular file before
    // opening it, so a FIFO or device can't block the lock.
    match std::fs::metadata(&a.path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return fail(
                Code::Invalid,
                format!("agreement {}: not a regular file", a.path),
            );
        }
        Err(e) => return fail(Code::Invalid, format!("agreement {}: {e}", a.path)),
    }
    let (_, sha) = store::sha256_file(std::path::Path::new(&a.path), store::MAX_ARTIFACT_BYTES)
        .map_err(|e| Failure::new(Code::Invalid, format!("agreement {}: {e}", a.path)))?;
    if !sha.eq_ignore_ascii_case(&a.sha256) {
        return fail(
            Code::Invalid,
            format!(
                "agreement {} has sha256 {sha}, not the {} the assignment pins",
                a.path, a.sha256
            ),
        );
    }
    Ok(())
}

// --- views for `brg task` ---

/// A task as `brg task list|show` prints it.
pub fn task_json(paths: &RunPaths, t: &Task, liveness: Probe) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
    let current = manifest::load_incarnation(paths, &t.assignee)
        .ok()
        .flatten()
        .map(|i| i.incarnation);
    let writers: Vec<Value> = t
        .writers
        .iter()
        .map(|inc| {
            json!({
                "incarnation": inc,
                "current": current.as_deref() == Some(inc.as_str()),
                "liveness": liveness_str(liveness(&t.assignee, inc)),
            })
        })
        .collect();
    if let Some(map) = v.as_object_mut() {
        map.insert("writers".into(), json!(writers));
    }
    v
}

pub fn next_json(t: &Task) -> Value {
    match t.next() {
        Some((actor, kinds)) => json!({"actor": actor, "kinds": kinds}),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::manifest::{ConfigSnapshot, ManifestMember};

    fn manifest() -> RunManifest {
        let members = [("lead", None), ("helper", Some("lead"))]
            .iter()
            .map(|(m, p)| {
                (
                    m.to_string(),
                    ManifestMember {
                        vendor: "claude".into(),
                        parent: p.map(str::to_string),
                        model: None,
                    },
                )
            })
            .collect();
        RunManifest {
            schema: 1,
            run: "r1".into(),
            created_at: "2026-11-05T19:00:00.000Z".into(),
            deployment: "d".into(),
            source: "/x/borgchart.yaml".into(),
            config: ConfigSnapshot {
                version: 1,
                digest: "00".into(),
                inputs: BTreeMap::new(),
            },
            members,
            linked_from: None,
        }
    }

    fn member(m: &str) -> Actor {
        Actor::Member {
            member: m.into(),
            incarnation: format!("{m}-1"),
        }
    }

    fn rec(seq: u64, actor: Actor, v: Value) -> Record {
        let mut r = json!({
            "schema": 1, "run": "r1", "id": format!("m{seq}"), "seq": seq,
            "at": "2026-11-05T19:00:00.000Z", "actor": actor, "key": format!("k{seq}"),
        });
        r.as_object_mut()
            .unwrap()
            .extend(v.as_object().unwrap().clone());
        serde_json::from_value(r).unwrap()
    }

    fn assign() -> Value {
        json!({"to": "helper", "kind": "assign", "task": "t", "attempt": 1,
            "facts": {"agreement": {"path": "/a", "sha256": "ab"}, "scope": "s"}})
    }

    #[test]
    fn fold_tracks_state_and_skips_what_does_not_apply() {
        let m = manifest();
        let records = [
            rec(1, member("lead"), assign()),
            // Out of order: a result before acceptance is skipped.
            rec(
                2,
                member("helper"),
                json!({"to": "lead", "kind": "result", "task": "t", "facts": {"status": "complete"}}),
            ),
            rec(
                3,
                member("helper"),
                json!({"to": "lead", "kind": "accept", "task": "t"}),
            ),
            rec(
                4,
                member("helper"),
                json!({"to": "lead", "kind": "question", "task": "t", "body": "?"}),
            ),
            rec(
                5,
                member("lead"),
                json!({"to": "helper", "kind": "decision", "reply_to": ["m4"]}),
            ),
            rec(
                6,
                member("helper"),
                json!({"to": "lead", "kind": "question", "body": "again?"}),
            ),
        ];
        let refs: Vec<&Record> = records.iter().collect();
        let f = fold_records(&m, &refs);
        let t = &f.tasks["t"];
        assert_eq!(t.state, State::Accepted);
        assert_eq!(t.writers, ["helper-1"]);
        assert_eq!(t.records["accept"], "m3");
        assert_eq!(f.anomalies.len(), 1, "{:?}", f.anomalies);
        assert_eq!(f.questions.len(), 1);
        assert_eq!(f.questions[0].id, "m6");
        assert_eq!(f.through.as_deref(), Some("m6"));
        assert_eq!(t.next().unwrap().0, "helper");
    }

    #[test]
    fn authority_is_the_parent_or_an_outside_caller() {
        let m = manifest();
        let ext = Actor::External { name: "ops".into() };
        assert!(check_authority(&m, &member("lead"), "helper", Kind::Assign).is_ok());
        assert_eq!(
            check_authority(&m, &ext, "helper", Kind::Assign)
                .unwrap_err()
                .code,
            Code::Fenced
        );
        assert!(check_authority(&m, &ext, "lead", Kind::Assign).is_ok());
        assert!(check_authority(&m, &member("helper"), "lead", Kind::Assign).is_err());
        assert!(check_authority(&m, &member("lead"), "external:ops", Kind::Decision).is_err());
    }

    #[test]
    fn recorded_liveness_is_gone_only_when_ended() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        assert_eq!(recorded_liveness(&paths, "a", "a-1"), Liveness::Unknown);
        let inc = |ended: bool| {
            json!({"member": "a", "incarnation": "a-1", "generation": 1,
                "started_at": "2026-11-05T19:00:00.000Z",
                "ended_at": if ended { json!("2026-11-05T20:00:00.000Z") } else { Value::Null }})
        };
        let path = paths.current_incarnation("a");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut v = inc(false);
        v.as_object_mut().unwrap().remove("ended_at");
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(recorded_liveness(&paths, "a", "a-1"), Liveness::Unknown);
        std::fs::write(&path, inc(true).to_string()).unwrap();
        assert_eq!(recorded_liveness(&paths, "a", "a-1"), Liveness::Gone);
        assert_eq!(recorded_liveness(&paths, "a", "a-2"), Liveness::Unknown);
    }
}
