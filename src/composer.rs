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
                            style.fg = Fg::Rgb(
                                nums[i + 2] as u8,
                                nums[i + 3] as u8,
                                nums[i + 4] as u8,
                            );
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

/// What the input box shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The box's lines, without the prompt marker or continuation indent.
    pub lines: Vec<String>,
    /// The single line is the vendor's dim placeholder.
    pub placeholder: bool,
    /// The vendor marks a running turn on the box itself.
    pub busy: bool,
    pub vim: Option<VimMode>,
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
        let empty = self.placeholder
            || (self.lines.len() == 1 && self.lines[0].trim().is_empty());
        if empty {
            Reading::Empty
        } else {
            Reading::Draft
        }
    }

    /// The box holds exactly `text`, allowing for the vendor wrapping it
    /// across lines.
    pub fn holds(&self, text: &str) -> bool {
        if self.placeholder || self.busy {
            return false;
        }
        let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let first = self.lines.first().map(|l| l.as_str()).unwrap_or("");
        first.starts_with(text.split_whitespace().next().unwrap_or(""))
            && squash(&self.lines.join(" ")) == squash(text)
    }
}

/// Read the input box from a styled capture.
pub fn view(vendor: Vendor, capture: &str) -> Result<View, String> {
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
    match vendor {
        Vendor::Claude => claude_view(&lines),
        Vendor::Codex => codex_view(&lines),
    }
}

/// Read the input box straight to a decision.
pub fn read(vendor: Vendor, capture: &str) -> Reading {
    match view(vendor, capture) {
        Ok(v) => v.reading(),
        Err(why) => Reading::Unknown(why),
    }
}

fn is_rule(line: &Line) -> bool {
    let t = line.text();
    let t = t.trim();
    t.chars().count() >= 10 && t.starts_with("──") && t.ends_with("──")
}

/// Continuation lines are indented two spaces (or blank).
fn continuation(line: &Line) -> Option<String> {
    let t = line.text();
    if line.is_blank() {
        return Some(String::new());
    }
    t.strip_prefix("  ").map(|s| s.trim_end().to_string())
}

fn claude_view(lines: &[Line]) -> Result<View, String> {
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
        return Err(format!("the input box starts with `{}`, not `❯`", marker.ch));
    }
    if !matches!(first.0.get(1).map(|c| c.ch), Some(' ' | '\u{a0}') | None) {
        return Err("no space after the prompt".into());
    }
    let busy = match (marker.style.fg, marker.style.dim) {
        (Fg::Default, false) => false,
        (Fg::Indexed(246), _) | (_, true) => true,
        _ => return Err("the prompt has an unfamiliar colour".into()),
    };
    let mut text = vec![
        first
            .cells_from(2)
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string(),
    ];
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
    Ok(View {
        lines: text,
        placeholder: false,
        busy,
        vim,
    })
}

fn codex_view(lines: &[Line]) -> Result<View, String> {
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
    let content = first.cells_from(2);
    let visible: Vec<&Cell> = content.iter().filter(|c| !c.ch.is_whitespace()).collect();
    let mut text = vec![
        content
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string(),
    ];
    for row in &lines[start + 1..=last] {
        text.push(continuation(row).ok_or("an input box line isn't indented")?);
    }
    let placeholder =
        text.len() == 1 && !visible.is_empty() && visible.iter().all(|c| c.style.dim);
    Ok(View {
        lines: text,
        placeholder,
        busy: false,
        vim: Some(vim),
    })
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
        assert_eq!(read(Vendor::Claude, &s), Reading::Draft);
        let s = screen(&[&rule, "! ls", &rule, "  footer"]);
        assert!(matches!(read(Vendor::Claude, &s), Reading::Unknown(_)));
        let s = screen(&[&rule, "\x1b[31m❯\x1b[39m ", &rule, "  footer"]);
        assert!(matches!(read(Vendor::Claude, &s), Reading::Unknown(_)));
    }

    #[test]
    fn short_panes_are_unknown() {
        let rule = "─".repeat(40);
        let s = format!("{rule}\n❯ \n{rule}\n  -- INSERT --\n");
        assert!(matches!(read(Vendor::Claude, &s), Reading::Unknown(_)));
    }

    #[test]
    fn codex_needs_its_layout() {
        let s = screen(&["\x1b[1m›\x1b[0m \x1b[2mAsk anything\x1b[0m", "", "  m · d  Vim: Insert"]);
        assert_eq!(read(Vendor::Codex, &s), Reading::Empty);
        // Not bold: some other `›` line.
        let s = screen(&["› \x1b[2mAsk anything\x1b[0m", "", "  m · d  Vim: Insert"]);
        assert!(matches!(read(Vendor::Codex, &s), Reading::Unknown(_)));
        // Placeholder partly normal text is a draft.
        let s = screen(&["\x1b[1m›\x1b[0m hi \x1b[2mAsk\x1b[0m", "", "  m · d  Vim: Insert"]);
        assert_eq!(read(Vendor::Codex, &s), Reading::Draft);
        // A popup between the box and the status line.
        let s = screen(&["\x1b[1m›\x1b[0m /mo", "  /model  choose", "", "  m  Vim: Insert"]);
        assert_eq!(read(Vendor::Codex, &s), Reading::Draft);
        let s = screen(&["\x1b[1m›\x1b[0m ", "", "  m · d"]);
        assert!(matches!(read(Vendor::Codex, &s), Reading::Unknown(_)));
    }

    #[test]
    fn holding_text_allows_wrapping_only() {
        let v = View {
            lines: vec!["brg: one two".into(), "three".into()],
            placeholder: false,
            busy: false,
            vim: None,
        };
        assert!(v.holds("brg: one two three"));
        assert!(!v.holds("brg: one two three four"));
        let v = View {
            lines: vec!["x brg: one two three".into()],
            ..v
        };
        assert!(!v.holds("brg: one two three"));
    }
}
