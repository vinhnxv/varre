use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::session::SessionId;
use crate::tmux::detection::ClaudeStatus;
use crate::tmux::TmuxWrapper;

/// Handle for a per-session polling task.
struct SessionPollHandle {
    task: JoinHandle<()>,
    cancel: CancellationToken,
}

/// Manages per-session polling tasks for interactive sessions.
pub struct SessionManager {
    handles: HashMap<SessionId, SessionPollHandle>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    parent_cancel: CancellationToken,
}

impl SessionManager {
    /// Create a new session manager.
    pub fn new(
        event_tx: mpsc::UnboundedSender<AppEvent>,
        parent_cancel: CancellationToken,
    ) -> Self {
        Self {
            handles: HashMap::new(),
            event_tx,
            parent_cancel,
        }
    }

    /// Start polling for a session.
    pub fn start_polling(
        &mut self,
        id: SessionId,
        session_name: String,
        tmux: Arc<TmuxWrapper>,
        poll_interval: Duration,
    ) {
        // Use child_token so parent cancellation cascades (Tide-4)
        let cancel = self.parent_cancel.child_token();
        let tx = self.event_tx.clone();
        let cancel_clone = cancel.clone();
        let id_for_task = id.clone();

        let task = tokio::spawn(async move {
            let id = id_for_task;
            // Stagger start using hash of session ID (Tide-3)
            let stagger = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                id.hash(&mut hasher);
                let hash = hasher.finish();
                Duration::from_millis((hash % poll_interval.as_millis() as u64).min(200))
            };
            tokio::time::sleep(stagger).await;

            let mut interval = tokio::time::interval(poll_interval);
            let mut consecutive_failures: u32 = 0;

            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    _ = interval.tick() => {
                        match tmux.capture_pane(&session_name, 50).await {
                            Ok(output) => {
                                consecutive_failures = 0;
                                let status = crate::tmux::detection::detect_status(
                                    &output,
                                    tmux.prompt_marker(),
                                );
                                let lines: Vec<String> =
                                    output.lines().map(|l| l.to_string()).collect();

                                let _ = tx.send(AppEvent::SessionUpdate {
                                    id: id.clone(),
                                    status,
                                    output: lines,
                                });
                            }
                            Err(e) => {
                                consecutive_failures += 1;
                                tracing::warn!(
                                    session = %session_name,
                                    failures = consecutive_failures,
                                    error = %e,
                                    "polling failed"
                                );

                                if consecutive_failures >= 3 {
                                    // Mark as unknown and reduce poll frequency
                                    let _ = tx.send(AppEvent::SessionUpdate {
                                        id: id.clone(),
                                        status: ClaudeStatus::Unknown,
                                        output: vec![format!("[polling error: {e}]")],
                                    });
                                    // Slow down to 5s
                                    interval = tokio::time::interval(Duration::from_secs(5));
                                }
                            }
                        }
                    }
                }
            }

            tracing::debug!(session = %session_name, "polling task stopped");
        });

        self.handles
            .insert(id, SessionPollHandle { task, cancel });
    }

    /// Stop polling for a specific session.
    pub fn stop_polling(&mut self, id: &SessionId) {
        if let Some(handle) = self.handles.remove(id) {
            handle.cancel.cancel();
        }
    }

    /// Graceful shutdown: cancel all polling tasks and await completion (Tide-4).
    pub async fn shutdown(&mut self) {
        // Phase 1: Cancel all tokens
        for handle in self.handles.values() {
            handle.cancel.cancel();
        }

        // Phase 2: Await all tasks with timeout
        for (id, handle) in self.handles.drain() {
            match tokio::time::timeout(Duration::from_secs(5), handle.task).await {
                Ok(Ok(())) => tracing::debug!(session_id = %id, "polling stopped"),
                Ok(Err(e)) => tracing::warn!(session_id = %id, "polling panicked: {:?}", e),
                Err(_) => tracing::warn!(session_id = %id, "polling stop timed out"),
            }
        }
    }

    /// Return the number of active polling tasks.
    pub fn active_count(&self) -> usize {
        self.handles.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_manager_lifecycle() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let mut mgr = SessionManager::new(tx, cancel.clone());

        assert_eq!(mgr.active_count(), 0);

        // We can't easily test polling without tmux, but we can test the manager itself
        let id = SessionId::new();
        // stop_polling on non-existent session is a no-op
        mgr.stop_polling(&id);
        assert_eq!(mgr.active_count(), 0);
    }

    #[tokio::test]
    async fn test_session_manager_shutdown() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let mut mgr = SessionManager::new(tx, cancel);
        mgr.shutdown().await;
        assert_eq!(mgr.active_count(), 0);
    }
}
