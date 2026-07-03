//! ratatui-based search and episode-selection UI.
//!
//! Each phase runs its own terminal session (setup/teardown) so async work can
//! happen between them without holding the alternate screen.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;

use crate::api::ShowResult;

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn setup() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

fn teardown(mut terminal: Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Phase 1: search box + results list, filtered client-side as the user types.
pub fn select_show(shows: Vec<ShowResult>) -> Result<Option<ShowResult>> {
    if shows.is_empty() {
        return Ok(None);
    }
    let mut terminal = setup()?;
    let result = run_search(&mut terminal, &shows);
    teardown(terminal)?;
    result.map(|idx| idx.map(|i| shows[i].clone()))
}

fn run_search(terminal: &mut Tui, shows: &[ShowResult]) -> Result<Option<usize>> {
    let mut input = String::new();
    let mut state = ListState::default();
    state.select(Some(0));

    loop {
        let filtered: Vec<usize> = shows
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                input.is_empty() || s.name.to_lowercase().contains(&input.to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        if state.selected().map(|s| s >= filtered.len()).unwrap_or(false) {
            state.select(if filtered.is_empty() { None } else { Some(filtered.len() - 1) });
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(3)])
                .split(f.size());

            let items: Vec<ListItem> = filtered
                .iter()
                .map(|&i| {
                    let s = &shows[i];
                    ListItem::new(Line::from(format!("{}  ({} episodes)", s.name, s.episodes)))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" ani-dl — search (Enter select, Esc quit) "))
                .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, chunks[0], &mut state);

            let prompt = Paragraph::new(format!("Search: {input}"))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(prompt, chunks[1]);
        })?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Enter => {
                    if let Some(sel) = state.selected() {
                        if let Some(&idx) = filtered.get(sel) {
                            return Ok(Some(idx));
                        }
                    }
                }
                KeyCode::Down => move_sel(&mut state, filtered.len(), 1),
                KeyCode::Up => move_sel(&mut state, filtered.len(), -1),
                KeyCode::Backspace => {
                    input.pop();
                    state.select(Some(0));
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    state.select(Some(0));
                }
                _ => {}
            }
        }
    }
}

/// Phase 2: multi-select the episodes to download.
pub fn select_episodes(episodes: Vec<String>) -> Result<Vec<String>> {
    if episodes.is_empty() {
        return Ok(Vec::new());
    }
    let mut terminal = setup()?;
    let result = run_episodes(&mut terminal, &episodes);
    teardown(terminal)?;
    result
}

fn run_episodes(terminal: &mut Tui, episodes: &[String]) -> Result<Vec<String>> {
    let mut state = ListState::default();
    state.select(Some(0));
    let mut selected = vec![false; episodes.len()];

    loop {
        let count = selected.iter().filter(|s| **s).count();
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(f.size());

            let header = Paragraph::new(format!(
                "Episodes {}–{} ({} selected)  [Space toggle · a all · Enter confirm · Esc quit]",
                episodes.first().cloned().unwrap_or_default(),
                episodes.last().cloned().unwrap_or_default(),
                count,
            ))
            .style(Style::default().fg(Color::Cyan));
            f.render_widget(header, chunks[0]);

            let items: Vec<ListItem> = episodes
                .iter()
                .enumerate()
                .map(|(i, ep)| {
                    let mark = if selected[i] { "[x]" } else { "[ ]" };
                    ListItem::new(Line::from(format!("{mark} Episode {ep}")))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" select episodes "))
                .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, chunks[1], &mut state);
        })?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(Vec::new()),
                KeyCode::Enter => {
                    let chosen: Vec<String> = episodes
                        .iter()
                        .zip(selected.iter())
                        .filter(|(_, s)| **s)
                        .map(|(ep, _)| ep.clone())
                        .collect();
                    return Ok(chosen);
                }
                KeyCode::Char(' ') => {
                    if let Some(i) = state.selected() {
                        selected[i] = !selected[i];
                    }
                }
                KeyCode::Char('a') => {
                    let all = selected.iter().all(|s| *s);
                    selected.iter_mut().for_each(|s| *s = !all);
                }
                KeyCode::Down | KeyCode::Char('j') => move_sel(&mut state, episodes.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => move_sel(&mut state, episodes.len(), -1),
                _ => {}
            }
        }
    }
}

fn move_sel(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        state.select(None);
        return;
    }
    let cur = state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).rem_euclid(len as i32);
    state.select(Some(next as usize));
}
