use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::jsonl::{self, ContentBlock, JsonlViewState, ParsedEntry};

use super::app::{App, InputMode, ViewMode};

// ---------------------------------------------------------------------------
// Solarized Dark palette — explicit RGB for terminal-agnostic rendering
// ---------------------------------------------------------------------------
mod sol {
    use ratatui::style::Color;

    // Base tones
    pub const BASE03: Color = Color::Rgb(0, 43, 54);      // darkest bg
    pub const BASE02: Color = Color::Rgb(7, 54, 66);      // highlight bg
    pub const BASE01: Color = Color::Rgb(88, 110, 117);   // comments, secondary
    pub const BASE00: Color = Color::Rgb(101, 123, 131);  // muted body
    pub const BASE0: Color = Color::Rgb(131, 148, 150);   // body text
    pub const BASE1: Color = Color::Rgb(147, 161, 161);   // emphasis

    // Accent colors
    pub const YELLOW: Color = Color::Rgb(181, 137, 0);
    pub const ORANGE: Color = Color::Rgb(203, 75, 22);
    pub const RED: Color = Color::Rgb(220, 50, 47);
    pub const MAGENTA: Color = Color::Rgb(211, 54, 130);
    pub const VIOLET: Color = Color::Rgb(108, 113, 196);
    pub const BLUE: Color = Color::Rgb(38, 139, 210);
    pub const CYAN: Color = Color::Rgb(42, 161, 152);
    pub const GREEN: Color = Color::Rgb(133, 153, 0);
}

/// Render the TUI layout.
pub fn render(f: &mut Frame, app: &App) {
    if app.is_terminal_too_small() {
        let area = f.area();
        let msg = Paragraph::new(format!(
            "Terminal too small ({}x{}). Minimum: 80x24",
            app.terminal_size.0, app.terminal_size.1
        ))
        .style(Style::default().fg(sol::RED));
        f.render_widget(msg, area);
        return;
    }

    let area = f.area();
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

fn render_header(f: &mut Frame, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " varre ",
            Style::default()
                .fg(sol::BASE03)
                .bg(sol::BLUE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" — Claude Code Monitor  ", Style::default().fg(sol::BASE0)),
        Span::styled("[q]uit", Style::default().fg(sol::BASE01)),
    ]));
    f.render_widget(header, area);
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(22),
            Constraint::Min(40),
        ])
        .split(area);

    render_sidebar(f, body_chunks[0], app);
    render_output(f, body_chunks[1], app);
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
                    .fg(sol::YELLOW)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(sol::BASE0)
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
            .border_style(Style::default().fg(sol::BASE01))
            .title(Span::styled(" Sessions ", Style::default().fg(sol::BASE1))),
    );
    f.render_widget(sidebar, area);
}

