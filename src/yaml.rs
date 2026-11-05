//! The one strict YAML parser configuration used for every borgchart file.

use serde::de::DeserializeOwned;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options};

/// Files larger than this are refused before parsing.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

fn options() -> Options {
    let mut opts = Options::default();
    opts.duplicate_keys = DuplicateKeyPolicy::Error;
    opts.merge_keys = MergeKeyPolicy::Error;
    // `yes`, `no`, `on` and `off` stay strings, so `start: no` is an error, not false.
    opts.strict_booleans = true;
    opts.budget = Some(serde_saphyr::Budget::default());
    let mut aliases = opts.alias_limits;
    aliases.max_total_replayed_events = 10_000;
    aliases.max_alias_expansions_per_anchor = 100;
    aliases.max_replay_stack_depth = 16;
    opts.alias_limits = aliases;
    opts
}

/// Parse one YAML document strictly into `T`.
pub fn parse<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    if text.len() > MAX_FILE_BYTES {
        return Err(format!("file is larger than {MAX_FILE_BYTES} bytes"));
    }
    serde_saphyr::from_str_with_options(text, options()).map_err(|e| tidy(&e.to_string()))
}

/// The parser's message without its own `error:` prefix or `<input>` snippet header.
fn tidy(message: &str) -> String {
    let first = message.lines().next().unwrap_or(message);
    first
        .strip_prefix("error: ")
        .unwrap_or(first)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::IgnoredAny;

    #[test]
    fn duplicate_keys_error() {
        let err = parse::<IgnoredAny>("a: 1\na: 2\n").unwrap_err();
        assert!(err.to_lowercase().contains("duplicate"), "{err}");
    }

    #[test]
    fn yes_is_a_string_not_a_boolean() {
        #[derive(serde::Deserialize)]
        struct T {
            #[allow(dead_code)]
            start: bool,
        }
        assert!(parse::<T>("start: no\n").is_err());
        assert!(parse::<T>("start: false\n").is_ok());
    }

    #[test]
    fn merge_keys_error() {
        let err = parse::<IgnoredAny>("a: &b {x: 1}\nc:\n  <<: *b\n").unwrap_err();
        assert!(err.to_lowercase().contains("merge"), "{err}");
    }

    #[test]
    fn anchor_bomb_errors() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for w in names.windows(2) {
            let (prev, next) = (w[0], w[1]);
            let refs = vec![format!("*{prev}"); 10].join(", ");
            bomb.push_str(&format!("{next}: &{next} [{refs}]\n"));
        }
        assert!(parse::<IgnoredAny>(&bomb).is_err());
    }

    #[test]
    fn oversized_input_is_refused() {
        let big = "x".repeat(MAX_FILE_BYTES + 1);
        assert!(parse::<IgnoredAny>(&big).unwrap_err().contains("larger"));
    }
}
