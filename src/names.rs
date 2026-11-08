//! Name rules shared by members, plugins, actions and environment variables.

/// Member, plugin and action names: `[a-z][a-z0-9_-]{0,31}`.
/// They become tmux window names, file names and environment values.
pub fn is_valid_id(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 32
        && first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A role must not also read as a member of another declared role.
pub fn conflicts_with_member_id(name: &str, is_role: impl Fn(&str) -> bool) -> bool {
    crate::registry::split_id(name).is_some_and(|(base, _)| is_role(base))
}

/// Member and instance IDs: like [`is_valid_id`], up to 40 characters, so
/// a 32-character role or team name with its `-<suffix>` fits.
pub fn is_valid_member_id(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 40
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Environment variable names: `[A-Z_][A-Z0-9_]*`.
pub fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A word that needs no shell quoting: `[A-Za-z0-9._:/@%+=,-]+`.
pub fn is_plain_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@%+=,-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_shaped_role_names_are_rejected() {
        let is_role = |name: &str| ["code-review", "lead"].contains(&name);
        assert!(conflicts_with_member_id("code-review-k3f9", is_role));
        assert!(conflicts_with_member_id("lead-2222", is_role));
        for name in [
            "code-review",
            "other-k3f9",
            "lead-k3f0",
            "lead-k3f",
            "lead-k3f99",
        ] {
            assert!(!conflicts_with_member_id(name, is_role), "{name}");
        }
    }

    #[test]
    fn ids() {
        assert!(is_valid_id("triager"));
        assert!(is_valid_id("reviewer-2"));
        assert!(!is_valid_id("Triager"));
        assert!(!is_valid_id("2nd"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id(&"a".repeat(33)));
    }

    #[test]
    fn env_names() {
        assert!(is_valid_env_name("SLACK_BOT_TOKEN"));
        assert!(is_valid_env_name("_X1"));
        assert!(!is_valid_env_name("slack"));
        assert!(!is_valid_env_name("1X"));
    }

    #[test]
    fn plain_words() {
        assert!(is_plain_word("slackbot"));
        assert!(is_plain_word("1234.5678"));
        assert!(!is_plain_word("$HOME"));
        assert!(!is_plain_word("a;b"));
        assert!(!is_plain_word(""));
    }
}
