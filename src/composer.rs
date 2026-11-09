//! Reading a member's input box ("composer") from its screen.
//!
//! The guarded wake only types into an input box it can read as empty, so
//! this module has to tell an empty box from one holding a draft. Both
//! vendors draw placeholder text in the empty box, and idle screens can show
//! text that looks like a prompt, so the reading works on a styled capture
//! (`tmux capture-pane -e`) and anchors on each vendor's own layout at the
//! bottom of the screen:
//!
//! - **Claude Code**: the box is the lines between the last two horizontal
//!   rules; its first line starts with `❯`, later lines are indented two
//!   spaces, and the footer under the bottom rule shows `-- INSERT --` in vim
//!   insert mode. A grey `❯` means a turn is running.
//! - **Codex**: the box ends one blank line above the status line (the one
//!   ending in `Vim: Insert` or `Vim: Normal`); its first line starts with a
//!   bold `›` and later lines are indented two spaces. Placeholder text is
//!   drawn dim.
//!
//! Anything that doesn't fit the layout reads as `Unknown`. Any text in the
//! box that isn't the vendor's dim placeholder reads as a draft, including
//! recalled history, pasted-image tags and suggestions, and so does a box of
//! more than one line, even if the visible lines are blank (the rest may be
//! scrolled out of view). A pane too short to show the whole layout reads as
//! `Unknown`.

use crate::team::Vendor;

/// The fewest rows a pane needs for its input box to be read.
pub const MIN_ROWS: usize = 20;
/// The most lines an input box is searched for.
const MAX_BOX_LINES: usize = 60;

