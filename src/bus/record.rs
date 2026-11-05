//! The record envelope.
//!
//! A caller supplies a [`Draft`] (what to say, to whom); `brg msg publish`
//! adds identity (schema, run, ID, sequence, time, actor, operation key) and
//! artifact metadata, and stores the result as an immutable [`Record`].
//! Bodies and facts are data: nothing in a record is ever typed into a
//! terminal or executed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::names::is_valid_id;

/// Largest record file brg will write or read.
pub const MAX_RECORD_BYTES: usize = 128 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_FACTS_BYTES: usize = 16 * 1024;
pub const MAX_ARTIFACTS: usize = 32;
pub const MAX_REFS: usize = 32;
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_LABEL_BYTES: usize = 200;

/// Who published a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Actor {
    /// A member, from its launch context.
    Member { member: String, incarnation: String },
    /// An outside caller that named itself with `--external`.
    External { name: String },
    /// brg itself (the attention worker).
    Bus,
}

/// What a record says. Task kinds carry their details in `facts`; their
/// transitions are checked when the task layer folds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A plain message.
    Note,
    Assign,
    Accept,
    Decline,
    Result,
    Verify,
    Release,
    CancelRequest,
    CancelConfirm,
    Reopen,
    Question,
    Decision,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Assign => "assign",
            Kind::Accept => "accept",
            Kind::Decline => "decline",
            Kind::Result => "result",
            Kind::Verify => "verify",
            Kind::Release => "release",
            Kind::CancelRequest => "cancel-request",
            Kind::CancelConfirm => "cancel-confirm",
            Kind::Reopen => "reopen",
            Kind::Question => "question",
            Kind::Decision => "decision",
        }
    }

    /// Kinds that belong to a task and so need `task` (and `attempt`).
    pub fn is_task_kind(self) -> bool {
        !matches!(self, Kind::Note | Kind::Question | Kind::Decision)
    }
}

/// An artifact as the caller names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// An absolute path to a regular file.
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// An artifact as stored: brg measures it at publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub bytes: u64,
    pub sha256: String,
}

/// What a caller hands to `brg msg publish --record FILE|-`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    /// The recipient member.
    pub to: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Record IDs this one answers or follows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    /// Kind-specific structured details (e.g. an assignment's agreement digest).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub facts: Map<String, Value>,
}

/// A stored record. Immutable once written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub run: String,
    /// `m<seq>`.
    pub id: String,
    pub seq: u64,
    /// UTC, RFC 3339.
    pub at: String,
    pub actor: Actor,
    pub key: String,
    pub to: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub facts: Map<String, Value>,
}

pub fn record_id(seq: u64) -> String {
    format!("m{seq}")
}

/// The sequence number in a record ID (`m42` → 42).
pub fn parse_record_id(id: &str) -> Option<u64> {
    let digits = id.strip_prefix('m')?;
    if digits.is_empty() || digits.starts_with('0') || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// Task IDs: `[a-z0-9][a-z0-9._-]{0,63}`.
pub fn is_valid_task_id(id: &str) -> bool {
    super::is_valid_run_id(id)
}

/// Operation keys: 1–128 bytes of `[A-Za-z0-9._:/@#+=-]`.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@#+=-".contains(c))
}

fn has_bad_control(text: &str) -> bool {
    text.chars()
        .any(|c| (c.is_control() && c != '\n' && c != '\t') || c == '\u{7f}')
}

