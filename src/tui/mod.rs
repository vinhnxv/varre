pub mod app;
pub mod event;
pub mod session_manager;
pub mod ui;

use std::io::{self, stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self as ct_event, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::orchestrator::Orchestrator;
use crate::session::SessionId;
use crate::tmux::detection::ClaudeStatus;
use crate::tmux::TmuxWrapper;

use app::{App, InputMode, SessionViewModel};
use event::AppEvent;
use session_manager::SessionManager;

/// Run the TUI application.
pub async fn run<B: crate::backend::ClaudeBackend + 'static>(
    config: Config,
    orchestrator: &mut Orchestrator<B>,
    cancel_token: CancellationToken,
) -> Result<()> {
    // GAP-10: Install panic hook for terminal restore BEFORE entering raw mode
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    // GAP-9: Detect if running inside tmux
    let inside_tmux = std::env::var("TMUX").is_ok();

    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = stdout();
    if inside_tmux {
        // Skip mouse capture inside tmux (GAP-9)
        execute!(stdout, EnterAlternateScreen)?;
    } else {
        execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        )?;
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new();
    let tmux = Arc::new(TmuxWrapper::new(&config.tmux));
    let mut session_mgr = SessionManager::new(event_tx.clone(), cancel_token.clone());

    // Get terminal size
    let size = terminal.size()?;
    app.terminal_size = (size.width, size.height);

    // Load existing sessions
    refresh_sessions(&mut app, orchestrator).await;

    // Orphan detection: find tmux sessions with varre prefix not in our store
    if let Ok(tmux_sessions) = tmux.list_sessions().await {
        let known_names: std::collections::HashSet<&str> =
            app.sessions.iter().map(|s| s.name.as_str()).collect();
        let orphans: Vec<_> = tmux_sessions
            .iter()
            .filter(|ts| !known_names.contains(ts.name.as_str()))
            .collect();
        if !orphans.is_empty() {
            let names: Vec<&str> = orphans.iter().map(|o| o.name.as_str()).collect();
            tracing::warn!(
                orphans = ?names,
                "found orphaned tmux sessions with varre prefix"
            );
            app.status_message = Some(format!(
                "Found {} orphaned tmux session(s): {}",
                orphans.len(),
                names.join(", ")
            ));
        }
    }

    // Start polling for existing interactive sessions
    start_polling_for_sessions(&mut app, &mut session_mgr, &tmux, &config);

    let _ui_tick_rate = Duration::from_millis(config.tui.refresh_rate_ms);
    let mut last_draw = Instant::now();

    // Main event loop
    loop {
        // Check for cancellation
        if cancel_token.is_cancelled() {
            break;
        }

        // Poll for crossterm events (non-blocking)
        if ct_event::poll(Duration::from_millis(10))? {
            match ct_event::read()? {
                Event::Key(key) => {
                    // Ctrl+C always quits
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        app.should_quit = true;
                    } else {
                        handle_key_event(
                            &mut app, key, orchestrator, &tmux, &mut session_mgr, &config,
                        )
                        .await;
                    }
                }
                Event::Resize(w, h) => {
                    app.terminal_size = (w, h);
                }
                _ => {}
            }
        }

        // Drain all pending session updates (GAP-12: batch before redraw)
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                AppEvent::SessionUpdate { id, status, output } => {
                    apply_session_update(&mut app, &id, status, output);
                }
                AppEvent::Resize(w, h) => {
                    app.terminal_size = (w, h);
                }
                _ => {}
            }
        }

        // Rate-limited rendering (Tide-6: max ~60fps)
        if last_draw.elapsed() >= Duration::from_millis(16) {
            terminal.draw(|f| ui::render(f, &app))?;
            last_draw = Instant::now();
        }

        if app.should_quit {
            break;
        }

        // Brief sleep to avoid busy-waiting
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    // Graceful shutdown (Tide-4)
    session_mgr.shutdown().await;

    // Terminal teardown
    disable_raw_mode()?;
    if inside_tmux {
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    } else {
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        )?;
    }
    terminal.show_cursor()?;

    Ok(())
}

