use std::collections::HashMap;

use crate::jsonl::JsonlSessionState;
use crate::tmux::detection::ClaudeStatus;
use crate::tmux::scanner::{DiscoveredSession, ProcessMetrics};

/// Input mode for the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    /// Normal mode: arrow keys navigate sessions, shortcuts active.
    Normal,
    /// Insert mode: typing a prompt, Enter submits.
    Insert,
}

/// A view model representing a discovered session for display.
#[derive(Debug, Clone)]
pub struct SessionViewModel {
    /// Display name (tmux session name).
    pub display_name: String,
    /// Unique pane identifier (e.g. "%5").
    pub pane_id: String,
    /// The tmux session name (for send-keys).
    pub tmux_session: String,
    /// Detected Claude Code status.
    pub claude_status: ClaudeStatus,
    /// Live pane content lines.
    pub output_lines: Vec<String>,
    /// Pane dimensions (columns, rows).
    pub pane_size: (u16, u16),
    /// Process metrics (PID, CPU, MEM, started, uptime).
    pub metrics: ProcessMetrics,
    /// Path to JSONL session log (if discovered).
    pub jsonl_path: Option<std::path::PathBuf>,
}

impl SessionViewModel {
    /// Create a view model from a discovered session.
    pub fn from_discovered(session: &DiscoveredSession) -> Self {
        Self {
            display_name: format!(
                "{}:{}",
                session.tmux_session, session.tmux_window
            ),
            pane_id: session.pane_id.clone(),
            tmux_session: session.tmux_session.clone(),
            claude_status: session.claude_status.clone(),
            output_lines: session.pane_content.clone(),
            pane_size: session.pane_size,
            metrics: session.metrics.clone(),
            jsonl_path: session.jsonl_path.clone(),
        }
    }

    /// Format the metrics info line for display.
    pub fn metrics_line(&self) -> String {
        let pid = self.metrics.pid
            .map(|p| format!("PID:{p}"))
            .unwrap_or_else(|| "PID:-".into());
        let tmux = format!("TMUX:{}[{}]", self.tmux_session, self.pane_id);
        let cpu = self.metrics.cpu_percent
            .map(|c| format!("CPU:{c:.1}%"))
            .unwrap_or_else(|| "CPU:-".into());
        let mem = self.metrics.mem_mb
            .map(|m| format!("MEM:{m:.0}MB"))
            .unwrap_or_else(|| "MEM:-".into());
        let started = self.metrics.started
            .as_deref()
            .map(|s| format!("START:{s}"))
            .unwrap_or_else(|| "START:-".into());
        let uptime = self.metrics.elapsed
            .as_deref()
            .map(|e| format!("UP:{e}"))
            .unwrap_or_else(|| "UP:-".into());

        let tmux_pid = self.metrics.tmux_pid
            .map(|p| format!("TPID:{p}"))
            .unwrap_or_else(|| "TPID:-".into());
        let mcp = format!("MCP:{}", self.metrics.mcp_count);
        let mates = format!("MATES:{}", self.metrics.mate_count);
        let branch = self.metrics.git_branch
            .as_deref()
            .map(|b| format!("BR:{b}"))
            .unwrap_or_else(|| "BR:-".into());
        let version = self.metrics.claude_version
            .as_deref()
            .map(|v| format!("VER:{v}"))
            .unwrap_or_else(|| "VER:-".into());
        let ccdir = self.metrics.claude_config_dir
            .as_deref()
            .map(|d| format!("CCDIR:{d}"))
            .unwrap_or_else(|| "CCDIR:-".into());
        format!("{pid}  {version}  {tmux}  {cpu}  {mem}  {started}  {uptime}  {tmux_pid}  {mcp}  {mates}  {ccdir}")
    }

    /// Return the status icon for TUI display.
    pub fn status_icon(&self) -> &'static str {
        self.claude_status.icon()
    }

    /// Return a display string for the session status.
    pub fn status_text(&self) -> String {
        self.claude_status.to_string()
    }
}

/// Output view mode — Raw (tmux capture) or JSONL (structured log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Raw = 0,
    Jsonl = 1,
}

impl ViewMode {
    pub fn toggle(&self) -> Self {
        match self {
            ViewMode::Raw => ViewMode::Jsonl,
            ViewMode::Jsonl => ViewMode::Raw,
        }
    }
}

