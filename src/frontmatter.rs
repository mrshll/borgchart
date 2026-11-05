//! Splitting a member file into its YAML frontmatter and Markdown body.

/// A member file split into frontmatter text and body text.
#[derive(Debug, PartialEq, Eq)]
pub struct Split {
    pub frontmatter: String,
    pub body: String,
}

/// Split `text` into frontmatter and body.
///
/// The file must start with a `---` line (after an optional byte-order mark);
/// the frontmatter ends at the next line that is exactly `---`. CRLF line
/// endings are accepted and normalized to LF. A `---` later in the body is
/// ordinary body text.
pub fn split(text: &str) -> Result<Split, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = text.replace("\r\n", "\n");
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| "member file must start with a `---` frontmatter line".to_string())?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches('\n') == "---" {
            let frontmatter = rest[..offset].to_string();
            let body = rest[offset + line.len()..].to_string();
            return Ok(Split { frontmatter, body });
        }
        offset += line.len();
    }
    Err("frontmatter is not closed: no second `---` line".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        let s = split("---\nrole: lead\n---\nHello\n").unwrap();
        assert_eq!(s.frontmatter, "role: lead\n");
        assert_eq!(s.body, "Hello\n");
    }

    #[test]
    fn bom_and_crlf() {
        let s = split("\u{feff}---\r\nrole: lead\r\n---\r\nHi\r\n").unwrap();
        assert_eq!(s.frontmatter, "role: lead\n");
        assert_eq!(s.body, "Hi\n");
    }

    #[test]
    fn dashes_in_body_are_body() {
        let s = split("---\nrole: lead\n---\nabove\n---\nbelow\n").unwrap();
        assert_eq!(s.body, "above\n---\nbelow\n");
    }

    #[test]
    fn missing_open() {
        assert!(split("role: lead\n---\n").is_err());
    }

    #[test]
    fn missing_close() {
        assert!(split("---\nrole: lead\nbody\n").is_err());
    }

    #[test]
    fn empty_body_without_trailing_newline() {
        let s = split("---\nrole: lead\n---").unwrap();
        assert_eq!(s.body, "");
    }
}
