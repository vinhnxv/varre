pub mod app;
pub mod event;
pub mod ui;

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self as ct_event, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::monitor::MonitorTask;
use crate::tmux::scanner::TmuxScanner;
use crate::tmux::TmuxWrapper;

use app::{App, InputMode};
use event::AppEvent;

/// Run the TUI application with auto-discovery of Claude Code sessions.
pub async fn run(config: Config, cancel_token: CancellationToken) -> Result<()> {
    // Install panic hook for terminal restore BEFORE entering raw mode.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    // Detect if running inside tmux.
    let inside_tmux = std::env::var("TMUX").is_ok();

    // Terminal setup.
    enable_raw_mode()?;
    let mut stdout = stdout();
    if inside_tmux {
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

    // Get terminal size.
    let size = terminal.size()?;
    app.terminal_size = (size.width, size.height);

    // Spawn the monitor task for auto-discovery.
    let scanner = TmuxScanner::new(config.tmux.prompt_marker.clone());
    let interval = Duration::from_millis(config.tmux.poll_interval_ms.max(500));
    let monitor = MonitorTask::new(
        scanner,
        event_tx.clone(),
        interval,
        cancel_token.child_token(),
    );
    let _monitor_handle = monitor.spawn();

    // Create a TmuxWrapper for send-keys capability.
    let tmux = TmuxWrapper::new(&config.tmux);

    let mut last_draw = Instant::now();

    // Main event loop.
    loop {
        // Check for cancellation.
        if cancel_token.is_cancelled() {
            break;
        }

        // Poll for crossterm events (non-blocking).
        if ct_event::poll(Duration::from_millis(10))? {
            match ct_event::read()? {
                Event::Key(key) => {
                    // Ctrl+C always quits.
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        app.should_quit = true;
                    } else {
                        handle_key_event(&mut app, key, &tmux, &config).await;
                    }
                }
                Event::Resize(w, h) => {
                    app.terminal_size = (w, h);
                }
                _ => {}
            }
        }

        // Drain all pending events.
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                AppEvent::SessionsRefreshed(sessions) => {
                    app.update_sessions(sessions);
                }
                AppEvent::Resize(w, h) => {
                    app.terminal_size = (w, h);
                }
                _ => {}
            }
        }

        // Rate-limited rendering (~60fps).
        if last_draw.elapsed() >= Duration::from_millis(16) {
            terminal.draw(|f| ui::render(f, &app))?;
            last_draw = Instant::now();
        }

        if app.should_quit {
            break;
        }

        // Brief sleep to avoid busy-waiting.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    // Graceful shutdown: cancel the monitor task via the parent token.
    cancel_token.cancel();

    // Terminal teardown.
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
async fn handle_key_event(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    tmux: &TmuxWrapper,
    config: &Config,
) {
    match app.input_mode {
        InputMode::Normal => match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
            KeyCode::Char('i') => {
                if !app.sessions.is_empty() {
                    app.input_mode = InputMode::Insert;
                    app.status_message = None;
                }
            }
            KeyCode::Char('d') => {
                // Kill the selected tmux pane.
                if let Some(session) = app.selected_session() {
                    let pane_id = session.pane_id.clone();
                    let display = session.display_name.clone();
                    match kill_tmux_pane(&pane_id).await {
                        Ok(()) => {
                            app.status_message =
                                Some(format!("Killed pane '{display}'"));
                        }
                        Err(e) => {
                            app.status_message =
                                Some(format!("Error killing pane: {e}"));
                        }
                    }
                }
            }
            KeyCode::Char('r') => {
                app.status_message = Some("Refresh scheduled (next scan cycle)".to_string());
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
                        let session_name = session.tmux_session.clone();
                        let display = session.display_name.clone();
                        let prompt = app.input_buffer.drain(..).collect::<String>();
                        match send_prompt_via_tmux(tmux, &session_name, &prompt, config).await {
                            Ok(()) => {
                                app.status_message =
                                    Some(format!("Sent prompt to '{display}'"));
                            }
                            Err(e) => {
                                app.status_message = Some(format!("Error: {e}"));
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

/// Send a prompt to a tmux session using the Escape+delay+Enter workaround.
async fn send_prompt_via_tmux(
    tmux: &TmuxWrapper,
    session_name: &str,
    prompt: &str,
    _config: &Config,
) -> Result<()> {
    tmux.send_keys(session_name, prompt).await
}

/// Kill a tmux pane by its pane ID.
async fn kill_tmux_pane(pane_id: &str) -> Result<()> {
    let output = tokio::process::Command::new("tmux")
        .args(["kill-pane", "-t", pane_id])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("kill-pane failed: {}", stderr.trim());
    }

    Ok(())
}
