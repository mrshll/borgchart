//! Braille routes, directed comets, lit newest routes, team boxes and body-free traffic facts.

use super::{
    feed::{Feed, Kind},
    layout::{self, Motion, Node},
};
use crate::{bus::time, tree::palette};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    symbols::Marker,
    widgets::{
        Block, BorderType, Paragraph, Widget, Wrap,
        canvas::{Canvas, Line},
    },
};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
fn rgb((r, g, b): crate::tree::Rgb) -> Color {
    Color::Rgb(r, g, b)
}
fn arrow(dx: f64, dy: f64) -> &'static str {
    let octant = ((2. * dy).atan2(dx) * 4. / std::f64::consts::PI).round() as i32;
    ["→", "↘", "↓", "↙", "←", "↖", "↑", "↗"][octant.rem_euclid(8) as usize]
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    frame: &mut ratatui::Frame,
    area: Rect,
    nodes: &[Node],
    scope: &str,
    feed: &Feed,
    motion: &mut Motion,
    now: Duration,
    clock: SystemTime,
) {
    let h = area.height.saturating_sub(3);
    if area.width < 2 || h < 2 {
        return;
    }
    let points = motion.at(nodes, area.width, h, now);
    let mut waiting: BTreeMap<String, usize> = BTreeMap::new();
    for msg in feed.waiting.values() {
        *waiting.entry(msg.to.clone()).or_default() += 1;
    }
    let (labels, hidden) = layout::labels(nodes, &points, area.width, h, &waiting);
    for (team, [left, top, right, bottom]) in layout::team_boxes(nodes, &points, &labels, scope) {
        let rect = Rect::new(0, 0, (right - left + 1) as u16, (bottom - top + 1) as u16);
        let mut buffer = ratatui::buffer::Buffer::empty(rect);
        Block::bordered()
            .border_type(BorderType::Rounded)
            .title(team.rsplit('/').next().unwrap())
            .style(Style::default().fg(rgb(palette::FADED)))
            .render(rect, &mut buffer);
        for (i, cell) in buffer
            .content
            .iter()
            .enumerate()
            .filter(|(_, c)| c.symbol() != " ")
        {
            let (x, y) = (
                left + i as i32 % rect.width as i32,
                top + i as i32 / rect.width as i32,
            );
            if x >= 0 && x < area.width as i32 && y >= 0 && y < h as i32 {
                frame.buffer_mut()[(area.x + x as u16, area.y + y as u16)] = cell.clone();
            }
        }
    }
    let last = feed
        .last
        .as_ref()
        .and_then(|m| Some((m, *points.get(&m.from)?, *points.get(&m.to)?)));
    let flights: Vec<_> = feed
        .events
        .iter()
        .filter(|e| {
            matches!(e.kind, Kind::Sent)
                && now.saturating_sub(e.observed) < Duration::from_millis(4500)
                && now.saturating_sub(e.observed).as_millis() % 1500 < 1200
        })
        .filter_map(|e| {
            let a = *points.get(&e.msg.from)?;
            let b = *points.get(&e.msg.to)?;
            let t = (now.saturating_sub(e.observed).as_secs_f64() % 1.5) / 1.2;
            let lerp = |t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            Some((
                lerp((t - 0.2).max(0.)),
                lerp(t),
                arrow(b.0 - a.0, b.1 - a.1),
            ))
        })
        .collect();
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0., area.width as f64 - 1.])
        .y_bounds([0., h as f64 - 1.])
        .paint(|ctx| {
            let segment = |a: (f64, f64), b: (f64, f64), color| Line {
                x1: a.0,
                y1: h as f64 - 1. - a.1,
                x2: b.0,
                y2: h as f64 - 1. - b.1,
                color,
            };
            for (a, b) in nodes
                .iter()
                .filter_map(|n| n.parent.as_ref().map(|p| (&n.id, p)))
                .chain(
                    feed.routes
                        .iter()
                        .filter(|(a, b)| a.starts_with("external:") || b.starts_with("external:"))
                        .map(|(a, b)| (a, b)),
                )
            {
                if let (Some(&a), Some(&b)) = (points.get(a), points.get(b)) {
                    ctx.draw(&segment(a, b, rgb(palette::FADED)));
                }
            }
            ctx.layer();
            if let Some((_, a, b)) = last {
                ctx.draw(&segment(a, b, rgb(palette::MESSAGE)));
            }
            ctx.layer();
            for &(tail, head, _) in &flights {
                ctx.draw(&segment(tail, head, rgb(palette::MESSAGE)));
            }
        });
    frame.render_widget(canvas, Rect::new(area.x, area.y, area.width, h));
    let mut put = |x: u16, y: u16, text: String, color: Color| {
        if y < h && x < area.width {
            let width = (text.chars().count() as u16).min(area.width - x);
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(color)),
                Rect::new(area.x + x, area.y + y, width, 1),
            );
        }
    };
    if let Some((msg, a, b)) = last {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy).max(1.);
        put(
            (b.0 - dx / length).round() as u16,
            (b.1 - dy / length).round() as u16,
            arrow(dx, dy).into(),
            rgb(palette::MESSAGE),
        );
        let x = ((a.0 + b.0) / 2.).round() as u16;
        let y = ((a.1 + b.1) / 2.).round() as u16;
        let end = x.saturating_add(msg.id.len() as u16);
        if end <= area.width
            && !labels.iter().any(|l| {
                l.y == y && x <= l.x + l.text.chars().count() as u16 && end > l.x.saturating_sub(1)
            })
            && !points
                .values()
                .any(|&(px, py)| py.round() as u16 == y && (x..end).contains(&(px.round() as u16)))
        {
            put(x, y, msg.id.clone(), rgb(palette::MESSAGE));
        }
    }
    for (_, head, direction) in flights {
        put(
            head.0.round() as u16,
            head.1.round() as u16,
            direction.into(),
            rgb(palette::MESSAGE),
        );
    }
    for label in labels {
        put(
            label.x.saturating_sub(1),
            label.y,
            format!(" {} ", label.text),
            if waiting.contains_key(&label.id)
                || last.is_some_and(|(m, _, _)| m.from == label.id || m.to == label.id)
            {
                rgb(palette::MESSAGE)
            } else {
                Color::Reset
            },
        );
    }
    for n in nodes {
        if let Some(&(x, y)) = points.get(&n.id) {
            let flashing = feed.events.iter().any(|e| {
                e.msg.to == n.id
                    && matches!(e.kind, Kind::Acked(_))
                    && now.saturating_sub(e.observed) < Duration::from_secs(2)
            });
            let (glyph, color) = if n.outside {
                ("◇", palette::FADED)
            } else {
                match n.state.as_str() {
                    "working" => ("●", palette::BUSY),
                    "stopped" | "not started" => ("·", palette::FADED),
                    "exited" => ("·", palette::ALERT),
                    "blocked" => ("○", palette::ALERT),
                    _ => ("○", palette::BUSY),
                }
            };
            let pending = flashing || waiting.contains_key(&n.id);
            let color = if pending && color != palette::ALERT {
                palette::MESSAGE
            } else {
                color
            };
            put(
                x.round() as u16,
                y.round() as u16,
                if flashing { "✓" } else { glyph }.into(),
                rgb(color),
            );
        }
    }
    if hidden > 0 {
        put(
            0,
            0,
            format!("{hidden} labels hidden · widen"),
            rgb(palette::FADED),
        );
    }
    let name = |id: &str| {
        nodes.iter().find(|n| n.id == id).map_or_else(
            || layout::short(id.rsplit('/').next().unwrap_or(id), area.width),
            |n| layout::short(&n.label, area.width),
        )
    };
    let mut events = Vec::new();
    for e in feed.events.iter().rev() {
        if !points.contains_key(&e.msg.from) || !points.contains_key(&e.msg.to) {
            continue;
        }
        let at = time::format_utc(match e.kind {
            Kind::Acked(d) => e.msg.at + d,
            _ => e.msg.at,
        });
        let fact = match e.kind {
            Kind::Sent => format!("{} → {}", name(&e.msg.from), name(&e.msg.to)),
            Kind::Acked(d) => format!("acked by {} after {}", name(&e.msg.to), age(d)),
            Kind::Delivered => "delivered".into(),
        };
        events.push(format!("{} {} {fact}", &at[11..19], e.msg.id));
    }
    frame.render_widget(
        Paragraph::new(events.join(" · ")).wrap(Wrap { trim: false }),
        Rect::new(area.x, area.y + h, area.width, 2),
    );
    let mut groups: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for msg in feed.waiting.values().filter(|m| points.contains_key(&m.to)) {
        groups.entry(msg.to.clone()).or_default().push(msg);
    }
    let waiting = format!(
        "waiting {}",
        groups
            .iter()
            .map(|(to, msgs)| format!(
                "{} {} {}",
                name(to),
                msgs.iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                age(clock.duration_since(msgs[0].at).unwrap_or_default())
            ))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    frame.render_widget(
        Paragraph::new(waiting),
        Rect::new(area.x, area.y + h + 2, area.width, 1),
    );
}
fn age(d: Duration) -> String {
    let s = d.as_secs();
    let (unit, suffix) = [(86400, "d"), (3600, "h"), (60, "m"), (1, "s")]
        .into_iter()
        .find(|(unit, _)| s >= *unit)
        .unwrap_or((1, "s"));
    format!("{}{suffix}", s / unit)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{fsx, record::Record, time};
    use crate::graph::{
        feed::Feed,
        layout::{self, Motion},
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::UNIX_EPOCH;
    fn route_nodes(feed: &Feed) -> Vec<Node> {
        layout::nodes(
            &[
                layout::tests::row("demo", "lead-abcd", None),
                layout::tests::row("demo", "alpha-abcd", Some("lead-abcd")),
                layout::tests::row("demo", "beta-abcd", Some("lead-abcd")),
                layout::tests::row("demo", "gamma-abcd", Some("lead-abcd")),
            ],
            "demo",
            None,
            feed,
        )
    }
    fn send(paths: &crate::bus::layout::StorePaths, seq: u64, from: &str, to: &str) {
        let record = Record {
            schema: crate::bus::SCHEMA,
            id: format!("m{seq}"),
            seq,
            from: from.into(),
            to: to.into(),
            at: time::format_utc(UNIX_EPOCH),
            ack_required: true,
            body: "PRIVATE-MARKER-7f49".into(),
            reply_to: vec![],
        };
        fsx::write_new(&paths.record(seq), &serde_json::to_vec(&record).unwrap()).unwrap();
    }
    fn render(feed: &Feed, nodes: &[Node], milliseconds: u64) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    f.area(),
                    nodes,
                    "demo",
                    feed,
                    &mut Motion::default(),
                    Duration::from_millis(milliseconds),
                    UNIX_EPOCH,
                )
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }
    fn dump(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn grouping_borders_survive_except_where_routes_or_readable_labels_cover_them() {
        let dir = tempfile::tempdir().unwrap();
        let feed = Feed::open(crate::bus::layout::StorePaths::from_root(dir.path())).unwrap();
        let nodes = layout::tests::fixture();
        let points = layout::positions(&nodes, 80, 21);
        let (labels, hidden) = layout::labels(&nodes, &points, 80, 21, &BTreeMap::new());
        assert_eq!(hidden, 0);
        let boxes = layout::team_boxes(&nodes, &points, &labels, "demo");
        let buffer = render(&feed, &nodes, 9000);
        assert!(
            dump(&buffer).contains('╭') || dump(&buffer).contains('╰'),
            "{}",
            dump(&buffer)
        );
        let mut crossed = 0;
        let mut retained = 0;
        for (_, [left, top, right, bottom]) in boxes {
            for y in top.max(0)..=bottom.min(20) {
                for x in left.max(0)..=right.min(79) {
                    if x != left && x != right && y != top && y != bottom {
                        continue;
                    }
                    let cell = &buffer[(x as u16, y as u16)];
                    if cell
                        .symbol()
                        .chars()
                        .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
                    {
                        crossed += 1;
                    }
                    if ["─", "│", "╭", "╮", "╰", "╯"].contains(&cell.symbol()) {
                        assert_eq!(cell.fg, rgb(palette::FADED));
                        retained += 1;
                    }
                }
            }
        }
        assert!(
            crossed > 0 && retained > 0,
            "crossed={crossed} retained={retained}"
        );
        let mut labels_on_borders = 0;
        let boxes = layout::team_boxes(&nodes, &points, &labels, "demo");
        for label in labels {
            let text: String = (label.x..label.x + label.text.len() as u16)
                .map(|x| buffer[(x, label.y)].symbol())
                .collect();
            assert_eq!(text, label.text);
            if boxes.iter().any(|(_, [left, top, right, bottom])| {
                let y = label.y as i32;
                let x = label.x as i32;
                (y == *top || y == *bottom) && x <= *right && x + label.text.len() as i32 >= *left
                    || y >= *top
                        && y <= *bottom
                        && [*left, *right]
                            .iter()
                            .any(|border| *border >= x && *border < x + label.text.len() as i32)
            }) {
                labels_on_borders += 1;
            }
        }
        assert!(labels_on_borders > 0, "fixture exercises label over border");
        println!(
            "borders crossed by canvas={crossed}; retained={retained}; labels over borders={labels_on_borders}"
        );
    }
    #[test]
    fn shallow_upward_diagonal_uses_terminal_cell_aspect_ratio() {
        assert_eq!(arrow(30., -10.), "↗");
    }
    #[test]
    fn sent_messages_fly_three_directed_passes_with_gaps_then_stop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        for (seq, from, to, arrow) in [
            (1, "demo/gamma-abcd", "demo/beta-abcd", "→"),
            (2, "demo/lead-abcd", "demo/alpha-abcd", "↑"),
        ] {
            send(&paths, seq, from, to);
            feed.poll(Duration::ZERO).unwrap();
            let nodes = route_nodes(&feed);
            let points = layout::positions(&nodes, 80, 21);
            let a = points[from];
            let b = points[to];
            let head = (
                (a.0 + (b.0 - a.0) * 0.4).round() as u16,
                (a.1 + (b.1 - a.1) * 0.4).round() as u16,
            );
            for milliseconds in [480, 1980, 3480] {
                let buffer = render(&feed, &nodes, milliseconds);
                assert_eq!(
                    buffer[head].symbol(),
                    arrow,
                    "pass at {milliseconds}:\n{}",
                    dump(&buffer)
                );
                assert_eq!(buffer[head].fg, rgb(palette::MESSAGE));
                if seq == 1 && milliseconds == 1980 {
                    println!("mid-flight 80x24:\n{}", dump(&buffer));
                }
            }
            for milliseconds in [1300, 2800, 4300, 4500, 9000] {
                let buffer = render(&feed, &nodes, milliseconds);
                assert_ne!(buffer[head].symbol(), arrow, "gap/end at {milliseconds}");
                assert_eq!(
                    dump(&buffer),
                    dump(&render(&feed, &nodes, 9000)),
                    "gap {seq} at {milliseconds}"
                );
            }
        }
    }
    #[test]
    fn newest_acked_snapshot_lights_its_route_and_id_without_replaying_or_leaking() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        send(&paths, 1, "demo/lead-abcd", "demo/alpha-abcd");
        send(&paths, 2, "demo/gamma-abcd", "demo/beta-abcd");
        let ack = crate::bus::store::Ack {
            schema: crate::bus::SCHEMA,
            id: "m2".into(),
            by: crate::bus::context::Actor::Member {
                member: "demo/beta-abcd".into(),
                incarnation: "example.1".into(),
            },
            at: time::format_utc(UNIX_EPOCH),
        };
        fsx::write_new(&paths.ack("m2"), &serde_json::to_vec(&ack).unwrap()).unwrap();
        send(&paths, 3, "external:\x1b[31m", "demo/beta-abcd");
        let feed = Feed::open(paths).unwrap();
        let nodes = route_nodes(&feed);
        assert!(!feed.moving(Duration::ZERO));
        for milliseconds in [0, 4500, 9000] {
            let buffer = render(&feed, &nodes, milliseconds);
            let graph: String = buffer.content[..80 * 21]
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(graph.contains("m2"), "{}", dump(&buffer));
            assert!(!graph.contains("m1"));
            assert!(!dump(&buffer).contains("PRIVATE-MARKER-7f49"));
            let (labels, _) = layout::labels(
                &nodes,
                &layout::positions(&nodes, 80, 21),
                80,
                21,
                &BTreeMap::from([("demo/alpha-abcd".into(), 1)]),
            );
            for label in labels
                .iter()
                .filter(|label| ["demo/beta-abcd", "demo/gamma-abcd"].contains(&label.id.as_str()))
            {
                assert_eq!(buffer[(label.x, label.y)].fg, rgb(palette::MESSAGE));
            }
            let id = buffer.content[..80 * 21]
                .windows(2)
                .find(|pair| pair[0].symbol() == "m" && pair[1].symbol() == "2")
                .unwrap();
            assert!(id.iter().all(|cell| cell.fg == rgb(palette::MESSAGE)));
            assert!(buffer.content[..80 * 21].iter().any(|cell| {
                cell.fg == rgb(palette::MESSAGE)
                    && cell
                        .symbol()
                        .chars()
                        .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
            }));
            if milliseconds == 9000 {
                println!("after flights 80x24:\n{}", dump(&buffer));
            }
        }
    }
    #[test]
    fn newest_polled_route_replaces_the_old_highlight_after_flights() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        send(&paths, 1, "demo/lead-abcd", "demo/alpha-abcd");
        send(&paths, 2, "demo/gamma-abcd", "demo/beta-abcd");
        feed.poll(Duration::ZERO).unwrap();
        let nodes = route_nodes(&feed);
        let buffer = render(&feed, &nodes, 9000);
        let graph: String = buffer.content[..80 * 21]
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            graph.contains("m2") && !graph.contains("m1"),
            "{}",
            dump(&buffer)
        );
        assert!(feed.moving(Duration::from_millis(4499)));
        assert!(!feed.moving(Duration::from_millis(4500)));
    }
    #[test]
    fn snapshot_route_keeps_its_outside_sender_after_acknowledgement() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        send(&paths, 1, "external:owner", "demo/lead-abcd");
        let ack = crate::bus::store::Ack {
            schema: crate::bus::SCHEMA,
            id: "m1".into(),
            by: crate::bus::context::Actor::Member {
                member: "demo/lead-abcd".into(),
                incarnation: "example.1".into(),
            },
            at: time::format_utc(UNIX_EPOCH),
        };
        fsx::write_new(&paths.ack("m1"), &serde_json::to_vec(&ack).unwrap()).unwrap();
        let feed = Feed::open(paths).unwrap();
        let nodes = layout::nodes(
            &[layout::tests::row("demo", "lead-abcd", None)],
            "demo",
            None,
            &feed,
        );
        let buffer = render(&feed, &nodes, 0);
        let text = dump(&buffer);
        assert!(
            text.contains("owner") && text.contains("m1") && text.contains('↓'),
            "{text}"
        );
        assert!(!text.contains("PRIVATE-MARKER-7f49") && !feed.moving(Duration::ZERO));
    }
    #[test]
    fn persistent_id_omits_colliding_nodes_and_labels_without_overwriting_them() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        send(&paths, 1, "demo/alpha-abcd", "demo/delta-abcd");
        feed.poll(Duration::ZERO).unwrap();
        let nodes = layout::nodes(
            &[
                layout::tests::row("demo", "lead-abcd", None),
                layout::tests::row("demo", "alpha-abcd", Some("lead-abcd")),
                layout::tests::row("demo", "beta-abcd", Some("lead-abcd")),
                layout::tests::row("demo", "gamma-abcd", Some("lead-abcd")),
                layout::tests::row("demo", "delta-abcd", Some("lead-abcd")),
            ],
            "demo",
            None,
            &feed,
        );
        let buffer = render(&feed, &nodes, 9000);
        let points = layout::positions(&nodes, 80, 21);
        let waiting = BTreeMap::from([("demo/delta-abcd".into(), 1)]);
        let (labels, hidden) = layout::labels(&nodes, &points, 80, 21, &waiting);
        assert_eq!(hidden, 0);
        let root = points["demo/lead-abcd"];
        assert_eq!(
            buffer[(root.0.round() as u16, root.1.round() as u16)].symbol(),
            "○"
        );
        let graph: String = buffer.content[..80 * 21]
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!graph.contains("m1"), "{}", dump(&buffer));
        for label in labels {
            let actual: String = (label.x..label.x + label.text.len() as u16)
                .map(|x| buffer[(x, label.y)].symbol())
                .collect();
            assert_eq!(actual, label.text);
            if label.id == "demo/alpha-abcd" {
                assert_eq!(buffer[(label.x, label.y)].fg, rgb(palette::MESSAGE));
            }
            if label.id == "demo/lead-abcd" {
                assert_eq!(buffer[(label.x, label.y)].fg, Color::Reset);
            }
        }
        // Pick a real route whose midpoint meets an existing label, away from nodes.
        let nodes = layout::tests::fixture();
        let points = layout::positions(&nodes, 80, 21);
        let (labels, _) = layout::labels(&nodes, &points, 80, 21, &BTreeMap::new());
        let (from, to) = nodes
            .iter()
            .flat_map(|a| nodes.iter().map(move |b| (a, b)))
            .find(|(a, b)| {
                let a = points[&a.id];
                let b = points[&b.id];
                let (x, y) = (
                    ((a.0 + b.0) / 2.).round() as u16,
                    ((a.1 + b.1) / 2.).round() as u16,
                );
                labels.iter().any(|l| {
                    l.y == y && x <= l.x + l.text.len() as u16 && x + 2 > l.x.saturating_sub(1)
                }) && !points.values().any(|&(px, py)| {
                    py.round() as u16 == y && (x..x + 2).contains(&(px.round() as u16))
                })
            })
            .expect("fixture has a route through a label");
        let record = Record {
            schema: crate::bus::SCHEMA,
            id: "m2".into(),
            seq: 2,
            from: from.id.clone(),
            to: to.id.clone(),
            at: time::format_utc(UNIX_EPOCH),
            ack_required: false,
            body: "PRIVATE-MARKER-7f49".into(),
            reply_to: vec![],
        };
        fsx::write_new(&paths.record(2), &serde_json::to_vec(&record).unwrap()).unwrap();
        feed.poll(Duration::ZERO).unwrap();
        let buffer = render(&feed, &nodes, 9000);
        for label in labels {
            let actual: String = (label.x..label.x + label.text.len() as u16)
                .map(|x| buffer[(x, label.y)].symbol())
                .collect();
            assert_eq!(actual, label.text);
        }
        let graph: String = buffer.content[..80 * 21]
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!graph.contains("m2"), "{}", dump(&buffer));
    }
    #[test]
    fn traffic_frame_shows_comet_waiting_and_ack_without_private_text() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        let r = Record {
            schema: crate::bus::SCHEMA,
            id: "m1".into(),
            seq: 1,
            from: "external:owner".into(),
            to: "demo/lead-abcd".into(),
            at: time::format_utc(UNIX_EPOCH + Duration::from_secs(100)),
            ack_required: true,
            body: "PRIVATE-MARKER-7f49".into(),
            reply_to: vec![],
        };
        fsx::write_new(&paths.record(1), &serde_json::to_vec(&r).unwrap()).unwrap();
        feed.poll(Duration::ZERO).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let rows = vec![layout::tests::row("demo", "lead-abcd", None)];
        let nodes = layout::nodes(&rows, "demo", None, &feed);
        let mut motion = Motion::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    f.area(),
                    &nodes,
                    "demo",
                    &feed,
                    &mut motion,
                    Duration::from_millis(600),
                    UNIX_EPOCH + Duration::from_secs(141),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("[1]"), "{text}");
        assert!(text.contains("waiting") && text.contains("m1"));
        assert!(!text.contains(&r.body));
        let points = layout::positions(&nodes, 80, 21);
        let a = points["external:owner"];
        let b = points["demo/lead-abcd"];
        assert_eq!(
            buffer[(
                ((a.0 + b.0) / 2.).round() as u16,
                ((a.1 + b.1) / 2.).round() as u16
            )]
                .symbol(),
            "↓"
        );
        assert!(buffer.content.iter().any(|c| {
            c.fg == rgb(crate::tree::palette::MESSAGE)
                && c.symbol()
                    .chars()
                    .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
        }));
        let ack = crate::bus::store::Ack {
            schema: crate::bus::SCHEMA,
            id: "m1".into(),
            by: crate::bus::context::Actor::Member {
                member: r.to,
                incarnation: "example.1".into(),
            },
            at: time::format_utc(UNIX_EPOCH + Duration::from_secs(141)),
        };
        fsx::write_new(&paths.ack("m1"), &serde_json::to_vec(&ack).unwrap()).unwrap();
        feed.poll(Duration::from_secs(1)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    f.area(),
                    &nodes,
                    "demo",
                    &feed,
                    &mut motion,
                    Duration::from_secs(1),
                    UNIX_EPOCH + Duration::from_secs(141),
                )
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains('✓') && text.contains("41s"), "{text}");
        assert!(text.contains("00:02:21"), "ack timestamp: {text}");
    }
    #[test]
    fn failed_members_stay_visibly_alert_in_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        let mut feed = Feed::open(crate::bus::layout::StorePaths::from_root(dir.path())).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut nodes = layout::tests::fixture();
        for state in ["exited", "blocked"] {
            for node in &mut nodes {
                node.state = state.into();
            }
            for (seq, node) in nodes.iter().filter(|n| !n.outside).enumerate() {
                feed.waiting.insert(
                    seq as u64,
                    crate::graph::feed::Msg {
                        id: format!("m{seq}"),
                        seq: seq as u64,
                        from: "external:owner".into(),
                        to: node.id.clone(),
                        at: UNIX_EPOCH,
                        ack_required: true,
                    },
                );
            }
            terminal
                .draw(|f| {
                    draw(
                        f,
                        f.area(),
                        &nodes,
                        "demo",
                        &feed,
                        &mut Motion::default(),
                        Duration::ZERO,
                        UNIX_EPOCH,
                    )
                })
                .unwrap();
            let points = layout::positions(&nodes, 80, 21);
            for node in nodes.iter().filter(|n| !n.outside) {
                let (x, y) = points[&node.id];
                assert_eq!(
                    terminal.backend().buffer()[(x.round() as u16, y.round() as u16)].fg,
                    rgb(palette::ALERT),
                    "{state}: {}",
                    node.id
                );
            }
        }
    }
    #[test]
    fn waiting_and_ack_ages_use_whole_readable_units() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths).unwrap();
        let msg = crate::graph::feed::Msg {
            id: "m1".into(),
            seq: 1,
            from: "external:owner".into(),
            to: "demo/lead-abcd".into(),
            at: UNIX_EPOCH,
            ack_required: true,
        };
        feed.routes.insert((msg.from.clone(), msg.to.clone()));
        let nodes = layout::nodes(
            &[layout::tests::row("demo", "lead-abcd", None)],
            "demo",
            None,
            &feed,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for (seconds, expected) in [
            (0, "0s"),
            (41, "41s"),
            (59, "59s"),
            (60, "1m"),
            (419, "6m"),
            (3599, "59m"),
            (3600, "1h"),
            (14399, "3h"),
            (86399, "23h"),
            (86400, "1d"),
            (259199, "2d"),
        ] {
            let age = Duration::from_secs(seconds);
            feed.waiting.insert(1, msg.clone());
            feed.events.clear();
            feed.events.push_back(crate::graph::feed::Event {
                msg: msg.clone(),
                kind: Kind::Acked(age),
                observed: Duration::ZERO,
            });
            terminal
                .draw(|f| {
                    draw(
                        f,
                        f.area(),
                        &nodes,
                        "demo",
                        &feed,
                        &mut Motion::default(),
                        Duration::from_secs(3),
                        UNIX_EPOCH + age,
                    )
                })
                .unwrap();
            let lines: Vec<_> = terminal
                .backend()
                .buffer()
                .content
                .chunks(80)
                .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
                .collect();
            assert!(
                lines[21].contains(&format!("after {expected}")),
                "{}",
                lines[21]
            );
            assert!(lines[23].trim_end().ends_with(expected), "{}", lines[23]);
        }
    }
}
