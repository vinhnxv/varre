use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tmux::scanner::TmuxScanner;
use crate::tui::event::AppEvent;

/// Background task that periodically scans all tmux panes for Claude Code sessions.
pub struct MonitorTask {
    /// Scanner used to discover Claude Code sessions.
    scanner: TmuxScanner,
    /// Channel sender for pushing discovered sessions to the TUI.
    event_tx: mpsc::UnboundedSender<AppEvent>,
    /// How often to scan (default 2 seconds).
    interval: Duration,
    /// Token for cooperative cancellation.
    cancel_token: CancellationToken,
}

impl MonitorTask {
    /// Create a new monitor task.
    pub fn new(
        scanner: TmuxScanner,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        interval: Duration,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            scanner,
            event_tx,
            interval,
            cancel_token,
        }
    }

    /// Spawn the monitor loop as a background tokio task.
    ///
    /// The first scan runs immediately, then repeats every `interval`.
    /// On scan errors, a warning is logged and the loop continues.
    /// Shuts down gracefully when the cancellation token is triggered.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!(
                interval_ms = self.interval.as_millis() as u64,
                "monitor task started"
            );

            // First scan is immediate (no initial sleep).
            self.run_scan_loop().await;

            tracing::info!("monitor task stopped");
        })
    }

    /// Run the scan-sleep loop until cancelled.
    async fn run_scan_loop(&self) {
        loop {
            // Perform a scan.
            match self.scanner.scan().await {
                Ok(sessions) => {
                    tracing::debug!(count = sessions.len(), "scan found sessions");
                    if self
                        .event_tx
                        .send(AppEvent::SessionsRefreshed(sessions))
                        .is_err()
                    {
                        // Receiver dropped, TUI is shutting down.
                        tracing::debug!("event receiver dropped, stopping monitor");
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "scan failed, will retry next interval");
                }
            }

            // Wait for the next interval or cancellation.
            tokio::select! {
                _ = self.cancel_token.cancelled() => break,
                _ = tokio::time::sleep(self.interval) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_monitor_task_shutdown() {
        let scanner = TmuxScanner::new("$".to_string());
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let task = MonitorTask::new(
            scanner,
            tx,
            Duration::from_millis(100),
            cancel.clone(),
        );

        let handle = task.spawn();

        // Let it run briefly then cancel.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        // Should complete without hanging.
        let result = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(result.is_ok(), "monitor task should shut down promptly");
    }

    #[tokio::test]
    async fn test_monitor_task_receiver_dropped() {
        let scanner = TmuxScanner::new("$".to_string());
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let task = MonitorTask::new(
            scanner,
            tx,
            Duration::from_millis(50),
            cancel.clone(),
        );

        let handle = task.spawn();

        // Drop the receiver — monitor should detect and stop.
        drop(rx);

        let result = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(result.is_ok(), "monitor task should stop when receiver is dropped");
    }
}
