//! `brg status -w` (a refreshing table) and `brg status -i` (an interactive
//! screen), for one team or, with none named, every team.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, TableState};

use crate::address::{self, Instance};
use crate::runtime::{self, StatusRow};
use crate::tree;

const REFRESH: Duration = Duration::from_secs(2);
const SELECTION: &str = "> ";
const FOLD_INSTANCE: &str = "~";

/// Spinner cadence; data is still read only at the chosen refresh interval.
pub const SPIN_INTERVAL: Duration = Duration::from_millis(500);

/// Painted watch uses the same tree renderer as interactive status.
#[allow(clippy::too_many_arguments)]
fn watch_draw(
    frame: &mut ratatui::Frame,
    sections: &[Section],
    headers: bool,
    messages: bool,
    selected: Option<&str>,
    feed: Option<&crate::graph::feed::Feed>,
    motion: &mut crate::graph::layout::Motion,
    now: Duration,
    errors: [&str; 2],
    refreshed: u64,
    all: bool,
) {
    let Rect { width, height, .. } = frame.area();
    let title = sections.first().map_or("", |s| s.header.as_str());
    let title = if headers { "teams" } else { title };
    if messages {
        let section = selected_source(sections, selected);
        if let Some(section) = section {
            let header = if headers && sections.len() > 1 {
                let i = sections
                    .iter()
                    .position(|s| s.inst.id == section.inst.id)
                    .unwrap();
                format!("tree {}/{} · {}", i + 1, sections.len(), section.header)
            } else {
                section.header.clone()
            };
            frame.render_widget(Paragraph::new(header), Rect::new(0, 1, width, 1));
        }
        let area = Rect::new(0, 2, width, height.saturating_sub(3));
        if let (Some(section), Some(feed)) = (section, feed) {
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
                &section.inst.id,
                feed,
                motion,
                now,
                std::time::SystemTime::now(),
            );
        } else {
            let prompt = if section.is_some() && !errors[1].is_empty() {
                errors[1]
            } else {
                "there are no teams"
            };
            frame.render_widget(Paragraph::new(prompt), area);
        }
    } else {
        draw(
            frame,
            title,
            sections,
            headers,
            &mut TableState::default(),
            "",
            None,
            true,
            working(sections).then(|| (now.as_millis() / SPIN_INTERVAL.as_millis()) as usize),
            all,
        );
    }
    let rows: Vec<_> = sections.iter().flat_map(|s| s.rows.clone()).collect();
    frame.render_widget(
        Paragraph::new(summary_line(&rows, true)),
        Rect::new(0, 0, width, 1),
    );
    let tabs = if messages {
        "tree │ ▸messages"
    } else {
        "▸tree │ messages"
    };
    let x = width.saturating_sub(18);
    frame.render_widget(Paragraph::new(tabs), Rect::new(x, 0, width - x, 1));
    let footer = Rect::new(0, height.saturating_sub(1), width, 1);
    frame.render_widget(Clear, footer);
    let error = errors.into_iter().find(|e| !e.is_empty()).unwrap_or("");
    frame.render_widget(
        Paragraph::new(if error.is_empty() {
            format!(
                "{}Tab {} · {}q quit · refreshed {} UTC",
                if messages && headers && sections.len() > 1 {
                    "j/k tree · "
                } else {
                    ""
                },
                if messages { "tree" } else { "messages" },
                if messages {
                    String::new()
                } else {
                    format!("{} · ", all_hint(all))
                },
                clock(refreshed)
            )
        } else {
            error.into()
        }),
        footer,
    );
}
fn watch_key(
    messages: &mut bool,
    selected: &mut Option<String>,
    sections: &[Section],
    code: KeyCode,
    modifiers: KeyModifiers,
    all: &mut bool,
) -> bool {
    match code {
        KeyCode::Char('j' | 'k') | KeyCode::Down | KeyCode::Up
            if *messages && sections.len() > 1 =>
        {
            let i = sections
                .iter()
                .position(|s| Some(&s.inst.id) == selected.as_ref())
                .unwrap_or(0);
            let step = if matches!(code, KeyCode::Char('j') | KeyCode::Down) {
                1
            } else {
                sections.len() - 1
            };
            *selected = Some(sections[(i + step) % sections.len()].inst.id.clone());
            true
        }
        KeyCode::Tab => {
            *messages = !*messages;
            true
        }
        KeyCode::Char('a') => {
            *all = !*all;
            true
        }
        KeyCode::Char('q') | KeyCode::Esc => false,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => false,
        _ => true,
    }
}