/// The main TUI application state.
pub struct App {
    /// All sessions displayed in the sidebar.
    pub sessions: Vec<SessionViewModel>,
    /// Index of the currently selected session.
    pub selected_index: usize,
    /// Text being typed by the user (in Insert mode).
    pub input_buffer: String,
    /// Current input mode.
    pub input_mode: InputMode,
    /// Scroll offset for the Raw output panel.
    pub scroll_offset: u16,
    /// Whether auto-scroll is enabled for Raw view.
    pub auto_scroll: bool,
    /// Whether the app should exit.
    pub should_quit: bool,
    /// Status bar message.
    pub status_message: Option<String>,
    /// Terminal size.
    pub terminal_size: (u16, u16),
    /// Current view mode (Raw or JSONL) — global, not per-session.
    pub view_mode: ViewMode,
    /// JSONL state per session, keyed by pane_id. Survives ViewModel rebuilds.
    pub jsonl_states: HashMap<String, JsonlSessionState>,
    /// Whether to show thinking blocks in JSONL view.
    pub show_thinking: bool,
    /// Whether to show system/progress entries in JSONL view.
    pub show_system: bool,
}

impl App {
    /// Create a new App instance.
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected_index: 0,
            input_buffer: String::new(),
            input_mode: InputMode::Normal,
            scroll_offset: 0,
            auto_scroll: true,
            should_quit: false,
            status_message: None,
            terminal_size: (80, 24),
            view_mode: ViewMode::Raw,
            jsonl_states: HashMap::new(),
            show_thinking: false,
            show_system: false,
        }
    }

    /// Update sessions from discovered sessions, preserving selection by pane_id.
    pub fn update_sessions(&mut self, discovered: Vec<DiscoveredSession>) {
        // Remember the currently selected pane_id to preserve selection.
        let selected_pane_id = self
            .selected_session()
            .map(|s| s.pane_id.clone());

        self.sessions = discovered
            .iter()
            .map(SessionViewModel::from_discovered)
            .collect();

        // Try to preserve the selected index by matching pane_id.
        if let Some(prev_id) = selected_pane_id {
            if let Some(new_idx) = self
                .sessions
                .iter()
                .position(|s| s.pane_id == prev_id)
            {
                self.selected_index = new_idx;
            } else {
                // Previously selected session is gone; clamp index.
                self.clamp_selection();
            }
        } else {
            self.clamp_selection();
        }

        // Clean up JSONL states for sessions that no longer exist.
        let active_panes: std::collections::HashSet<&str> = self
            .sessions
            .iter()
            .map(|s| s.pane_id.as_str())
            .collect();
        self.jsonl_states
            .retain(|pane_id, _| active_panes.contains(pane_id.as_str()));
    }

    /// Clamp selected_index to valid range.
    fn clamp_selection(&mut self) {
        if self.sessions.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.sessions.len() {
            self.selected_index = self.sessions.len() - 1;
        }
    }

    /// Select the previous session.
    pub fn select_prev(&mut self) {
        if !self.sessions.is_empty() && self.selected_index > 0 {
            self.selected_index -= 1;
            self.scroll_offset = 0;
            self.auto_scroll = true;
        }
    }

    /// Select the next session.
    pub fn select_next(&mut self) {
        if !self.sessions.is_empty() && self.selected_index < self.sessions.len() - 1 {
            self.selected_index += 1;
            self.scroll_offset = 0;
            self.auto_scroll = true;
        }
    }

    /// Get the currently selected session (if any).
    pub fn selected_session(&self) -> Option<&SessionViewModel> {
        self.sessions.get(self.selected_index)
    }

    /// Get the output lines for the selected session.
    pub fn selected_output(&self) -> &[String] {
        self.selected_session()
            .map(|s| s.output_lines.as_slice())
            .unwrap_or(&[])
    }

    /// Scroll output up (dispatches to active mode's offset).
    pub fn scroll_up(&mut self, amount: u16) {
        match self.view_mode {
            ViewMode::Raw => {
                self.scroll_offset = self.scroll_offset.saturating_add(amount);
                self.auto_scroll = false;
            }
            ViewMode::Jsonl => {
                if let Some(pane_id) = self.selected_session().map(|s| s.pane_id.clone()) {
                    if let Some(state) = self.jsonl_states.get_mut(&pane_id) {
                        state.scroll_offset = state.scroll_offset.saturating_add(amount);
                        state.auto_scroll = false;
                    }
                }
            }
        }
    }

    /// Scroll output down (dispatches to active mode's offset).
    pub fn scroll_down(&mut self, amount: u16) {
        match self.view_mode {
            ViewMode::Raw => {
                self.scroll_offset = self.scroll_offset.saturating_sub(amount);
                if self.scroll_offset == 0 {
                    self.auto_scroll = true;
                }
            }
            ViewMode::Jsonl => {
                if let Some(pane_id) = self.selected_session().map(|s| s.pane_id.clone()) {
                    if let Some(state) = self.jsonl_states.get_mut(&pane_id) {
                        state.scroll_offset = state.scroll_offset.saturating_sub(amount);
                        if state.scroll_offset == 0 {
                            state.auto_scroll = true;
                        }
                    }
                }
            }
        }
    }

    /// Check if terminal size is below minimum.
    pub fn is_terminal_too_small(&self) -> bool {
        self.terminal_size.0 < 80 || self.terminal_size.1 < 24
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_discovered(pane_id: &str, session: &str) -> DiscoveredSession {
        DiscoveredSession {
            tmux_session: session.to_string(),
            tmux_window: 0,
            pane_id: pane_id.to_string(),
            pane_pid: None,
            claude_status: ClaudeStatus::Idle,
            pane_content: vec!["output".to_string()],
            pane_size: (200, 50),
            metrics: ProcessMetrics::default(),
            jsonl_path: None,
        }
    }

    fn make_app_with_sessions(n: usize) -> App {
        let mut app = App::new();
        let discovered: Vec<DiscoveredSession> = (0..n)
            .map(|i| make_discovered(&format!("%{i}"), &format!("session-{i}")))
            .collect();
        app.update_sessions(discovered);
        app
    }

    #[test]
    fn test_navigation() {
        let mut app = make_app_with_sessions(3);
        assert_eq!(app.selected_index, 0);

        app.select_next();
        assert_eq!(app.selected_index, 1);

        app.select_next();
        assert_eq!(app.selected_index, 2);

        // Can't go past end
        app.select_next();
        assert_eq!(app.selected_index, 2);

        app.select_prev();
        assert_eq!(app.selected_index, 1);

        app.select_prev();
        assert_eq!(app.selected_index, 0);

        // Can't go past start
        app.select_prev();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_input_mode() {
        let app = App::new();
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn test_scroll() {
        let mut app = App::new();
        assert!(app.auto_scroll);

        app.scroll_up(5);
        assert_eq!(app.scroll_offset, 5);
        assert!(!app.auto_scroll);

        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 2);
        assert!(!app.auto_scroll);

        app.scroll_down(10);
        assert_eq!(app.scroll_offset, 0);
        assert!(app.auto_scroll);
    }

    #[test]
    fn test_session_viewmodel_icon() {
        let discovered = DiscoveredSession {
            tmux_session: "test".into(),
            tmux_window: 0,
            pane_id: "%1".into(),
            pane_pid: None,
            claude_status: ClaudeStatus::Working,
            pane_content: vec![],
            pane_size: (200, 50),
            metrics: ProcessMetrics::default(),
            jsonl_path: None,
        };
        let vm = SessionViewModel::from_discovered(&discovered);
        assert_eq!(vm.status_icon(), "\u{25cf}"); // filled circle
    }

    #[test]
    fn test_terminal_too_small() {
        let mut app = App::new();
        app.terminal_size = (79, 24);
        assert!(app.is_terminal_too_small());

        app.terminal_size = (80, 23);
        assert!(app.is_terminal_too_small());

        app.terminal_size = (80, 24);
        assert!(!app.is_terminal_too_small());
    }

    #[test]
    fn test_update_sessions_preserves_selection() {
        let mut app = App::new();
        let sessions = vec![
            make_discovered("%1", "sess-a"),
            make_discovered("%2", "sess-b"),
            make_discovered("%3", "sess-c"),
        ];
        app.update_sessions(sessions);
        app.selected_index = 1; // select %2

        // Update with same sessions in different order.
        let sessions = vec![
            make_discovered("%3", "sess-c"),
            make_discovered("%1", "sess-a"),
            make_discovered("%2", "sess-b"),
        ];
        app.update_sessions(sessions);
        // %2 is now at index 2.
        assert_eq!(app.selected_index, 2);
        assert_eq!(app.selected_session().unwrap().pane_id, "%2");
    }

    #[test]
    fn test_update_sessions_clamps_on_removal() {
        let mut app = make_app_with_sessions(3);
        app.selected_index = 2;

        // Reduce to 1 session.
        let sessions = vec![make_discovered("%0", "session-0")];
        app.update_sessions(sessions);
        assert_eq!(app.selected_index, 0);
    }
}
