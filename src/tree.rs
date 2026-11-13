//! The team as a tree: a start and `brg status` print a team's header line,
//! then each member under its parent with aligned columns, then the
//! plugin processes.
//!
//! ```text
//! oncall
//! └─ lead        claude opus high      started
//!    ├─ helper   codex example-model   started
//!    └─ reviewer claude sonnet medium  not started (start: false)
//! ```

use std::collections::BTreeSet;

use crate::team::Role;

/// One member's line: its ID, parent and the columns after its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub cols: Vec<String>,
    pub look: Look,
}

/// How a line is drawn on a terminal. Piped output ignores it entirely, so
/// the text a script reads never changes.
///
/// Shape carries the state (the mark in front of the name), which frees
/// colour to do two things only: say whose agent is busy, and pull the eye to
/// the rows that need a person. Everything that is not running recedes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    /// The state mark drawn between the tree furniture and the name.
    pub mark: Option<char>,
    pub tone: Tone,
    /// The name in italics: a role with no member yet, so it reads as a slot
    /// rather than an agent.
    pub italic: bool,
}

/// All working rows share one spinner phase; other state marks stay still.
pub fn state_mark(look: Look, spin: Option<usize>) -> Option<char> {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    match (look.tone, spin) {
        (Tone::Busy, Some(frame)) => Some(FRAMES[frame % FRAMES.len()]),
        _ => look.mark,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// Alive and needing nothing: ordinary ink.
    #[default]
    Plain,
    /// Busy: one colour for every vendor, far from the alert red.
    Busy,
    /// A running nested team, rather than one working member.
    Team,
    /// Waiting on a person, or ended when it should run.
    Alert,
    /// State could not be read.
    Odd,
    /// Not running: never started, stopped, not created, on demand.
    Faded,
}

pub type Rgb = (u8, u8, u8);

/// The palette: every value clears 3:1 on both a light and a dark
/// terminal, because a CLI cannot know which it is printing on.
/// Faded rows are an explicit grey rather than SGR `dim`, which every
/// terminal renders at a different strength (some not at all).
pub mod palette {
    use super::Rgb;
    /// Working rows, whatever their vendor.
    pub const BUSY: Rgb = (0x1f, 0x93, 0x9a);
    /// New messages, waiting recipients and acknowledgement flashes.
    pub const MESSAGE: Rgb = (0x3d, 0x7e, 0xc5);
    pub const TEAM: Rgb = (0xa9, 0x81, 0x2a);
    pub const ALERT: Rgb = (0xc0, 0x4a, 0x4a);
    pub const ODD: Rgb = (0x90, 0x7a, 0xa9);
    /// Furniture, details and rows that are not running.
    pub const FADED: Rgb = (0x8a, 0x8d, 0x9a);
}

fn fg((r, g, b): Rgb) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

const BOLD: &str = "\x1b[1m";
const ITALIC: &str = "\x1b[3m";
const RESET: &str = "\x1b[0m";

/// `text` in a palette colour, bold or not, for lines drawn outside the
/// tree (the `-w` summary and footer). Callers decide whether to paint.
pub fn ink_text(text: &str, rgb: Rgb, bold: bool) -> String {
    let style = format!("{}{}", if bold { BOLD } else { "" }, fg(rgb));
    paint(text, &style)
}

/// An attention label: keep its one-space padding inside reverse video.
pub fn alert_chip(text: &str) -> String {
    format!("{}\x1b[7m {text} {RESET}", ink(Tone::Alert))
}

/// Whether to paint: a terminal, and no `NO_COLOR` (no-color.org).
pub fn colour_wanted(terminal: bool) -> bool {
    terminal && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// The escape that starts a tone's ink, for the name and state cells.
fn ink(tone: Tone) -> String {
    match tone {
        Tone::Plain => String::new(),
        Tone::Busy => format!("{BOLD}{}", fg(palette::BUSY)),
        Tone::Team => format!("{BOLD}{}", fg(palette::TEAM)),
        Tone::Alert => format!("{BOLD}{}", fg(palette::ALERT)),
        Tone::Odd => fg(palette::ODD),
        Tone::Faded => fg(palette::FADED),
    }
}

fn paint(text: &str, style: &str) -> String {
    if style.is_empty() || text.trim().is_empty() {
        text.to_string()
    } else {
        // Keep the padding outside the escape so trailing blanks stay bare.
        let body = text.trim_end();
        format!("{style}{body}{RESET}{}", &text[body.len()..])
    }
}

/// `"<vendor> <model>"`, as declared (`default` when the member names no
/// model).
pub fn vendor_model(member: &Role) -> String {
    let model = match &member.model {
        Some(m) => match &m.effort {
            Some(e) => format!("{} {e}", m.name),
            None => m.name.clone(),
        },
        None => "default".to_string(),
    };
    format!("{} {model}", member.vendor.as_str())
}

/// The nodes in tree order (each parent before its children, siblings by
/// ID), with each one's line prefix. A node whose parent isn't listed is a
/// root.
pub fn order(nodes: &[Node]) -> Vec<(usize, String)> {
    order_by(nodes, false)
}

/// [`order`], with siblings that are not running (faded) after the ones
/// that are, so a glance down the tree finds what runs. Terminal output
/// only: piped and JSON order stays by ID.
pub fn order_live_first(nodes: &[Node]) -> Vec<(usize, String)> {
    order_by(nodes, true)
}

fn order_by(nodes: &[Node], live_first: bool) -> Vec<(usize, String)> {
    let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let children = |parent: Option<&str>| -> Vec<usize> {
        let mut out: Vec<usize> = (0..nodes.len())
            .filter(|&i| {
                let p = nodes[i].parent.as_deref().filter(|p| ids.contains(p));
                p == parent
            })
            .collect();
        out.sort_by_key(|&i| {
            (
                live_first && nodes[i].look.tone == Tone::Faded,
                nodes[i].id.as_str(),
            )
        });
        out
    };
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    // (node, indent of its children, whether it is its parent's last child)
    let mut stack: Vec<(usize, String, bool)> = Vec::new();
    let roots = children(None);
    for (k, &i) in roots.iter().enumerate().rev() {
        stack.push((i, String::new(), k + 1 == roots.len()));
    }
    while let Some((i, indent, last)) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        out.push((i, format!("{indent}{}", if last { "└─ " } else { "├─ " })));
        let below = format!("{indent}{}", if last { "   " } else { "│  " });
        let kids = children(Some(&nodes[i].id));
        for (k, &c) in kids.iter().enumerate().rev() {
            stack.push((c, below.clone(), k + 1 == kids.len()));
        }
    }
    out
}

/// Lay out rows of cells: the first column is followed by one space, the
/// others by two; the last isn't padded. Each cell comes back with its
/// separator and padding, so it can be painted without moving the columns.
fn cells(rows: &[Vec<String>]) -> Vec<Vec<String>> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; n];
    for row in rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(visible_width(c));
        }
    }
    rows.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut cell = String::new();
                    if i > 0 {
                        cell.push_str(if i == 1 { " " } else { "  " });
                    }
                    cell.push_str(c);
                    if i + 1 < n {
                        cell.extend(std::iter::repeat_n(' ', widths[i] - visible_width(c)));
                    }
                    cell
                })
                .collect()
        })
        .collect()
}

