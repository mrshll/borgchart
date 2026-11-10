//! Exit codes and JSON results for the bus command family. These are their
//! own codes; they are not the same as `brg send`'s.

use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Code {
    Ok,
    /// An I/O or other unexpected error.
    Error,
    Usage,
    /// No `bus:` in the team, or its store has not been initialised.
    NotConfigured,
    /// The caller's context doesn't allow this (e.g. `--external` inside a member).
    Refused,
    /// The record or request failed validation.
    Invalid,
    /// The store is unreadable or has a gap: mutations are stopped.
    Corrupt,
    /// A stale actor or incarnation: the mutation was fenced off.
    Fenced,
    /// A bounded wait ended with nothing new.
    Timeout,
    NotFound,
}

impl Code {
    pub fn exit(self) -> u8 {
        match self {
            Code::Ok => 0,
            Code::Error => 1,
            Code::Usage => 2,
            Code::NotConfigured => 3,
            Code::Refused => 4,
            Code::Invalid => 5,
            Code::Corrupt => 7,
            Code::Fenced => 8,
            Code::Timeout => 9,
            Code::NotFound => 10,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Code::Ok => "ok",
            Code::Error => "error",
            Code::Usage => "usage",
            Code::NotConfigured => "not-configured",
            Code::Refused => "refused",
            Code::Invalid => "invalid",
            Code::Corrupt => "corrupt",
            Code::Fenced => "fenced",
            Code::Timeout => "timeout",
            Code::NotFound => "not-found",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub message: String,
}

impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Failure {
            code,
            message: message.into(),
        }
    }

    pub fn io(context: &str, e: std::io::Error) -> Self {
        Failure::new(Code::Error, format!("{context}: {e}"))
    }

    /// `{"ok": false, "code": <exit>, "outcome": "<name>", "message": …}`
    pub fn to_json(&self) -> Value {
        json!({
            "ok": false,
            "code": self.code.exit(),
            "outcome": self.code.as_str(),
            "message": self.message,
        })
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// `{"ok": true, "code": 0, "outcome": <outcome>, …data}`. `data` must be an
/// object; its fields are merged in.
pub fn ok_json(outcome: &str, data: Value) -> Value {
    let mut out = json!({"ok": true, "code": 0, "outcome": outcome});
    if let (Some(map), Value::Object(extra)) = (out.as_object_mut(), data) {
        map.extend(extra);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_distinct() {
        let all = [
            Code::Ok,
            Code::Error,
            Code::Usage,
            Code::NotConfigured,
            Code::Refused,
            Code::Invalid,
            Code::Corrupt,
            Code::Fenced,
            Code::Timeout,
            Code::NotFound,
        ];
        let mut exits: Vec<u8> = all.iter().map(|c| c.exit()).collect();
        exits.sort();
        exits.dedup();
        assert_eq!(exits.len(), all.len());
        let f = Failure::new(Code::Invalid, "x").to_json();
        assert_eq!(f["code"], 5);
        assert_eq!(f["outcome"], "invalid");
        let ok = ok_json("committed", json!({"id": "m1"}));
        assert_eq!(ok["id"], "m1");
        assert_eq!(ok["ok"], true);
    }
}