fn selected_source<'a>(sections: &'a [Section], selected: Option<&str>) -> Option<&'a Section> {
    sections
        .iter()
        .find(|s| Some(s.inst.id.as_str()) == selected)
        .or_else(|| sections.first())
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
    let mut all = false;
    let mut sections = load(one, all)?;
    let mut source = None;
    let mut selected = None;
    let mut feed = None;
    let mut motion = Motion::default();
    let mut messages = false;
    let mut error = String::new();
    let mut follower_error = String::new();
    let mut refresh_time = epoch();
    let mut terminal = ratatui::try_init().map_err(|e| format!("cannot start the screen: {e}"))?;
    // Alternate-screen entry may leave old terminal contents visible.
    if let Err(e) = terminal.clear() {
        ratatui::restore();
        return Err(format!("cannot clear the screen: {e}"));
    }
    let start = Instant::now();
    let mut refreshed = Instant::now();
    let mut refresh_now = false;
    let mut polled = Instant::now() - store::POLL;
    let result = (|| {
        loop {
            let refresh = std::mem::take(&mut refresh_now) || refreshed.elapsed() >= interval;
            if refresh {
                match load(one, all) {
                    Ok(new) => {
                        sections = new;
                        error.clear();
                        refresh_time = epoch();
                    }
                    Err(e) => error = e,
                }
                refreshed = Instant::now();
            }
            let section = selected_source(&sections, selected.as_deref());
            selected = section.map(|s| s.inst.id.clone());
            if selected != source || (refresh && feed.is_none()) {
                if selected != source {
                    follower_error.clear();
                }
                source = selected.clone();
                motion = Motion::default();
                feed = section
                    .map(|s| Feed::open(StorePaths::new(&s.inst.state)))
                    .transpose()
                    .unwrap_or_else(|e| {
                        follower_error = e;
                        None
                    });
            }
            if polled.elapsed() >= store::POLL {
                if let Some(feed) = &mut feed {
                    follower_error = feed.poll(start.elapsed()).err().unwrap_or_default();
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
                        selected.as_deref(),
                        feed.as_ref(),
                        &mut motion,
                        start.elapsed(),
                        [&error, &follower_error],
                        refresh_time,
                        all,
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
            if !messages && working(&sections) {
                let step = SPIN_INTERVAL.as_millis() as u64;
                let remaining = step - start.elapsed().as_millis() as u64 % step;
                timeout = timeout.min(Duration::from_millis(remaining));
            }
            if event::poll(timeout).map_err(|e| e.to_string())?
                && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
                && key.kind == KeyEventKind::Press
            {
                let was_all = all;
                if !watch_key(
                    &mut messages,
                    &mut selected,
                    &sections,
                    key.code,
                    key.modifiers,
                    &mut all,
                ) {
                    return Ok(());
                }
                refresh_now = all != was_all;
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

/// One team on the screen, with the full rows and the tree drawn from them.
struct Section {
    inst: Instance,
    header: String,
    rows: Vec<StatusRow>,
    tree: Vec<StatusRow>,
}

impl Section {
    fn new(inst: Instance, header: String, rows: Vec<StatusRow>, all: bool) -> Self {
        let rows = live_first(if all {
            rows
        } else {
            runtime::hide_empty_roles(rows)
        });
        let tree = if all {
            rows.clone()
        } else {
            fold(rows.clone())
        };
        Self {
            inst,
            header,
            rows,
            tree,
        }
    }
}

/// The named team, or every team read from the registry now. With
/// every team, one whose status fails shows its error as its header.
/// `all` shows every row, including roles that a terminal usually hides.
fn load(one: Option<&Instance>, all: bool) -> Result<Vec<Section>, String> {
    let section = |inst: Instance, rows: Result<Vec<StatusRow>, String>| {
        let (header, rows) = match rows {
            Ok(rows) => (runtime::header_as(&inst, true), rows),
            Err(e) => (format!("{}: {e}", inst.id), Vec::new()),
        };
        Section::new(inst, header, rows, all)
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

/// Keep marked rows and their ancestors, then summarize each parent's hidden children.
fn fold(rows: Vec<StatusRow>) -> Vec<StatusRow> {
    let parents: BTreeMap<_, _> = runtime::status_nodes(&rows)
        .into_iter()
        .map(|node| (node.id, node.parent))
        .collect();
    let mut kept = BTreeSet::new();
    for row in rows
        .iter()
        .filter(|r| runtime::status_look(r).mark.is_some())
    {
        let mut key = runtime::status_key(row);
        while kept.insert(key.clone()) {
            let Some(Some(parent)) = parents.get(&key) else {
                break;
            };
            key = parent.clone();
        }
    }
    let mut visible = Vec::new();
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for row in rows {
        let key = runtime::status_key(&row);
        if kept.contains(&key) {
            visible.push(row);
        } else {
            let parent = parents
                .get(&key)
                .and_then(Option::as_deref)
                .filter(|parent| parents.contains_key(*parent));
            if parent.is_none_or(|parent| kept.contains(parent)) {
                // Top-level plugins sit outside the tree, including their fold.
                let kind = if row.kind == "plugin" && row.parent.is_none() {
                    "plugin"
                } else {
                    "fold"
                };
                groups.entry((parent, kind)).or_default().push(row);
            }
        }
    }
    visible.extend(groups.into_iter().map(|((parent, kind), rows)| StatusRow {
        kind,
        // The reserved instance keeps each group's key stable and sorts it last.
        instance: FOLD_INSTANCE.into(),
        name: parent.unwrap_or_default().into(),
        parent: parent.map(str::to_owned),
        state: String::new(),
        detail: Some(fold_label(&rows)),
        unacked: None,
        delivery: None,
        running: None,
        expected_running: false,
        input_unreadable: false,
        ..rows[0].clone()
    }));
    live_first(visible)
}

fn is_fold(row: &StatusRow) -> bool {
    row.instance == FOLD_INSTANCE
}

fn fold_label(rows: &[StatusRow]) -> String {
    let mut counts = BTreeMap::new();
    for row in rows {
        let (state, _) = row.state.split_once(" (").unwrap_or((&row.state, ""));
        let bucket = match (row.kind, state) {
            ("member", runtime::STOPPED) => (0, "stopped"),
            ("member", runtime::NOT_STARTED) => (1, "not started"),
            ("team", runtime::STOPPED) => (2, "stopped team"),
            ("plugin", runtime::STOPPED) => (3, "stopped plugin"),
            ("role", "on demand") => (4, "on demand"),
            ("role", "not created") => (5, "not created"),
            _ => (6, state),
        };
        *counts.entry(bucket).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|((order, label), count)| {
            let suffix = if count > 1 && matches!(order, 2 | 3) {
                "s"
            } else {
                ""
            };
            format!("{count} {label}{suffix}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The rows in the order the screen draws them: as `brg status` on a
/// terminal, siblings that are not running after the ones that are.
fn live_first(rows: Vec<StatusRow>) -> Vec<StatusRow> {
    let nodes = runtime::status_nodes(&rows);
    let mut slots: Vec<Option<StatusRow>> = rows.into_iter().map(Some).collect();
    let mut keyed: BTreeMap<String, usize> = BTreeMap::new();
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
    rest.sort_by_key(|r| runtime::status_look(r).sorts_last());
    out.extend(rest);
    out
}

/// A screen line by what it shows: its team and, unless it is the
/// header line, its row's key.
fn line_key(sections: &[Section], (s, r): (usize, Option<usize>)) -> (String, Option<String>) {
    let section = &sections[s];
    (
        section.inst.id.clone(),
        r.map(|r| runtime::status_key(&section.tree[r])),
    )
}

/// Read the screen again, with the selection on the line it was on, if
/// that line is still drawn: `a` and a refresh add and remove lines above it.
fn reload(
    one: Option<&Instance>,
    all: bool,
    headers: bool,
    sections: &mut Vec<Section>,
    table_state: &mut TableState,
) {
    let Ok(new) = load(one, all) else {
        return;
    };
    replace_sections(new, headers, sections, table_state);
}

fn replace_sections(
    new: Vec<Section>,
    headers: bool,
    sections: &mut Vec<Section>,
    table_state: &mut TableState,
) {
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
        out.extend((0..section.tree.len()).map(|r| (s, Some(r))));
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

fn all_hint(all: bool) -> &'static str {
    if all { "a fold" } else { "a show all" }
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
    all: bool,
) {
    let [band, top, bottom] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    // The same tree and columns as `brg status`, each team's tree its own.
    let prefixes: Vec<BTreeMap<String, String>> = sections
        .iter()
        .map(|section| {
            let nodes = runtime::status_nodes(&section.tree);
            tree::order_live_first(&nodes)
                .into_iter()
                .map(|(i, p)| (nodes[i].id.clone(), p))
                .collect()
        })
        .collect();
    let prefix = |s: usize, row: &StatusRow| {
        prefixes[s]
            .get(&runtime::status_key(row))
            .map_or("", String::as_str)
    };
    let lines = entries(sections, headers);
    let cells: Vec<[String; 4]> = lines
        .iter()
        .map(|&(s, r)| {
            // A header line is drawn over its row below.
            let Some(r) = r else {
                return Default::default();
            };
            let r = &sections[s].tree[r];
            let prefix = prefix(s, r);
            if is_fold(r) {
                return [
                    format!("{prefix}▸"),
                    String::new(),
                    String::new(),
                    String::new(),
                ];
            }
            let name = format!(
                "{prefix}{} {}",
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
            let r = r.map(|r| &sections[s].tree[r]);
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
        .highlight_symbol(SELECTION)
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
    // Headers and fold labels span the row without affecting the column widths.
    let inner = top.inner(Margin::new(1, 1));
    // Every team, and there are none: say so, as `brg status` does.
    if let Some(note) = note {
        frame.render_widget(Paragraph::new(format!("  {note}")), inner);
    }
    let offset = table_state.offset();
    let selection_width = if table_state.selected().is_some() {
        SELECTION.len() as u16
    } else {
        0
    };
    for (i, &(s, r)) in lines.iter().enumerate().skip(offset) {
        let y = inner.y + (i - offset) as u16;
        if y >= inner.bottom() {
            break;
        }
        let selected = table_state.selected() == Some(i);
        let (text, mut style, x) = match r.map(|r| &sections[s].tree[r]) {
            None => {
                let symbol = if selected { SELECTION } else { "  " };
                (
                    format!("{symbol}{}", sections[s].header),
                    Style::default().add_modifier(Modifier::BOLD),
                    inner.x,
                )
            }
            Some(row) if is_fold(row) => (
                row.detail.clone().unwrap_or_default(),
                tone_style(tree::Tone::Faded, colour),
                inner.x + selection_width + prefix(s, row).chars().count() as u16 + 2,
            ),
            Some(_) => continue,
        };
        if selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        frame.render_widget(
            Paragraph::new(text).style(style),
            Rect::new(x, y, inner.right().saturating_sub(x), 1),
        );
    }
    let help = Line::from(format!(
        "j/k move · enter attach/expand (detach: tmux prefix, then d) · {} · r refresh · q quit",
        all_hint(all),
    ))
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
    let colour = tree::colour_wanted(true);
    let mut all = false;
    let mut sections = load(one, all)?;
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
    // Alternate-screen entry may leave old terminal contents visible.
    if let Err(e) = terminal.clear() {
        ratatui::restore();
        return Err(format!("cannot clear the screen: {e}"));
    }
    let mut last = Instant::now();
    let mut last_spin = Instant::now();
    let mut spin: usize = 0;
    let result = loop {
        if last.elapsed() >= REFRESH {
            reload(one, all, headers, &mut sections, &mut table_state);
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
                all,
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
                all = !all;
                reload(one, all, headers, &mut sections, &mut table_state);
                message.clear();
            }
            KeyCode::Char('r') => {
                reload(one, all, headers, &mut sections, &mut table_state);
                message.clear();
            }
            KeyCode::Enter => {
                let Some(&(s, r)) = lines.get(selected) else {
                    continue;
                };
                let section = &sections[s];
                let Some(row) = r.map(|r| &section.tree[r]) else {
                    message = format!("{} is a team; select one of its members", section.inst.id);
                    continue;
                };
                if is_fold(row) {
                    all = true;
                    reload(one, all, headers, &mut sections, &mut table_state);
                    message.clear();
                    continue;
                }
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
                reload(one, all, headers, &mut sections, &mut table_state);
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
                        false,
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
                run: None,
                start_id: None,
                role: None,
                vendor: Some(vendor.into()),
                model: None,
                effort: None,
                overrides: Default::default(),
                parent: None,
                restricted_access: false,
                state: "working".into(),
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
            run: None,
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
            restricted_access: false,
            state: crate::runtime::ON_DEMAND.into(),
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
        let sections = [super::Section::new(inst, "t".into(), vec![role], true)];
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
                    true,
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
    fn section() -> Section {
        Section::new(
            Instance {
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
            "demo".into(),
            vec![crate::graph::layout::tests::row("demo", "lead-abcd", None)],
            false,
        )
    }
    fn other_section() -> Section {
        let mut other = section();
        other.inst.id = "other".into();
        other.header = "other".into();
        other.rows = vec![crate::graph::layout::tests::row("other", "lead-efgh", None)];
        other
    }
    #[test]
    fn tree_keys_wrap_without_selecting_on_the_tree_tab_or_a_single_tree() {
        let sections = [section(), other_section()];
        let mut selected = None;
        let mut messages = false;
        for code in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('k'),
            KeyCode::Up,
        ] {
            assert!(watch_key(
                &mut messages,
                &mut selected,
                &sections,
                code,
                KeyModifiers::NONE,
                &mut false,
            ));
            assert!(selected.is_none());
        }
        messages = true;
        for (code, expected) in [
            (KeyCode::Char('j'), "other"),
            (KeyCode::Down, "demo"),
            (KeyCode::Char('k'), "other"),
            (KeyCode::Up, "demo"),
        ] {
            assert!(watch_key(
                &mut messages,
                &mut selected,
                &sections,
                code,
                KeyModifiers::NONE,
                &mut false,
            ));
            assert_eq!(selected.as_deref(), Some(expected));
        }
        for code in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('k'),
            KeyCode::Up,
        ] {
            watch_key(
                &mut messages,
                &mut selected,
                &sections[..1],
                code,
                KeyModifiers::NONE,
                &mut false,
            );
            assert_eq!(selected.as_deref(), Some("demo"));
            watch_key(
                &mut messages,
                &mut selected,
                &[],
                code,
                KeyModifiers::NONE,
                &mut false,
            );
        }
    }
    #[test]
    fn selected_tree_survives_reordering_and_falls_back_after_removal() {
        let sections = [section(), other_section()];
        assert_eq!(selected_source(&sections, None).unwrap().inst.id, "demo");
        assert_eq!(
            selected_source(&sections, Some("other")).unwrap().inst.id,
            "other"
        );
        let reordered = [other_section(), section()];
        assert_eq!(
            selected_source(&reordered, Some("other")).unwrap().inst.id,
            "other"
        );
        assert_eq!(
            selected_source(&sections[..1], Some("other"))
                .unwrap()
                .inst
                .id,
            "demo"
        );
        assert!(selected_source(&[], Some("other")).is_none());
    }
    #[test]
    fn messages_header_identifies_the_selected_tree_without_changing_the_tree_tab() {
        let sections = [section(), other_section()];
        let dir = tempfile::tempdir().unwrap();
        let feed =
            crate::graph::feed::Feed::open(crate::bus::layout::StorePaths::from_root(dir.path()))
                .unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        for (selected, heading) in [
            (None, "tree 1/2 · demo"),
            (Some("other"), "tree 2/2 · other"),
        ] {
            terminal
                .draw(|f| {
                    watch_draw(
                        f,
                        &sections,
                        true,
                        true,
                        selected,
                        Some(&feed),
                        &mut crate::graph::layout::Motion::default(),
                        Duration::ZERO,
                        ["", ""],
                        0,
                        false,
                    )
                })
                .unwrap();
            let lines: Vec<String> = terminal
                .backend()
                .buffer()
                .content
                .chunks(80)
                .map(|row| row.iter().map(|c| c.symbol()).collect())
                .collect();
            assert_eq!(lines[1].trim_end(), heading);
            assert!(lines[23].starts_with("j/k tree · Tab tree · q quit · refreshed"));
            println!("two-tree messages 80x24:\n{}", lines.join("\n"));
        }
    }
    #[test]
    fn watch_keeps_the_latest_route_visible_after_the_flights_end() {
        let mut section = section();
        for name in ["alpha-abcd", "beta-abcd", "gamma-abcd"] {
            section.rows.push(crate::graph::layout::tests::row(
                "demo",
                name,
                Some("lead-abcd"),
            ));
        }
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let mut feed = crate::graph::feed::Feed::open(paths.clone()).unwrap();
        let record = crate::bus::record::Record {
            schema: crate::bus::SCHEMA,
            id: "m1".into(),
            seq: 1,
            from: "demo/gamma-abcd".into(),
            to: "demo/beta-abcd".into(),
            at: "2026-01-01T00:00:00.000Z".into(),
            ack_required: false,
            body: "PRIVATE-MARKER-7f49".into(),
            reply_to: vec![],
        };
        crate::bus::fsx::write_new(&paths.record(1), &serde_json::to_vec(&record).unwrap())
            .unwrap();
        feed.poll(Duration::ZERO).unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        for milliseconds in [1980, 9000] {
            terminal
                .draw(|f| {
                    watch_draw(
                        f,
                        std::slice::from_ref(&section),
                        false,
                        true,
                        Some("demo"),
                        Some(&feed),
                        &mut crate::graph::layout::Motion::default(),
                        Duration::from_millis(milliseconds),
                        ["", ""],
                        0,
                        false,
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let graph: String = buffer.content[160..1600]
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(graph.contains("m1") && graph.contains('→'), "{graph}");
            let lines: Vec<String> = buffer
                .content
                .chunks(80)
                .map(|row| row.iter().map(|c| c.symbol()).collect())
                .collect();
            assert!(!lines.join("\n").contains(&record.body));
            println!("watch at {milliseconds}ms 80x24:\n{}", lines.join("\n"));
        }
    }
    #[test]
    fn grouped_watch_shows_only_eligible_team_box_titles() {
        let mut section = section();
        section.rows = crate::graph::layout::tests::grouped_rows();
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bus::layout::StorePaths::from_root(dir.path());
        let record = crate::bus::record::Record {
            schema: crate::bus::SCHEMA,
            id: "m1".into(),
            seq: 1,
            from: "external:owner".into(),
            to: "demo/lead-root".into(),
            at: "2026-01-01T00:00:00.000Z".into(),
            ack_required: false,
            body: "PRIVATE-MARKER-7f49".into(),
            reply_to: vec![],
        };
        crate::bus::fsx::write_new(&paths.record(1), &serde_json::to_vec(&record).unwrap())
            .unwrap();
        let feed = crate::graph::feed::Feed::open(paths).unwrap();
        for (w, h) in [(80, 24), (160, 48)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| {
                    watch_draw(
                        f,
                        std::slice::from_ref(&section),
                        false,
                        true,
                        Some("demo"),
                        Some(&feed),
                        &mut crate::graph::layout::Motion::default(),
                        Duration::ZERO,
                        ["", ""],
                        0,
                        false,
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let lines: Vec<String> = buffer
                .content
                .chunks(w as usize)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect())
                .collect();
            let text = lines.join("\n");
            assert!(
                text.contains("╭dev-build") && text.contains("╭dev-rank"),
                "{text}"
            );
            assert!(!text.contains("dev-search─"), "{text}");
            for label in ["dev-search", "dev-build", "dev-rank"] {
                assert!(text.contains(label), "missing {label}: {text}");
            }
            assert!(!text.contains(&record.body));
            println!("grouped messages {w}x{h}:\n{text}");
        }
    }
    #[test]
    fn selected_team_without_a_feed_shows_the_follower_error() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                watch_draw(
                    f,
                    &[section()],
                    false,
                    true,
                    None,
                    None,
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    ["", "cannot follow; repair the message store"],
                    0,
                    false,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let graph: String = buffer.content[160..1840]
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(graph.contains("cannot follow"), "{graph}");
        let footer: String = buffer.content[1840..].iter().map(|c| c.symbol()).collect();
        assert!(footer.contains("cannot follow") && !footer.contains("refreshed"));
    }
    #[test]
    fn tab_switch_preserves_the_shared_tree_renderer_without_selection() {
        let mut tab = false;
        assert!(watch_key(
            &mut tab,
            &mut None,
            &[],
            KeyCode::Tab,
            KeyModifiers::NONE,
            &mut false,
        ));
        assert!(tab);
        assert!(watch_key(
            &mut tab,
            &mut None,
            &[],
            KeyCode::Tab,
            KeyModifiers::NONE,
            &mut false,
        ));
        assert!(!tab);
        assert!(!watch_key(
            &mut tab,
            &mut None,
            &[],
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            &mut false,
        ));
        assert!(!watch_key(
            &mut tab,
            &mut None,
            &[],
            KeyCode::Esc,
            KeyModifiers::NONE,
            &mut false,
        ));
        assert!(!watch_key(
            &mut tab,
            &mut None,
            &[],
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            &mut false,
        ));
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
        let mut sections = vec![section()];
        sections[0].rows[0].state = "working".into();
        sections[0].tree = sections[0].rows.clone();
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
                Some(1),
                false,
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
                    None,
                    Some(&feed),
                    &mut crate::graph::layout::Motion::default(),
                    SPIN_INTERVAL,
                    ["", ""],
                    0,
                    false,
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
        let footer = b
            .content
            .chunks(80)
            .last()
            .unwrap()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            footer.contains("Tab messages")
                && footer.trim_end().ends_with("refreshed 00:00:00 UTC"),
            "{footer}"
        );
        watch
            .draw(|f| {
                watch_draw(
                    f,
                    &sections,
                    true,
                    true,
                    None,
                    Some(&feed),
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    ["", ""],
                    0,
                    false,
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
        assert!(
            text.contains("Tab tree") && text.contains("refreshed 00:00:00 UTC"),
            "{text}"
        );
    }
    #[test]
    fn empty_registry_and_refresh_errors_stay_visible() {
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
                    None,
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    ["cannot refresh; check the registry", ""],
                    0,
                    false,
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
        assert!(text.contains("there are no teams"));
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
                        None,
                        &mut crate::graph::layout::Motion::default(),
                        Duration::ZERO,
                        ["", ""],
                        0,
                        false,
                    )
                })
                .unwrap();
        }
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use crate::graph::layout::tests::row;
    use ratatui::{Terminal, backend::TestBackend};

    fn status(
        instance: &str,
        name: &str,
        parent: Option<&str>,
        kind: &'static str,
        state: &str,
    ) -> StatusRow {
        StatusRow {
            kind,
            state: state.into(),
            ..row(instance, name, parent)
        }
    }

    fn fixture() -> Vec<StatusRow> {
        let lead = Some("lead-root");
        let build = "demo/build";
        let archive = "demo/archive";
        let busy = Some("lead-build");
        let mut team = status(build, "build", Some("demo/lead-root"), "team", "running");
        team.running = Some(1);
        let mut worker = status(build, "worker-a", busy, "member", "stopped");
        worker.unacked = Some(3);
        let mut reviewer = status("demo", "reviewer", lead, "role", runtime::ON_DEMAND);
        reviewer.overrides.vendor = Some(crate::team::Vendor::Codex);
        let mut scout = status("demo", "scout", lead, "role", runtime::NOT_STARTING);
        scout.start = false;
        vec![
            row("demo", "lead-root", None),
            team,
            status(build, "lead-build", Some(build), "member", "working"),
            worker,
            status(build, "worker-b", busy, "member", "stopped"),
            status(build, "worker-c", busy, "member", "not started"),
            status(build, "clock", Some(build), "plugin", "stopped"),
            status(
                archive,
                "archive",
                Some("demo/lead-root"),
                "team",
                "stopped",
            ),
            status(archive, "lead-archive", Some(archive), "member", "stopped"),
            status(
                archive,
                "exporter",
                Some("lead-archive"),
                "member",
                "stopped",
            ),
            status(archive, "timer", Some(archive), "plugin", "stopped"),
            reviewer,
            status("demo", "planner", lead, "role", "not created (start: true)"),
            scout,
            status("demo", "mail", None, "plugin", "stopped"),
            status("demo", "queue", None, "plugin", "stopped"),
            status("demo", "github", None, "plugin", "alive"),
        ]
    }

    fn section(rows: Vec<StatusRow>, all: bool) -> Section {
        let inst = Instance {
            id: "demo".into(),
            state: crate::state::StateDir::from_root(std::path::Path::new("/example/state/demo")),
            record: serde_json::from_value(serde_json::json!({
                "schema": 1, "id": "demo", "template": "example", "template_dir": "/example/template",
                "folder": "/example/work", "created_at": "2026-01-01T00:00:00Z", "members": {}
            })).unwrap(),
        };
        Section::new(inst, "demo  folder /example/work".into(), rows, all)
    }

    fn paint(
        terminal: &mut Terminal<TestBackend>,
        draw: impl FnOnce(&mut ratatui::Frame),
    ) -> String {
        terminal.draw(draw).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn watch_frame(section: &Section, messages: bool, all: bool) -> String {
        paint(
            &mut Terminal::new(TestBackend::new(100, 24)).unwrap(),
            |frame| {
                watch_draw(
                    frame,
                    std::slice::from_ref(section),
                    false,
                    messages,
                    None,
                    None,
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    ["", ""],
                    0,
                    all,
                );
            },
        )
    }

    #[test]
    fn fold_defaults_to_counting_only_the_first_hidden_row_of_each_branch() {
        let screen = watch_frame(&section(fixture(), false), false, false);
        for line in [
            "│└─ · lead-root",
            "│   │  │  └─ ▸ 2 stopped · 1 not started",
            "│   │  └─ ▸ 1 stopped plugin",
            "│   └─ ▸ 1 stopped team · 1 on demand · 1 not created",
            "3 unacked",
        ] {
            assert!(screen.contains(line), "missing {line:?}:\n{screen}");
        }
        assert!(
            screen
                .lines()
                .zip(screen.lines().skip(1))
                .any(|(healthy, fold)| {
                    healthy.starts_with("│· plugin github")
                        && fold.starts_with("│▸ 2 stopped plugins")
                }),
            "{screen}"
        );
        for hidden in [
            "worker-a", "worker-b", "worker-c", "clock", "archive", "exporter", "timer",
            "reviewer", "planner", "scout", "mail", "queue",
        ] {
            assert!(!screen.contains(hidden), "visible {hidden:?}:\n{screen}");
        }
        let mut rows = fixture();
        rows.push(status("demo", "spare-root", None, "member", "stopped"));
        let screen = watch_frame(&section(rows, false), false, false);
        for line in ["│└─ ▸ 1 stopped", "│▸ 2 stopped plugins"] {
            assert!(
                screen
                    .lines()
                    .any(|row| row.trim_end_matches('│').trim_end() == line),
                "{screen}"
            );
        }
    }

    #[test]
    fn fold_keeps_marked_rows_and_their_path_through_a_stopped_team() {
        let marked = |kind, state| status("demo", "marked", Some("lead-root"), kind, state);
        let member = |state| marked("member", state);
        let plugin = |detail: &str| StatusRow {
            detail: Some(detail.into()),
            ..marked("plugin", "alive")
        };
        let delivery = serde_json::from_value(serde_json::json!({
            "outcome": "blocked", "reason": "input busy", "at": "2026-01-01T00:00:00Z",
            "id": "m1", "incarnation": "example"
        }))
        .unwrap();
        let mut expected_running = member("stopped");
        expected_running.expected_running = true;
        let mut undelivered = member("stopped");
        undelivered.delivery = Some(delivery);
        let mut unreadable_input = member("stopped");
        unreadable_input.input_unreadable = true;
        let cases = [
            (member("blocked"), '?'),
            (member("exited"), '✗'),
            (member("gate-inactive"), '!'),
            (expected_running, '!'),
            (undelivered, '!'),
            (unreadable_input, '!'),
            (plugin("error: unavailable"), '!'),
            (plugin("stale (600s since last report)"), '!'),
            (plugin("ok"), '·'),
            (member("stopping"), '◌'),
            (member("unknown"), '◌'),
            (marked("team", "unreadable"), '◌'),
        ];
        for (mut marked, mark) in cases {
            // Team rows identify themselves by their full paths.
            if marked.kind == "team" {
                marked.instance = "demo/marked".into();
                marked.parent = Some("demo/lead-root".into());
            }
            let nested = StatusRow {
                instance: if marked.kind == "team" {
                    "demo/archive/marked"
                } else {
                    "demo/archive"
                }
                .into(),
                parent: Some("demo/archive/lead-archive".into()),
                ..marked.clone()
            };
            let archive = "demo/archive";
            let rows = vec![
                row("demo", "lead-root", None),
                status("demo", "quiet-root", Some("lead-root"), "member", "stopped"),
                status(
                    archive,
                    "archive",
                    Some("demo/lead-root"),
                    "team",
                    "stopped",
                ),
                status(archive, "lead-archive", Some(archive), "member", "stopped"),
                status(
                    archive,
                    "quiet-child",
                    Some("lead-archive"),
                    "member",
                    "stopped",
                ),
                marked,
                nested,
            ];
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            let section = section(rows, false);
            let screen = paint(&mut terminal, |frame| {
                watch_draw(
                    frame,
                    &[section],
                    false,
                    false,
                    None,
                    None,
                    &mut crate::graph::layout::Motion::default(),
                    Duration::ZERO,
                    ["", ""],
                    0,
                    false,
                );
            });
            for branch in [format!("   ├─ {mark} "), format!("   │     ├─ {mark} ")] {
                assert!(
                    screen
                        .lines()
                        .any(|line| line.contains(&branch) && line.contains("marked")),
                    "{screen}"
                );
            }
            assert!(
                screen.contains("├─   archive") && screen.contains("└─   lead-archive"),
                "{screen}"
            );
            assert_eq!(screen.matches("└─ ▸ 1 stopped").count(), 2, "{screen}");
            assert!(!screen.contains("quiet-"), "{screen}");
            if mark == '✗' {
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .filter(|c| c.symbol() == "✗")
                        .all(|c| c.fg == rgb(tree::palette::ALERT))
                );
            }
        }
    }

    #[test]
    fn fold_watch_a_shows_all_and_folds_again_across_tab_switches() {
        let rows = fixture();
        let mut all = false;
        let mut messages = false;
        let press = |code, messages: &mut bool, all: &mut bool| {
            assert!(watch_key(
                messages,
                &mut None,
                &[],
                code,
                KeyModifiers::NONE,
                all
            ));
        };
        let folded = watch_frame(&section(rows.clone(), all), messages, all);
        assert!(folded.contains("Tab messages · a show all · q quit"));
        assert!(folded.contains("▸ 2 stopped"));
        press(KeyCode::Char('a'), &mut messages, &mut all);
        let current = section(rows.clone(), all);
        let expanded = watch_frame(&current, messages, all);
        assert!(expanded.contains("Tab messages · a fold · q quit"));
        for r in &rows {
            assert!(
                expanded.contains(&r.name),
                "missing {}:\n{expanded}",
                r.name
            );
        }
        assert!(!expanded.contains("▸ 2 stopped"));
        for _ in 0..2 {
            press(KeyCode::Tab, &mut messages, &mut all);
            let screen = watch_frame(&current, messages, all);
            let footer = screen.lines().last().unwrap();
            assert_eq!(footer.contains("a fold"), !messages, "{footer}");
        }
        press(KeyCode::Char('a'), &mut messages, &mut all);
        assert_eq!(watch_frame(&section(rows, all), messages, all), folded);
        println!("Folded 100x24:\n{folded}\nExpanded 100x24:\n{expanded}");
    }

    #[test]
    fn fold_refresh_keeps_selection_when_a_live_sibling_joins_the_fold() {
        let mut rows = fixture();
        rows.push(row("demo", "spare-abcd", Some("lead-root")));
        let mut stopped = rows.clone();
        stopped.last_mut().unwrap().state = runtime::STOPPED.into();
        for label in ["1 stopped team", "plugin github"] {
            let mut sections = vec![section(rows.clone(), false)];
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            let frame = watch_frame(&sections[0], false, false);
            let before = frame.lines().position(|l| l.contains(label)).unwrap();
            // Rows start below the two-line band and the border.
            let mut selection = TableState::default().with_selected(Some(before - 3));
            replace_sections(
                vec![section(stopped.clone(), false)],
                false,
                &mut sections,
                &mut selection,
            );
            let screen = paint(&mut terminal, |frame| {
                draw(
                    frame,
                    "demo",
                    &sections,
                    false,
                    &mut selection,
                    "",
                    None,
                    true,
                    None,
                    false,
                );
            });
            let after = screen
                .lines()
                .position(|l| l.starts_with("│> ") && l.contains(label))
                .unwrap_or_else(|| panic!("{screen}"));
            assert_eq!(after + 1, before, "{screen}");
            if label == "1 stopped team" {
                let line = screen.lines().nth(after).unwrap();
                assert!(line.contains("▸ 1 stopped · 1 stopped team"), "{line}");
                let x = line.chars().position(|c| c == '▸').unwrap() + 2;
                let cell = &terminal.backend().buffer()[(x as u16, after as u16)];
                assert!(cell.modifier.contains(Modifier::REVERSED));
                assert_eq!(cell.fg, rgb(tree::palette::FADED));
            }
        }
    }

    #[test]
    fn fold_leaves_stopped_rows_in_plain_status_and_json() {
        let rows = fixture();
        let plain = runtime::render_status("demo", &rows);
        let json = serde_json::to_string(&rows).unwrap();
        let folded = watch_frame(&section(rows, false), false, false);
        for name in [
            "worker-a",
            "worker-b",
            "worker-c",
            "archive",
            "lead-archive",
            "exporter",
            "clock",
        ] {
            assert!(plain.contains(name) && json.contains(name), "{name}");
            assert!(!folded.contains(name), "{folded}");
        }
    }
}
