//! `brg status -w` (a refreshing table) and `brg status -i` (an interactive
//! screen), for one instance or, with none named, every instance.

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::address::{self, Instance};
use crate::runtime::{self, StatusRow};
use crate::tree;

const REFRESH: Duration = Duration::from_secs(2);

/// `brg status -w`: print the table every few seconds until interrupted.
/// With no instance, every instance, as a bare `brg status` shows them.
pub fn watch(one: Option<&Instance>, interval: Duration) -> Result<(), String> {
    loop {
        let text = match one {
            Some(inst) => runtime::render_status(&runtime::header(inst), &runtime::status(inst)?),
            None => every_text()?,
        };
        let mut out = std::io::stdout();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = write!(
            out,
            "\x1b[H\x1b[2J{}\nrefreshed at {} (UTC epoch); Ctrl-C to stop\n",
            text, now
        );
        let _ = out.flush();
        std::thread::sleep(interval);
    }
}

/// Every instance's header and tree, read again now; an instance whose
/// status fails shows its error in its place.
fn every_text() -> Result<String, String> {
    let all = address::every()?;
    if all.is_empty() {
        return Ok(format!("{}\n", address::known()));
    }
    let mut text = String::new();
    for inst in &all {
        match runtime::status(inst) {
            Ok(rows) => text.push_str(&runtime::render_status(&runtime::header(inst), &rows)),
            Err(e) => text.push_str(&format!("{}: {e}\n", inst.id)),
        }
    }
    Ok(text)
}

/// One instance on the `-i` screen: its header line and its rows.
struct Section {
    inst: Instance,
    header: String,
    rows: Vec<StatusRow>,
}

/// The named instance, or every instance read from the registry now. With
/// every instance, one whose status fails shows its error as its header.
fn load(one: Option<&Instance>) -> Result<Vec<Section>, String> {
    let section = |inst: Instance, rows: Result<Vec<StatusRow>, String>| match rows {
        Ok(rows) => Section {
            header: runtime::header(&inst),
            inst,
            rows,
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

fn state_style(row: &StatusRow) -> Style {
    let alert = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
    match row.state.as_str() {
        "blocked" | "exited" => alert,
        // A `start: true` member that ended.
        runtime::STOPPED if row.expected_running => alert,
        "working" => Style::default().add_modifier(Modifier::BOLD),
        runtime::STOPPED | runtime::NOT_STARTED | runtime::ON_DEMAND => {
            Style::default().add_modifier(Modifier::DIM)
        }
        _ => Style::default(),
    }
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
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(frame.area());
    // The same tree and columns as `brg status`, each instance's tree its own.
    let prefixes: Vec<std::collections::BTreeMap<String, String>> = sections
        .iter()
        .map(|section| {
            let nodes: Vec<tree::Node> = section
                .rows
                .iter()
                .filter(|r| r.kind == "member" || r.kind == "role")
                .map(|r| tree::Node {
                    id: r.name.clone(),
                    parent: r.parent.clone(),
                    cols: Vec::new(),
                    dim: r.kind == "role" && r.state == runtime::ON_DEMAND,
                })
                .collect();
            tree::order(&nodes)
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
            let name = match r.kind {
                "member" | "role" => format!(
                    "{}{}",
                    prefixes[s].get(&r.name).cloned().unwrap_or_default(),
                    r.name
                ),
                _ => format!("plugin {}", r.name),
            };
            let mut cols = runtime::status_cells(r);
            match r.kind {
                "member" => {}
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
            let row = Row::new(vec![
                Cell::from(name),
                Cell::from(vendor),
                Cell::from(state).style(style),
                Cell::from(detail),
            ]);
            if r.is_some_and(|r| r.kind == "role" && r.state == runtime::ON_DEMAND) {
                row.style(Style::default().add_modifier(Modifier::DIM))
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
    let help =
        Line::from("j/k move · enter attach (detach: tmux prefix, then d) · r refresh · q quit");
    frame.render_widget(
        Paragraph::new(vec![Line::from(message.to_string()), help]),
        bottom,
    );
}

/// `brg status -i`. With no instance, every instance, each under its
/// header line; Enter attaches in the selected member's own instance.
pub fn interactive(one: Option<&Instance>) -> Result<(), String> {
    let mut sections = load(one)?;
    let headers = one.is_none();
    // One instance: the same header line as `brg status` (the instance, its
    // team folder and its working folder) as the title.
    let title = match one {
        Some(inst) => runtime::header(inst),
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
                if let Ok(s) = load(one) {
                    sections = s;
                }
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
            KeyCode::Char('r') => {
                if let Ok(s) = load(one) {
                    sections = s;
                }
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
                if row.state == runtime::STOPPED || row.state == runtime::NOT_STARTED {
                    message = format!("{} is not running", row.name);
                    continue;
                }
                let name = row.name.clone();
                let inst = section.inst.clone();
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
                if let Ok(s) = load(one) {
                    sections = s;
                }
                last = Instant::now();
            }
            _ => {}
        }
    };
    ratatui::restore();
    result
}
