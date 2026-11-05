//! The coordination bus: durable records members and outside callers
//! publish to each other, kept under a per-run root outside any checkout.
//!
//! This module holds the shared interfaces (configuration, on-disk layout,
//! caller context, record envelope, outcome codes and the no-replace write
//! primitive). See `docs/bus.md` for the contract each piece builds on.

pub mod context;
pub mod fsx;
pub mod layout;
pub mod manifest;
pub mod outcome;
pub mod record;
pub mod time;

/// The on-disk schema of run manifests and records written by this brg.
pub const SCHEMA: u32 = 1;

/// Run IDs: `[a-z0-9][a-z0-9._-]{0,63}`. They name a directory.
pub fn is_valid_run_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids() {
        assert!(is_valid_run_id("example-run-1"));
        assert!(is_valid_run_id("2026.11.05_a"));
        assert!(!is_valid_run_id(""));
        assert!(!is_valid_run_id("-run"));
        assert!(!is_valid_run_id("Run"));
        assert!(!is_valid_run_id("a/b"));
        assert!(!is_valid_run_id(".."));
        assert!(!is_valid_run_id(&"a".repeat(65)));
    }
}
