//! `brg status -w` (a refreshing table) and `brg status -i` (an interactive
//! screen), for one team or, with none named, every team.

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, TableState};

use crate::address::{self, Instance};
use crate::runtime::{self, StatusRow};
use crate::tree;

const REFRESH: Duration = Duration::from_secs(2);

/// Spinner cadence; data is still read only at the chosen refresh interval.
pub const SPIN_INTERVAL: Duration = Duration::from_millis(500);

/// Painted watch uses the same tree renderer as interactive status.
#[allow(clippy::too_many_arguments)]
fn watch_draw(
    frame: &mut ratatui::Frame,
    sections: &[Section],
    headers: bool,
    messages: bool,
    feed: Option<&crate::graph::feed::Feed>,
    motion: &mut crate::graph::layout::Motion,
    now: Duration,
    error: &str,
) {
    let title = if headers {
        "teams".into()
    } else {
        sections
            .first()
            .map_or_else(String::new, |s| s.header.clone())
    };
    if messages {
        let area = Rect::new(
            0,
            2,
            frame.area().width,
            frame.area().height.saturating_sub(3),
        );
        if let (Some(section), Some(feed)) = (sections.first(), feed) {
            let nodes = crate::graph::layout::nodes(
                &section.rows,
                &section.inst.id,
                section.inst.record.parent.as_deref(),
                feed,
            );
            crate::graph::draw::draw(
                frame,
                area,
                &nodes,
                feed,
                motion,
                now,
                std::time::SystemTime::now(),
            );
        } else {
            frame.render_widget(
                Paragraph::new("name a team for its messages: brg status -w <team>"),
                area,
            );
        }
    } else {
        draw(
            frame,
            &title,
            sections,
            headers,
            &mut TableState::default(),
            "",
            None,
            true,
            None,
        );
    }
    let rows: Vec<_> = sections.iter().flat_map(|s| s.rows.clone()).collect();
    frame.render_widget(
        Paragraph::new(summary_line(&rows, true)),
        Rect::new(0, 0, frame.area().width, 1),
    );
    let tabs = if messages {
        "tree │ ▸messages"
    } else {
        "▸tree │ messages"
    };
    let x = frame.area().width.saturating_sub(18);
    frame.render_widget(
        Paragraph::new(tabs),
        Rect::new(x, 0, frame.area().width - x, 1),
    );
    let y = frame.area().height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(if error.is_empty() {
            "Tab messages · q quit"
        } else {
            error
        }),
        Rect::new(0, y, frame.area().width, 1),
    );
}
fn watch_key(messages: &mut bool, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Tab => {
            *messages = !*messages;
            true
        }
        KeyCode::Char('q') | KeyCode::Esc => false,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => false,
        _ => true,
    }
}

/// `brg status -w`: tabs on painted terminals, the plain stream otherwise.
pub fn watch(one: Option<&Instance>, interval: Duration) -> Result<(), String> {
    use crate::{
        bus::{layout::StorePaths, store},
        graph::{feed::Feed, layout::Motion},
    };
    use std::io::IsTerminal;
    if !tree::colour_wanted(std::io::stdout().is_terminal()) {
        return watch_plain(one, interval);
    }
    let mut sections = load(one, false)?;
    let mut source = None;
    let mut feed = None;
    let mut motion = Motion::default();
    let mut messages = false;
    let mut error = String::new();
    let mut terminal = ratatui::try_init().map_err(|e| format!("cannot start the screen: {e}"))?;
    let start = Instant::now();
    let mut refreshed = Instant::now();
    let mut polled = Instant::now() - store::POLL;
    let result = (|| {
        loop {
            if refreshed.elapsed() >= interval {
                match load(one, false) {
                    Ok(new) => {
                        sections = new;
                        error.clear();
                    }
                    Err(e) => error = e,
                }
                refreshed = Instant::now();
            }
            let selected = (sections.len() == 1).then(|| sections[0].inst.id.clone());
            if selected != source {
                source = selected;
                motion = Motion::default();
                feed = match sections.first().filter(|_| source.is_some()) {
                    Some(s) => match Feed::open(StorePaths::new(&s.inst.state)) {
                        Ok(f) => Some(f),
                        Err(e) => {
                            error = e;
                            None
                        }
                    },
                    None => None,
                };
            }
            if polled.elapsed() >= store::POLL {
                if let Some(feed) = &mut feed
                    && let Err(e) = feed.poll(start.elapsed())
                {
                    error = e;
                }
                polled = Instant::now();
            }
            terminal
                .draw(|f| {
                    watch_draw(
                        f,
                        &sections,
                        one.is_none(),
                        messages,
                        feed.as_ref(),
                        &mut motion,
                        start.elapsed(),
                        &error,
                    )
                })
                .map_err(|e| e.to_string())?;
            let moving = messages
                && (feed.as_ref().is_some_and(|f| f.moving(start.elapsed()))
                    || motion.moving(start.elapsed()));
            let mut timeout = interval
                .saturating_sub(refreshed.elapsed())
                .min(store::POLL.saturating_sub(polled.elapsed()));
            if moving {
                timeout = timeout.min(Duration::from_millis(80));
            }
            if event::poll(timeout).map_err(|e| e.to_string())?
                && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
                && key.kind == KeyEventKind::Press
                && !watch_key(&mut messages, key.code, key.modifiers)
            {
                return Ok(());
            }
        }
    })();
    ratatui::restore();
    result
}

fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Keep the pre-existing watch stream byte-for-byte without terminal painting.
fn watch_plain(one: Option<&Instance>, interval: Duration) -> Result<(), String> {
    loop {
        let (text, rows) = match one {
            Some(inst) => {
                let rows = runtime::status(inst)?;
                (runtime::render_status(&runtime::header(inst), &rows), rows)
            }
            None => every_text()?,
        };
        let footer = format!("refreshed {} UTC · Ctrl-C to stop", clock(epoch()));
        let mut out = std::io::stdout();
        if std::io::IsTerminal::is_terminal(&out) {
            let _ = write!(
                out,
                "\x1b[H\x1b[2J{}\n\n{text}\n{footer}\n",
                runtime::summary_band(&rows, false)
            );
        } else {
            let _ = write!(
                out,
                "\x1b[H\x1b[2J{text}\n{}\n{footer}\n",
                runtime::summary(&rows, false)
            );
        }
        let _ = out.flush();
        std::thread::sleep(interval);
    }
}

fn working(sections: &[Section]) -> bool {
    sections
        .iter()
        .flat_map(|s| &s.rows)
        .any(|r| runtime::status_look(r).tone == tree::Tone::Busy)
}

/// `HH:MM:SS` of a UTC epoch second.
fn clock(epoch: u64) -> String {
    let day = epoch % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, day % 3600 / 60, day % 60)
}

/// Every team's header and tree, read again now, and all their rows; a
/// team whose status fails shows its error in its place.
fn every_text() -> Result<(String, Vec<StatusRow>), String> {
    let all = address::every()?;
    if all.is_empty() {
        return Ok((format!("{}\n", address::known()), Vec::new()));
    }
    let mut text = String::new();
    let mut every = Vec::new();
    for inst in &all {
        match runtime::status(inst) {
            Ok(rows) => {
                text.push_str(&runtime::render_status(&runtime::header(inst), &rows));
                every.extend(rows);
            }
            Err(e) => text.push_str(&format!("{}: {e}\n", inst.id)),
        }
    }
    Ok((text, every))
}

/// One team on the `-i` screen: its header line and its rows.
struct Section {
    inst: Instance,
    header: String,
    rows: Vec<StatusRow>,
}

/// The named team, or every team read from the registry now. With
/// every team, one whose status fails shows its error as its header.
/// `roles` keeps the rows of roles with no member that a terminal hides
/// ([`runtime::hide_empty_roles`]).
fn load(one: Option<&Instance>, roles: bool) -> Result<Vec<Section>, String> {
    let section = |inst: Instance, rows: Result<Vec<StatusRow>, String>| match rows {
        Ok(rows) => Section {
            header: runtime::header_as(&inst, true),
            inst,
            rows: live_first(if roles {
                rows
            } else {
                runtime::hide_empty_roles(rows)
            }),
        },
        Err(e) => Section {
            header: format!("{}: {e}", inst.id),
            inst,
            rows: Vec::new(),
        },
    };
    match one {
        Some(inst) => Ok(vec![section(inst.clone(), Ok(runtime::status(inst)?))]),
        None => Ok(address::every()?
            .into_iter()
            .map(|inst| {
                let rows = runtime::status(&inst);
                section(inst, rows)
            })
            .collect()),
    }
}

