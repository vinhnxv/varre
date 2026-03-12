use crossterm::event::KeyEvent;

use crate::session::SessionId;
use crate::tmux::detection::ClaudeStatus;

/// Events processed by the TUI application.
#[derive(Debug)]
pub enum AppEvent {
    /// A keyboard input event.
    Key(KeyEvent),
    /// Periodic tick for UI refresh.
    Tick,
    /// Status update from a session's polling task.
    SessionUpdate {
        id: SessionId,
        status: ClaudeStatus,
        output: Vec<String>,
    },
    /// Terminal resize.
    Resize(u16, u16),
}