/// A cell's foreground colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fg {
    #[default]
    Default,
    /// SGR 30–37 / 90–97, as 0–15.
    Basic(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Fg,
    pub bold: bool,
    pub dim: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

/// One screen line as styled cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line(pub Vec<Cell>);

impl Line {
    pub fn text(&self) -> String {
        self.0.iter().map(|c| c.ch).collect()
    }
    fn is_blank(&self) -> bool {
        self.0.iter().all(|c| c.ch.is_whitespace())
    }
    fn cells_from(&self, n: usize) -> &[Cell] {
        &self.0[n.min(self.0.len())..]
    }
}

fn apply_sgr(style: &mut Style, params: &str) {
    let nums: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let mut i = 0;
    while i < nums.len() {
        match nums[i] {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            n @ 30..=37 => style.fg = Fg::Basic((n - 30) as u8),
            n @ 90..=97 => style.fg = Fg::Basic((n - 90 + 8) as u8),
            39 => style.fg = Fg::Default,
            38 | 48 => {
                let fg = nums[i] == 38;
                match nums.get(i + 1) {
                    Some(5) => {
                        if fg && let Some(&n) = nums.get(i + 2) {
                            style.fg = Fg::Indexed(n as u8);
                        }
                        i += 2;
                    }
                    Some(2) => {
                        if fg && i + 4 < nums.len() {
                            style.fg =
                                Fg::Rgb(nums[i + 2] as u8, nums[i + 3] as u8, nums[i + 4] as u8);
                        }
                        i += 4;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Split a `capture-pane -e` capture into styled lines. Style carries across
/// lines, as tmux writes it. Escape sequences other than SGR are dropped.
pub fn parse(capture: &str) -> Vec<Line> {
    let mut lines = vec![Line::default()];
    let mut style = Style::default();
    let mut chars = capture.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut fin = None;
                    for p in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&p) {
                            fin = Some(p);
                            break;
                        }
                        params.push(p);
                    }
                    if fin == Some('m') {
                        apply_sgr(&mut style, &params);
                    }
                }
                Some(']') => {
                    // OSC: up to BEL or ST.
                    while let Some(p) = chars.next() {
                        if p == '\x07' {
                            break;
                        }
                        if p == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => lines.push(Line::default()),
            '\r' => {}
            c if c.is_control() => {}
            c => lines.last_mut().unwrap().0.push(Cell { ch: c, style }),
        }
    }
    if capture.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// The plain text of a styled capture, for the state rules.
pub fn plain(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(l.text().trim_end());
        out.push('\n');
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Insert,
    Normal,
}

/// The pane's width and cursor, read with the capture. Without them the
/// readings that depend on them fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: usize,
    pub cursor_x: usize,
    pub cursor_y: usize,
}

/// What the input box shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The box's lines, without the prompt marker or continuation indent.
    /// Leading spaces are kept; trailing spaces can't be seen in a capture
    /// (the cursor stands in for them).
    pub lines: Vec<String>,
    /// The single line is the vendor's dim placeholder.
    pub placeholder: bool,
    /// The vendor marks a running turn on the box itself.
    pub busy: bool,
    pub vim: Option<VimMode>,
    /// The cursor as (box line, column after the indent), if it is in the box.
    pub cursor: Option<(usize, usize)>,
    /// Columns available to text on each box line.
    pub text_width: Option<usize>,
    vendor: Vendor,
}

/// The reading the wake decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Empty,
    Draft,
    Busy,
    Unknown(String),
}

impl View {
    pub fn reading(&self) -> Reading {
        if self.busy {
            return Reading::Busy;
        }
        if self.placeholder {
            return Reading::Empty;
        }
        if self.lines.len() != 1 || !self.lines[0].trim().is_empty() {
            return Reading::Draft;
        }
        match self.vendor {
            // Codex draws its placeholder only while the box is empty, so a
            // blank box without it holds typed whitespace.
            Vendor::Codex => Reading::Draft,
            // Claude has no placeholder; typed spaces look like nothing. In
            // insert mode they move the cursor; in normal mode a single
            // space leaves it where an empty box would.
            Vendor::Claude => match (self.vim, self.cursor) {
                (_, Some((0, x))) if x > 0 => Reading::Draft,
                (Some(VimMode::Insert), Some((0, 0))) => Reading::Empty,
                (Some(VimMode::Normal), _) => Reading::Unknown(
                    "in vim normal mode an empty input box looks like one holding a space".into(),
                ),
                _ => Reading::Unknown("the cursor isn't where an empty input box puts it".into()),
            },
        }
    }

    /// Verify a submitted message, including hard newlines and soft wrapping.
    /// The caller separately checks pane identity, dialogs and turn state.
    pub fn holds_submission(&self, text: &str) -> bool {
        if self.placeholder || self.lines.is_empty() || self.vim == Some(VimMode::Normal) {
            return false;
        }
        let (Some(width), Some(cursor)) = (self.text_width, self.cursor) else {
            return false;
        };
        let last = self.lines.len() - 1;
        if cursor != (last, self.lines[last].chars().count()) {
            return false;
        }
        let mut rest = text;
        for (i, line) in self.lines.iter().enumerate() {
            let Some(tail) = rest.strip_prefix(line) else {
                return false;
            };
            rest = tail;
            if i == last {
                return rest.is_empty();
            }
            if let Some(tail) = rest.strip_prefix('\n') {
                rest = tail;
                continue;
            }
            let next = &self.lines[i + 1];
            let word = next.split(' ').next().unwrap_or("");
            if !line.ends_with(' ')
                && !next.starts_with(' ')
                && !word.is_empty()
                && line.chars().count() + 1 + word.chars().count() > width
                && let Some(tail) = rest.strip_prefix(' ')
            {
                rest = tail;
                continue;
            }
            // A word longer than the pane wraps without an inserted space.
            if line.chars().count() != width {
                return false;
            }
        }
        false
    }

    /// Whether `text`, wrapped as the vendors wrap it (at the last space
    /// that keeps a line within the box, a longer word cut at the edge),
    /// would end exactly at the box's right edge. The cursor after it would
    /// then be past the pane's last column, wherever the vendor puts it, so
    /// [`holds`](Self::holds) and [`holds_submission`](Self::holds_submission)
    /// could never confirm it: check before typing, not after.
    pub fn ends_at_the_edge(&self, text: &str) -> bool {
        let Some(width) = self.text_width.filter(|width| *width > 0) else {
            return false;
        };
        let last = text.rsplit('\n').next().unwrap_or("");
        let mut line = 0;
        for (i, word) in last.split(' ').enumerate() {
            let n = word.chars().count();
            line = if i == 0 {
                n
            } else if line + 1 + n <= width {
                line + 1 + n
            } else {
                n
            };
            while line > width {
                line -= width;
            }
        }
        line == width
    }

    /// Conservative ASCII-width estimate, not a vendor paste limit.
    /// Wide/combining characters can fail the exact post-paste check.
    /// Larger messages remain in the inbox without leaving a collapsed draft.
    pub fn can_verify_submission(&self, text: &str, rows: usize) -> bool {
        let Some(width) = self.text_width.filter(|width| *width > 0) else {
            return false;
        };
        text.len() <= 1024
            && !self.ends_at_the_edge(text)
            && text
                .split('\n')
                .map(|line| line.chars().count().max(1).div_ceil(width))
                .sum::<usize>()
                <= (rows / 2).min(10)
    }

    /// The box holds exactly `text`. A line break is accepted only where the
    /// vendor must have wrapped (the next word could not have fitted), and
    /// the cursor must sit right after the text, so trailing spaces show.
    pub fn holds(&self, text: &str) -> bool {
        if self.placeholder || self.busy {
            return false;
        }
        let (Some(width), Some(cursor)) = (self.text_width, self.cursor) else {
            return false;
        };
        let last = self.lines.len() - 1;
        if cursor != (last, self.lines[last].chars().count()) {
            return false;
        }
        let mut joined = self.lines[0].clone();
        for pair in self.lines.windows(2) {
            let (line, next) = (&pair[0], &pair[1]);
            let word = next.split(' ').next().unwrap_or("");
            let soft = !line.is_empty()
                && !line.ends_with(' ')
                && !next.starts_with(' ')
                && !word.is_empty()
                && line.chars().count() + 1 + word.chars().count() > width;
            if !soft {
                return false;
            }
            joined.push(' ');
            joined.push_str(next);
        }
        joined == text
    }
}

/// Read the input box from a styled capture.
pub fn view(vendor: Vendor, capture: &str, geometry: Option<Geometry>) -> Result<View, String> {
    let mut lines = parse(capture);
    if lines.len() < MIN_ROWS {
        return Err(format!(
            "the pane has {} rows; at least {MIN_ROWS} are needed to read its input box",
            lines.len()
        ));
    }
    while lines.last().is_some_and(Line::is_blank) {
        lines.pop();
    }
    let (first, mut view) = match vendor {
        Vendor::Claude => claude_view(&lines)?,
        Vendor::Codex => codex_view(&lines)?,
    };
    if let Some(g) = geometry {
        view.text_width = Some(g.width.saturating_sub(2));
        let row = g.cursor_y.checked_sub(first);
        view.cursor = row
            .filter(|&r| r < view.lines.len() && g.cursor_x >= 2)
            .map(|r| (r, g.cursor_x - 2));
    }
    Ok(view)
}

/// Read the input box straight to a decision.
pub fn read(vendor: Vendor, capture: &str, geometry: Option<Geometry>) -> Reading {
    match view(vendor, capture, geometry) {
        Ok(v) => v.reading(),
        Err(why) => Reading::Unknown(why),
    }
}

fn is_rule(line: &Line) -> bool {
    let t = line.text();
    let t = t.trim();
    t.chars().count() >= 10 && t.starts_with("──") && t.ends_with("──")
}

/// Text after the first `n` cells, without the padding at the end.
fn text_after(line: &Line, n: usize) -> String {
    line.cells_from(n)
        .iter()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Continuation lines are indented two spaces (or blank).
fn continuation(line: &Line) -> Option<String> {
    if line.is_blank() {
        return Some(String::new());
    }
    line.text().starts_with("  ").then(|| text_after(line, 2))
}

/// The box's first screen row and its view (geometry not yet applied).
fn claude_view(lines: &[Line]) -> Result<(usize, View), String> {
    let n = lines.len();
    let bottom = (n.saturating_sub(4)..n)
        .rev()
        .find(|&i| is_rule(&lines[i]))
        .ok_or("no input box (no rule above the footer)")?;
    let footer: Vec<String> = lines[bottom + 1..].iter().map(Line::text).collect();
    let top = (bottom.saturating_sub(MAX_BOX_LINES + 1)..bottom)
        .rev()
        .find(|&i| is_rule(&lines[i]))
        .ok_or("no input box (no top rule)")?;
    let rows = &lines[top + 1..bottom];
    let first = rows.first().ok_or("the input box has no lines")?;
    let marker = first.0.first().ok_or("the input box has no prompt")?;
    if marker.ch != '❯' {
        return Err(format!(
            "the input box starts with `{}`, not `❯`",
            marker.ch
        ));
    }
    if !matches!(first.0.get(1).map(|c| c.ch), Some(' ' | '\u{a0}') | None) {
        return Err("no space after the prompt".into());
    }
    let busy = match (marker.style.fg, marker.style.dim) {
        (Fg::Default, false) => false,
        (Fg::Indexed(246), _) | (_, true) => true,
        _ => return Err("the prompt has an unfamiliar colour".into()),
    };
    let mut text = vec![text_after(first, 2)];
    for row in &rows[1..] {
        text.push(continuation(row).ok_or("an input box line isn't indented")?);
    }
    let vim = if footer.is_empty() {
        None
    } else if footer.iter().any(|l| l.contains("-- INSERT --")) {
        Some(VimMode::Insert)
    } else {
        Some(VimMode::Normal)
    };
    Ok((
        top + 1,
        View {
            lines: text,
            placeholder: false,
            busy,
            vim,
            cursor: None,
            text_width: None,
            vendor: Vendor::Claude,
        },
    ))
}

fn codex_view(lines: &[Line]) -> Result<(usize, View), String> {
    let n = lines.len();
    let status = (n.saturating_sub(3)..n)
        .rev()
        .find(|&i| {
            let t = lines[i].text();
            let t = t.trim_end();
            t.ends_with("Vim: Insert") || t.ends_with("Vim: Normal")
        })
        .ok_or("no status line under the input box")?;
    let vim = if lines[status].text().trim_end().ends_with("Insert") {
        VimMode::Insert
    } else {
        VimMode::Normal
    };
    if status < 2 || !lines[status - 1].is_blank() {
        return Err("no blank line between the input box and the status line".into());
    }
    let last = status - 2;
    let start = (last.saturating_sub(MAX_BOX_LINES)..=last)
        .rev()
        .find(|&i| lines[i].0.first().is_some_and(|c| c.ch == '›'))
        .ok_or("no `›` prompt above the status line")?;
    let first = &lines[start];
    if !first.0[0].style.bold {
        return Err("the `›` prompt isn't drawn bold".into());
    }
    if !matches!(first.0.get(1).map(|c| c.ch), Some(' ') | None) {
        return Err("no space after the prompt".into());
    }
    let visible: Vec<&Cell> = first
        .cells_from(2)
        .iter()
        .filter(|c| !c.ch.is_whitespace())
        .collect();
    let mut text = vec![text_after(first, 2)];
    for row in &lines[start + 1..=last] {
        text.push(continuation(row).ok_or("an input box line isn't indented")?);
    }
    let placeholder = text.len() == 1 && !visible.is_empty() && visible.iter().all(|c| c.style.dim);
    Ok((
        start,
        View {
            lines: text,
            placeholder,
            busy: false,
            vim: Some(vim),
            cursor: None,
            text_width: None,
            vendor: Vendor::Codex,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_state_carries_across_lines() {
        let lines = parse("\x1b[38;5;244m──\n\x1b[39m❯ a \x1b[2mdim\x1b[0m\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0[0].style.fg, Fg::Indexed(244));
        assert_eq!(lines[1].0[0].style.fg, Fg::Default);
        assert!(lines[1].0[4].style.dim);
        assert_eq!(plain(&lines), "──\n❯ a dim\n");
    }

    #[test]
    fn other_escapes_are_dropped() {
        let lines = parse("a\x1b]8;;http://x\x1b\\b\x1b[2Kc\x07\n");
        assert_eq!(lines[0].text(), "abc");
    }

    fn screen(rows: &[&str]) -> String {
        let mut s = "\n".repeat(30);
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    #[test]
    fn claude_rule_labels_and_markers() {
        let rule = "─".repeat(40);
        let labelled = format!("─── History 1/1 {}", "─".repeat(30));
        let s = screen(&[&labelled, "❯\u{a0}recalled text", &rule, "  -- INSERT --"]);
        assert_eq!(read(Vendor::Claude, &s, None), Reading::Draft);
        let s = screen(&[&rule, "! ls", &rule, "  footer"]);
        assert!(matches!(
            read(Vendor::Claude, &s, None),
            Reading::Unknown(_)
        ));
        let s = screen(&[&rule, "\x1b[31m❯\x1b[39m ", &rule, "  footer"]);
        assert!(matches!(
            read(Vendor::Claude, &s, None),
            Reading::Unknown(_)
        ));
    }

    #[test]
    fn short_panes_are_unknown() {
        let rule = "─".repeat(40);
        let s = format!("{rule}\n❯ \n{rule}\n  -- INSERT --\n");
        assert!(matches!(
            read(Vendor::Claude, &s, None),
            Reading::Unknown(_)
        ));
    }

    #[test]
    fn codex_needs_its_layout() {
        let s = screen(&[
            "\x1b[1m›\x1b[0m \x1b[2mAsk anything\x1b[0m",
            "",
            "  m · d  Vim: Insert",
        ]);
        assert_eq!(read(Vendor::Codex, &s, None), Reading::Empty);
        // Not bold: some other `›` line.
        let s = screen(&["› \x1b[2mAsk anything\x1b[0m", "", "  m · d  Vim: Insert"]);
        assert!(matches!(read(Vendor::Codex, &s, None), Reading::Unknown(_)));
        // Placeholder partly normal text is a draft.
        let s = screen(&[
            "\x1b[1m›\x1b[0m hi \x1b[2mAsk\x1b[0m",
            "",
            "  m · d  Vim: Insert",
        ]);
        assert_eq!(read(Vendor::Codex, &s, None), Reading::Draft);
        // A popup between the box and the status line.
        let s = screen(&[
            "\x1b[1m›\x1b[0m /mo",
            "  /model  choose",
            "",
            "  m  Vim: Insert",
        ]);
        assert_eq!(read(Vendor::Codex, &s, None), Reading::Draft);
        let s = screen(&["\x1b[1m›\x1b[0m ", "", "  m · d"]);
        assert!(matches!(read(Vendor::Codex, &s, None), Reading::Unknown(_)));
    }

    fn view_of(lines: &[&str], width: usize) -> View {
        let last = lines.len() - 1;
        View {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            placeholder: false,
            busy: false,
            vim: Some(VimMode::Insert),
            cursor: Some((last, lines[last].chars().count())),
            text_width: Some(width),
            vendor: Vendor::Claude,
        }
    }

    #[test]
    fn wide_character_cursor_is_not_mistaken_for_exact_ascii_width() {
        let mut v = view_of(&["界"], 40);
        v.cursor = Some((0, 2));
        assert!(v.can_verify_submission("界", 40));
        assert!(!v.holds_submission("界"));
    }

    #[test]
    fn holding_text_is_exact_but_for_soft_wraps() {
        let text = "brg: one two three";
        assert!(view_of(&[text], 40).holds(text));
        // Wrapped where "three" could not have fitted on the first line.
        assert!(view_of(&["brg: one two", "three"], 14).holds(text));
        // A break where "three" would have fitted is a typed newline.
        assert!(!view_of(&["brg: one two", "three"], 40).holds(text));
        // Changed spacing, extra text, leading text.
        assert!(!view_of(&["brg: one  two three"], 40).holds(text));
        assert!(!view_of(&["brg: onetwo three"], 40).holds(text));
        assert!(!view_of(&["brg: one two three four"], 40).holds(text));
        assert!(!view_of(&[" brg: one two three"], 40).holds(text));
        assert!(!view_of(&["brg: one two", " three"], 14).holds(text));
        // Trailing spaces show only through the cursor.
        let mut v = view_of(&[text], 40);
        v.cursor = Some((0, text.len() + 1));
        assert!(!v.holds(text));
        v.cursor = None;
        assert!(!v.holds(text));
        v.cursor = Some((0, text.len()));
        v.text_width = None;
        assert!(!v.holds(text));
    }

    /// Catches: a text pasted whose last line would end at the box's edge,
    /// where no cursor position can confirm it, so Enter is withheld and
    /// the text left in the box.
    #[test]
    fn text_ending_at_the_edge_is_found_before_typing() {
        let v = |width| view_of(&[""], width);
        assert!(v(18).ends_at_the_edge("brg: one two three"));
        assert!(!v(19).ends_at_the_edge("brg: one two three"));
        // Wrapped at a space: only the last line counts.
        assert!(v(6).ends_at_the_edge("aa bb ccc dd"));
        assert!(!v(7).ends_at_the_edge("aa bb ccc dd"));
        // A word longer than a line is cut at the edge.
        assert!(v(4).ends_at_the_edge("abcdefgh"));
        assert!(!v(4).ends_at_the_edge("abcdefghi"));
        // After a hard newline.
        assert!(v(3).ends_at_the_edge("a long line\nend"));
        assert!(!v(18).can_verify_submission("brg: one two three", 40));
        assert!(v(19).can_verify_submission("brg: one two three", 40));
    }

    #[test]
    fn claude_blank_box_needs_the_cursor() {
        let mut v = view_of(&[""], 40);
        v.cursor = Some((0, 0));
        assert_eq!(v.reading(), Reading::Empty);
        // Typed spaces moved the cursor.
        v.cursor = Some((0, 3));
        assert_eq!(v.reading(), Reading::Draft);
        // Normal mode: one space would leave the cursor at 0.
        v.cursor = Some((0, 0));
        v.vim = Some(VimMode::Normal);
        assert!(matches!(v.reading(), Reading::Unknown(_)));
        v.vim = Some(VimMode::Insert);
        v.cursor = None;
        assert!(matches!(v.reading(), Reading::Unknown(_)));
    }

    #[test]
    fn codex_blank_box_without_placeholder_is_a_draft() {
        let mut v = view_of(&[""], 40);
        v.vendor = Vendor::Codex;
        assert_eq!(v.reading(), Reading::Draft);
        v.placeholder = true;
        assert_eq!(v.reading(), Reading::Empty);
    }
}
