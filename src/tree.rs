//! The team as a tree: a start and `brg status` print an instance's header line,
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
    pub dim: bool,
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
    let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let children = |parent: Option<&str>| -> Vec<usize> {
        let mut out: Vec<usize> = (0..nodes.len())
            .filter(|&i| {
                let p = nodes[i].parent.as_deref().filter(|p| ids.contains(p));
                p == parent
            })
            .collect();
        out.sort_by(|&a, &b| nodes[a].id.cmp(&nodes[b].id));
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
/// others by two; the last isn't padded and trailing blanks are dropped.
fn columns(rows: &[Vec<String>]) -> Vec<String> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; n];
    for row in rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, c) in row.iter().enumerate() {
                if i > 0 {
                    line.push_str(if i == 1 { " " } else { "  " });
                }
                line.push_str(c);
                let pad = widths[i] - c.chars().count();
                if i + 1 < n {
                    line.extend(std::iter::repeat_n(' ', pad));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// The instance's tree, then one `plugin <name>` line per plugin process.
pub fn render(
    team: &str,
    nodes: &[Node],
    plugins: &[(String, Vec<String>)],
    colour: bool,
) -> String {
    render_named(team, nodes, plugins, colour, |id| id.to_string())
}

/// Render qualified tree keys with caller-provided display names.
pub fn render_named(
    team: &str,
    nodes: &[Node],
    plugins: &[(String, Vec<String>)],
    colour: bool,
    name: impl Fn(&str) -> String,
) -> String {
    let ordered = order(nodes);
    let rows: Vec<Vec<String>> = ordered
        .iter()
        .map(|(i, prefix)| {
            let mut row = vec![format!("{prefix}{}", name(&nodes[*i].id))];
            row.extend(nodes[*i].cols.iter().cloned());
            row
        })
        .collect();
    let mut out = format!("{team}\n");
    for (line, (i, _)) in columns(&rows).into_iter().zip(ordered) {
        let dim = colour && nodes[i].dim;
        if dim {
            out.push_str("\x1b[2m");
        }
        out.push_str(&line);
        if dim {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    let plugin_rows: Vec<Vec<String>> = plugins
        .iter()
        .map(|(name, cols)| {
            let mut row = vec![format!("plugin {name}")];
            row.extend(cols.iter().cloned());
            row
        })
        .collect();
    for line in columns(&plugin_rows) {
        out.push_str(&line);
        out.push('\n');
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
            dim: false,
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
            ),
            ("pager".to_string(), vec!["stopped".to_string(), "".into()]),
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
}
