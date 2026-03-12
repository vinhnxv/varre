use anyhow::{Context, Result};
use tokio::process::Command;

use super::detection::{detect_status, strip_ansi, ClaudeStatus};

/// A Claude Code session discovered by scanning all tmux panes.
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    /// The tmux session name (e.g. "my-project").
    pub tmux_session: String,
    /// The tmux window index within the session.
    pub tmux_window: u32,
    /// The tmux pane identifier (e.g. "%5").
    pub pane_id: String,
    /// The shell PID running in the pane (if available).
    pub pane_pid: Option<u32>,
    /// Detected Claude Code status.
    pub claude_status: ClaudeStatus,
    /// Last N lines captured from the pane.
    pub pane_content: Vec<String>,
    /// Pane dimensions (columns, rows).
    pub pane_size: (u16, u16),
}

/// Scans all tmux panes for running Claude Code sessions.
#[derive(Debug, Clone)]
pub struct TmuxScanner {
    /// Prompt marker used for status detection.
    prompt_marker: String,
    /// Number of lines to capture from each pane.
    capture_lines: i32,
}

impl TmuxScanner {
    /// Create a new scanner with the given prompt marker.
    pub fn new(prompt_marker: String) -> Self {
        Self {
            prompt_marker,
            capture_lines: 30,
        }
    }

    /// Create a scanner with a custom capture line count.
    pub fn with_capture_lines(mut self, lines: i32) -> Self {
        self.capture_lines = lines;
        self
    }

    /// Scan all tmux panes and return only those running Claude Code.
    pub async fn scan(&self) -> Result<Vec<DiscoveredSession>> {
        let panes = self.list_all_panes().await?;
        let mut sessions = Vec::new();

        for pane in panes {
            match self.inspect_pane(&pane).await {
                Ok(Some(discovered)) => sessions.push(discovered),
                Ok(None) => {} // not a Claude Code pane
                Err(e) => {
                    tracing::debug!(
                        pane_id = %pane.pane_id,
                        error = %e,
                        "failed to inspect pane, skipping"
                    );
                }
            }
        }

        Ok(sessions)
    }

