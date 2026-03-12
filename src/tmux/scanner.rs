use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::process::Command;

use super::detection::{detect_status, strip_ansi, ClaudeStatus};

/// Process metrics for a Claude Code session.
#[derive(Debug, Clone, Default)]
pub struct ProcessMetrics {
    /// The Claude process PID (child of shell in pane).
    pub pid: Option<u32>,
    /// CPU usage percentage.
    pub cpu_percent: Option<f32>,
    /// Memory usage in MB.
    pub mem_mb: Option<f32>,
    /// Process start time (e.g. "14:32" or "Mar10").
    pub started: Option<String>,
    /// Process elapsed time (e.g. "01:23:45").
    pub elapsed: Option<String>,
    /// Tmux server PID.
    pub tmux_pid: Option<u32>,
    /// Number of MCP server child processes.
    pub mcp_count: u32,
    /// Number of teammate child processes.
    pub mate_count: u32,
    /// Git branch of the working directory.
    pub git_branch: Option<String>,
    /// Claude Code version (e.g. "2.1.74").
    pub claude_version: Option<String>,
    /// Claude Code config directory (CLAUDE_CONFIG_DIR or ~/.claude).
    pub claude_config_dir: Option<String>,
    /// GitHub PR number for the current branch (if any).
    pub pr_number: Option<u32>,
    /// Working directory of the claude session (pane's cwd).
    pub cwd: Option<String>,
}

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
    /// Process metrics (PID, CPU, MEM, etc.).
    pub metrics: ProcessMetrics,
    /// Path to the most recent JSONL session log (if discovered).
    pub jsonl_path: Option<PathBuf>,
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

        // Collect process metrics from the shell PID in the pane.
        let mut metrics = if let Some(shell_pid) = pane.pane_pid {
            collect_process_metrics(shell_pid).await
        } else {
            ProcessMetrics::default()
        };

        // Collect tmux server PID.
        metrics.tmux_pid = get_tmux_server_pid().await;

        // Count MCP servers and teammates under the claude process.
        if let Some(claude_pid) = metrics.pid {
            let (mcp, mates) = count_children(claude_pid).await;
            metrics.mcp_count = mcp;
            metrics.mate_count = mates;
        }

        // Get pane working directory, then derive git branch from it.
        let pane_cwd = get_pane_cwd(&pane.pane_id).await;
        metrics.git_branch = if let Some(ref cwd) = pane_cwd {
            get_git_branch(cwd).await
        } else {
            None
        };
        // Shorten cwd for display (replace $HOME with ~).
        metrics.cwd = pane_cwd.map(|cwd| {
            let home = std::env::var("HOME").unwrap_or_default();
            if !home.is_empty() && cwd.starts_with(&home) {
                format!("~{}", &cwd[home.len()..])
            } else {
                cwd
            }
        });

        // Get PR number for the current branch.
        metrics.pr_number = if let (Some(ref cwd), Some(ref _branch)) = (&metrics.cwd, &metrics.git_branch) {
            // Use the original (non-shortened) path isn't available, use cwd which may have ~
            // Expand ~ back for the command
            let full_cwd = if cwd.starts_with('~') {
                let home = std::env::var("HOME").unwrap_or_default();
                format!("{}{}", home, &cwd[1..])
            } else {
                cwd.clone()
            };
            get_pr_number(&full_cwd).await
        } else {
            None
        };

        // Extract Claude Code version from binary or pane content.
        metrics.claude_version = get_claude_version(metrics.pid, &content).await;

        // Detect Claude config dir from process environment.
        metrics.claude_config_dir = if let Some(cpid) = metrics.pid {
            get_claude_config_dir(cpid).await
        } else {
            None
        };

        // Discover JSONL path using CWD + config dir.
        let jsonl_path = if let Some(ref cwd) = metrics.cwd {
            crate::jsonl::resolve_jsonl_path(cwd, metrics.claude_config_dir.as_deref())
        } else {
            None
        };

        Ok(Some(DiscoveredSession {
            tmux_session: pane.session_name.clone(),
            tmux_window: pane.window_index,
            pane_id: pane.pane_id.clone(),
            pane_pid: pane.pane_pid,
            claude_status: final_status,
            pane_content: lines,
            pane_size: (pane.width, pane.height),
            metrics,
            jsonl_path,
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

/// Get the command name of a process by PID.
async fn get_process_comm(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if comm.is_empty() { None } else { Some(comm) }
}

/// Find a claude child process under a shell PID.
async fn find_claude_child(shell_pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-eo", "pid,ppid,comm"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().find_map(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return None;
        }
        let pid: u32 = parts[0].parse().ok()?;
        let ppid: u32 = parts[1].parse().ok()?;
        let comm = parts[2..].join(" ").to_lowercase();
        if ppid == shell_pid && (comm.contains("claude") || comm.contains("node")) {
            Some(pid)
        } else {
            None
        }
    })
}