/// OSC 8 hyperlinks enclose visible names but take no terminal columns.
fn visible_width(mut text: &str) -> usize {
    let mut width = 0;
    while let Some((before, control)) = text.split_once("\x1b]8;;") {
        let Some((_, after)) = control.split_once("\x1b\\") else {
            break;
        };
        width += before.chars().count();
        text = after;
    }
    width + text.chars().count()
}

/// Plain lines: the cells joined, trailing blanks dropped.
fn columns(rows: &[Vec<String>]) -> Vec<String> {
    cells(rows)
        .into_iter()
        .map(|row| row.concat().trim_end().to_string())
        .collect()
}

/// One painted line. The first cell is `prefix`, `mark`, `name`: furniture
/// recedes, the mark and name take the row's tone, as does the cell at
/// `state`. Metadata stays plain; details recede, unless the row is faded,
/// when everything does.
fn painted(
    prefix: &str,
    row: &[String],
    look: Look,
    state: usize,
    metadata: Option<usize>,
) -> String {
    let faded = fg(palette::FADED);
    let tone = ink(look.tone);
    let mut line = String::new();
    let mut cells = row.iter();
    if let Some(first) = cells.next() {
        let rest = &first[prefix.len()..];
        line.push_str(&paint(prefix, &faded));
        let name = if look.italic {
            format!("{tone}{ITALIC}")
        } else {
            tone.clone()
        };
        line.push_str(&paint(rest, &name));
    }
    for (i, cell) in cells.enumerate() {
        let i = i + 1;
        if i == state && look.tone == Tone::Alert {
            let body = cell.trim();
            // Separators and alignment stay outside the filled label.
            let before = (cell.len() - cell.trim_start().len()).saturating_sub(1);
            let after = (cell.len() - cell.trim_end().len()).saturating_sub(1);
            line.push_str(&" ".repeat(before));
            line.push_str(&alert_chip(body));
            line.push_str(&" ".repeat(after));
            continue;
        }
        let style = match look.tone {
            Tone::Faded => faded.clone(),
            Tone::Team if i == state => String::new(),
            Tone::Team if Some(i) == metadata => fg(palette::TEAM),
            _ if i == state => tone.clone(),
            _ if Some(i) == metadata => String::new(),
            // Details: what a person must do next stays readable.
            Tone::Alert => String::new(),
            _ => faded.clone(),
        };
        line.push_str(&paint(cell, &style));
    }
    let trimmed = line.trim_end();
    trimmed.to_string()
}