fn render_output(f: &mut Frame, area: Rect, app: &App) {
    match app.view_mode {
        ViewMode::Raw => render_raw_output(f, area, app),
        ViewMode::Jsonl => render_jsonl_output(f, area, app),
    }
}

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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(sol::BASE01))
                .title(Span::styled(title, Style::default().fg(sol::BASE1))),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(output_widget, area);
}

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
                let short = model.replace("claude-", "").replace("-20250514", "");
                parts.push(short);
            }
            if !parts.is_empty() {
                format!(" {} ", parts.join(" \u{2502} "))
            } else {
                String::new()
            }
        })
        .unwrap_or_default();

    let title = format!(" Output — {session_name} ({status_text}) [JSONL]{stats_str}");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(sol::CYAN))
        .title(Span::styled(title, Style::default().fg(sol::BASE1)));

    let Some(jstate) = jstate else {
        let msg = Paragraph::new("No JSONL state. Switch to a session with JSONL data.")
            .style(Style::default().fg(sol::BASE01))
            .block(block);
        f.render_widget(msg, area);
        return;
    };

    match &jstate.state {
        JsonlViewState::NotFound => {
            let msg = Paragraph::new("No JSONL file found. Watching for session logs...")
                .style(Style::default().fg(sol::BASE01).add_modifier(Modifier::ITALIC))
                .block(block);
            f.render_widget(msg, area);
        }
        JsonlViewState::Empty => {
            let msg = Paragraph::new("Session active, no entries yet.")
                .style(Style::default().fg(sol::BASE01).add_modifier(Modifier::ITALIC))
                .block(block);
            f.render_widget(msg, area);
        }
        JsonlViewState::Error { message } => {
            let msg = Paragraph::new(format!("JSONL error: {message}"))
                .style(Style::default().fg(sol::ORANGE))
                .block(block);
            f.render_widget(msg, area);
        }
        JsonlViewState::Loading { .. } | JsonlViewState::Ready => {
            let is_loading = matches!(&jstate.state, JsonlViewState::Loading { .. });
            let visible_height = area.height.saturating_sub(2) as usize;
            let content_width = area.width.saturating_sub(2) as usize;

            let mut lines: Vec<Line> = Vec::new();
            let mut entry_num: usize = 0;
            let mut cum_in: u64 = 0;
            let mut cum_out: u64 = 0;
            let mut cum_cost: f64 = 0.0;

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

                if let ParsedEntry::Assistant { usage, .. } = entry {
                    if let Some(u) = usage {
                        cum_in += u.input_tokens.unwrap_or(0);
                        cum_out += u.output_tokens.unwrap_or(0);
                    }
                }
                if let ParsedEntry::Result { cost, .. } = entry {
                    cum_cost = *cost;
                }

                entry_num += 1;
                render_entry_cclv(entry, entry_num, &mut lines, content_width,
                    cum_in, cum_out, cum_cost);
            }

            if is_loading {
                if let JsonlViewState::Loading { count } = &jstate.state {
                    lines.push(Line::from(Span::styled(
                        format!("  Loading... ({count} entries)"),
                        Style::default().fg(sol::BASE01).add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            if jstate.stats.parse_errors > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  {} malformed entries skipped", jstate.stats.parse_errors),
                    Style::default().fg(sol::ORANGE),
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
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));
            f.render_widget(output_widget, area);
        }
    }
}

// ---------------------------------------------------------------------------
// cclv-style entry rendering (solarized dark)
// ---------------------------------------------------------------------------

fn render_entry_cclv<'a>(
    entry: &ParsedEntry,
    num: usize,
    lines: &mut Vec<Line<'a>>,
    content_width: usize,
    cum_in: u64,
    cum_out: u64,
    cum_cost: f64,
) {
    let idx_style = Style::default().fg(sol::BASE01);
    let idx_prefix = format!("\u{2502}{:>3} ", num);
    let cont_prefix = "\u{2502}    ".to_string();

    match entry {
        ParsedEntry::User { text, .. } => {
            if num > 1 {
                render_token_divider(lines, content_width, cum_in, cum_out, cum_cost);
            }
            lines.push(Line::from(vec![
                Span::styled(idx_prefix.clone(), idx_style),
                Span::styled(
                    "\u{25b6} User",
                    Style::default().fg(sol::CYAN).add_modifier(Modifier::BOLD),
                ),
            ]));
            // Filter out system XML tags from user messages
            let cleaned = strip_xml_tags(text);
            let cleaned = cleaned.trim();
            if !cleaned.is_empty() {
                for line in cleaned.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    lines.push(Line::from(vec![
                        Span::styled(cont_prefix.clone(), idx_style),
                        Span::styled(line.to_string(), Style::default().fg(sol::CYAN)),
                    ]));
                }
            }
        }
        ParsedEntry::Assistant { blocks, model, .. } => {
            let model_tag = model
                .as_ref()
                .map(|m| {
                    let short = m.replace("claude-", "").replace("-20250514", "");
                    format!(" [{short}]")
                })
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(idx_prefix.clone(), idx_style),
                Span::styled(
                    format!("\u{25c0} Assistant{model_tag}"),
                    Style::default().fg(sol::GREEN).add_modifier(Modifier::BOLD),
                ),
            ]));
            let mut text_lines: Vec<String> = Vec::new();
            for block in blocks {
                if let ContentBlock::Text(text) = block {
                    for line in text.lines() {
                        text_lines.push(line.to_string());
                    }
                }
            }
            let collapse_threshold = 10;
            let summary_count = 3;
            if text_lines.len() > collapse_threshold {
                for line in text_lines.iter().take(summary_count) {
                    lines.push(Line::from(vec![
                        Span::styled(cont_prefix.clone(), idx_style),
                        Span::styled(line.clone(), Style::default().fg(sol::BASE0)),
                    ]));
                }
                let remaining = text_lines.len() - summary_count;
                lines.push(Line::from(vec![
                    Span::styled(cont_prefix.clone(), idx_style),
                    Span::styled(
                        format!("(+{remaining} more lines)"),
                        Style::default().fg(sol::BASE01),
                    ),
                ]));
            } else {
                for line in &text_lines {
                    lines.push(Line::from(vec![
                        Span::styled(cont_prefix.clone(), idx_style),
                        Span::styled(line.clone(), Style::default().fg(sol::BASE0)),
                    ]));
                }
            }
        }
        ParsedEntry::ToolUse { name, summary, .. } => {
            lines.push(Line::from(vec![
                Span::styled(idx_prefix.clone(), idx_style),
                Span::styled(
                    format!("\u{1f527} {name}"),
                    Style::default().fg(sol::YELLOW).add_modifier(Modifier::BOLD),
                ),
            ]));
            if !summary.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(cont_prefix.clone(), idx_style),
                    Span::styled(
                        format!("  {summary}"),
                        Style::default().fg(sol::YELLOW),
                    ),
                ]));
            }
        }
        ParsedEntry::ToolResult { content, .. } => {
            if !content.is_empty() {
                let result_lines: Vec<&str> = content.lines().collect();
                let max_lines = 5;
                let show = result_lines.len().min(max_lines);
                for line in result_lines.iter().take(show) {
                    lines.push(Line::from(vec![
                        Span::styled(cont_prefix.clone(), idx_style),
                        Span::styled(line.to_string(), Style::default().fg(sol::BASE01)),
                    ]));
                }
                if result_lines.len() > max_lines {
                    let remaining = result_lines.len() - max_lines;
                    lines.push(Line::from(vec![
                        Span::styled(cont_prefix.clone(), idx_style),
                        Span::styled(
                            format!("(+{remaining} more lines)"),
                            Style::default().fg(sol::BASE01),
                        ),
                    ]));
                }
            }
        }
        ParsedEntry::Thinking { text, .. } => {
            lines.push(Line::from(vec![
                Span::styled(idx_prefix.clone(), idx_style),
                Span::styled(
                    "\u{1f4ad} Thinking",
                    Style::default()
                        .fg(sol::VIOLET)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
            let think_lines: Vec<&str> = text.lines().collect();
            let max = 3;
            for line in think_lines.iter().take(max) {
                lines.push(Line::from(vec![
                    Span::styled(cont_prefix.clone(), idx_style),
                    Span::styled(
                        line.to_string(),
                        Style::default().fg(sol::VIOLET).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            if think_lines.len() > max {
                let remaining = think_lines.len() - max;
                lines.push(Line::from(vec![
                    Span::styled(cont_prefix.clone(), idx_style),
                    Span::styled(
                        format!("(+{remaining} more lines)"),
                        Style::default().fg(sol::BASE01),
                    ),
                ]));
            }
        }
        ParsedEntry::System { subtype, text, .. } => {
            let display = if text.is_empty() {
                format!("[system: {subtype}]")
            } else {
                format!("[system: {subtype}] {}", truncate_display(text, 80))
            };
            lines.push(Line::from(vec![
                Span::styled(idx_prefix.clone(), idx_style),
                Span::styled(display, Style::default().fg(sol::BASE01)),
            ]));
        }
        ParsedEntry::Progress { message, .. } => {
            lines.push(Line::from(vec![
                Span::styled(idx_prefix.clone(), idx_style),
                Span::styled(
                    format!("\u{25cb} {message}"),
                    Style::default().fg(sol::BASE01),
                ),
            ]));
        }
        ParsedEntry::Result {
            cost,
            duration_ms,
            turns,
            ..
        } => {
            let dash = "\u{2500}";
            let divider = dash.repeat(content_width.saturating_sub(2));
            lines.push(Line::from(Span::styled(
                divider.clone(),
                Style::default().fg(sol::GREEN),
            )));
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!(
                        "\u{2714} Session complete: {} | {} turns | {}",
                        jsonl::format_cost(*cost),
                        turns,
                        jsonl::format_duration_ms(*duration_ms)
                    ),
                    Style::default()
                        .fg(sol::GREEN)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                divider,
                Style::default().fg(sol::GREEN),
            )));
        }
    }
}

/// Token divider between conversation turns.
fn render_token_divider<'a>(
    lines: &mut Vec<Line<'a>>,
    content_width: usize,
    cum_in: u64,
    cum_out: u64,
    cum_cost: f64,
) {
    let stats = format!(
        " \u{2193}{} \u{2191}{} {}",
        jsonl::format_tokens(cum_in),
        jsonl::format_tokens(cum_out),
        if cum_cost > 0.0 {
            format!("/ {}", jsonl::format_cost(cum_cost))
        } else {
            String::new()
        }
    );
    let dash = "\u{2500}";
    let stats_len = stats.chars().count();
    let left = 2;
    let right = content_width.saturating_sub(left + stats_len + 1);
    let divider = format!("{}{}{}", dash.repeat(left), stats, dash.repeat(right));
    lines.push(Line::from(Span::styled(
        divider,
        Style::default().fg(sol::BASE00),
    )));
}

/// Strip XML-like tags from user messages (system-injected content).
fn strip_xml_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            // Check if this looks like an XML tag (starts with letter or /)
            if let Some(&next) = chars.peek() {
                if next.is_ascii_alphabetic() || next == '/' {
                    in_tag = true;
                    continue;
                }
            }
            result.push(ch);
        } else if ch == '>' && in_tag {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}

fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Render the session info bar.
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
        Span::styled(info, Style::default().fg(sol::CYAN)),
    ]));
    f.render_widget(bar, area);
}

