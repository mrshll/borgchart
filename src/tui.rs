//! `brg status -w` (a refreshing table) and `brg status -i` (an interactive screen).

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::address::Instance;
use crate::runtime::{self, StatusRow};
use crate::tree;

const REFRESH: Duration = Duration::from_secs(2);

/// `brg status -w`: print the table every few seconds until interrupted.
pub fn watch(inst: &Instance, interval: Duration) -> Result<(), String> {
    loop {
        let rows = runtime::status(inst)?;
        let mut out = std::io::stdout();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = write!(
            out,
            "\x1b[H\x1b[2J{}\nrefreshed at {} (UTC epoch); Ctrl-C to stop\n",
            runtime::render_status(&runtime::header(inst), &rows),
            now
        );
        let _ = out.flush();
        std::thread::sleep(interval);
    }
}

fn state_style(row: &StatusRow) -> Style {
    let alert = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
    match row.state.as_str() {
        "blocked" | "exited" => alert,
        // A `start: true` member that ended.
        runtime::STOPPED if row.expected_running => alert,
        "working" => Style::default().add_modifier(Modifier::BOLD),
        runtime::STOPPED | runtime::NOT_STARTED => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    team: &str,
    rows: &[StatusRow],
    table_state: &mut TableState,
    message: &str,
) {
    let [top, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(frame.area());
    // The same tree and columns as `brg status`.
    let nodes: Vec<tree::Node> = rows
        .iter()
        .filter(|r| r.kind == "member" || r.kind == "role")
        .map(|r| tree::Node {
            id: r.name.clone(),
            parent: r.parent.clone(),
            cols: Vec::new(),
        })
        .collect();
    let prefixes: std::collections::BTreeMap<String, String> = tree::order(&nodes)
        .into_iter()
        .map(|(i, p)| (nodes[i].id.clone(), p))
        .collect();
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|r| {
            let name = match r.kind {
                "member" | "role" => format!(
                    "{}{}",
                    prefixes.get(&r.name).cloned().unwrap_or_default(),
                    r.name
                ),
                _ => format!("plugin {}", r.name),
            };
            let mut cols = runtime::status_cells(r);
            match r.kind {
                "member" => {}
                // A role with no member yet: `not created (start: false)` as its state.
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
        .zip(rows)
        .map(|([name, vendor, state, detail], r)| {
            Row::new(vec![
                Cell::from(name),
                Cell::from(vendor),
                Cell::from(state).style(state_style(r)),
                Cell::from(detail),
            ])
        })
        .collect();
    let table = Table::new(body, widths)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ")
        .block(Block::bordered().title(format!(" {team} ")));
    frame.render_stateful_widget(table, top, table_state);
    let help =
        Line::from("j/k move · enter attach (detach: tmux prefix, then d) · r refresh · q quit");
    frame.render_widget(
        Paragraph::new(vec![Line::from(message.to_string()), help]),
        bottom,
    );
}

/// `brg status -i`.
pub fn interactive(inst: &Instance) -> Result<(), String> {
    let mut rows = runtime::status(inst)?;
    // The same header line as `brg status`: the instance, its team folder
    // and its working folder.
    let team = runtime::header(inst);
    let mut table_state = TableState::default().with_selected(Some(0));
    let mut message = String::new();
    let mut terminal = ratatui::try_init().map_err(|e| format!("cannot start the screen: {e}"))?;
    let mut last = Instant::now();
    let result = loop {
        if let Err(e) = terminal.draw(|f| draw(f, &team, &rows, &mut table_state, &message)) {
            break Err(e.to_string());
        }
        let timeout = REFRESH.saturating_sub(last.elapsed());
        let polled = event::poll(timeout).map_err(|e| e.to_string());
        match polled {
            Err(e) => break Err(e),
            Ok(false) => {
                if let Ok(r) = runtime::status(inst) {
                    rows = r;
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
                if !rows.is_empty() {
                    table_state.select(Some((selected + 1).min(rows.len() - 1)));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                table_state.select(Some(selected.saturating_sub(1)))
            }
            KeyCode::Char('r') => {
                if let Ok(r) = runtime::status(inst) {
                    rows = r;
                }
                message.clear();
            }
            KeyCode::Enter => {
                let Some(row) = rows.get(selected) else {
                    continue;
                };
                if row.state == runtime::STOPPED || row.state == runtime::NOT_STARTED {
                    message = format!("{} is not running", row.name);
                    continue;
                }
                let name = row.name.clone();
                ratatui::restore();
                let outcome = runtime::attach_and_wait(inst, &name);
                terminal = match ratatui::try_init() {
                    Ok(t) => t,
                    Err(e) => break Err(format!("cannot restore the screen: {e}")),
                };
                let _ = terminal.clear();
                message = match outcome {
                    Ok(()) => format!("back from {name}"),
                    Err(e) => e,
                };
                if let Ok(r) = runtime::status(inst) {
                    rows = r;
                }
                last = Instant::now();
            }
            _ => {}
        }
    };
    ratatui::restore();
    result
}
