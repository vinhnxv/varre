use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::ClaudeConfig;
use crate::error::VarreError;
use crate::session::state::{SessionEvent, SessionState};
use crate::session::SessionId;
use crate::tmux::detection::ClaudeStatus;
use crate::tmux::TmuxWrapper;

/// Default maximum output lines kept in buffer (sliding window).
const DEFAULT_MAX_OUTPUT_LINES: usize = 5000;

/// An interactive Claude Code session running in tmux.
#[derive(Debug)]
pub struct InteractiveSession {
    /// Unique session identifier.
    pub id: SessionId,
    /// Current lifecycle state.
    state: Arc<RwLock<SessionState>>,
    /// Working directory for the session.
    pub working_dir: PathBuf,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// Claude configuration snapshot for this session.
    pub config: ClaudeConfig,
    /// Tmux wrapper for controlling the session.
    tmux: Arc<TmuxWrapper>,
    /// Last detected Claude Code status.
    claude_status: Arc<RwLock<ClaudeStatus>>,
    /// Recent captured output lines (sliding window via VecDeque — Finding 6).
    output_buffer: Arc<RwLock<VecDeque<String>>>,
    /// Maximum output lines to keep.
    max_output_lines: usize,
}

impl InteractiveSession {
    /// Create a new interactive session.
    ///
    /// The tmux session must be created separately via TmuxWrapper::create_session().
    pub fn new(working_dir: PathBuf, config: ClaudeConfig, tmux: Arc<TmuxWrapper>) -> Self {
        Self {
            id: SessionId::new(),
            state: Arc::new(RwLock::new(SessionState::Creating)),
            working_dir,
            created_at: Utc::now(),
            config,
            tmux,
            claude_status: Arc::new(RwLock::new(ClaudeStatus::Unknown)),
            output_buffer: Arc::new(RwLock::new(VecDeque::new())),
            max_output_lines: DEFAULT_MAX_OUTPUT_LINES,
        }
    }

    /// Start Claude Code in the tmux session.
    pub async fn start_claude(&self, session_name: &str) -> Result<()> {
        self.tmux.start_claude(session_name).await?;
        // Transition Creating -> Ready
        self.send_event(&SessionEvent::Spawned, 3).await?;
        Ok(())
    }

    /// Send a prompt to the tmux session via the Escape+delay+Enter workaround.
    pub async fn send(&self, session_name: &str, prompt: &str) -> Result<()> {
        self.tmux.send_keys(session_name, prompt).await?;
        Ok(())
    }

    /// Capture the current pane output.
    pub async fn capture(&self, session_name: &str, lines: i32) -> Result<String> {
        self.tmux.capture_pane(session_name, lines).await
    }

    /// Return the last detected Claude Code status.
    pub async fn status(&self) -> ClaudeStatus {
        self.claude_status.read().await.clone()
    }

    /// Perform a single poll cycle: capture pane, detect status, update buffers.
    pub async fn poll_once(&self, session_name: &str) -> Result<ClaudeStatus> {
        let output = self.tmux.capture_pane(session_name, 50).await?;

        // Update output buffer (VecDeque sliding window)
        {
            let mut buf = self.output_buffer.write().await;
            buf.clear();
            for line in output.lines() {
                if buf.len() >= self.max_output_lines {
                    buf.pop_front();
                }
                buf.push_back(line.to_string());
            }
        }

        // Detect status
        let status =
            crate::tmux::detection::detect_status(&output, self.tmux.prompt_marker());

        // Update status
        {
            let mut s = self.claude_status.write().await;
            *s = status.clone();
        }

        Ok(status)
    }

    /// Read the current lifecycle state.
    pub async fn state(&self) -> SessionState {
        self.state.read().await.clone()
    }

    /// Send an event to transition the session lifecycle state.
    pub async fn send_event(
        &self,
        event: &SessionEvent,
        max_retries: u32,
    ) -> Result<SessionState, VarreError> {
        let mut state = self.state.write().await;
        let new_state = state.transition(event, max_retries)?;
        *state = new_state.clone();
        Ok(new_state)
    }

    /// Return the buffered output lines for display.
    pub async fn output_lines(&self) -> Vec<String> {
        self.output_buffer.read().await.iter().cloned().collect()
    }

    /// Return shared references for the polling task.
    pub fn shared_status(&self) -> Arc<RwLock<ClaudeStatus>> {
        self.claude_status.clone()
    }

    /// Return shared output buffer reference.
    pub fn shared_output(&self) -> Arc<RwLock<VecDeque<String>>> {
        self.output_buffer.clone()
    }

    /// Return shared state reference.
    pub fn shared_state(&self) -> Arc<RwLock<SessionState>> {
        self.state.clone()
    }

    /// Return the session ID.
    pub fn id(&self) -> &SessionId {
        &self.id
    }
}

/// Serializable representation of an interactive session (for persistence).
#[derive(Debug, Serialize, Deserialize)]
pub struct InteractiveSessionData {
    pub id: SessionId,
    pub state: SessionState,
    pub working_dir: PathBuf,
    pub created_at: DateTime<Utc>,
    pub config: ClaudeConfig,
    /// The tmux session name (for reconnection on load).
    pub tmux_session_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TmuxConfig;

    fn make_session() -> InteractiveSession {
        let tmux = Arc::new(TmuxWrapper::new(&TmuxConfig::default()));
        InteractiveSession::new(
            PathBuf::from("/tmp/test"),
            ClaudeConfig::default(),
            tmux,
        )
    }

    #[tokio::test]
    async fn test_interactive_session_initial_state() {
        let session = make_session();
        assert_eq!(session.state().await, SessionState::Creating);
        assert_eq!(session.status().await, ClaudeStatus::Unknown);
    }

    #[tokio::test]
    async fn test_interactive_session_lifecycle() {
        let session = make_session();
        // Creating -> Ready
        session.send_event(&SessionEvent::Spawned, 3).await.unwrap();
        assert_eq!(session.state().await, SessionState::Ready);

        // Ready -> Busy
        session.send_event(&SessionEvent::PromptSent, 3).await.unwrap();
        assert_eq!(session.state().await, SessionState::Busy { retry_count: 0 });

        // Busy -> Ready
        session.send_event(&SessionEvent::Completed, 3).await.unwrap();
        assert_eq!(session.state().await, SessionState::Ready);
    }

    #[tokio::test]
    async fn test_output_lines_empty_initially() {
        let session = make_session();
        assert!(session.output_lines().await.is_empty());
    }
}