/// The rows in the order the screen draws them: as `brg status` on a
/// terminal, siblings that are not running after the ones that are.
fn live_first(rows: Vec<StatusRow>) -> Vec<StatusRow> {
    let nodes = runtime::status_nodes(&rows);
    let mut slots: Vec<Option<StatusRow>> = rows.into_iter().map(Some).collect();
    let mut keyed: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (i, row) in slots.iter().enumerate() {
        if let Some(row) = row {
            keyed.insert(runtime::status_key(row), i);
        }
    }
    let mut out: Vec<StatusRow> = tree::order_live_first(&nodes)
        .into_iter()
        .filter_map(|(i, _)| keyed.get(&nodes[i].id).and_then(|&r| slots[r].take()))
        .collect();
    // Top-level plugins have no node: running ones first, as `brg status`.
    let mut rest: Vec<StatusRow> = slots.into_iter().flatten().collect();
    rest.sort_by_key(|r| runtime::status_look(r).tone == tree::Tone::Faded);
    out.extend(rest);
    out
}

/// A screen line by what it shows: its team and, unless it is the
/// header line, its row's key.
fn line_key(sections: &[Section], (s, r): (usize, Option<usize>)) -> (String, Option<String>) {
    let section = &sections[s];
    (
        section.inst.id.clone(),
        r.map(|r| runtime::status_key(&section.rows[r])),
    )
}

/// Read the screen again, with the selection on the line it was on, if
/// that line is still drawn: `a` and a refresh add and remove lines above it.
fn reload(
    one: Option<&Instance>,
    roles: bool,
    headers: bool,
    sections: &mut Vec<Section>,
    table_state: &mut TableState,
) {
    let Ok(new) = load(one, roles) else {
        return;
    };
    let selected = table_state
        .selected()
        .and_then(|i| entries(sections, headers).get(i).copied())
        .map(|line| line_key(sections, line));
    *sections = new;
    if let Some(at) = selected.and_then(|key| {
        entries(sections, headers)
            .into_iter()
            .position(|line| line_key(sections, line) == key)
    }) {
        table_state.select(Some(at));
    }
}

/// The screen's lines: (section, row), where no row is the section's
/// header line. One team has no header lines: its header is the title.
fn entries(sections: &[Section], headers: bool) -> Vec<(usize, Option<usize>)> {
    let mut out = Vec::new();
    for (s, section) in sections.iter().enumerate() {
        if headers {
            out.push((s, None));
        }
        out.extend((0..section.rows.len()).map(|r| (s, Some(r))));
    }
    out
}

