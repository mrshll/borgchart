//! Deterministic radial clusters, easing, collision-free labels and nested team boxes.

use super::feed::Feed;
use crate::runtime::StatusRow;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

type Points = BTreeMap<String, (f64, f64)>;
#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub label: String,
    pub outside: bool,
    pub state: String,
}
// Records lack a plugin's originating team; use the nearest declaring ancestor.
pub(crate) fn endpoint(nodes: &[Node], from: &str, to: &str) -> String {
    if let Some(name) = from
        .strip_prefix("external:")
        .filter(|name| *name != "owner")
    {
        let mut ancestor = to.rsplit_once('/').map(|(team, _)| team);
        while let Some(team) = ancestor {
            let id = format!("{team}/plugin:{name}");
            if nodes.iter().any(|n| n.id == id) {
                return id;
            }
            ancestor = team.rsplit_once('/').map(|(parent, _)| parent);
        }
    }
    from.into()
}
pub(crate) fn nodes(
    rows: &[StatusRow],
    scope: &str,
    parent: Option<&str>,
    feed: &Feed,
) -> Vec<Node> {
    let teams: BTreeMap<_, _> = rows
        .iter()
        .filter(|r| r.kind == "team")
        .map(|r| (r.instance.as_str(), r.parent.as_deref()))
        .collect();
    let mut out: BTreeMap<_, _> = rows
        .iter()
        .filter(|r| {
            r.kind == "member"
                && (r.instance == scope || r.instance.starts_with(&format!("{scope}/")))
        })
        .map(|r| {
            let root = r.parent.as_deref().is_none_or(|p| p == r.instance);
            let parent = if root {
                if r.instance == scope {
                    parent
                } else {
                    teams.get(r.instance.as_str()).copied().flatten()
                }
                .map(str::to_owned)
            } else {
                r.parent.as_ref().map(|p| {
                    if p.contains('/') {
                        p.clone()
                    } else {
                        format!("{}/{p}", r.instance)
                    }
                })
            };
            let id = format!("{}/{}", r.instance, r.name);
            let label = if root && r.instance != scope {
                let team = r.instance.rsplit('/').next().unwrap_or(&r.instance);
                format!("{team}/{}", r.name)
            } else {
                r.name.clone()
            };
            (
                id.clone(),
                Node {
                    id,
                    parent,
                    label,
                    outside: false,
                    state: r.state.clone(),
                },
            )
        })
        .collect();
    // Status emits only process plugins, never actions-only declarations.
    for r in rows.iter().filter(|r| {
        r.kind == "plugin" && (r.instance == scope || r.instance.starts_with(&format!("{scope}/")))
    }) {
        let parent = rows.iter().find(|m| {
            m.kind == "member"
                && m.instance == r.instance
                && m.parent.as_deref().is_none_or(|p| p == m.instance)
        });
        let id = crate::runtime::status_key(r);
        out.insert(
            id.clone(),
            Node {
                id,
                parent: parent.map(crate::runtime::status_key),
                label: crate::runtime::status_name(r),
                outside: false,
                state: if r.detail.as_deref().is_some_and(|s| s.starts_with("error:")) {
                    "error".into()
                } else {
                    r.state.clone()
                },
            },
        );
    }
    let members: BTreeSet<_> = out.keys().cloned().collect();
    let inside: Vec<_> = out.values().cloned().collect();
    let mut outside: BTreeSet<String> = feed
        .routes
        .iter()
        .flat_map(|(a, b)| {
            [(a, b), (b, a)]
                .into_iter()
                .filter(|(a, b)| a.starts_with("external:") && members.contains(*b))
                .map(|(a, b)| endpoint(&inside, a, b))
                .filter(|a| a.starts_with("external:"))
        })
        .collect();
    if let Some(parent) = parent {
        outside.insert(parent.into());
    }
    for id in outside {
        out.entry(id.clone()).or_insert_with(|| Node {
            label: id
                .strip_prefix("external:")
                .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(&id))
                .into(),
            id,
            parent: None,
            outside: true,
            state: String::new(),
        });
    }
    out.into_values().collect()
}

