use serde::{Deserialize, Serialize};

/// Represents the detected status of Claude Code running in a tmux pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeStatus {
    /// Claude is processing a prompt.
    Working,
    /// Claude is idle, waiting for input.
    Idle,
    /// Claude is asking for permission (y/n).
    WaitingApproval,
    /// Claude Code is starting up.
    Starting,
    /// Cannot determine status.
    Unknown,
}

impl std::fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Working => write!(f, "working"),
            Self::Idle => write!(f, "idle"),
            Self::WaitingApproval => write!(f, "waiting_approval"),
            Self::Starting => write!(f, "starting"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl ClaudeStatus {
    /// Return a single-character icon for TUI display.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Working => "●",
            Self::Idle => "○",
            Self::WaitingApproval => "!",
            Self::Starting => "◌",
            Self::Unknown => "?",
        }
    }
}

/// Strip ANSI escape codes from a string.
pub fn strip_ansi(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ESC sequence: consume until letter or end
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() || c == 'm' {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Detect Claude Code's status from captured tmux pane output.
///
/// Scans the last 30 lines from bottom up, applying detection rules
/// with priority: Working > WaitingApproval > Idle > Starting > Unknown.
pub fn detect_status(captured_output: &str, prompt_marker: &str) -> ClaudeStatus {
    let lines: Vec<&str> = captured_output.lines().collect();
    if lines.is_empty() {
        return ClaudeStatus::Unknown;
    }

    let stripped_lines: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
    let total = stripped_lines.len();
    let scan_start = total.saturating_sub(30);
    let scan_lines = &stripped_lines[scan_start..];

    let mut has_working = false;
    let mut has_approval = false;
    let mut has_idle = false;
    let mut has_border = false;
    let mut has_starting = false;

    for (i, line) in scan_lines.iter().enumerate().rev() {
        let line_from_bottom = scan_lines.len() - 1 - i;

        // Working: "ctrl+c to interrupt" only in last 3 lines (Finding 5)
        if line_from_bottom < 3 && line.contains("ctrl+c to interrupt") {
            has_working = true;
        }

        // Approval: "[y/n]" or "[Y/n]"
        if line.contains("[y/n]") || line.contains("[Y/n]") {
            has_approval = true;
        }

        // Idle: prompt marker at start of line (Finding 5)
        if line.trim_start().starts_with(prompt_marker) {
            has_idle = true;
        }

        // Border detection for idle confirmation
        if line.contains('─') {
            has_border = true;
        }

        // Starting
        if line.to_lowercase().contains("starting")
            || (line.to_lowercase().contains("claude") && line_from_bottom < 10)
        {
            has_starting = true;
        }
    }

    // Priority: Working > WaitingApproval > Idle > Starting > Unknown
    if has_working {
        ClaudeStatus::Working
    } else if has_approval {
        ClaudeStatus::WaitingApproval
    } else if has_idle && has_border {
        ClaudeStatus::Idle
    } else if has_starting {
        ClaudeStatus::Starting
    } else {
        ClaudeStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_working() {
        let output = "Processing your request...\n\
                      Analyzing files...\n\
                      ctrl+c to interrupt";
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::Working);
    }

    #[test]
    fn test_detect_idle() {
        let output = "Done. Created 3 files.\n\
                      ─────────────────────\n\
                      ❯ ";
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::Idle);
    }

    #[test]
    fn test_detect_waiting_approval() {
        let output = "I need to edit this file.\n\
                      Allow? [y/n]";
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::WaitingApproval);
    }

    #[test]
    fn test_detect_unknown() {
        let output = "some random text\nmore text";
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::Unknown);
    }

    #[test]
    fn test_detect_priority_working_over_idle() {
        // Working overrides idle when both present
        let output = "─────────────────────\n\
                      ❯ working on something\n\
                      ctrl+c to interrupt";
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::Working);
    }

    #[test]
    fn test_detect_starting() {
        let output = "Starting Claude Code...\nLoading...";
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::Starting);
    }

    #[test]
    fn test_strip_ansi() {
        let input = "\x1b[32mgreen\x1b[0m normal";
        assert_eq!(strip_ansi(input), "green normal");
    }

    #[test]
    fn test_empty_output() {
        assert_eq!(detect_status("", "❯"), ClaudeStatus::Unknown);
    }

    #[test]
    fn test_idle_without_border_is_not_idle() {
        let output = "Some output\n❯ ";
        // No border character, so not idle
        assert_eq!(detect_status(output, "❯"), ClaudeStatus::Unknown);
    }

    #[test]
    fn test_working_not_in_last_3_lines_ignored() {
        // "ctrl+c to interrupt" far from bottom should not trigger Working
        let mut lines = vec!["ctrl+c to interrupt"];
        for _ in 0..10 {
            lines.push("some other output");
        }
        let output = lines.join("\n");
        assert_eq!(detect_status(&output, "❯"), ClaudeStatus::Unknown);
    }

    #[test]
    fn test_custom_prompt_marker() {
        let output = "─────────────────────\n$ ";
        assert_eq!(detect_status(output, "$"), ClaudeStatus::Idle);
    }
}
