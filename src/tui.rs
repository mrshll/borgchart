//! `brg status -w` (a refreshing table) and `brg status -i` (an interactive
//! screen), for one instance or, with none named, every instance.

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::address::{self, Instance};
use crate::runtime::{self, StatusRow};
use crate::tree;

const REFRESH: Duration = Duration::from_secs(2);

/// `brg status -w`: print the table every few seconds until interrupted.
/// With no instance, every instance, as a bare `brg status` shows them.
pub fn watch(one: Option<&Instance>, interval: Duration) -> Result<(), String> {
    use std::io::IsTerminal;
    loop {
        let (text, rows) = match one {
            Some(inst) => {
                let rows = runtime::status(inst)?;
                (runtime::render_status(&runtime::header(inst), &rows), rows)
            }
            None => every_text()?,
        };
        let colour = tree::colour_wanted(std::io::stdout().is_terminal());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let footer = format!("refreshed {} UTC · Ctrl-C to stop", clock(now));
        let footer = if colour {
            tree::ink_text(&footer, tree::palette::FADED, false)
        } else {
            footer
        };
        let mut out = std::io::stdout();
        let _ = write!(
            out,
            "\x1b[H\x1b[2J{text}\n{}\n{footer}\n",
            runtime::summary(&rows, colour)
        );
        let _ = out.flush();
        std::thread::sleep(interval);
    }
}

/// `HH:MM:SS` of a UTC epoch second.
fn clock(epoch: u64) -> String {
    let day = epoch % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, day % 3600 / 60, day % 60)
}

/// Every instance's header and tree, read again now, and all their rows; an
/// instance whose status fails shows its error in its place.
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

/// One instance on the `-i` screen: its header line and its rows.
struct Section {
    inst: Instance,
    header: String,
    rows: Vec<StatusRow>,
}

/// The named instance, or every instance read from the registry now. With
/// every instance, one whose status fails shows its error as its header.
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

/// A screen line by what it shows: its instance and, unless it is the
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
/// header line. One instance has no header lines: its header is the title.
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
fn state_style(row: &StatusRow) -> Style {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match runtime::status_look(row).tone {
        tree::Tone::Plain => Style::default(),
        tree::Tone::Busy(Some(c)) => bold.fg(rgb(c)),
        tree::Tone::Busy(None) => bold,
        tree::Tone::Alert => bold.fg(rgb(tree::palette::ALERT)),
        tree::Tone::Odd => Style::default().fg(rgb(tree::palette::ODD)),
        tree::Tone::Faded => Style::default().fg(rgb(tree::palette::FADED)),
    }
}

/// The summary line on the screen: grey, with the rows that need a person
/// in bold red, as `-w` paints it.
fn summary_line(rows: &[StatusRow]) -> Line<'static> {
    let faded = Style::default().fg(rgb(tree::palette::FADED));
    let alert = Style::default()
        .fg(rgb(tree::palette::ALERT))
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    for (i, (text, need)) in runtime::summary_parts(rows).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(text, if need { alert } else { faded }));
    }
    Line::from(spans)
}

fn draw(
    frame: &mut ratatui::Frame,
    title: &str,
    sections: &[Section],
    headers: bool,
    table_state: &mut TableState,
    message: &str,
    note: Option<&str>,
) {
    let [top, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(frame.area());
    // The same tree and columns as `brg status`, each instance's tree its own.
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
                runtime::status_look(r).mark.unwrap_or(' '),
                runtime::status_name(r)
            );
            let mut cols = runtime::status_cells(r, true);
            match r.kind {
                "member" | "copy" => {}
                // A role with no kept member shows its creation state.
                "role" => {
                    cols.insert(0, String::new());
                    cols.push(String::new());
                }
                _ => cols.insert(0, String::new()),
            }
            let [vendor, state, detail] = <[String; 3]>::try_from(cols).unwrap_or_default();
            [name, vendor, state, detail]
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
        .map(|([name, vendor, state, detail], &(s, r))| {
            let r = r.map(|r| &sections[s].rows[r]);
            let style = r.map_or(Style::default(), state_style);
            let faded = Style::default().fg(rgb(tree::palette::FADED));
            let row = Row::new(vec![
                Cell::from(name).style({
                    let name = style.remove_modifier(Modifier::BOLD);
                    if r.is_some_and(|r| runtime::status_look(r).italic) {
                        name.add_modifier(Modifier::ITALIC)
                    } else {
                        name
                    }
                }),
                Cell::from(vendor),
                Cell::from(state).style(style),
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
        .block(Block::bordered().title(format!(" {title} ")));
    frame.render_stateful_widget(table, top, table_state);
    // Each visible header line across the whole row, as `brg status` prints it.
    let inner = top.inner(Margin::new(1, 1));
    // Every instance, and there are none: say so, as `brg status` does.
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
    );
    let every: Vec<StatusRow> = sections.iter().flat_map(|s| s.rows.clone()).collect();
    let summary = summary_line(&every);
    frame.render_widget(
        Paragraph::new(vec![summary, Line::from(message.to_string()), help]),
        bottom,
    );
}

/// `brg status -i`. With no instance, every instance, each under its
/// header line; Enter attaches in the selected member's own instance.
pub fn interactive(one: Option<&Instance>) -> Result<(), String> {
    // The `a` key shows and hides roles with no member.
    let mut roles = false;
    let mut sections = load(one, roles)?;
    let headers = one.is_none();
    // One instance: the same header line as `brg status` on a terminal (the
    // instance and its working folder) as the title.
    let title = match one {
        Some(inst) => runtime::header_as(inst, true),
        None => "instances".to_string(),
    };
    let mut table_state = TableState::default().with_selected(Some(0));
    let mut message = String::new();
    let mut terminal = ratatui::try_init().map_err(|e| format!("cannot start the screen: {e}"))?;
    let mut last = Instant::now();
    let result = loop {
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
            )
        }) {
            break Err(e.to_string());
        }
        let timeout = REFRESH.saturating_sub(last.elapsed());
        let polled = event::poll(timeout).map_err(|e| e.to_string());
        match polled {
            Err(e) => break Err(e),
            Ok(false) => {
                reload(one, roles, headers, &mut sections, &mut table_state);
                last = Instant::now();
                continue;
            }
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
                    message = format!(
                        "{} is an instance; select one of its members",
                        section.inst.id
                    );
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
    fn clock_is_utc_time_of_day() {
        assert_eq!(super::clock(0), "00:00:00");
        // 2026-11-10 14:02:07 UTC.
        assert_eq!(super::clock(1_794_319_327), "14:02:07");
    }
}
