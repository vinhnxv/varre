use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::jsonl::{self, ContentBlock, JsonlViewState, ParsedEntry};

use super::app::{App, InputMode, ViewMode};

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

    // Main layout: header + body + session info + prompt + status bar.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(10),  // body (sidebar + output)
            Constraint::Length(1), // session info bar
            Constraint::Length(3), // prompt input
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_header(f, chunks[0]);
    render_body(f, chunks[1], app);
    render_session_info(f, chunks[2], app);
    render_prompt_input(f, chunks[3], app);
    render_status_bar(f, chunks[4], app);
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
    match app.view_mode {
        ViewMode::Raw => render_raw_output(f, area, app),
        ViewMode::Jsonl => render_jsonl_output(f, area, app),
    }
}

/// Render Raw mode output (tmux capture-pane).
fn render_raw_output(f: &mut Frame, area: Rect, app: &App) {
    let output = app.selected_output();
    let mode_tag = match app.view_mode {
        ViewMode::Raw => "[RAW]",
        ViewMode::Jsonl => "[JSONL]",
    };
    let title = app
        .selected_session()
        .map(|s| format!(" Output — {} ({}) {mode_tag} ", s.display_name, s.status_text()))
        .unwrap_or_else(|| " Output ".to_string());

    let total_lines = output.len() as u16;
    let visible_height = area.height.saturating_sub(2);

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

/// Render JSONL mode output (structured session log).
fn render_jsonl_output(f: &mut Frame, area: Rect, app: &App) {
    let pane_id = app.selected_session().map(|s| s.pane_id.as_str());
    let jstate = pane_id.and_then(|pid| app.jsonl_states.get(pid));

    let session_name = app
        .selected_session()
        .map(|s| s.display_name.as_str())
        .unwrap_or("?");
    let status_text = app
        .selected_session()
        .map(|s| s.status_text())
        .unwrap_or_default();

    // Build title with stats.
    let stats_str = jstate
        .map(|js| {
            let mut parts = Vec::new();
            if js.stats.total_input_tokens > 0 || js.stats.total_output_tokens > 0 {
                parts.push(format!(
                    "IN:{} OUT:{}",
                    jsonl::format_tokens(js.stats.total_input_tokens),
                    jsonl::format_tokens(js.stats.total_output_tokens)
                ));
            }
            if js.stats.total_cost_usd > 0.0 {
                parts.push(jsonl::format_cost(js.stats.total_cost_usd));
            }
            if js.stats.num_turns > 0 {
                parts.push(format!("{} turns", js.stats.num_turns));
            }
            if let Some(ref model) = js.stats.model {
                // Shorten model name for display.
                let short = model
                    .replace("claude-", "")
                    .replace("-20250514", "");
                parts.push(short);
            }
            if !parts.is_empty() {
                format!(" {} ", parts.join(" | "))
            } else {
                String::new()
            }
        })
        .unwrap_or_default();

    let title = format!(" Output — {session_name} ({status_text}) [JSONL]{stats_str}");

    // Handle graceful degradation states.
    let Some(jstate) = jstate else {
        let msg = Paragraph::new("No JSONL state. Switch to a session with JSONL data.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(msg, area);
        return;
    };

    match &jstate.state {
        JsonlViewState::NotFound => {
            let msg = Paragraph::new("No JSONL file found. Watching for session logs...")
                .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(msg, area);
        }
        JsonlViewState::Empty => {
            let msg = Paragraph::new("Session active, no entries yet.")
                .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(msg, area);
        }
        JsonlViewState::Error { message } => {
            let msg = Paragraph::new(format!("JSONL error: {message}"))
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(msg, area);
        }
        JsonlViewState::Loading { .. } | JsonlViewState::Ready => {
            let is_loading = matches!(&jstate.state, JsonlViewState::Loading { .. });
            let visible_height = area.height.saturating_sub(2) as usize;

            // Build visible lines from entries.
            let mut lines: Vec<Line> = Vec::new();
            for entry in &jstate.entries {
                match entry {
                    ParsedEntry::Thinking { .. } if !app.show_thinking => continue,
                    ParsedEntry::System { .. } | ParsedEntry::Progress { .. }
                        if !app.show_system =>
                    {
                        continue;
                    }
                    _ => {}
                }
                render_entry_to_lines(entry, &mut lines);
            }

            if is_loading {
                if let JsonlViewState::Loading { count } = &jstate.state {
                    lines.push(Line::from(Span::styled(
                        format!("Loading... ({count} entries)"),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            // Parse errors footer.
            if jstate.stats.parse_errors > 0 {
                lines.push(Line::from(Span::styled(
                    format!("{} malformed entries skipped", jstate.stats.parse_errors),
                    Style::default().fg(Color::Yellow),
                )));
            }

            let total_lines = lines.len() as u16;
            let scroll = if jstate.auto_scroll {
                total_lines.saturating_sub(visible_height as u16)
            } else {
                total_lines
                    .saturating_sub(visible_height as u16)
                    .saturating_sub(jstate.scroll_offset)
            };

            let output_widget = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(title))
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));

            f.render_widget(output_widget, area);
        }
    }
}

/// Render a single ParsedEntry into display lines.
fn render_entry_to_lines<'a>(entry: &ParsedEntry, lines: &mut Vec<Line<'a>>) {
    match entry {
        ParsedEntry::User { text, .. } => {
            lines.push(Line::from(Span::styled(
                format!("> {text}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::raw(""));
        }
        ParsedEntry::Assistant { blocks, .. } => {
            for block in blocks {
                match block {
                    ContentBlock::Text(text) => {
                        for line in text.lines() {
                            lines.push(Line::from(Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::White),
                            )));
                        }
                    }
                    // ToolUse/ToolResult/Thinking rendered as separate entries
                    _ => {}
                }
            }
            lines.push(Line::raw(""));
        }
        ParsedEntry::ToolUse { name, summary, .. } => {
            let display = if summary.is_empty() {
                format!("\u{1f527} {name}")
            } else {
                format!("\u{1f527} {name}: {summary}")
            };
            lines.push(Line::from(Span::styled(
                display,
                Style::default().fg(Color::Cyan),
            )));
        }
        ParsedEntry::ToolResult { content, .. } => {
            lines.push(Line::from(Span::styled(
                content.clone(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        ParsedEntry::Thinking { text, .. } => {
            lines.push(Line::from(Span::styled(
                format!("\u{1f4ad} [thinking...] {}", truncate_display(text, 100)),
                Style::default().fg(Color::Magenta),
            )));
        }
        ParsedEntry::System { subtype, text, .. } => {
            let display = if text.is_empty() {
                format!("[system: {subtype}]")
            } else {
                format!("[system: {subtype}] {text}")
            };
            lines.push(Line::from(Span::styled(
                display,
                Style::default().fg(Color::DarkGray),
            )));
        }
        ParsedEntry::Progress { message, .. } => {
            lines.push(Line::from(Span::styled(
                format!("[progress] {message}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        ParsedEntry::Result {
            cost,
            duration_ms,
            turns,
            ..
        } => {
            let display = format!(
                "--- Session complete: {} | {} turns | {} ---",
                jsonl::format_cost(*cost),
                turns,
                jsonl::format_duration_ms(*duration_ms)
            );
            lines.push(Line::from(Span::styled(
                display,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }
}

fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Render the session info bar with process metrics or JSONL stats.
fn render_session_info(f: &mut Frame, area: Rect, app: &App) {
    let info = match app.view_mode {
        ViewMode::Raw => app
            .selected_session()
            .map(|s| s.metrics_line())
            .unwrap_or_default(),
        ViewMode::Jsonl => {
            if let Some(pane_id) = app.selected_session().map(|s| &s.pane_id) {
                if let Some(jstate) = app.jsonl_states.get(pane_id) {
                    let s = &jstate.stats;
                    let mut parts = Vec::new();
                    parts.push(format!(
                        "IN:{}  OUT:{}",
                        jsonl::format_tokens(s.total_input_tokens),
                        jsonl::format_tokens(s.total_output_tokens)
                    ));
                    if s.total_cache_read > 0 || s.total_cache_creation > 0 {
                        parts.push(format!(
                            "CACHE:{}r/{}w",
                            jsonl::format_tokens(s.total_cache_read),
                            jsonl::format_tokens(s.total_cache_creation)
                        ));
                    }
                    parts.push(jsonl::format_cost(s.total_cost_usd));
                    parts.push(format!("{} turns", s.num_turns));
                    if let Some(ref m) = s.model {
                        parts.push(m.clone());
                    }
                    if s.parse_errors > 0 {
                        parts.push(format!("{} skipped", s.parse_errors));
                    }
                    parts.join("  ")
                } else {
                    "No JSONL data".to_string()
                }
            } else {
                String::new()
            }
        }
    };

    let bar = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(info, Style::default().fg(Color::Cyan)),
    ]));

    f.render_widget(bar, area);
}

/// Render the prompt input / instructions area.
fn render_prompt_input(f: &mut Frame, area: Rect, app: &App) {
    let has_sessions = !app.sessions.is_empty();

    let (title, text_style, border_style, hint) = match app.input_mode {
        InputMode::Normal => {
            let hint = if !has_sessions {
                "No Claude Code sessions found. Press Ctrl+N to create one."
            } else {
                "Press 'i' to send a prompt | Ctrl+N: new session | 'd': kill | 'q': quit"
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

    let view_mode_indicator = match app.view_mode {
        ViewMode::Raw => Span::styled(
            " [RAW] ",
            Style::default().fg(Color::White).bg(Color::DarkGray),
        ),
        ViewMode::Jsonl => Span::styled(
            " [JSONL] ",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
    };

    let session_count = Span::raw(format!(" {} sessions ", app.sessions.len()));

    let status_msg = app
        .status_message
        .as_deref()
        .unwrap_or("j/k: navigate | i: input | Ctrl+N: new | d: kill | q: quit");

    // Branch + CWD of selected session, right-aligned with icons.
    let branch_raw = app
        .selected_session()
        .and_then(|s| s.metrics.git_branch.as_deref())
        .unwrap_or("-");
    // Truncate long branch names.
    let branch = if branch_raw.len() > 40 {
        format!("{}...", &branch_raw[..37])
    } else {
        branch_raw.to_string()
    };
    let pr_number = app
        .selected_session()
        .and_then(|s| s.metrics.pr_number);
    let pr_suffix = pr_number
        .map(|n| format!(" #{n}"))
        .unwrap_or_default();
    let cwd = app
        .selected_session()
        .and_then(|s| s.metrics.cwd.as_deref())
        .unwrap_or("-");
    let right_display = format!(" \u{2387} {}{} \u{2502} {} ", branch, pr_suffix, cwd);

    // Calculate left side width to pad with spaces for right-alignment.
    let view_tag = match app.view_mode {
        ViewMode::Raw => "[RAW]",
        ViewMode::Jsonl => "[JSONL]",
    };
    let left_parts = format!(" {} {} {} sessions  {status_msg}", match app.input_mode {
        InputMode::Normal => "NORMAL",
        InputMode::Insert => "INSERT",
    }, view_tag, app.sessions.len());
    let padding = (area.width as usize).saturating_sub(left_parts.len() + right_display.len());

    let bar = Paragraph::new(Line::from(vec![
        mode_indicator,
        view_mode_indicator,
        session_count,
        Span::styled(
            format!(" {status_msg}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" ".repeat(padding)),
        Span::styled(
            format!(" \u{2387} {branch}"),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("{pr_suffix} "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            "\u{2502}",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(" {cwd} "),
            Style::default().fg(Color::Cyan),
        ),
    ]));

    f.render_widget(bar, area);
}