pub(crate) fn short(label: &str, width: u16) -> String {
    let label = label.strip_prefix("external:").unwrap_or(label);
    let mut s = if width < 120 && label.contains('/') {
        label.split('/').next().unwrap_or(label).to_string()
    } else {
        label.to_string()
    };
    if width < 120 {
        for (full, cut) in [
            ("implementer-", "impl-"),
            ("researcher-", "res-"),
            ("reviewer-", "rev-"),
            ("monitor-", "mon-"),
        ] {
            s = s.replace(full, cut);
        }
        if s.chars().count() > 13 {
            s = format!("{}…", s.chars().take(12).collect::<String>());
        }
    }
    s
}

// Descendants occupy one proportional sector; recursion fans outward from its lead.
fn cluster(nodes: &[Node], id: &str, start: f64, end: f64, depth: f64, points: &mut Points) {
    let angle = (start + end) / 2.;
    points.insert(id.into(), (depth * angle.sin(), -depth * angle.cos()));
    if nodes.iter().any(|n| n.id == id && n.outside) {
        return;
    }
    let children: Vec<_> = nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some(id) && !n.outside)
        .collect();
    let total: usize = children.iter().map(|n| weight(nodes, &n.id)).sum();
    let mut at = start;
    for child in children {
        let next = at + (end - start) * weight(nodes, &child.id) as f64 / total as f64;
        cluster(nodes, &child.id, at, next, depth + 0.65, points);
        at = next;
    }
}
fn weight(nodes: &[Node], id: &str) -> usize {
    if nodes.iter().any(|n| n.id == id && n.outside) {
        return 1;
    }
    1 + nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some(id) && !n.outside)
        .map(|n| weight(nodes, &n.id))
        .sum::<usize>()
}
pub(crate) fn positions(nodes: &[Node], width: u16, height: u16) -> Points {
    let Some(root) = nodes.iter().find(|n| {
        !n.outside
            && !nodes
                .iter()
                .any(|p| !p.outside && Some(p.id.as_str()) == n.parent.as_deref())
    }) else {
        return Points::new();
    };
    let mut points = Points::from([(root.id.clone(), (0., 0.))]);
    let items: Vec<_> = nodes
        .iter()
        .filter(|n| n.outside)
        .chain(nodes.iter().filter(|n| {
            (n.parent.as_deref() == Some(&root.id)
                || (n.id != root.id
                    && !nodes
                        .iter()
                        .any(|p| !p.outside && Some(&p.id) == n.parent.as_ref())))
                && !n.outside
        }))
        .collect();
    let sizes: Vec<_> = items.iter().map(|n| weight(nodes, &n.id)).collect();
    let total = sizes.iter().sum::<usize>().max(1) as f64;
    let mut at = -std::f64::consts::PI * sizes.first().copied().unwrap_or(1) as f64 / total;
    for (item, size) in items.into_iter().zip(sizes) {
        let next = at + std::f64::consts::TAU * size as f64 / total;
        cluster(nodes, &item.id, at, next, 1., &mut points);
        at = next;
    }
    // Reserve label room horizontally, then fit the occupied bounds vertically.
    let pad = (width as f64 * 0.20).min(26.);
    for (axis, min, max) in [
        (false, pad, width as f64 - 1. - pad),
        (true, 1., height.saturating_sub(2) as f64),
    ] {
        let coord = |&(x, y): &(f64, f64)| if axis { y } else { x };
        let low = points.values().map(coord).fold(f64::INFINITY, f64::min);
        let high = points.values().map(coord).fold(f64::NEG_INFINITY, f64::max);
        for p in points.values_mut() {
            let v = if axis { &mut p.1 } else { &mut p.0 };
            *v = if high == low {
                (min + max) / 2.
            } else {
                min + (*v - low) / (high - low) * (max - min)
            };
        }
    }
    points
}