/// Handle a key event based on the current input mode.
async fn handle_key_event<B: crate::backend::ClaudeBackend>(
    app: &mut App,
    key: KeyEvent,
    orchestrator: &mut Orchestrator<B>,
    tmux: &Arc<TmuxWrapper>,
    session_mgr: &mut SessionManager,
    config: &Config,
) {
    match app.input_mode {
        InputMode::Normal => match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
            KeyCode::Char('i') => {
                app.input_mode = InputMode::Insert;
                app.status_message = None;
            }
            KeyCode::Char('n') => {
                // Create new interactive session with unique name
                let mut n = app.sessions.len() + 1;
                let mut name = format!("agent-{n}");
                let existing: std::collections::HashSet<&str> =
                    app.sessions.iter().map(|s| s.name.as_str()).collect();
                while existing.contains(name.as_str()) {
                    n += 1;
                    name = format!("agent-{n}");
                }
                match orchestrator
                    .create_interactive_session(&name, None)
                    .await
                {
                    Ok(_id) => {
                        refresh_sessions(app, orchestrator).await;
                        start_polling_for_sessions(app, session_mgr, tmux, config);
                        app.status_message =
                            Some(format!("Created session '{name}'"));
                    }
                    Err(e) => {
                        app.status_message = Some(format!("Error: {e}"));
                    }
                }
            }
            KeyCode::Char('d') => {
                // Kill selected session (GAP-15: confirmation via status message)
                if let Some(session) = app.selected_session() {
                    let name = session.name.clone();
                    match orchestrator.kill_session(&name).await {
                        Ok(_) => {
                            let id = session.id.clone();
                            session_mgr.stop_polling(&id);
                            refresh_sessions(app, orchestrator).await;
                            app.status_message =
                                Some(format!("Killed session '{name}'"));
                        }
                        Err(e) => {
                            app.status_message =
                                Some(format!("Error killing '{name}': {e}"));
                        }
                    }
                }
            }
            KeyCode::Char('r') => {
                refresh_sessions(app, orchestrator).await;
                app.status_message = Some("Refreshed sessions".to_string());
            }
            KeyCode::PageUp => app.scroll_up(10),
            KeyCode::PageDown => app.scroll_down(10),
            KeyCode::Home => {
                app.scroll_offset = app.selected_output().len() as u16;
                app.auto_scroll = false;
            }
            KeyCode::End => {
                app.scroll_offset = 0;
                app.auto_scroll = true;
            }
            _ => {}
        },
        InputMode::Insert => match key.code {
            KeyCode::Esc => {
                app.input_mode = InputMode::Normal;
                app.input_buffer.clear();
            }
            KeyCode::Enter => {
                if !app.input_buffer.is_empty() {
                    if let Some(session) = app.selected_session() {
                        let name = session.name.clone();
                        let prompt = app.input_buffer.drain(..).collect::<String>();
                        match orchestrator.send_prompt(&name, &prompt).await {
                            Ok(_) => {
                                app.status_message =
                                    Some(format!("Sent prompt to '{name}'"));
                            }
                            Err(e) => {
                                app.status_message =
                                    Some(format!("Error: {e}"));
                            }
                        }
                    }
                    app.input_mode = InputMode::Normal;
                }
            }
            KeyCode::Backspace => {
                app.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                app.input_buffer.push(c);
            }
            _ => {}
        },
    }
}

/// Refresh the session list from the orchestrator.
async fn refresh_sessions<B: crate::backend::ClaudeBackend>(
    app: &mut App,
    orchestrator: &Orchestrator<B>,
) {
    let sessions = orchestrator.list_sessions().await;
    app.sessions = sessions
        .into_iter()
        .map(|info| SessionViewModel {
            name: info.name,
            id: info.id,
            state: info.state,
            claude_status: ClaudeStatus::Unknown,
            output_lines: Vec::new(),
            kind: "headless", // Will be updated by polling
        })
        .collect();

    // Clamp selected_index
    if !app.sessions.is_empty() && app.selected_index >= app.sessions.len() {
        app.selected_index = app.sessions.len() - 1;
    }
}

/// Start polling tasks for interactive sessions that don't already have one.
fn start_polling_for_sessions(
    app: &App,
    session_mgr: &mut SessionManager,
    tmux: &Arc<TmuxWrapper>,
    config: &Config,
) {
    let poll_interval = Duration::from_millis(config.tmux.poll_interval_ms);
    for session in &app.sessions {
        session_mgr.start_polling(
            session.id.clone(),
            session.name.clone(),
            tmux.clone(),
            poll_interval,
        );
    }
}

/// Apply a session update to the app state (from polling task).
fn apply_session_update(
    app: &mut App,
    id: &SessionId,
    status: ClaudeStatus,
    output: Vec<String>,
) {
    if let Some(session) = app.sessions.iter_mut().find(|s| s.id == *id) {
        session.claude_status = status;
        session.output_lines = output;
        session.kind = "interactive";
    }
}