/// Collect process metrics for a Claude Code session.
/// First checks if the pane PID itself is a claude process, then looks for child processes.
async fn collect_process_metrics(pane_pid: u32) -> ProcessMetrics {
    // Step 1: Check if pane_pid itself is a claude process (e.g. tmux launched claude directly).
    let pane_comm = get_process_comm(pane_pid).await.unwrap_or_default().to_lowercase();
    let is_claude_direct = pane_comm.contains("claude") || pane_comm.contains(".claude");

    let target_pid = if is_claude_direct {
        // Pane PID IS the claude process.
        pane_pid
    } else {
        // Pane PID is a shell — find claude child process.
        match find_claude_child(pane_pid).await {
            Some(pid) => pid,
            None => return ProcessMetrics { pid: None, ..Default::default() },
        }
    };

    // Get metrics: ps -o pid,%cpu,rss,lstart,etime -p <pid>
    let output = match Command::new("ps")
        .args(["-o", "pid,%cpu,rss,lstart,etime", "-p", &target_pid.to_string()])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return ProcessMetrics { pid: Some(target_pid), ..Default::default() },
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Skip header line, parse data line
    if let Some(line) = stdout.lines().nth(1) {
        parse_ps_metrics(line, target_pid)
    } else {
        ProcessMetrics { pid: Some(target_pid), ..Default::default() }
    }
}

/// Parse a ps output line with format: PID %CPU RSS LSTART ETIME
/// LSTART is multi-word (e.g. "Wed Mar 12 14:32:00 2026"), ETIME is last field.
fn parse_ps_metrics(line: &str, pid: u32) -> ProcessMetrics {
    let parts: Vec<&str> = line.split_whitespace().collect();
    // Minimum: pid, cpu, rss, + lstart(5 words) + etime = 9 fields
    if parts.len() < 9 {
        return ProcessMetrics { pid: Some(pid), ..Default::default() };
    }

    let cpu = parts[1].parse::<f32>().ok();
    let rss_kb = parts[2].parse::<f32>().ok();
    let mem_mb = rss_kb.map(|kb| kb / 1024.0);

    // LSTART is 5 words: "Day Mon DD HH:MM:SS YYYY"
    // Extract just "HH:MM:SS" for compact display
    let started = if parts.len() >= 7 {
        Some(parts[6].to_string()) // HH:MM:SS
    } else {
        None
    };

    // ETIME is the last field (e.g. "01:23:45" or "23:45")
    let elapsed = parts.last().map(|s| s.to_string());

    ProcessMetrics {
        pid: Some(pid),
        cpu_percent: cpu,
        mem_mb,
        started,
        elapsed,
        tmux_pid: None,
        mcp_count: 0,
        mate_count: 0,
        git_branch: None,
        claude_version: None,
        claude_config_dir: None,
        pr_number: None,
        cwd: None,
    }
}

