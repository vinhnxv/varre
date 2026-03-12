pub mod detection;

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::config::TmuxConfig;
use crate::error::VarreError;

pub use detection::{detect_status, ClaudeStatus};

/// Information about a tmux session managed by varre.
#[derive(Debug, Clone)]
pub struct TmuxSessionInfo {
    /// Session name (without prefix).
    pub name: String,
    /// Full tmux session name (with prefix).
    pub full_name: String,
    /// Last activity timestamp.
    pub activity: Option<String>,
}

/// Thin async wrapper around tmux CLI commands.
#[derive(Debug)]
pub struct TmuxWrapper {
    /// Session name prefix (e.g., "varre-").
    prefix: String,
    /// Delay between send-keys steps for Ink workaround.
    send_delay: Duration,
    /// Prompt marker for status detection.
    prompt_marker: String,
}

impl TmuxWrapper {
    /// Create a new TmuxWrapper from configuration.
    pub fn new(config: &TmuxConfig) -> Self {
        Self {
            prefix: config.session_prefix.clone(),
            send_delay: Duration::from_millis(config.send_delay_ms),
            prompt_marker: config.prompt_marker.clone(),
        }
    }

    /// Build the full tmux session name with prefix.
    pub fn session_name(&self, name: &str) -> String {
        format!("{}{}", self.prefix, name)
    }

    /// Check if tmux is available and return its version string.
    pub async fn check_available(&self) -> Result<String> {
        let output = Command::new("tmux")
            .arg("-V")
            .output()
            .await
            .map_err(|_| VarreError::TmuxNotFound)?;

        if !output.status.success() {
            return Err(VarreError::TmuxNotFound.into());
        }

        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(version)
    }

    /// Create a new detached tmux session.
    pub async fn create_session(&self, name: &str, size: (u16, u16)) -> Result<()> {
        let full_name = self.session_name(name);

        // Check for existing session first (GAP-2)
        if self.has_session(name).await? {
            return Err(VarreError::TmuxCommandFailed(format!(
                "session '{}' already exists",
                full_name
            ))
            .into());
        }

        let output = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &full_name,
                "-x",
                &size.0.to_string(),
                "-y",
                &size.1.to_string(),
            ])
            .output()
            .await
            .context("failed to execute tmux new-session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VarreError::TmuxCommandFailed(stderr.to_string()).into());
        }

        Ok(())
    }

    /// Kill a tmux session.
    pub async fn kill_session(&self, name: &str) -> Result<()> {
        let full_name = self.session_name(name);
        let output = Command::new("tmux")
            .args(["kill-session", "-t", &full_name])
            .output()
            .await
            .context("failed to execute tmux kill-session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("session not found") || stderr.contains("no such session") {
                return Err(VarreError::TmuxSessionNotFound(name.to_string()).into());
            }
            return Err(VarreError::TmuxCommandFailed(stderr.to_string()).into());
        }

        Ok(())
    }

    /// Check if a tmux session exists.
    pub async fn has_session(&self, name: &str) -> Result<bool> {
        let full_name = self.session_name(name);
        let output = Command::new("tmux")
            .args(["has-session", "-t", &full_name])
            .output()
            .await;

        match output {
            Ok(o) => Ok(o.status.success()),
            Err(_) => Ok(false),
        }
    }

    /// Send keys to a tmux session using the Escape+delay+Enter workaround.
    ///
    /// This bypasses Claude Code's Ink raw terminal mode:
    /// 1. Send the prompt text (no Enter)
    /// 2. Sleep for send_delay (300ms)
    /// 3. Send Escape to dismiss autocomplete
    /// 4. Sleep 100ms
    /// 5. Send Enter to submit
    pub async fn send_keys(&self, name: &str, text: &str) -> Result<()> {
        let full_name = self.session_name(name);

        // Verify session exists first
        if !self.has_session(name).await? {
            return Err(VarreError::TmuxSessionNotFound(name.to_string()).into());
        }

        // Step 1: Send the text (no Enter)
        let output = Command::new("tmux")
            .args(["send-keys", "-t", &full_name, text])
            .output()
            .await
            .context("failed to send text to tmux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VarreError::TmuxCommandFailed(stderr.to_string()).into());
        }

        // Step 2: Wait for autocomplete to render
        tokio::time::sleep(self.send_delay).await;

        // Step 3: Send Escape to dismiss autocomplete
        Command::new("tmux")
            .args(["send-keys", "-t", &full_name, "Escape"])
            .output()
            .await
            .context("failed to send Escape to tmux")?;

        // Step 4: Brief wait for Ink to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Step 5: Send Enter to submit
        let output = Command::new("tmux")
            .args(["send-keys", "-t", &full_name, "Enter"])
            .output()
            .await
            .context("failed to send Enter to tmux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VarreError::TmuxCommandFailed(stderr.to_string()).into());
        }

        Ok(())
    }

    /// Capture the pane output from a tmux session.
    pub async fn capture_pane(&self, name: &str, lines: i32) -> Result<String> {
        let full_name = self.session_name(name);
        let start_line = format!("-{}", lines);

        let output = Command::new("tmux")
            .args(["capture-pane", "-t", &full_name, "-p", "-S", &start_line])
            .output()
            .await
            .context("failed to capture tmux pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("session not found") || stderr.contains("no such session") {
                return Err(VarreError::TmuxSessionNotFound(name.to_string()).into());
            }
            return Err(VarreError::TmuxCommandFailed(stderr.to_string()).into());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Detect the status of Claude Code in a tmux session.
    pub async fn detect_session_status(&self, name: &str) -> Result<ClaudeStatus> {
        let output = self.capture_pane(name, 50).await?;
        Ok(detect_status(&output, &self.prompt_marker))
    }

    /// List all varre-managed tmux sessions (filtered by prefix).
    pub async fn list_sessions(&self) -> Result<Vec<TmuxSessionInfo>> {
        let output = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}:#{session_activity}",
            ])
            .output()
            .await;

        let output = match output {
            Ok(o) => o,
            Err(_) => return Ok(Vec::new()), // tmux not running
        };

        if !output.status.success() {
            // No sessions or server not running
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let sessions = stdout
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                if parts.len() < 2 {
                    return None;
                }
                let full_name = parts[0];
                if !full_name.starts_with(&self.prefix) {
                    return None;
                }
                let name = full_name.strip_prefix(&self.prefix)?.to_string();
                Some(TmuxSessionInfo {
                    name,
                    full_name: full_name.to_string(),
                    activity: Some(parts[1].to_string()),
                })
            })
            .collect();

        Ok(sessions)
    }

    /// Start Claude Code in a tmux session.
    pub async fn start_claude(&self, name: &str) -> Result<()> {
        let full_name = self.session_name(name);
        let output = Command::new("tmux")
            .args(["send-keys", "-t", &full_name, "claude", "Enter"])
            .output()
            .await
            .context("failed to start claude in tmux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VarreError::TmuxCommandFailed(stderr.to_string()).into());
        }

        Ok(())
    }

    /// Return the prompt marker used for detection.
    pub fn prompt_marker(&self) -> &str {
        &self.prompt_marker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_name_with_prefix() {
        let config = TmuxConfig::default();
        let wrapper = TmuxWrapper::new(&config);
        assert_eq!(wrapper.session_name("test"), "varre-test");
    }

    #[test]
    fn test_session_name_custom_prefix() {
        let config = TmuxConfig {
            session_prefix: "myapp-".into(),
            ..Default::default()
        };
        let wrapper = TmuxWrapper::new(&config);
        assert_eq!(wrapper.session_name("session1"), "myapp-session1");
    }
}
