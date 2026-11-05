//! `brg status -w` (a refreshing table) and `brg status -i` (an interactive screen).

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::runtime::{self, StatusRow};
use crate::tree;
use crate::validate::Input;

const REFRESH: Duration = Duration::from_secs(2);

/// `brg status -w`: print the table every few seconds until interrupted.
pub fn watch(input: &Input, interval: Duration) -> Result<(), String> {
    loop {
        let rows = runtime::status(input)?;
        let mut out = std::io::stdout();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = write!(
            out,
            "\x1b[H\x1b[2J{}\nrefreshed at {} (UTC epoch); Ctrl-C to stop\n",
            runtime::render_status(&runtime::team_name(input), &rows),
            now
        );
        let _ = out.flush();
        std::thread::sleep(interval);
    }
}

fn state_style(state: &str) -> Style {
    match state {
        "blocked" | "exited" => Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        "working" => Style::default().add_modifier(Modifier::BOLD),
        "stopped" => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    rows: &[StatusRow],
    table_state: &mut TableState,
    message: &str,
) {
    let [top, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(frame.area());
    let header = Row::new(["NAME", "KIND", "VENDOR", "POSTURE", "STATE", "DETAIL"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    // Members show where they sit in the team's tree.
    let nodes: Vec<tree::Node> = rows
        .iter()
        .filter(|r| r.kind == "member")
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
    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            let prefix = match r.kind {
                "member" => prefixes.get(&r.name).cloned().unwrap_or_default(),
                _ => String::new(),
            };
            Row::new(vec![
                Cell::from(format!("{prefix}{}", r.name)),
                Cell::from(r.kind),
                Cell::from(r.vendor.clone().unwrap_or_else(|| "-".into())),
                Cell::from(r.posture.clone().unwrap_or_else(|| "-".into())),
                Cell::from(r.state.clone()).style(state_style(&r.state)),
                Cell::from(r.detail.clone().unwrap_or_default()),
            ])
        })
        .collect();
    let table = Table::new(
        body,
        [
            Constraint::Length(20),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Min(10),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ")
    .block(Block::bordered().title(" brg status "));
    frame.render_stateful_widget(table, top, table_state);
    let help =
        Line::from("j/k move · enter attach (detach: tmux prefix, then d) · r refresh · q quit");
    frame.render_widget(
        Paragraph::new(vec![Line::from(message.to_string()), help]),
        bottom,
    );
}

/// `brg status -i`.
pub fn interactive(input: &Input) -> Result<(), String> {
    let mut rows = runtime::status(input)?;
    let mut table_state = TableState::default().with_selected(Some(0));
    let mut message = String::new();
    let mut terminal = ratatui::try_init().map_err(|e| format!("cannot start the screen: {e}"))?;
    let mut last = Instant::now();
    let result = loop {
        if let Err(e) = terminal.draw(|f| draw(f, &rows, &mut table_state, &message)) {
            break Err(e.to_string());
        }
        let timeout = REFRESH.saturating_sub(last.elapsed());
        let polled = event::poll(timeout).map_err(|e| e.to_string());
        match polled {
            Err(e) => break Err(e),
            Ok(false) => {
                if let Ok(r) = runtime::status(input) {
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
                if let Ok(r) = runtime::status(input) {
                    rows = r;
                }
                message.clear();
            }
            KeyCode::Enter => {
                let Some(row) = rows.get(selected) else {
                    continue;
                };
                if row.state == "stopped" {
                    message = format!("{} is not running", row.name);
                    continue;
                }
                let name = row.name.clone();
                ratatui::restore();
                let outcome = runtime::attach_and_wait(input, &name);
                terminal = match ratatui::try_init() {
                    Ok(t) => t,
                    Err(e) => break Err(format!("cannot restore the screen: {e}")),
                };
                let _ = terminal.clear();
                message = match outcome {
                    Ok(()) => format!("back from {name}"),
                    Err(e) => e,
                };
                if let Ok(r) = runtime::status(input) {
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