/// Get the tmux server PID.
async fn get_tmux_server_pid() -> Option<u32> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{pid}"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Count MCP server and teammate child processes under a claude PID.
/// MCP: child with "server.py", "/mcp/", or "mcp-server" in command.
/// Mates: child that is another "claude" or "node" process.
async fn count_children(parent_pid: u32) -> (u32, u32) {
    let output = match Command::new("ps")
        .args(["-eo", "pid,ppid,args"])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return (0, 0),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut mcp = 0u32;
    let mut mates = 0u32;

    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
        if parts.len() < 3 {
            continue;
        }
        let ppid: u32 = match parts[1].trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if ppid != parent_pid {
            continue;
        }
        let args = parts[2].to_lowercase();
        if args.contains("server.py") || args.contains("/mcp/") || args.contains("mcp-server") {
            mcp += 1;
        } else if args.contains("claude") || args.contains("node") {
            mates += 1;
        }
    }

    (mcp, mates)
}

/// Get the current working directory of a tmux pane.
async fn get_pane_cwd(pane_id: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{pane_current_path}"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let cwd = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cwd.is_empty() { None } else { Some(cwd) }
}

/// Get git branch for a given directory.
async fn get_git_branch(cwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() { None } else { Some(branch) }
}

/// Get the GitHub PR number for the current branch in a given directory.
async fn get_pr_number(cwd: &str) -> Option<u32> {
    let output = Command::new("gh")
        .args(["pr", "view", "--json", "number", "-q", ".number"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Extract Claude Code version from pane content (e.g. "Claude Code v2.1.74").
/// Get Claude Code version from the binary resolved via the process PID.
/// Falls back to parsing pane content if binary detection fails.
async fn get_claude_version(pid: Option<u32>, content: &str) -> Option<String> {
    // Try to get version from the binary itself (most reliable).
    if let Some(p) = pid {
        if let Some(ver) = get_version_from_process(p).await {
            return Some(ver);
        }
    }
    // Fallback: parse from pane content.
    extract_claude_version_from_content(content)
}

/// Get version by finding the binary path from PID and running --version.
async fn get_version_from_process(pid: u32) -> Option<String> {
    // On macOS, get the binary path from the process.
    let output = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let binary = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if binary.is_empty() {
        return None;
    }

    // Run `<binary> --version` to get the version string.
    let output = Command::new(&binary)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ver_output = String::from_utf8_lossy(&output.stdout);
    // Parse version from output like "Claude Code v2.1.74" or just "2.1.74"
    for line in ver_output.lines() {
        let trimmed = line.trim();
        if let Some(pos) = trimmed.find("Claude Code v") {
            let after = &trimmed[pos + "Claude Code v".len()..];
            let version: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !version.is_empty() {
                return Some(version);
            }
        }
        // Try bare version number.
        let version: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if version.contains('.') && version.len() >= 3 {
            return Some(version);
        }
    }
    None
}

/// Extract Claude Code version from pane content (fallback).
fn extract_claude_version_from_content(content: &str) -> Option<String> {
    let stripped = strip_ansi(content);
    for line in stripped.lines() {
        let trimmed = line.trim();
        if let Some(pos) = trimmed.find("Claude Code v") {
            let after = &trimmed[pos + "Claude Code v".len()..];
            let version: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !version.is_empty() {
                return Some(version);
            }
        }
    }
    None
}

/// Detect which Claude config directory a process uses via `lsof`.
/// This is the most reliable method on macOS since `/proc` is not available.
async fn get_claude_config_dir(pid: u32) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return None;
    }

    // Use lsof to check which .claude* directory the process has open files in.
    let output = Command::new("lsof")
        .args(["-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pattern = format!("{home}/.claude");

    // Check for .claude-true first (more specific), then .claude.
    for line in stdout.lines() {
        if line.contains(&format!("{home}/.claude-true")) {
            return Some("~/.claude-true".to_string());
        }
    }
    for line in stdout.lines() {
        if line.contains(&pattern) {
            return Some("~/.claude".to_string());
        }
    }

    None
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
    fn test_discovered_session_jsonl_field() {
        let session = DiscoveredSession {
            tmux_session: "test".into(),
            tmux_window: 0,
            pane_id: "%1".into(),
            pane_pid: None,
            claude_status: ClaudeStatus::Idle,
            pane_content: vec![],
            pane_size: (200, 50),
            metrics: ProcessMetrics::default(),
            jsonl_path: None,
        };
        assert!(session.jsonl_path.is_none());
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
