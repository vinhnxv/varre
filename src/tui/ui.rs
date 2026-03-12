use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::app::{App, InputMode};

/// Render the TUI layout.
pub fn render(f: &mut Frame, app: &App) {
    // Show overlay if terminal is too small.
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

    // Main layout: header + body + prompt + status bar.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(10),  // body (sidebar + output)
            Constraint::Length(3), // prompt input
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_header(f, chunks[0]);
    render_body(f, chunks[1], app);
    render_prompt_input(f, chunks[2], app);
    render_status_bar(f, chunks[3], app);
}

/// Render the header bar.
fn render_header(f: &mut Frame, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " varre ",
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — Claude Code Monitor  "),
        Span::styled("[q]uit", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(header, area);
}

/// Render the body (sidebar + main panel).
fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(22), // sidebar
            Constraint::Min(40),   // main panel
        ])
        .split(area);

    render_sidebar(f, body_chunks[0], app);
    render_output(f, body_chunks[1], app);
}

/// Render the session sidebar.
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
                Span::styled(&session.display_name, style),
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

/// Render the output panel for the selected session.
fn render_output(f: &mut Frame, area: Rect, app: &App) {
    let output = app.selected_output();
    let title = app
        .selected_session()
        .map(|s| format!(" Output — {} ({}) ", s.display_name, s.status_text()))
        .unwrap_or_else(|| " Output ".to_string());

    let total_lines = output.len() as u16;
    let visible_height = area.height.saturating_sub(2); // borders

    // Calculate scroll position.
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

/// Render the prompt input / instructions area.
fn render_prompt_input(f: &mut Frame, area: Rect, app: &App) {
    let has_sessions = !app.sessions.is_empty();

    let (title, text_style, border_style, hint) = match app.input_mode {
        InputMode::Normal => {
            let hint = if !has_sessions {
                "No Claude Code sessions found in tmux. Start one in any tmux pane."
            } else {
                "Press 'i' to send a prompt | 'r' to refresh | 'q' to quit"
            };
            (
                " Instructions ",
                Style::default().fg(Color::DarkGray),
                Style::default().fg(Color::Gray),
                Some(hint),
            )
        }
        InputMode::Insert => (
            " Prompt (Enter to send, Esc to cancel) ",
            Style::default().fg(Color::White),
            Style::default().fg(Color::Yellow),
            None,
        ),
    };

    let content = if app.input_buffer.is_empty() {
        if let Some(msg) = hint {
            Paragraph::new(msg)
                .style(Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC))
        } else {
            Paragraph::new("").style(text_style)
        }
    } else {
        Paragraph::new(app.input_buffer.as_str()).style(text_style)
    };

    let input = content.block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title),
    );

    f.render_widget(input, area);

    // Show cursor in Insert mode.
    if app.input_mode == InputMode::Insert {
        let x = area.x + 1 + app.input_buffer.width() as u16;
        let y = area.y + 1;
        f.set_cursor_position((x, y));
    }
}

/// Render the status bar at the bottom.
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

    let session_count = Span::raw(format!(" {} sessions ", app.sessions.len()));

    let status_msg = app
        .status_message
        .as_deref()
        .unwrap_or("j/k: navigate | i: input | d: kill pane | q: quit");

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