/// The team's tree, then one `plugin <name>` line per plugin process.
pub fn render(
    team: &str,
    nodes: &[Node],
    plugins: &[(String, Vec<String>, Look)],
    colour: bool,
) -> String {
    render_named(team, nodes, plugins, colour, None, false, |id| {
        id.to_string()
    })
}

/// Render qualified tree keys with caller-provided display names.
/// Status terminals project state first; generic create/start trees do not.
pub fn render_named(
    team: &str,
    nodes: &[Node],
    plugins: &[(String, Vec<String>, Look)],
    colour: bool,
    spin: Option<usize>,
    state_first: bool,
    name: impl Fn(&str) -> String,
) -> String {
    let spin = if colour { spin } else { None };
    let terminal = colour || state_first;
    let ordered = if terminal {
        order_live_first(nodes)
    } else {
        order(nodes)
    };
    // A mark slot only when some line carries a mark, on terminal layouts:
    // piped text stays exactly as it was.
    let marked = terminal
        && (nodes.iter().any(|n| n.look.mark.is_some())
            || plugins.iter().any(|p| p.2.mark.is_some()));
    let mark = |look: &Look| {
        if marked {
            format!("{} ", state_mark(*look, spin).unwrap_or(' '))
        } else {
            String::new()
        }
    };
    let rows: Vec<Vec<String>> = ordered
        .iter()
        .map(|(i, prefix)| {
            let n = &nodes[*i];
            let mut row = vec![format!("{prefix}{}{}", mark(&n.look), name(&n.id))];
            row.extend(n.cols.iter().cloned());
            if terminal {
                let state = if state_first || n.cols.len() == 1 {
                    1
                } else {
                    2
                };
                if let Some(cell) = row.get_mut(state) {
                    *cell = format!(" {cell} ");
                }
            }
            row
        })
        .collect();
    let mut out = if colour {
        // The team's ID leads; where it lives recedes.
        match team.split_once("  ") {
            Some((id, rest)) => {
                format!("{BOLD}{id}{RESET}  {}\n", paint(rest, &fg(palette::FADED)))
            }
            None => format!("{BOLD}{team}{RESET}\n"),
        }
    } else {
        format!("{team}\n")
    };
    if colour {
        for (row, (i, prefix)) in cells(&rows).into_iter().zip(ordered) {
            // A role with no member has one column, its state.
            let state = if state_first || nodes[i].cols.len() == 1 {
                1
            } else {
                2
            };
            let metadata = (nodes[i].cols.len() > 1).then_some(if state_first { 2 } else { 1 });
            out.push_str(&painted(&prefix, &row, nodes[i].look, state, metadata));
            out.push('\n');
        }
    } else {
        for line in columns(&rows) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let mut plugins: Vec<&(String, Vec<String>, Look)> = plugins.iter().collect();
    if terminal {
        // Stable: running plugins first, each group in its given order.
        plugins.sort_by_key(|p| p.2.tone == Tone::Faded);
    }
    let plugin_rows: Vec<Vec<String>> = plugins
        .iter()
        .map(|(name, cols, look)| {
            let mut row = vec![format!("{}plugin {name}", mark(look))];
            row.extend(cols.iter().cloned());
            if terminal && let Some(cell) = row.get_mut(1) {
                *cell = format!(" {cell} ");
            }
            row
        })
        .collect();
    if colour {
        for (row, (_, _, look)) in cells(&plugin_rows).into_iter().zip(plugins) {
            out.push_str(&painted("", &row, *look, 1, None));
            out.push('\n');
        }
    } else {
        for line in columns(&plugin_rows) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, cols: &[&str]) -> Node {
        Node {
            id: id.into(),
            parent: parent.map(String::from),
            cols: cols.iter().map(|c| c.to_string()).collect(),
            look: Look::default(),
        }
    }

    #[test]
    fn the_example_renders_exactly() {
        let nodes = [
            node(
                "reviewer",
                Some("lead"),
                &["claude sonnet medium", "not started (start: false)"],
            ),
            node("lead", None, &["claude opus high", "started"]),
            node("helper", Some("lead"), &["codex example-model", "started"]),
        ];
        assert_eq!(
            render("oncall", &nodes, &[], false),
            "oncall\n\
             └─ lead        claude opus high      started\n   \
             ├─ helper   codex example-model   started\n   \
             └─ reviewer claude sonnet medium  not started (start: false)\n"
        );
    }

    #[test]
    fn deeper_trees_and_several_roots() {
        let nodes = [
            node("b", None, &["x"]),
            node("a", None, &["x"]),
            node("a2", Some("a"), &["x"]),
            node("a1", Some("a"), &["x"]),
            node("a1x", Some("a1"), &["x"]),
            node("orphan", Some("missing"), &["x"]),
        ];
        let lines: Vec<String> = order(&nodes)
            .into_iter()
            .map(|(i, p)| format!("{p}{}", nodes[i].id))
            .collect();
        assert_eq!(
            lines,
            [
                "├─ a",
                "│  ├─ a1",
                "│  │  └─ a1x",
                "│  └─ a2",
                "├─ b",
                "└─ orphan"
            ]
        );
    }

    #[test]
    fn columns_align_and_plugins_follow() {
        let nodes = [
            node("lead", None, &["claude default", "idle", ""]),
            node("helper", Some("lead"), &["codex o3", "stopped", "locked"]),
        ];
        let plugins = [
            (
                "slackbot".to_string(),
                vec!["alive".to_string(), "ok".into()],
                Look::default(),
            ),
            (
                "pager".to_string(),
                vec!["stopped".to_string(), "".into()],
                Look::default(),
            ),
        ];
        assert_eq!(
            render("team", &nodes, &plugins, false),
            "team\n\
             └─ lead      claude default  idle\n   \
             └─ helper codex o3        stopped  locked\n\
             plugin slackbot alive    ok\n\
             plugin pager    stopped\n"
        );
    }

    #[test]
    fn the_palette_reads_on_light_and_dark_terminals() {
        // WCAG relative luminance and contrast ratio.
        let luminance = |(r, g, b): Rgb| {
            let lin = |c: u8| {
                let c = f64::from(c) / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
        };
        let contrast = |a: f64, b: f64| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        for rgb in [
            palette::BUSY,
            palette::TEAM,
            palette::ALERT,
            palette::ODD,
            palette::FADED,
        ] {
            let l = luminance(rgb);
            assert!(contrast(l, 1.0) >= 3.0, "{rgb:?} on white");
            assert!(contrast(l, 0.0) >= 3.0, "{rgb:?} on black");
        }
    }

    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|&c| c == 'm');
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn painting_only_adds_escapes_and_marks() {
        let look = |mark, tone| Look {
            mark,
            tone,
            italic: false,
        };
        let mut nodes = vec![
            node("lead", None, &["claude opus high", "working", ""]),
            node(
                "helper",
                Some("lead"),
                &["codex o3", "blocked", "brg attach x"],
            ),
            node("absent", Some("lead"), &["not created (start: false)"]),
        ];
        nodes[0].look = look(Some('●'), Tone::Busy);
        nodes[1].look = look(Some('?'), Tone::Alert);
        nodes[2].look = Look {
            italic: true,
            ..look(None, Tone::Faded)
        };
        let plain = render("team  team /t · folder /w", &nodes, &[], false);
        let painted = render("team  team /t · folder /w", &nodes, &[], true);
        assert_ne!(painted, plain);
        assert!(
            painted.contains("\x1b[7m"),
            "alert states must have a filled label"
        );
        assert_eq!(
            strip(&painted),
            "team  team /t · folder /w\n\
             └─ ● lead      claude opus high               working\n   \
             ├─ ? helper codex o3                       blocked   brg attach x\n   \
             └─   absent  not created (start: false)\n"
        );
        assert!(painted.contains(&alert_chip("blocked")));
        // A working row is bold in the busy colour, never the alert red.
        let working = painted.lines().nth(1).unwrap();
        assert!(working.contains(&format!("{BOLD}{}", fg(palette::BUSY))));
        assert!(!working.contains(&fg(palette::ALERT)));
        // Live rows lead; the row that is not running is last and grey from
        // end to end.
        let absent = painted.lines().last().unwrap();
        assert!(absent.contains(&format!("{}  not created", fg(palette::FADED))));
        // A role with no member yet: its name in italics.
        assert!(absent.contains(&format!("{}{ITALIC}  absent", fg(palette::FADED))));
        let mut team = node("copy", None, &["dev-team", "2 running", ""]);
        team.look = look(Some('◆'), Tone::Team);
        let painted = render("team", &[team], &[], true);
        assert!(painted.contains(&format!("{BOLD}{}◆ copy{RESET}", fg(palette::TEAM))));
        assert!(painted.contains(&format!("{} dev-team{RESET}", fg(palette::TEAM))));
        assert!(painted.contains("   2 running"));
    }

    #[test]
    fn spin_changes_only_busy_marks_and_keeps_all_workers_in_phase() {
        let mut nodes = vec![
            node("worker-a", None, &["§ opus", "working", "topic"]),
            node("worker-b", None, &["Λ model", "working", ""]),
            node("reviewer", None, &["§ sonnet", "blocked", "next"]),
            node("copy", None, &["dev-team", "2 running", ""]),
            node("idle", None, &["§ haiku", "idle", ""]),
        ];
        for (n, (mark, tone)) in nodes.iter_mut().zip([
            ('●', Tone::Busy),
            ('●', Tone::Busy),
            ('?', Tone::Alert),
            ('◆', Tone::Team),
            ('·', Tone::Plain),
        ]) {
            n.look = Look {
                mark: Some(mark),
                tone,
                italic: false,
            };
        }
        let frame = |colour, spin| {
            render_named("team", &nodes, &[], colour, spin, false, |id| {
                id.to_string()
            })
        };
        let still = frame(true, None);
        assert_eq!(frame(true, Some(0)), still.replace("● worker", "⠋ worker"));
        assert_eq!(frame(true, Some(1)), still.replace("● worker", "⠙ worker"));
        assert_eq!(frame(true, Some(10)), frame(true, Some(0)));
        for node in &mut nodes {
            node.cols.swap(0, 1);
        }
        let monochrome =
            |spin| render_named("team", &nodes, &[], false, spin, true, |id| id.to_string());
        assert_eq!(monochrome(Some(0)), monochrome(None));
    }
}
