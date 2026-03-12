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

use app::{App, InputMode, ViewMode};
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
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('n')
                    {
                        // Ctrl+N: create new tmux session with Claude Code.
                        match spawn_claude_session(&config).await {
                            Ok(name) => {
                                app.status_message =
                                    Some(format!("Created session '{name}' — will appear on next scan"));
                            }
                            Err(e) => {
                                app.status_message =
                                    Some(format!("Error creating session: {e}"));
                            }
                        }
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
                    // Before updating sessions, do incremental JSONL reads.
                    for session in &sessions {
                        if let Some(ref jsonl_path) = session.jsonl_path {
                            let pane_id = session.pane_id.clone();
                            let jstate = app
                                .jsonl_states
                                .entry(pane_id)
                                .or_default();

                            // Initialize tailer if needed or path changed.
                            let needs_init = jstate.tailer.is_none()
                                || jstate.path.as_ref() != Some(jsonl_path);

                            if needs_init {
                                jstate.path = Some(jsonl_path.clone());
                                if let Some(mut tailer) = crate::jsonl::JsonlTailer::new(jsonl_path.clone()) {
                                    let (entries, errors) = tailer.read_all();
                                    jstate.stats = crate::jsonl::JsonlStats::default();
                                    jstate.stats.update_from_entries(&entries);
                                    jstate.stats.parse_errors = errors;
                                    jstate.entries = entries;
                                    if jstate.entries.is_empty() {
                                        jstate.state = crate::jsonl::JsonlViewState::Empty;
                                    } else {
                                        jstate.state = crate::jsonl::JsonlViewState::Ready;
                                    }
                                    jstate.tailer = Some(tailer);
                                }
                            } else if let Some(ref mut tailer) = jstate.tailer {
                                let (new_entries, errors) = tailer.read_new();
                                if !new_entries.is_empty() || errors > 0 {
                                    jstate.append_entries(new_entries, errors);
                                }
                            }
                        }
                    }
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
    _tmux: &TmuxWrapper,
    _config: &Config,
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
            KeyCode::Tab => {
                app.view_mode = app.view_mode.toggle();
            }
            KeyCode::Char('1') => {
                app.view_mode = ViewMode::Raw;
            }
            KeyCode::Char('2') => {
                app.view_mode = ViewMode::Jsonl;
            }
            KeyCode::Char('t') if app.view_mode == ViewMode::Jsonl => {
                app.show_thinking = !app.show_thinking;
            }
            KeyCode::Char('s') if app.view_mode == ViewMode::Jsonl => {
                app.show_system = !app.show_system;
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
                        let pane_id = session.pane_id.clone();
                        let display = session.display_name.clone();
                        let prompt = app.input_buffer.drain(..).collect::<String>();
                        match send_prompt_to_pane(&pane_id, &prompt).await {
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

/// Send a prompt to a tmux pane using the Escape+delay+Enter workaround.
/// Uses pane_id directly (e.g. "%5") to avoid prefix issues.
async fn send_prompt_to_pane(pane_id: &str, prompt: &str) -> Result<()> {
    use tokio::process::Command;

    // Step 1: Send text literally.
    let output = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", prompt])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("send-keys failed: {}", stderr.trim());
    }

    // Step 2: Wait for autocomplete to render.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Step 3: Dismiss autocomplete.
    Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "Escape"])
        .output()
        .await?;

    // Step 4: Brief wait.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Step 5: Submit.
    let output = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "Enter"])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("send Enter failed: {}", stderr.trim());
    }

    Ok(())
}

/// Spawn a new detached tmux session running Claude Code.
/// Returns the session name. The monitor task will auto-discover it.
async fn spawn_claude_session(config: &Config) -> Result<String> {
    // Find next available session number by checking existing tmux sessions.
    let existing = tokio::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .await
        .ok();
    let max_n = existing
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
        .lines()
        .filter_map(|name| name.strip_prefix("varre-agent-"))
        .filter_map(|s| s.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let n = max_n + 1;
    let session_name = format!("varre-agent-{n}");
    let binary = &config.claude.binary;
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Create detached tmux session.
    let output = tokio::process::Command::new("tmux")
        .args([
            "new-session", "-d", "-s", &session_name,
            "-x", "200", "-y", "50",
            "-c", &cwd.to_string_lossy(),
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-session failed: {}", stderr.trim());
    }

    // Start Claude Code in the session.
    let output = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", &session_name, "-l", binary])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("failed to start claude: {}", stderr.trim());
    }

    // Send Enter separately (not via -l which would send literal "Enter" text)
    tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", &session_name, "Enter"])
        .output()
        .await?;

    Ok(session_name)
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
