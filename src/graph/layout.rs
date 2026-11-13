//! Deterministic radial clusters, easing and collision-free labels.

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
                format!(
                    "{}/{}",
                    r.instance.rsplit('/').next().unwrap_or(&r.instance),
                    r.name
                )
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
    let members: BTreeSet<_> = out.keys().cloned().collect();
    let mut outside: BTreeSet<String> = feed
        .routes
        .iter()
        .flat_map(|(a, b)| {
            [(a, b), (b, a)]
                .into_iter()
                .filter(|(a, b)| a.starts_with("external:") && members.contains(*b))
                .map(|(a, _)| a.clone())
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
            (n.parent.as_deref() == Some(&root.id) || (n.parent.is_none() && n.id != root.id))
                && !n.outside
        }))
        .collect();
    let total: usize = items
        .iter()
        .map(|n| if n.outside { 1 } else { weight(nodes, &n.id) })
        .sum();
    let first = items
        .first()
        .map_or(1, |n| if n.outside { 1 } else { weight(nodes, &n.id) });
    let mut at = -std::f64::consts::PI * first as f64 / total.max(1) as f64;
    for item in items {
        let next = at
            + std::f64::consts::TAU
                * (if item.outside {
                    1
                } else {
                    weight(nodes, &item.id)
                }) as f64
                / total as f64;
        cluster(nodes, &item.id, at, next, 1., &mut points);
        at = next;
    }
    // Reserve label room horizontally, then fit the occupied bounds vertically.
    let pad = (width as f64 * 0.20).min(26.);
    for axis in [false, true] {
        let values: Vec<_> = points
            .values()
            .map(|&(x, y)| if axis { y } else { x })
            .collect();
        let low = values.iter().copied().fold(f64::INFINITY, f64::min);
        let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let (min, max) = if axis {
            (1., height.saturating_sub(2) as f64)
        } else {
            (pad, width as f64 - 1. - pad)
        };
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
                (
                    id.clone(),
                    (
                        start.0 + (end.0 - start.0) * t,
                        start.1 + (end.1 - start.1) * t,
                    ),
                )
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
        assert_eq!(
            mid[id],
            ((old[id].0 + end[id].0) / 2., (old[id].1 + end[id].1) / 2.)
        );
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
    fn child_roots_and_scoped_parents_follow_member_routes() {
        let rows = vec![
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
            posture: None,
            state: "idle".into(),
            rule: None,
            activity: None,
            remote_control_url: None,
            detail: None,
            vendor_version: None,
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