/// Render the prompt input area.
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
                Style::default().fg(sol::BASE01),
                Style::default().fg(sol::BASE01),
                Some(hint),
            )
        }
        InputMode::Insert => (
            " Prompt (Enter to send, Esc to cancel) ",
            Style::default().fg(sol::BASE0),
            Style::default().fg(sol::YELLOW),
            None,
        ),
    };

    let content = if app.input_buffer.is_empty() {
        if let Some(msg) = hint {
            Paragraph::new(msg)
                .style(Style::default().fg(sol::BASE01).add_modifier(Modifier::ITALIC))
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

    if app.input_mode == InputMode::Insert {
        let x = area.x + 1 + app.input_buffer.width() as u16;
        let y = area.y + 1;
        f.set_cursor_position((x, y));
    }
}

/// Render the status bar.
fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let mode_indicator = match app.input_mode {
        InputMode::Normal => Span::styled(
            " NORMAL ",
            Style::default().fg(sol::BASE03).bg(sol::GREEN),
        ),
        InputMode::Insert => Span::styled(
            " INSERT ",
            Style::default().fg(sol::BASE03).bg(sol::YELLOW),
        ),
    };

    let view_mode_indicator = match app.view_mode {
        ViewMode::Raw => Span::styled(
            " [RAW] ",
            Style::default().fg(sol::BASE0).bg(sol::BASE02),
        ),
        ViewMode::Jsonl => Span::styled(
            " [JSONL] ",
            Style::default().fg(sol::BASE03).bg(sol::CYAN),
        ),
    };

    let session_count = Span::styled(
        format!(" {} sessions ", app.sessions.len()),
        Style::default().fg(sol::BASE0),
    );

    let status_msg = app
        .status_message
        .as_deref()
        .unwrap_or("j/k: navigate | i: input | Tab: mode | q: quit");

    let branch_raw = app
        .selected_session()
        .and_then(|s| s.metrics.git_branch.as_deref())
        .unwrap_or("-");
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
            Style::default().fg(sol::BASE01),
        ),
        Span::raw(" ".repeat(padding)),
        Span::styled(
            format!(" \u{2387} {branch}"),
            Style::default().fg(sol::GREEN),
        ),
        Span::styled(
            format!("{pr_suffix} "),
            Style::default().fg(sol::YELLOW),
        ),
        Span::styled(
            "\u{2502}",
            Style::default().fg(sol::BASE01),
        ),
        Span::styled(
            format!(" {cwd} "),
            Style::default().fg(sol::BLUE),
        ),
    ]));
    f.render_widget(bar, area);
}
