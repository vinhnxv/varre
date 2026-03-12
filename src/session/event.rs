use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use crate::session::state::SessionEvent;
use crate::session::SessionId;

/// An update combining a session identifier, event, and timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdate {
    /// The session this update applies to.
    pub session_id: SessionId,
    /// The event that occurred.
    pub event: SessionEvent,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
}

impl SessionUpdate {
    /// Create a new session update with the current timestamp.
    pub fn new(session_id: SessionId, event: SessionEvent) -> Self {
        Self {
            session_id,
            event,
            timestamp: Utc::now(),
        }
    }
}

/// Create a bounded mpsc channel for session updates.
///
/// Returns a (sender, receiver) pair. The buffer size controls backpressure.
pub fn session_update_channel(
    buffer: usize,
) -> (mpsc::Sender<SessionUpdate>, mpsc::Receiver<SessionUpdate>) {
    mpsc::channel(buffer)
}