    /// List all panes across all tmux sessions.
    async fn list_all_panes(&self) -> Result<Vec<RawPane>> {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name}:#{window_index}:#{pane_id}:#{pane_pid}:#{pane_width}:#{pane_height}",
            ])
            .output()
            .await;

        let output = match output {
            Ok(o) => o,
            Err(_) => return Ok(Vec::new()), // tmux not running
        };

        if !output.status.success() {
            return Ok(Vec::new()); // no sessions or server not running
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let panes = stdout
            .lines()
            .filter_map(|line| RawPane::parse(line))
            .collect();

        Ok(panes)
    }

    /// Capture pane content and check if Claude Code is running.
    async fn inspect_pane(&self, pane: &RawPane) -> Result<Option<DiscoveredSession>> {
        let content = self.capture_pane_content(&pane.pane_id).await?;
        let status = detect_status(&content, &self.prompt_marker);

        // Check if this pane has Claude Code running.
        // If detect_status returns anything other than Unknown, it is Claude.
        // Also check for Claude-specific patterns in the content.
        let is_claude = status != ClaudeStatus::Unknown || has_claude_patterns(&content);

        if !is_claude {
            return Ok(None);
        }

        let final_status = if status == ClaudeStatus::Unknown && has_claude_patterns(&content) {
            // We detected Claude patterns but detect_status returned Unknown.
            // Use Starting as a fallback since we know Claude is present.
            ClaudeStatus::Starting
        } else {
            status
        };

        let lines: Vec<String> = content
            .lines()
            .map(|l| strip_ansi(l))
            .collect();

        Ok(Some(DiscoveredSession {
            tmux_session: pane.session_name.clone(),
            tmux_window: pane.window_index,
            pane_id: pane.pane_id.clone(),
            pane_pid: pane.pane_pid,
            claude_status: final_status,
            pane_content: lines,
            pane_size: (pane.width, pane.height),
        }))
    }

    /// Capture the last N lines from a tmux pane by its pane ID.
    async fn capture_pane_content(&self, pane_id: &str) -> Result<String> {
        let start_line = format!("-{}", self.capture_lines);
        let output = Command::new("tmux")
            .args(["capture-pane", "-t", pane_id, "-p", "-S", &start_line])
            .output()
            .await
            .context("failed to capture tmux pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("capture-pane failed for {}: {}", pane_id, stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Check if pane content contains Claude Code-specific patterns beyond what
/// detect_status checks (status bar with model name, token count, cost).
fn has_claude_patterns(content: &str) -> bool {
    let stripped = strip_ansi(content);
    let lower = stripped.to_lowercase();

    // Claude status bar patterns
    let has_model = lower.contains("opus")
        || lower.contains("sonnet")
        || lower.contains("haiku")
        || lower.contains("claude");

    // Cost pattern like "$0.12" or "$12.34"
    let has_cost = stripped.contains('$')
        && stripped
            .split('$')
            .skip(1)
            .any(|after| {
                after
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .count()
                    >= 2
            });

    // Token count patterns (e.g. "12.3k tokens" or "1,234 tokens")
    let has_tokens = lower.contains("tokens") || lower.contains("token");

    // "ctrl+c to interrupt" already handled by detect_status, but as a safety net
    let has_interrupt = lower.contains("ctrl+c to interrupt");

    // Need at least two signals to confirm Claude Code presence
    let signals = [has_model, has_cost, has_tokens, has_interrupt];
    let signal_count = signals.iter().filter(|&&s| s).count();

    signal_count >= 1 && has_model
}

/// Raw pane information parsed from tmux list-panes output.
#[derive(Debug)]
struct RawPane {
    /// The tmux session name.
    session_name: String,
    /// The window index.
    window_index: u32,
    /// The pane identifier (e.g. "%5").
    pane_id: String,
    /// The shell PID in the pane.
    pane_pid: Option<u32>,
    /// Pane width in columns.
    width: u16,
    /// Pane height in rows.
    height: u16,
}

impl RawPane {
    /// Parse a line from `tmux list-panes -a -F "..."`.
    ///
    /// Expected format: `session_name:window_index:pane_id:pane_pid:width:height`
    fn parse(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.splitn(6, ':').collect();
        if parts.len() < 6 {
            return None;
        }

        Some(Self {
            session_name: parts[0].to_string(),
            window_index: parts[1].parse().ok()?,
            pane_id: parts[2].to_string(),
            pane_pid: parts[3].parse().ok(),
            width: parts[4].parse().ok()?,
            height: parts[5].parse().ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_pane_parse_valid() {
        let line = "my-session:0:%1:12345:200:50";
        let pane = RawPane::parse(line).expect("should parse");
        assert_eq!(pane.session_name, "my-session");
        assert_eq!(pane.window_index, 0);
        assert_eq!(pane.pane_id, "%1");
        assert_eq!(pane.pane_pid, Some(12345));
        assert_eq!(pane.width, 200);
        assert_eq!(pane.height, 50);
    }

    #[test]
    fn test_raw_pane_parse_invalid() {
        assert!(RawPane::parse("incomplete:data").is_none());
        assert!(RawPane::parse("").is_none());
    }

    #[test]
    fn test_raw_pane_parse_missing_pid() {
        let line = "sess:0:%2::80:24";
        let pane = RawPane::parse(line).expect("should parse");
        assert_eq!(pane.pane_pid, None);
    }

    #[test]
    fn test_has_claude_patterns_with_model() {
        let content = "Working on your request...\nUsing claude-sonnet-4-20250514\n$0.05 spent";
        assert!(has_claude_patterns(content));
    }

    #[test]
    fn test_has_claude_patterns_with_opus() {
        let content = "opus model active\n1.2k tokens used";
        assert!(has_claude_patterns(content));
    }

    #[test]
    fn test_has_claude_patterns_no_match() {
        let content = "plain shell output\nls -la\ntotal 42";
        assert!(!has_claude_patterns(content));
    }

    #[test]
    fn test_has_claude_patterns_cost_only_not_enough() {
        let content = "The price is $5.00\nNothing else";
        assert!(!has_claude_patterns(content));
    }

    #[test]
    fn test_scanner_new() {
        let scanner = TmuxScanner::new("$".to_string());
        assert_eq!(scanner.prompt_marker, "$");
        assert_eq!(scanner.capture_lines, 30);
    }

    #[test]
    fn test_scanner_with_capture_lines() {
        let scanner = TmuxScanner::new("$".to_string()).with_capture_lines(50);
        assert_eq!(scanner.capture_lines, 50);
    }
}
