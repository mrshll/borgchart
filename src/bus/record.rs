//! Plain message drafts and stored records. Content is data, never executed.

use serde::{Deserialize, Serialize};

use crate::names::is_valid_id;

/// Largest record file brg will write or read.
pub const MAX_RECORD_BYTES: usize = 128 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_REFS: usize = 32;

/// Who a record is addressed to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Recipient {
    Member(String),
    /// `external:<name>`: an outside caller, which reads and acks with
    /// `--external <name>`. Only a member with no parent may address one.
    External(String),
}

pub const EXTERNAL_PREFIX: &str = "external:";

impl Recipient {
    pub fn parse(to: &str) -> Option<Recipient> {
        match to.strip_prefix(EXTERNAL_PREFIX) {
            Some(name) if is_valid_id(name) => Some(Recipient::External(name.to_string())),
            Some(_) => None,
            None if crate::names::is_valid_member_id(to) => Some(Recipient::Member(to.to_string())),
            None => None,
        }
    }
}

impl std::fmt::Display for Recipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Recipient::Member(m) => f.write_str(m),
            Recipient::External(n) => write!(f, "{EXTERNAL_PREFIX}{n}"),
        }
    }
}

/// A message prepared by `brg send`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub to: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<String>,
}

/// A stored message, immutable once published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub ack_required: bool,
    pub schema: u32,
    pub id: String,
    pub seq: u64,
    pub at: String,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<String>,
    pub body: String,
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

fn has_bad_control(text: &str) -> bool {
    text.chars()
        .any(|c| (c.is_control() && c != '\n' && c != '\t') || c == '\u{7f}')
}

impl Draft {
    /// Checks that need nothing but the draft itself. Topology and references
    /// are checked against the store at publication.
    pub fn check_shape(&self) -> Result<(), String> {
        if Recipient::parse(&self.to).is_none() {
            return Err(format!(
                "`to`: `{}` is not a member name or `external:<name>`",
                self.to
            ));
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn draft(v: Value) -> Result<Draft, String> {
        serde_json::from_value::<Draft>(v).map_err(|e| e.to_string())
    }

    #[test]
    fn ids() {
        assert_eq!(record_id(42), "m42");
        assert_eq!(parse_record_id("m42"), Some(42));
        for bad in ["m0", "m042", "42"] {
            assert_eq!(parse_record_id(bad), None);
        }
    }

    #[test]
    fn drafts() {
        draft(json!({"to": "lead", "body": "hello\nthere"}))
            .unwrap()
            .check_shape()
            .unwrap();
        assert!(draft(json!({"to": "lead", "body": "hello", "extra": 1})).is_err());
        assert!(draft(json!({"to": "lead"})).is_err());
        for v in [
            json!({"to": "lead", "body": "a\u{1b}[201~"}),
            json!({"to": "lead", "body": "hi", "reply_to": ["x1"]}),
        ] {
            assert!(draft(v).unwrap().check_shape().is_err());
        }
    }

    #[test]
    fn recipients() {
        assert_eq!(
            Recipient::parse("lead"),
            Some(Recipient::Member("lead".into()))
        );
        assert_eq!(
            Recipient::parse("external:ops"),
            Some(Recipient::External("ops".into()))
        );
        for bad in ["external:", "external:Bad", "Lead"] {
            assert_eq!(Recipient::parse(bad), None);
        }
        assert_eq!(
            Recipient::External("ops".into()).to_string(),
            "external:ops"
        );
    }
}