impl Draft {
    /// Checks that need nothing but the draft itself. Topology, references
    /// and transitions are checked against the store at publication.
    pub fn check_shape(&self) -> Result<(), String> {
        if !is_valid_id(&self.to) {
            return Err(format!("`to`: `{}` is not a valid member name", self.to));
        }
        if self.kind.is_task_kind() && self.task.is_none() {
            return Err(format!("a `{}` record needs `task`", self.kind.as_str()));
        }
        if let Some(task) = &self.task
            && !is_valid_task_id(task)
        {
            return Err(format!(
                "`task`: `{task}` must match [a-z0-9][a-z0-9._-]{{0,63}}"
            ));
        }
        if self.attempt == Some(0) {
            return Err("`attempt` starts at 1".into());
        }
        if self.reply_to.len() > MAX_REFS {
            return Err(format!("`reply_to`: at most {MAX_REFS} IDs"));
        }
        if let Some(bad) = self.reply_to.iter().find(|r| parse_record_id(r).is_none()) {
            return Err(format!("`reply_to`: `{bad}` is not a record ID"));
        }
        if self.body.len() > MAX_BODY_BYTES {
            return Err(format!("`body` is over {MAX_BODY_BYTES} bytes"));
        }
        if has_bad_control(&self.body) {
            return Err("`body` contains a control character".into());
        }
        if self.artifacts.len() > MAX_ARTIFACTS {
            return Err(format!("`artifacts`: at most {MAX_ARTIFACTS}"));
        }
        for a in &self.artifacts {
            let shown = a.path.display().to_string();
            if !a.path.is_absolute() {
                return Err(format!("artifact `{shown}` must be an absolute path"));
            }
            if has_bad_control(&shown) || shown.contains('\n') || shown.contains('\t') {
                return Err("an artifact path contains a control character".into());
            }
            if let Some(label) = &a.label
                && (label.len() > MAX_LABEL_BYTES || has_bad_control(label))
            {
                return Err(format!("artifact `{shown}`: bad label"));
            }
        }
        let facts = serde_json::to_vec(&self.facts).map_err(|e| e.to_string())?;
        if facts.len() > MAX_FACTS_BYTES {
            return Err(format!("`facts` is over {MAX_FACTS_BYTES} bytes"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn draft(v: Value) -> Result<Draft, String> {
        serde_json::from_value::<Draft>(v).map_err(|e| e.to_string())
    }

    #[test]
    fn ids() {
        assert_eq!(record_id(42), "m42");
        assert_eq!(parse_record_id("m42"), Some(42));
        assert_eq!(parse_record_id("m0"), None);
        assert_eq!(parse_record_id("m042"), None);
        assert_eq!(parse_record_id("42"), None);
        assert!(is_valid_key("approved-task-1"));
        assert!(is_valid_key("gh:example/repo#12@2026-11-05"));
        assert!(!is_valid_key("has space"));
        assert!(!is_valid_key(""));
        assert!(!is_valid_key(&"k".repeat(129)));
    }

    #[test]
    fn drafts() {
        let d = draft(json!({"to": "lead", "kind": "note", "body": "hello\nthere"})).unwrap();
        d.check_shape().unwrap();
        assert!(
            draft(json!({"to": "lead", "kind": "note", "extra": 1})).is_err(),
            "unknown fields are refused"
        );
        assert!(draft(json!({"to": "lead", "kind": "shout"})).is_err());
        let d = draft(json!({"to": "lead", "kind": "assign"})).unwrap();
        assert!(d.check_shape().unwrap_err().contains("needs `task`"));
        let d = draft(json!({"to": "lead", "kind": "note", "body": "a\u{1b}[201~"})).unwrap();
        assert!(d.check_shape().is_err());
        let d =
            draft(json!({"to": "lead", "kind": "note", "artifacts": [{"path": "rel/x"}]})).unwrap();
        assert!(d.check_shape().is_err());
        let d = draft(json!({"to": "lead", "kind": "note", "reply_to": ["x1"]})).unwrap();
        assert!(d.check_shape().is_err());
        let d = draft(json!({
            "to": "helper", "kind": "assign", "task": "t-1", "attempt": 1,
            "facts": {"agreement": {"path": "/x/agreement.md", "sha256": "ab"}}
        }))
        .unwrap();
        d.check_shape().unwrap();
    }

    #[test]
    fn actor_shape() {
        let a = Actor::Member {
            member: "lead".into(),
            incarnation: "lead-1".into(),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v,
            json!({"type": "member", "member": "lead", "incarnation": "lead-1"})
        );
        assert_eq!(
            serde_json::to_value(Actor::Bus).unwrap(),
            json!({"type": "bus"})
        );
    }
}
