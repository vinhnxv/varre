use unicode_width::UnicodeWidthStr;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, InputMode};

/// Render the TUI layout.
pub fn render(f: &mut Frame, app: &App) {
    // GAP-8: Show overlay if terminal is too small
    if app.is_terminal_too_small() {
        let area = f.area();
        let msg = Paragraph::new(format!(
            "Terminal too small ({}x{}). Minimum: 80x24",
            app.terminal_size.0, app.terminal_size.1
        ))
        .style(Style::default().fg(Color::Red));
        f.render_widget(msg, area);
        return;
    }

    let area = f.area();

    // Main layout: header + body + status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(10),  // body
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_header(f, chunks[0], app);
    render_body(f, chunks[1], app);
    render_status_bar(f, chunks[2], app);
}

fn render_header(f: &mut Frame, area: Rect, _app: &App) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " varre ",
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — Claude Code Orchestrator  "),
        Span::styled("[q]uit", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(header, area);
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    // Split body into sidebar + main panel
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(22), // sidebar
            Constraint::Min(40),   // main panel
        ])
        .split(area);

    render_sidebar(f, body_chunks[0], app);
    render_main_panel(f, body_chunks[1], app);
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, session)| {
            let icon = session.status_icon();
            let style = if i == app.selected_index {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(Line::from(vec![
                Span::raw(format!(" {icon} ")),
                Span::styled(&session.name, style),
            ]))
        })
        .collect();

    let sidebar = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Sessions "),
    );

    f.render_widget(sidebar, area);
}

fn render_main_panel(f: &mut Frame, area: Rect, app: &App) {
    // Split main panel into output + prompt input
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),   // output
            Constraint::Length(3), // prompt input
        ])
        .split(area);

    render_output(f, main_chunks[0], app);
    render_prompt_input(f, main_chunks[1], app);
}

fn render_output(f: &mut Frame, area: Rect, app: &App) {
    let output = app.selected_output();
    let title = app
        .selected_session()
        .map(|s| format!(" Output — {} ({}) ", s.name, s.status_text()))
        .unwrap_or_else(|| " Output ".to_string());

    let total_lines = output.len() as u16;
    let visible_height = area.height.saturating_sub(2); // borders

    // Calculate scroll position
    let scroll = if app.auto_scroll {
        total_lines.saturating_sub(visible_height)
    } else {
        total_lines
            .saturating_sub(visible_height)
            .saturating_sub(app.scroll_offset)
    };

    let text: Vec<Line> = output.iter().map(|l| Line::raw(l.as_str())).collect();

    let output_widget = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(output_widget, area);
}

fn render_prompt_input(f: &mut Frame, area: Rect, app: &App) {
    let (title, style) = match app.input_mode {
        InputMode::Normal => (
            " Prompt (press 'i' to type) ",
            Style::default().fg(Color::DarkGray),
        ),
        InputMode::Insert => (
            " Prompt (Enter to send, Esc to cancel) ",
            Style::default().fg(Color::White),
        ),
    };

    let input = Paragraph::new(app.input_buffer.as_str())
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));

    f.render_widget(input, area);

    // Show cursor in Insert mode
    if app.input_mode == InputMode::Insert {
        let x = area.x + 1 + app.input_buffer.width() as u16;
        let y = area.y + 1;
        f.set_cursor_position((x, y));
    }
}

fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let mode_indicator = match app.input_mode {
        InputMode::Normal => Span::styled(
            " NORMAL ",
            Style::default().fg(Color::Black).bg(Color::Green),
        ),
        InputMode::Insert => Span::styled(
            " INSERT ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
    };

    let session_count = Span::raw(format!(
        " {} sessions ",
        app.sessions.len()
    ));

    let status_msg = app
        .status_message
        .as_deref()
        .unwrap_or("j/k: navigate | i: input | n: new | d: kill | q: quit");

    let bar = Paragraph::new(Line::from(vec![
        mode_indicator,
        session_count,
        Span::styled(
            format!(" {status_msg}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    f.render_widget(bar, area);
}
