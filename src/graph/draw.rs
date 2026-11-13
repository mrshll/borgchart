//! Braille routes, moving comets and body-free traffic facts.

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
        Paragraph, Wrap,
        canvas::{Canvas, Line},
    },
};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
fn rgb((r, g, b): crate::tree::Rgb) -> Color {
    Color::Rgb(r, g, b)
}

pub(crate) fn draw(
    frame: &mut ratatui::Frame,
    area: Rect,
    nodes: &[Node],
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
    let flights: Vec<_> = feed
        .events
        .iter()
        .filter(|e| {
            matches!(e.kind, Kind::Sent)
                && now.saturating_sub(e.observed) < Duration::from_millis(800)
        })
        .filter_map(|e| {
            let a = *points.get(&e.msg.from)?;
            let b = *points.get(&e.msg.to)?;
            let t = now.saturating_sub(e.observed).as_secs_f64() / 0.8;
            let lerp = |t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            Some((lerp((t - 0.2).max(0.)), lerp(t)))
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
            for &(tail, head) in &flights {
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
    for (_, head) in flights {
        put(
            head.0.round() as u16,
            head.1.round() as u16,
            "•".into(),
            rgb(palette::MESSAGE),
        );
    }
    for label in labels {
        put(
            label.x.saturating_sub(1),
            label.y,
            format!(" {} ", label.text),
            if waiting.contains_key(&label.id) {
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
                    &feed,
                    &mut motion,
                    Duration::from_millis(400),
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
            "•"
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
