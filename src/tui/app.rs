use crate::session::state::SessionState;
use crate::session::SessionId;
use crate::tmux::detection::ClaudeStatus;

/// Input mode for the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    /// Normal mode: arrow keys navigate sessions, shortcuts active.
    Normal,
    /// Insert mode: typing a prompt, Enter submits.
    Insert,
}

/// A view model representing a session for display.
#[derive(Debug, Clone)]
pub struct SessionViewModel {
    pub name: String,
    pub id: SessionId,
    pub state: SessionState,
    pub claude_status: ClaudeStatus,
    pub output_lines: Vec<String>,
    pub kind: &'static str,
}

impl SessionViewModel {
    /// Return the status icon for TUI display.
    pub fn status_icon(&self) -> &'static str {
        match self.kind {
            "interactive" => self.claude_status.icon(),
            _ => match &self.state {
                SessionState::Ready => "○",
                SessionState::Busy { .. } => "●",
                SessionState::WaitingInput => "!",
                SessionState::Error { .. } => "✗",
                SessionState::Dead => "✗",
                SessionState::Creating => "◌",
            },
        }
    }

    /// Return a display string for the session status.
    pub fn status_text(&self) -> String {
        match self.kind {
            "interactive" => self.claude_status.to_string(),
            _ => match &self.state {
                SessionState::Ready => "ready".to_string(),
                SessionState::Busy { .. } => "busy".to_string(),
                SessionState::WaitingInput => "waiting".to_string(),
                SessionState::Error { last_error, .. } => format!("error: {last_error}"),
                SessionState::Dead => "dead".to_string(),
                SessionState::Creating => "creating".to_string(),
            },
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
    /// Scroll offset for the output panel.
    pub scroll_offset: u16,
    /// Whether auto-scroll is enabled (GAP-13).
    pub auto_scroll: bool,
    /// Whether the app should exit.
    pub should_quit: bool,
    /// Status bar message.
    pub status_message: Option<String>,
    /// Terminal size.
    pub terminal_size: (u16, u16),
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

    /// Scroll output up.
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        self.auto_scroll = false;
    }

    /// Scroll output down.
    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Check if terminal size is below minimum (GAP-8).
    pub fn is_terminal_too_small(&self) -> bool {
        self.terminal_size.0 < 80 || self.terminal_size.1 < 24
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app_with_sessions(n: usize) -> App {
        let mut app = App::new();
        for i in 0..n {
            app.sessions.push(SessionViewModel {
                name: format!("session-{i}"),
                id: SessionId::new(),
                state: SessionState::Ready,
                claude_status: ClaudeStatus::Idle,
                output_lines: vec![format!("output from session {i}")],
                kind: "interactive",
            });
        }
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
        let vm = SessionViewModel {
            name: "test".into(),
            id: SessionId::new(),
            state: SessionState::Ready,
            claude_status: ClaudeStatus::Working,
            output_lines: vec![],
            kind: "interactive",
        };
        assert_eq!(vm.status_icon(), "●");

        let vm_headless = SessionViewModel {
            kind: "headless",
            ..vm.clone()
        };
        assert_eq!(vm_headless.status_icon(), "○"); // Ready state icon
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
}
