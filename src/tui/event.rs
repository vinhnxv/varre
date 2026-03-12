use crossterm::event::KeyEvent;

use crate::jsonl::{JsonlStats, ParsedEntry};
use crate::tmux::scanner::DiscoveredSession;

/// Events processed by the TUI application.
#[derive(Debug)]
pub enum AppEvent {
    /// A keyboard input event.
    Key(KeyEvent),
    /// Periodic tick for UI refresh.
    Tick,
    /// All discovered Claude Code sessions from a monitor scan.
    SessionsRefreshed(Vec<DiscoveredSession>),
    /// Terminal resize.
    Resize(u16, u16),
    /// New JSONL entries parsed for a session.
    JsonlUpdated {
        pane_id: String,
        new_entries: Vec<ParsedEntry>,
        stats_delta: JsonlStats,
    },
}