#[derive(Default)]
pub(crate) struct Motion {
    old: Points,
    target: Points,
    changed: Duration,
}
impl Motion {
    pub fn at(&mut self, nodes: &[Node], w: u16, h: u16, now: Duration) -> Points {
        let target = positions(nodes, w, h);
        if target != self.target {
            self.old = self.current(now);
            self.target = target;
            self.changed = now;
        }
        self.current(now)
    }
    fn current(&self, now: Duration) -> Points {
        let t = (now.saturating_sub(self.changed).as_secs_f64() / 0.5).min(1.);
        self.target
            .iter()
            .map(|(id, &end)| {
                let start = self.old.get(id).copied().unwrap_or(end);
                let x = start.0 + (end.0 - start.0) * t;
                let y = start.1 + (end.1 - start.1) * t;
                (id.clone(), (x, y))
            })
            .collect()
    }
    pub fn moving(&self, now: Duration) -> bool {
        now.saturating_sub(self.changed) < Duration::from_millis(500) && !self.old.is_empty()
    }
}
#[derive(Debug)]
pub(crate) struct Label {
    pub id: String,
    pub text: String,
    pub x: u16,
    pub y: u16,
}
pub(crate) fn labels(
    nodes: &[Node],
    points: &Points,
    w: u16,
    h: u16,
    waiting: &BTreeMap<String, usize>,
) -> (Vec<Label>, usize) {
    let mut taken: BTreeSet<_> = points
        .values()
        .map(|&(x, y)| (x.round() as i32, y.round() as i32))
        .collect();
    let mut labels = Vec::new();
    for n in nodes {
        let Some(&(x, y)) = points.get(&n.id) else {
            continue;
        };
        let (x, y) = (x.round() as i32, y.round() as i32);
        let mut text = short(&n.label, w);
        if let Some(count) = waiting.get(&n.id) {
            text.push_str(&format!(" [{count}]"));
        }
        let len = text.chars().count() as i32;
        let (away, other) = if x >= i32::from(w) / 2 {
            (x + 2, x - len - 1)
        } else {
            (x - len - 1, x + 2)
        };
        for (lx, ly) in [
            (away, y),
            (other, y),
            (x - len / 2, y + 1),
            (x - len / 2, y - 1),
        ] {
            if lx < 1
                || lx + len >= i32::from(w)
                || ly < 0
                || ly >= i32::from(h)
                || (lx - 1..=lx + len).any(|cx| taken.contains(&(cx, ly)))
            {
                continue;
            }
            taken.extend((lx - 1..=lx + len).map(|cx| (cx, ly)));
            labels.push(Label {
                id: n.id.clone(),
                text,
                x: lx as u16,
                y: ly as u16,
            });
            break;
        }
    }
    let hidden = nodes.len() - labels.len();
    (labels, hidden)
}
pub(crate) fn team_boxes(
    nodes: &[Node],
    points: &Points,
    labels: &[Label],
    scope: &str,
) -> Vec<(String, [i32; 4])> {
    let mut teams = BTreeSet::new();
    for node in nodes.iter().filter(|n| !n.outside) {
        let mut team = node.id.rsplit_once('/').unwrap().0;
        while team != scope {
            teams.insert(team);
            team = team.rsplit_once('/').map_or(scope, |(parent, _)| parent);
        }
    }
    let mut teams: Vec<_> = teams.into_iter().collect();
    teams.sort_by_key(|t| std::cmp::Reverse(t.len()));
    let mut out: Vec<(String, [i32; 4])> = Vec::new();
    for team in teams {
        let owns = |id: &str| id.starts_with(&format!("{team}/"));
        let mut cells: Vec<_> = points
            .iter()
            .filter(|(id, _)| owns(id))
            .map(|(_, &(x, y))| (x.round() as i32, y.round() as i32))
            .collect();
        for l in labels.iter().filter(|l| owns(&l.id)) {
            cells.push((l.x as i32 - 1, l.y as i32));
            cells.push((l.x as i32 + l.text.chars().count() as i32, l.y as i32));
        }
        for (_, b) in out.iter().filter(|(id, _)| owns(id)) {
            cells.extend([(b[0], b[1]), (b[2], b[3])]);
        }
        let xs = cells.iter().map(|p| p.0);
        let ys = cells.iter().map(|p| p.1);
        let b = [
            xs.clone().min().unwrap() - 2,
            ys.clone().min().unwrap() - 2,
            xs.max().unwrap() + 2,
            ys.max().unwrap() + 2,
        ];
        if points.iter().any(|(id, &(x, y))| {
            let (x, y) = (x.round() as i32, y.round() as i32);
            !owns(id) && (b[0]..=b[2]).contains(&x) && (b[1]..=b[3]).contains(&y)
        }) {
            continue;
        }
        out.push((team.into(), b));
    }
    out
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture() -> Vec<Node> {
        let mut nodes = vec![Node {
            id: "demo/lead-abcd".into(),
            parent: None,
            label: "lead-abcd".into(),
            outside: false,
            state: "idle".into(),
        }];
        for (team, names) in [
            ("dev-alerts", vec!["implementer-pager", "reviewer-r4"]),
            (
                "dev-search",
                vec!["implementer-index", "implementer-ranker", "reviewer-x2"],
            ),
            (
                "dev-billing",
                vec!["implementer-tax", "implementer-invoices"],
            ),
            ("dev-website", vec!["implementer-pages", "researcher-fonts"]),
        ] {
            let lead = format!("demo/{team}/lead-efgh");
            nodes.push(Node {
                id: lead.clone(),
                parent: Some("demo/lead-abcd".into()),
                label: format!("{team}/lead-efgh"),
                outside: false,
                state: "working".into(),
            });
            for name in names {
                nodes.push(Node {
                    id: format!("demo/{team}/{name}"),
                    parent: Some(lead.clone()),
                    label: name.into(),
                    outside: false,
                    state: "working".into(),
                });
            }
        }
        for name in ["monitor-ci", "researcher-compat"] {
            nodes.push(Node {
                id: format!("demo/{name}"),
                parent: Some("demo/lead-abcd".into()),
                label: name.into(),
                outside: false,
                state: "stopped".into(),
            });
        }
        for name in ["owner", "slack"] {
            nodes.push(Node {
                id: format!("external:{name}"),
                parent: None,
                label: name.into(),
                outside: true,
                state: String::new(),
            });
        }
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        nodes
    }
    pub(crate) fn grouped_rows() -> Vec<StatusRow> {
        let mut rows = vec![row("demo", "lead-root", None)];
        for (team, parent, lead, members) in [
            (
                "demo/dev-search",
                "demo/lead-root",
                "lead-search",
                vec!["implementer-index", "reviewer-search"],
            ),
            (
                "demo/dev-build",
                "demo/lead-root",
                "lead-build",
                vec!["implementer-compile"],
            ),
            (
                "demo/dev-search/dev-rank",
                "demo/dev-search/lead-search",
                "lead-rank",
                vec!["implementer-score", "reviewer-rank"],
            ),
        ] {
            let mut child = row(team, team.rsplit('/').next().unwrap(), Some(parent));
            child.kind = "team";
            rows.push(child);
            rows.push(row(team, lead, Some(team)));
            for member in members {
                rows.push(row(team, member, Some(lead)));
            }
        }
        rows
    }
    #[test]
    fn descendant_boxes_enclose_labels_and_nested_boxes_with_a_cell_between_borders() {
        let dir = tempfile::tempdir().unwrap();
        let feed = Feed::open(crate::bus::layout::StorePaths::from_root(dir.path())).unwrap();
        let rows = grouped_rows();
        for scope in ["demo", "demo/dev-search"] {
            let nodes = nodes(&rows, scope, Some("external:owner"), &feed);
            for (w, h) in [(80, 18), (160, 42)] {
                let points = positions(&nodes, w, h);
                let (labels, _) = labels(&nodes, &points, w, h, &BTreeMap::new());
                let boxes = team_boxes(&nodes, &points, &labels, scope);
                let expected = if scope == "demo" { 2 } else { 1 };
                assert_eq!(boxes.len(), expected);
                assert!(
                    boxes
                        .iter()
                        .any(|(team, _)| team == "demo/dev-search/dev-rank")
                );
                if scope == "demo" {
                    assert!(boxes.iter().any(|(team, _)| team == "demo/dev-build"));
                    assert!(!boxes.iter().any(|(team, _)| team == "demo/dev-search"));
                }
                assert!(!boxes.iter().any(|(team, _)| team == scope));
                for (team, [left, top, right, bottom]) in &boxes {
                    for node in nodes
                        .iter()
                        .filter(|node| !node.outside && node.id.starts_with(&format!("{team}/")))
                    {
                        let (x, y) = points[&node.id];
                        assert!(
                            *left < x.round() as i32
                                && (x.round() as i32) < *right
                                && *top < y.round() as i32
                                && (y.round() as i32) < *bottom,
                            "{team}: {}",
                            node.id
                        );
                        if let Some(label) = labels.iter().find(|label| label.id == node.id) {
                            assert!(
                                *left < label.x as i32 - 1
                                    && label.x as i32 + (label.text.len() as i32) < *right
                                    && *top < label.y as i32
                                    && (label.y as i32) < *bottom
                            );
                        }
                    }
                    for (child, b) in boxes
                        .iter()
                        .filter(|(child, _)| child.starts_with(&format!("{team}/")))
                    {
                        assert!(
                            b[0] - left >= 2
                                && b[1] - top >= 2
                                && right - b[2] >= 2
                                && bottom - b[3] >= 2,
                            "{team}: {child}"
                        );
                    }
                    let others: Vec<_> = nodes
                        .iter()
                        .filter(|node| !node.id.starts_with(&format!("{team}/")))
                        .filter(|node| {
                            let (x, y) = points[&node.id];
                            *left < x.round() as i32
                                && (x.round() as i32) < *right
                                && *top < y.round() as i32
                                && (y.round() as i32) < *bottom
                        })
                        .map(|node| node.id.as_str())
                        .collect();
                    assert!(others.is_empty(), "foreign nodes in {team}: {others:?}");
                    println!(
                        "{w}x{h} scope={scope} team={team} bounds={left},{top},{right},{bottom} unrelated nodes={others:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn foreign_nodes_drop_only_their_enclosing_boxes_and_preserve_drawn_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let feed = Feed::open(crate::bus::layout::StorePaths::from_root(dir.path())).unwrap();
        let nodes = nodes(&grouped_rows(), "demo", Some("external:owner"), &feed);
        for (blocker, blocker_x) in [
            (None, 5.),
            (Some("demo/lead-root"), 5.),
            (Some("external:owner"), 5.),
            (Some("demo/lead-root"), 14.),
            (Some("external:owner"), -2.),
        ] {
            let points: Points = nodes
                .iter()
                .map(|node| {
                    let x = if blocker == Some(node.id.as_str()) {
                        blocker_x
                    } else if node.id.starts_with("demo/dev-search/dev-rank/") {
                        10.
                    } else if node.id.starts_with("demo/dev-search/") {
                        0.
                    } else if node.id.starts_with("demo/dev-build/") {
                        20.
                    } else {
                        30.
                    };
                    (node.id.clone(), (x, 5.))
                })
                .collect();
            let boxes = team_boxes(&nodes, &points, &[], "demo");
            assert_eq!(
                boxes.len(),
                if blocker.is_some() { 2 } else { 3 },
                "{blocker:?}"
            );
            assert!(
                boxes
                    .iter()
                    .any(|(team, _)| team == "demo/dev-search/dev-rank")
            );
            assert!(boxes.iter().any(|(team, _)| team == "demo/dev-build"));
            let parent = boxes.iter().find(|(team, _)| team == "demo/dev-search");
            if let Some((_, parent)) = parent {
                assert!(blocker.is_none());
                let child = boxes
                    .iter()
                    .find(|(team, _)| team == "demo/dev-search/dev-rank")
                    .unwrap()
                    .1;
                assert!(
                    child[0] - parent[0] >= 2
                        && child[1] - parent[1] >= 2
                        && parent[2] - child[2] >= 2
                        && parent[3] - child[3] >= 2
                );
            } else {
                assert!(blocker.is_some());
            }
        }
    }
    #[test]
    fn dropped_descendant_borders_cannot_expand_an_otherwise_clear_parent_box() {
        let dir = tempfile::tempdir().unwrap();
        let feed = Feed::open(crate::bus::layout::StorePaths::from_root(dir.path())).unwrap();
        let nodes = nodes(&grouped_rows(), "demo", Some("external:owner"), &feed);
        let points: Points = nodes
            .iter()
            .map(|node| {
                let x = if node.id == "demo/lead-root" {
                    -3.
                } else if node.id.starts_with("demo/dev-search/dev-rank/") {
                    0.
                } else if node.id.starts_with("demo/dev-search/") {
                    2.
                } else if node.id.starts_with("demo/dev-build/") {
                    20.
                } else {
                    30.
                };
                (node.id.clone(), (x, 5.))
            })
            .collect();
        let boxes = team_boxes(&nodes, &points, &[], "demo");
        assert!(
            !boxes
                .iter()
                .any(|(team, _)| team == "demo/dev-search/dev-rank")
        );
        let parent = boxes
            .iter()
            .find(|(team, _)| team == "demo/dev-search")
            .expect("discarded child border must not enlarge the parent into the root")
            .1;
        assert_eq!(parent, [-2, 3, 4, 7]);
    }
    #[test]
    fn radial_clusters_keep_nodes_distinct_and_ease_membership_changes() {
        let mut nodes = fixture();
        for name in ["implementer-extra", "reviewer-extra"] {
            nodes.push(Node {
                id: format!("demo/{name}"),
                parent: Some("demo/lead-abcd".into()),
                label: name.into(),
                outside: false,
                state: "idle".into(),
            });
        }
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let p = positions(&nodes, 80, 20);
        assert_eq!(p, positions(&nodes, 80, 20));
        let cells: BTreeSet<_> = p
            .values()
            .map(|&(x, y)| (x.round() as i32, y.round() as i32))
            .collect();
        assert_eq!(cells.len(), nodes.len());
        // Outside parties occupy the first sectors, clockwise from the top.
        assert!(p["external:owner"].1 < p["demo/lead-abcd"].1);
        assert!(p["external:slack"].0 > p["external:owner"].0);
        let root = "demo/lead-abcd";
        let items: Vec<_> = nodes
            .iter()
            .filter(|n| n.outside)
            .chain(nodes.iter().filter(|n| n.parent.as_deref() == Some(root)))
            .collect();
        let total = nodes.len() - 1;
        let mut start_angle = -std::f64::consts::PI / total as f64;
        let origin = p[root];
        let scale_y = origin.1 - p["external:owner"].1;
        let scale_x =
            (p["external:slack"].0 - origin.0) / (std::f64::consts::TAU / total as f64).sin();
        for item in items {
            let members: Vec<_> = nodes
                .iter()
                .filter(|n| n.id == item.id || n.parent.as_deref() == Some(&item.id))
                .collect();
            let end_angle =
                start_angle + std::f64::consts::TAU * members.len() as f64 / total as f64;
            for member in members {
                let (x, y) = p[&member.id];
                let (x, y) = ((x - origin.0) / scale_x, (y - origin.1) / scale_y);
                let mut angle = x.atan2(-y);
                if angle < start_angle {
                    angle += std::f64::consts::TAU;
                }
                assert!(
                    angle >= start_angle && angle <= end_angle,
                    "{} outside its team sector",
                    member.id
                );
            }
            start_angle = end_angle;
        }
        let mut motion = Motion::default();
        let old = motion.at(&nodes, 80, 20, Duration::ZERO);
        let mut changed = nodes.clone();
        changed.retain(|n| !n.id.contains("dev-billing"));
        let start = motion.at(&changed, 80, 20, Duration::from_secs(1));
        assert_eq!(
            start["demo/dev-search/lead-efgh"],
            old["demo/dev-search/lead-efgh"]
        );
        let end = positions(&changed, 80, 20);
        let mid = motion.at(&changed, 80, 20, Duration::from_millis(1250));
        let id = "demo/dev-search/lead-efgh";
        for (start, middle, finish) in [
            (old[id].0, mid[id].0, end[id].0),
            (old[id].1, mid[id].1, end[id].1),
        ] {
            assert!(middle > start.min(finish) && middle < start.max(finish));
        }
        assert_eq!(
            motion.at(&changed, 80, 20, Duration::from_millis(1500)),
            end
        );
    }
    #[test]
    fn labels_never_overwrite_nodes_or_each_other_and_report_crowding() {
        let nodes = fixture();
        for width in [80, 160] {
            let height = if width == 80 { 20 } else { 36 };
            let p = positions(&nodes, width, height);
            let (labels, hidden) = labels(&nodes, &p, width, height, &BTreeMap::new());
            assert_eq!(hidden, 0, "{width}: {labels:?}");
            let mut occupied: BTreeSet<_> = p
                .values()
                .map(|&(x, y)| (x.round() as u16, y.round() as u16))
                .collect();
            for l in labels {
                if width == 160 && l.id.ends_with("implementer-invoices") {
                    assert_eq!(l.text, "implementer-invoices");
                }
                for x in l.x.saturating_sub(1)..=l.x + l.text.chars().count() as u16 {
                    assert!(occupied.insert((x, l.y)), "collision at {x},{}", l.y);
                }
            }
        }
        let p = positions(&nodes, 12, 4);
        assert!(labels(&nodes, &p, 12, 4, &BTreeMap::new()).1 > 0);
    }
    #[test]
    fn scoped_team_keeps_all_roots_connected_to_the_outside_parent() {
        let rows = vec![
            row("demo/dev-search", "lead-abcd", None),
            row("demo/dev-search", "lead-efgh", None),
            row("demo/dev-search", "implementer-index", Some("lead-efgh")),
        ];
        let dir = tempfile::tempdir().unwrap();
        let feed = Feed::open(crate::bus::layout::StorePaths::from_root(dir.path())).unwrap();
        let nodes = nodes(&rows, "demo/dev-search", Some("demo/lead-ijkl"), &feed);
        let points = positions(&nodes, 80, 20);
        for node in &nodes {
            assert!(points.contains_key(&node.id), "missing {}", node.id);
        }
        let mut unlinked = nodes.clone();
        for node in &mut unlinked {
            if node.parent.as_deref() == Some("demo/lead-ijkl") {
                node.parent = None;
            }
        }
        assert_eq!(points, positions(&unlinked, 80, 20));
    }
    #[test]
    fn child_roots_and_scoped_parents_follow_member_routes() {
        let mut rows = vec![
            row("demo", "lead-abcd", None),
            row("demo/dev-search", "lead-efgh", Some("demo/dev-search")),
            row("demo/dev-search", "implementer-index", Some("lead-efgh")),
        ];
        let dir = tempfile::tempdir().unwrap();
        let feed =
            crate::graph::feed::Feed::open(crate::bus::layout::StorePaths::from_root(dir.path()))
                .unwrap();
        let nodes = nodes(&rows, "demo/dev-search", Some("demo/lead-abcd"), &feed);
        assert!(nodes.iter().any(|n| n.id == "demo/lead-abcd" && n.outside));
        assert_eq!(
            nodes
                .iter()
                .find(|n| n.id.ends_with("lead-efgh"))
                .unwrap()
                .parent
                .as_deref(),
            Some("demo/lead-abcd")
        );
        assert!(!nodes.iter().any(|n| n.id == "demo/lead-abcd" && !n.outside));
        let mut team = row("demo/dev-search", "dev-search", Some("demo/lead-abcd"));
        team.kind = "team";
        rows.push(team);
        let all = super::nodes(&rows, "demo", None, &feed);
        assert_eq!(
            all.iter()
                .find(|n| n.id.ends_with("lead-efgh"))
                .unwrap()
                .parent
                .as_deref(),
            Some("demo/lead-abcd")
        );
        assert_eq!(positions(&all, 80, 20).len(), 3);
    }
    pub(crate) fn row(team: &str, name: &str, parent: Option<&str>) -> crate::runtime::StatusRow {
        crate::runtime::StatusRow {
            name: name.into(),
            kind: "member",
            instance: team.into(),
            team: "example".into(),
            member: Some(name.into()),
            start_id: None,
            role: None,
            vendor: None,
            model: None,
            effort: None,
            overrides: Default::default(),
            parent: parent.map(Into::into),
            restricted_access: false,
            state: "idle".into(),
            rule: None,
            activity: None,
            remote_control_url: None,
            detail: None,
            vendor_version: None,
            delivery: None,
            input_unreadable: false,
            verified: None,
            trust_accepted: None,
            worktree: None,
            liveness: None,
            unacked: None,
            running: None,
            expected_running: false,
            start: true,
        }
    }
}