fn rgb((r, g, b): tree::Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// The same tones as `brg status` paints: the state cell in the row's tone,
/// and a row that is not running faded as a whole.
fn state_style(row: &StatusRow, colour: bool) -> Style {
    tone_style(runtime::status_look(row).tone, colour)
}

fn tone_style(tone: tree::Tone, colour: bool) -> Style {
    if !colour {
        return Style::default();
    }
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match tone {
        tree::Tone::Plain => Style::default(),
        tree::Tone::Busy => bold.fg(rgb(tree::palette::BUSY)),
        tree::Tone::Team => bold.fg(rgb(tree::palette::TEAM)),
        tree::Tone::Alert => bold.fg(rgb(tree::palette::ALERT)),
        tree::Tone::Odd => Style::default().fg(rgb(tree::palette::ODD)),
        tree::Tone::Faded => Style::default().fg(rgb(tree::palette::FADED)),
    }
}

/// The same toned summary as the text status view.
fn summary_line(rows: &[StatusRow], colour: bool) -> Line<'static> {
    let title = if colour {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled("borgchart", title), Span::raw("    ")];
    for (i, (text, tone)) in runtime::summary_band_parts(rows).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let (text, style) = if colour && tone == tree::Tone::Alert {
            (
                format!(" {text} "),
                tone_style(tone, colour).add_modifier(Modifier::REVERSED),
            )
        } else {
            (text, tone_style(tone, colour))
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn draw(
    frame: &mut ratatui::Frame,
    title: &str,
    sections: &[Section],
    headers: bool,
    table_state: &mut TableState,
    message: &str,
    note: Option<&str>,
    colour: bool,
    spin: Option<usize>,
) {
    let [band, top, bottom] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    // The same tree and columns as `brg status`, each team's tree its own.
    let prefixes: Vec<std::collections::BTreeMap<String, String>> = sections
        .iter()
        .map(|section| {
            let nodes = runtime::status_nodes(&section.rows);
            tree::order_live_first(&nodes)
                .into_iter()
                .map(|(i, p)| (nodes[i].id.clone(), p))
                .collect()
        })
        .collect();
    let lines = entries(sections, headers);
    let cells: Vec<[String; 4]> = lines
        .iter()
        .map(|&(s, r)| {
            // A header line is drawn over its row below.
            let Some(r) = r else {
                return Default::default();
            };
            let r = &sections[s].rows[r];
            let name = format!(
                "{}{} {}",
                prefixes[s]
                    .get(&runtime::status_key(r))
                    .cloned()
                    .unwrap_or_default(),
                tree::state_mark(runtime::status_look(r), spin).unwrap_or(' '),
                runtime::status_name(r)
            );
            let mut cols = runtime::status_cells(r, true);
            match r.kind {
                "member" | "team" => {}
                // A role with no kept member shows its creation state.
                "role" if r.overrides.is_empty() => {
                    cols.insert(0, String::new());
                    cols.push(String::new());
                }
                "role" => {}
                _ => cols.insert(0, String::new()),
            }
            let [vendor, state, detail] = <[String; 3]>::try_from(cols).unwrap_or_default();
            let state = format!(" {state} ");
            [name, state, vendor, detail]
        })
        .collect();
    // Each column as wide as its widest cell, as `brg status` aligns them.
    let width = |i: usize| {
        cells
            .iter()
            .map(|c| c[i].chars().count())
            .max()
            .unwrap_or(0) as u16
            + 2
    };
    let widths = [
        Constraint::Length(width(0)),
        Constraint::Length(width(1)),
        Constraint::Length(width(2)),
        Constraint::Min(10),
    ];
    let body: Vec<Row> = cells
        .into_iter()
        .zip(&lines)
        .map(|([name, state, vendor, detail], &(s, r))| {
            let r = r.map(|r| &sections[s].rows[r]);
            let style = r.map_or(Style::default(), |r| state_style(r, colour));
            let faded = tone_style(tree::Tone::Faded, colour);
            let row = Row::new(vec![
                Cell::from(name).style({
                    let name =
                        if r.is_some_and(|r| runtime::status_look(r).tone == tree::Tone::Team) {
                            style
                        } else {
                            style.remove_modifier(Modifier::BOLD)
                        };
                    if r.is_some_and(|r| runtime::status_look(r).italic) {
                        name.add_modifier(Modifier::ITALIC)
                    } else {
                        name
                    }
                }),
                Cell::from(Line::from(Span::styled(
                    state,
                    if colour
                        && r.is_some_and(|r| runtime::status_look(r).tone == tree::Tone::Alert)
                    {
                        style.add_modifier(Modifier::REVERSED)
                    } else if r.is_some_and(|r| runtime::status_look(r).tone == tree::Tone::Team) {
                        Style::default()
                    } else {
                        style
                    },
                ))),
                Cell::from(vendor).style(
                    if r.is_some_and(|r| runtime::status_look(r).tone == tree::Tone::Team) {
                        style.remove_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                // What a person must do next stays readable.
                Cell::from(detail).style(
                    if r.is_some_and(|r| runtime::status_look(r).tone == tree::Tone::Alert) {
                        Style::default()
                    } else {
                        faded
                    },
                ),
            ]);
            if r.is_some_and(|r| runtime::status_look(r).tone == tree::Tone::Faded) {
                row.style(faded)
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(body, widths)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ")
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(tone_style(tree::Tone::Faded, colour))
                .title(Span::styled(
                    format!(" {title} "),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
        );
    frame.render_stateful_widget(table, top, table_state);
    // Each visible header line across the whole row, as `brg status` prints it.
    let inner = top.inner(Margin::new(1, 1));
    // Every team, and there are none: say so, as `brg status` does.
    if let Some(note) = note {
        frame.render_widget(Paragraph::new(format!("  {note}")), inner);
    }
    let offset = table_state.offset();
    for (i, &(s, r)) in lines.iter().enumerate().skip(offset) {
        let y = inner.y + (i - offset) as u16;
        if y >= inner.bottom() {
            break;
        }
        if r.is_some() {
            continue;
        }
        let selected = table_state.selected() == Some(i);
        let mut style = Style::default().add_modifier(Modifier::BOLD);
        if selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let symbol = if selected { "> " } else { "  " };
        frame.render_widget(
            Paragraph::new(format!("{symbol}{}", sections[s].header)).style(style),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
    let help = Line::from(
        "j/k move · enter attach (detach: tmux prefix, then d) · a roles · r refresh · q quit",
    )
    .style(tone_style(tree::Tone::Faded, colour));
    let every: Vec<StatusRow> = sections.iter().flat_map(|s| s.rows.clone()).collect();
    let summary = summary_line(&every, colour);
    frame.render_widget(Paragraph::new(summary), band);
    frame.render_widget(
        Paragraph::new(vec![Line::from(message.to_string()), help]),
        bottom,
    );
}

/// `brg status -i`. With no team, every team, each under its
/// header line; Enter attaches in the selected member's own team.
pub fn interactive(one: Option<&Instance>) -> Result<(), String> {
    // The `a` key shows and hides roles with no member.
    let colour = tree::colour_wanted(true);
    let mut roles = false;
    let mut sections = load(one, roles)?;
    let headers = one.is_none();
    // One team: the same header line as `brg status` on a terminal (the
    // team and its working folder) as the title.
    let title = match one {
        Some(inst) => runtime::header_as(inst, true),
        None => "teams".to_string(),
    };
    let mut table_state = TableState::default().with_selected(Some(0));
    let mut message = String::new();
    let mut terminal = ratatui::try_init().map_err(|e| format!("cannot start the screen: {e}"))?;
    let mut last = Instant::now();
    let mut last_spin = Instant::now();
    let mut spin: usize = 0;
    let result = loop {
        if last.elapsed() >= REFRESH {
            reload(one, roles, headers, &mut sections, &mut table_state);
            last = Instant::now();
        }
        let animate = colour && working(&sections);
        if animate && last_spin.elapsed() >= SPIN_INTERVAL {
            spin = spin.wrapping_add(1);
            last_spin = Instant::now();
        }
        let lines = entries(&sections, headers);
        // The list can shrink on a refresh.
        if table_state.selected().is_some_and(|s| s >= lines.len()) {
            table_state.select(Some(lines.len().saturating_sub(1)));
        }
        let note = (headers && sections.is_empty()).then(address::known);
        if let Err(e) = terminal.draw(|f| {
            draw(
                f,
                &title,
                &sections,
                headers,
                &mut table_state,
                &message,
                note.as_deref(),
                colour,
                animate.then_some(spin),
            )
        }) {
            break Err(e.to_string());
        }
        let mut timeout = REFRESH.saturating_sub(last.elapsed());
        if animate {
            timeout = timeout.min(SPIN_INTERVAL.saturating_sub(last_spin.elapsed()));
        }
        let polled = event::poll(timeout).map_err(|e| e.to_string());
        match polled {
            Err(e) => break Err(e),
            Ok(false) => continue,
            Ok(true) => {}
        }
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let selected = table_state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
            KeyCode::Char('j') | KeyCode::Down => {
                if !lines.is_empty() {
                    table_state.select(Some((selected + 1).min(lines.len() - 1)));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                table_state.select(Some(selected.saturating_sub(1)))
            }
            KeyCode::Char('a') => {
                roles = !roles;
                reload(one, roles, headers, &mut sections, &mut table_state);
                message.clear();
            }
            KeyCode::Char('r') => {
                reload(one, roles, headers, &mut sections, &mut table_state);
                message.clear();
            }
            KeyCode::Enter => {
                let Some(&(s, r)) = lines.get(selected) else {
                    continue;
                };
                let section = &sections[s];
                let Some(row) = r.map(|r| &section.rows[r]) else {
                    message = format!("{} is a team; select one of its members", section.inst.id);
                    continue;
                };
                if row.kind != "member" {
                    message = format!("{} is a {}; select one of its members", row.name, row.kind);
                    continue;
                }
                if row.state == runtime::STOPPED || row.state == runtime::NOT_STARTED {
                    message = format!("{} is not running", row.name);
                    continue;
                }
                let name = row.name.clone();
                let inst = match address::instance(&row.instance) {
                    Ok(inst) => inst,
                    Err(e) => {
                        message = e;
                        continue;
                    }
                };
                ratatui::restore();
                let outcome = runtime::attach_and_wait(&inst, &name);
                terminal = match ratatui::try_init() {
                    Ok(t) => t,
                    Err(e) => break Err(format!("cannot restore the screen: {e}")),
                };
                let _ = terminal.clear();
                message = match outcome {
                    Ok(()) => format!("back from {name}"),
                    Err(e) => e,
                };
                reload(one, roles, headers, &mut sections, &mut table_state);
                last = Instant::now();
            }
            _ => {}
        }
    };
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn spinner_advances_twice_a_second() {
        assert_eq!(super::SPIN_INTERVAL, super::Duration::from_millis(500));
    }

    #[test]
    fn interactive_frame_distinguishes_the_title_and_keys_from_the_tree() {
        use ratatui::backend::TestBackend;
        use ratatui::style::{Color, Modifier};
        let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 12)).unwrap();
        for colour in [true, false] {
            terminal
                .draw(|f| {
                    super::draw(
                        f,
                        "t",
                        &[],
                        false,
                        &mut super::TableState::default(),
                        "",
                        None,
                        colour,
                        None,
                    )
                })
                .unwrap();
            let screen = terminal.backend().buffer();
            assert_eq!(screen[(0, 2)].symbol(), "╭");
            assert!(screen[(2, 2)].modifier.contains(Modifier::BOLD));
            if colour {
                let grey = super::rgb(super::tree::palette::FADED);
                assert_eq!(screen[(0, 2)].fg, grey);
                assert_eq!(screen[(0, 11)].fg, grey);
            } else {
                assert!(screen.content.iter().all(|cell| cell.fg == Color::Reset));
            }
        }
    }

    #[test]
    fn clock_is_utc_time_of_day() {
        assert_eq!(super::clock(0), "00:00:00");
        // 2026-11-10 14:02:07 UTC.
        assert_eq!(super::clock(1_794_319_327), "14:02:07");
    }

    #[test]
    fn working_rows_take_the_busy_colour_whatever_the_vendor() {
        for vendor in ["claude", "codex"] {
            let row = super::StatusRow {
                name: "m".into(),
                kind: "member",
                instance: "i".into(),
                team: "t".into(),
                member: None,
                start_id: None,
                role: None,
                vendor: Some(vendor.into()),
                model: None,
                effort: None,
                overrides: Default::default(),
                parent: None,
                posture: None,
                state: "working".into(),
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
            };
            let style = super::state_style(&row, true);
            assert_eq!(style.fg, Some(super::rgb(super::tree::palette::BUSY)));
        }
    }
    #[test]
    fn interactive_empty_role_keeps_its_choice_and_creation_state() {
        let role = super::StatusRow {
            name: "worker".into(),
            kind: "role",
            instance: "i".into(),
            team: "t".into(),
            member: None,
            start_id: None,
            role: Some("worker".into()),
            vendor: Some("codex".into()),
            model: Some("example-model".into()),
            effort: Some("medium".into()),
            overrides: crate::overrides::Overrides {
                vendor: Some(crate::team::Vendor::Codex),
                ..Default::default()
            },
            parent: None,
            posture: Some("vendor".into()),
            state: crate::runtime::ON_DEMAND.into(),
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
            start: false,
        };
        let inst = crate::address::Instance {
            id: "i".into(),
            state: crate::state::StateDir::from_root(std::path::Path::new("/example/state/i")),
            record: serde_json::from_value(serde_json::json!({
                "schema": 1, "id": "i", "template": "t", "template_dir": "/example/template",
                "folder": "/example/work", "created_at": "2026-01-01T00:00:00Z", "members": {}
            }))
            .unwrap(),
        };
        let sections = [super::Section {
            inst,
            header: "t".into(),
            rows: vec![role],
        }];
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 12)).unwrap();
        terminal
            .draw(|f| {
                super::draw(
                    f,
                    "t",
                    &sections,
                    false,
                    &mut super::TableState::default(),
                    "",
                    None,
                    false,
                    None,
                )
            })
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Λ example-model medium"), "{screen}");
        assert!(screen.contains(crate::runtime::ON_DEMAND), "{screen}");
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    #[test]
    fn tab_switch_preserves_the_shared_tree_renderer_without_selection() {
        let mut tab = false;
        assert!(watch_key(&mut tab, KeyCode::Tab, KeyModifiers::NONE));
        assert!(tab);
        assert!(watch_key(&mut tab, KeyCode::Tab, KeyModifiers::NONE));
        assert!(!tab);
        assert!(!watch_key(&mut tab, KeyCode::Char('q'), KeyModifiers::NONE));
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = crate::graph::feed::Feed::open(paths.clone()).unwrap();
        let record = crate::bus::record::Record {
            schema: crate::bus::SCHEMA,
            id: "m1".into(),
            seq: 1,
            from: "external:owner".into(),
            to: "demo/lead-abcd".into(),
            at: "2026-01-01T00:00:00.000Z".into(),
            ack_required: true,
            reply_to: vec![],
            body: "PRIVATE-MARKER-7f49".into(),
        };
        crate::bus::fsx::write_new(&paths.record(1), &serde_json::to_vec(&record).unwrap())
            .unwrap();
        feed.poll(Duration::ZERO).unwrap();
        let sections = vec![Section {
            inst: Instance {
                id: "demo".into(),
                state: crate::state::StateDir::from_root(std::path::Path::new(
                    "/example/state/demo",
                )),
                record: serde_json::from_value(serde_json::json!({
                    "schema":1,"id":"demo","template":"example","template_dir":"/example/template",
                    "folder":"/example/work","created_at":"2026-01-01T00:00:00Z","members":{}
                }))
                .unwrap(),
            },
            header: "demo".into(),
            rows: vec![crate::graph::layout::tests::row("demo", "lead-abcd", None)],
        }];
        let mut tree = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut watch = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        tree.draw(|f| {
            draw(
                f,
                "teams",
                &sections,
                true,
                &mut TableState::default(),
                "",
                None,
                true,
                None,
            )
        })
        .unwrap();
        watch
            .draw(|f| {
                watch_draw(
                    f,
                    &sections,
                    true,
                    false,
                    Some(&feed),
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    "",
                )
            })
            .unwrap();
        let a = tree.backend().buffer();
        let b = watch.backend().buffer();
        for y in 2..22 {
            for x in 0..80 {
                assert_eq!(a[(x, y)], b[(x, y)]);
            }
        }
        let text = b.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(!text.contains(&record.body));
        watch
            .draw(|f| {
                watch_draw(
                    f,
                    &sections,
                    true,
                    true,
                    Some(&feed),
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    "",
                )
            })
            .unwrap();
        let text = watch
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("waiting") && text.contains("m1"));
        assert!(!text.contains(&record.body));
        println!(
            "messages tab 80x24:\n{}",
            watch
                .backend()
                .buffer()
                .content
                .chunks(80)
                .map(|line| line.iter().map(|c| c.symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    #[test]
    fn multiple_tree_graph_requests_a_scope_and_refresh_errors_stay_visible() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                watch_draw(
                    f,
                    &[],
                    true,
                    true,
                    None,
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    "cannot refresh; check the registry",
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
        assert!(text.contains("brg status -w <team>"));
        assert!(text.contains("cannot refresh"));
        for (w, h) in [(1, 1), (2, 3), (10, 6)] {
            terminal.resize(Rect::new(0, 0, w, h)).unwrap();
            terminal
                .draw(|f| {
                    watch_draw(
                        f,
                        &[],
                        true,
                        true,
                        None,
                        &mut crate::graph::layout::Motion::default(),
                        Duration::ZERO,
                        "",
                    )
                })
                .unwrap();
        }
    }
}
