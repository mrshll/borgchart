//! Conservative explicit-address scanner. Ambiguous Markdown fails closed.
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
macro_rules! pattern {
    ($name:ident, $value:literal) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($value).expect("constant regex"));
    };
}
pattern!(LOGIN, r"^[A-Za-z0-9][A-Za-z0-9-]{0,38}$");
pattern!(NEWLINE, r"\r\n?|\n");
pattern!(ADDRESS, r"^ {0,3}@([A-Za-z0-9-]+)");
pattern!(ADDRESS_END, r"^[:,.!?;)\]]*(?:[ \t]|$)");
pattern!(LIST, r"^(?:[-+*]|[0-9]{1,9}[.)])[ \t]");
pattern!(TICKS, r"`+");
pattern!(BRACKET, r"\\.|[\[\]()]");
pattern!(TAG_OPEN, r"<[A-Za-z/!?]");
pattern!(DOLLAR, r"\\.|\$");
// Python's Unicode whitespace also includes the four information separators.
pattern!(
    TAG,
    r"<!--|-->|<(/?)([A-Za-z][A-Za-z0-9]*)([\s\x1c-\x1f/>]|$)"
);
pattern!(
    PLAIN,
    r"^[^\s\x1c-\x1f`<>\[\]()$\\|~=*+#@0-9-][^`<>\[\]()$\\|]*$"
);
pattern!(HTML_NAME, r"^</?([a-z][a-z0-9-]*)");
pattern!(SETEXT, r"^ {0,3}(?:=+|-+)[ \t]*$");
pattern!(
    TABLE,
    r"^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$"
);
const RAW: &[&str] = &[
    "a",
    "code",
    "pre",
    "script",
    "style",
    "textarea",
    "kbd",
    "samp",
    "tt",
    "xmp",
    "plaintext",
    "listing",
    "noscript",
    "template",
    "iframe",
    "svg",
    "math",
    "title",
    "noembed",
    "noframes",
];
const TYPE6: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];
fn indent(line: &str, mut pos: usize) -> (usize, usize) {
    let mut col = 0;
    while let Some(b) = line.as_bytes().get(pos) {
        match b {
            b' ' => col += 1,
            b'\t' => col += 4 - col % 4,
            _ => break,
        }
        pos += 1;
    }
    (col, pos)
}
fn containers(line: &str) -> (usize, usize, bool) {
    let (first, mut pos) = indent(line, 0);
    let mut markers = false;
    loop {
        if line[pos..].starts_with('>') {
            pos += 1;
        } else if let Some(m) = LIST.find(&line[pos..]) {
            pos += m.end() - 1;
        } else {
            return (first, pos, markers);
        }
        markers = true;
        pos = indent(line, pos).1;
    }
}
fn fence_like(line: &str, pos: usize) -> bool {
    line[pos..].starts_with("```") || line[pos..].starts_with("~~~")
}
fn blank(line: &str) -> bool {
    line.trim_matches([' ', '\t']).is_empty()
}
fn inline(line: &str) -> Result<(bool, String), ()> {
    let mut tainted = false;
    let mut masked = line.to_owned();
    if line.contains('`') {
        tainted = line.contains('<');
        let runs: Vec<_> = TICKS.find_iter(line).collect();
        let mut spans = Vec::new();
        let mut i = 0;
        let mut valid = !line.contains("\\`");
        while valid && i < runs.len() {
            let close = (i + 1..runs.len()).find(|&j| runs[j].len() == runs[i].len());
            if let Some(j) = close {
                spans.push((runs[i].start(), runs[j].end()));
                i = j + 1;
            } else {
                valid = false;
            }
        }
        if !valid {
            tainted = true;
        } else {
            let mut bytes = line.as_bytes().to_vec();
            for (start, end) in spans {
                bytes[start..end].fill(b' ');
            }
            masked = String::from_utf8(bytes).map_err(|_| ())?;
        }
    }
    if masked.matches("$$").count() % 2 == 1 {
        return Err(());
    }
    if tainted || masked.trim_start_matches([' ', '\t']).starts_with('[') {
        return Ok((true, masked));
    }
    let (mut square, mut paren) = (0usize, 0usize);
    for m in BRACKET.find_iter(&masked) {
        match m.as_str() {
            "[" => square += 1,
            "(" => paren += 1,
            "]" => square = square.saturating_sub(1),
            ")" => paren = paren.saturating_sub(1),
            _ => {}
        }
    }
    if square > 0 || paren > 0 {
        return Ok((true, masked));
    }
    if let Some(m) = TAG_OPEN.find_iter(&masked).last()
        && !masked[m.start()..].contains('>')
    {
        return Ok((true, masked));
    }
    let odd = DOLLAR
        .find_iter(&masked)
        .filter(|m| m.as_str() == "$")
        .count()
        % 2
        == 1;
    Ok((odd, masked))
}
fn html_start(line: &str, pos: usize) -> (Option<String>, bool, usize) {
    let lower = line.to_ascii_lowercase();
    let rest = &line[pos..];
    for (start, end) in [("<!--", "-->"), ("<?", "?>"), ("<![CDATA[", "]]>")] {
        if rest.starts_with(start) {
            return (Some(end.into()), true, pos + start.len());
        }
    }
    if rest.starts_with("<!") {
        return (
            Some(">".into()),
            line.as_bytes()
                .get(pos + 2)
                .is_some_and(u8::is_ascii_alphabetic),
            pos + 2,
        );
    }
    let Some(m) = HTML_NAME.captures(&lower[pos..]) else {
        return (None, false, pos);
    };
    let name = &m[1];
    let end = pos + m.get(0).unwrap().end();
    let tail = &lower[end..];
    let bounded = tail.is_empty() || tail.starts_with([' ', '\t', '>']);
    if !rest.starts_with("</") && ["pre", "script", "style", "textarea"].contains(&name) && bounded
    {
        return (Some(format!("</{name}>")), true, end);
    }
    (
        None,
        TYPE6.contains(&name) && (bounded || tail.starts_with("/>")),
        end,
    )
}
#[derive(Default)]
struct Scanner {
    login: String,
    tainted: bool,
    table: bool,
    candidates: Vec<usize>,
    quote: bool,
    fence: Option<(u8, usize, usize)>,
    ends: BTreeSet<String>,
    region_blank: bool,
    trusted: bool,
    comment: bool,
    raw: BTreeMap<String, usize>,
}
impl Scanner {
    fn region(&self) -> bool {
        !self.ends.is_empty() || self.region_blank
    }
    fn masked(&self) -> bool {
        self.comment || self.raw.values().any(|&v| v > 0)
    }
    fn finish(&mut self) -> bool {
        let found = !self.candidates.is_empty() && !self.table;
        self.candidates.clear();
        self.table = false;
        found
    }
    fn tags(&mut self, text: &str, closers: bool) {
        // Resume at the end of the tag name: the boundary character is a lookahead in Python.
        let mut pos = 0;
        while pos < text.len() {
            let Some(c) = TAG.captures(&text[pos..]) else {
                break;
            };
            let m = c.get(0).unwrap();
            let token = m.as_str();
            let end = if let Some(name) = c.get(2) {
                name.end()
            } else {
                m.end()
            };
            if self.comment {
                self.comment = token != "-->";
            } else if token == "<!--" {
                self.comment = true;
            } else if token != "-->" {
                let name = c[2].to_ascii_lowercase();
                if RAW.contains(&name.as_str()) {
                    let depth = self.raw.entry(name).or_default();
                    if c[1].is_empty() {
                        *depth += 1;
                    } else if closers && *depth > 0 {
                        *depth -= 1;
                    }
                }
            }
            pos += end;
        }
    }
    fn require(&mut self, line: &str, end: Option<String>, search: usize) {
        if let Some(end) = end {
            if !line.to_ascii_lowercase()[search..].contains(&end) {
                self.ends.insert(end);
            }
        } else {
            self.region_blank = true;
        }
    }
    fn after_region(&mut self) {
        if !self.region() {
            self.tainted = !self.trusted;
        }
    }
    fn line(&mut self, index: usize, line: &str) -> Result<bool, ()> {
        if let Some((ch, len, opener)) = self.fence {
            if blank(line) {
                return Ok(false);
            }
            let (ind, pos) = indent(line, 0);
            if ind < opener {
                return Err(());
            }
            let end = pos
                + line.as_bytes()[pos..]
                    .iter()
                    .take_while(|&&b| b == ch)
                    .count();
            if end - pos >= len && blank(&line[end..]) {
                if ind <= 3 {
                    self.fence = None;
                    self.tainted = false;
                } else if ind <= 3 + opener {
                    return Err(());
                }
            }
            return Ok(false);
        }
        if !self.region() && !self.quote && PLAIN.is_match(line) {
            return Ok(false);
        }
        if blank(line) {
            self.quote = false;
            self.region_blank = false;
            self.tainted = false;
            return Ok(self.finish());
        }
        if self.region() {
            let lower = line.to_ascii_lowercase();
            self.ends.retain(|end| !lower.contains(end));
            if !self.trusted {
                let (_, pos, _) = containers(line);
                if fence_like(line, pos) {
                    return Err(());
                }
                if TAG_OPEN.find(&line[pos..]).is_some_and(|m| m.start() == 0) {
                    let (end, _, search) = html_start(line, pos);
                    self.require(line, end, search);
                }
            }
            self.tags(line, self.trusted);
            self.after_region();
            return Ok(false);
        }
        let (ind, pos) = indent(line, 0);
        if ind <= 3 && line[pos..].starts_with('>') {
            self.quote = true;
            self.tags(line, false);
            return Ok(self.finish());
        }
        let (first, pos, markers) = containers(line);
        let fence = fence_like(line, pos);
        let html = TAG_OPEN.find(&line[pos..]).is_some_and(|m| m.start() == 0);
        if self.quote {
            if !fence && !html {
                self.tags(line, false);
                return Ok(false);
            }
            self.quote = false;
        }
        if fence {
            if self.finish() {
                return Ok(true);
            }
            let ch = line.as_bytes()[pos];
            let end = pos
                + line.as_bytes()[pos..]
                    .iter()
                    .take_while(|&&b| b == ch)
                    .count();
            if markers || first > 3 || (ch == b'`' && line[end..].contains('`')) {
                return Err(());
            }
            self.fence = Some((ch, end - pos, first));
            return Ok(false);
        }
        if html {
            let found = self.finish();
            let (end, known, search) = html_start(line, pos);
            self.trusted = first == 0 && !markers && known;
            self.require(line, end, search);
            self.tags(line, self.trusted);
            self.after_region();
            return Ok(found);
        }
        let clean = !self.tainted;
        if clean
            && !self.masked()
            && let Some(m) = ADDRESS.captures(line)
            && m[1].eq_ignore_ascii_case(&self.login)
            && ADDRESS_END.is_match(&line[m.get(0).unwrap().end()..])
        {
            self.candidates.push(index);
        }
        if SETEXT.is_match(line)
            && index
                .checked_sub(1)
                .is_some_and(|i| self.candidates.last() == Some(&i))
        {
            self.candidates.pop();
        }
        if line.contains('|') && TABLE.is_match(line) {
            self.table = true;
        }
        let (tainted, masked) = inline(line)?;
        self.tainted |= tainted;
        self.tags(&masked, clean && !tainted);
        Ok(false)
    }
}
pub fn explicit_mention(body: &str, login: &str) -> bool {
    if !LOGIN.is_match(login)
        || !body
            .to_ascii_lowercase()
            .contains(&format!("@{}", login.to_ascii_lowercase()))
    {
        return false;
    }
    let mut scanner = Scanner {
        login: login.to_ascii_lowercase(),
        ..Default::default()
    };
    for (i, line) in NEWLINE.split(body).enumerate() {
        match scanner.line(i, line) {
            Ok(true) => return true,
            Err(()) => return false,
            _ => {}
        }
    }
    scanner.finish()
}
