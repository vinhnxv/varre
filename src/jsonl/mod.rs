use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// ---------------------------------------------------------------------------
// JSONL path resolution
// ---------------------------------------------------------------------------

/// Resolve the most recently modified `.jsonl` file for a given CWD and config dir.
///
/// Claude Code stores session logs at `{config_dir}/projects/{encoded_cwd}/*.jsonl`.
/// The CWD is encoded by replacing `/` with `-`.
pub fn resolve_jsonl_path(cwd: &str, config_dir: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded_cwd = if cwd.starts_with('~') {
        format!("{}{}", home, &cwd[1..])
    } else {
        cwd.to_string()
    };
    let encoded = expanded_cwd.replace('/', "-");

    let dirs_to_check: Vec<PathBuf> = if let Some(cd) = config_dir {
        let expanded_cd = if cd.starts_with('~') {
            format!("{}{}", home, &cd[1..])
        } else {
            cd.to_string()
        };
        vec![PathBuf::from(expanded_cd)]
    } else {
        let home_path = PathBuf::from(&home);
        vec![home_path.join(".claude-true"), home_path.join(".claude")]
    };

    for base in &dirs_to_check {
        let project_dir = base.join("projects").join(&encoded);
        if let Some(path) = find_newest_jsonl(&project_dir) {
            return Some(path);
        }
    }

    // Fallback: scan all dirs under projects/ and match by encoded name.
    // Note: the encoding (replace '/' with '-') is lossy for paths with hyphens,
    // so we compare encoded forms rather than trying to reverse-decode.
    for base in &dirs_to_check {
        let projects_dir = base.join("projects");
        if let Ok(entries) = fs::read_dir(&projects_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    // Compare encoded form directly — avoids lossy reverse-decoding.
                    if dir_name == encoded {
                        if let Some(path) = find_newest_jsonl(&entry.path()) {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Find the most recently modified `.jsonl` file in a directory.
fn find_newest_jsonl(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|x| x == "jsonl")
                .unwrap_or(false)
        })
        .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
        .map(|e| e.path())
}

// ---------------------------------------------------------------------------
// JSONL entry types (serde)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawJsonlEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[allow(dead_code)]
    uuid: Option<String>,
    #[allow(dead_code)]
    session_id: Option<String>,
    timestamp: Option<String>,
    message: Option<MessageData>,
    data: Option<serde_json::Value>,
    // result fields
    total_cost_usd: Option<f64>,
    duration_ms: Option<u64>,
    num_turns: Option<u32>,
    is_sidechain: Option<bool>,
    version: Option<String>,
    #[allow(dead_code)]
    parent_uuid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageData {
    role: Option<String>,
    content: serde_json::Value,
    model: Option<String>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// Parsed entry types (display-ready)
// ---------------------------------------------------------------------------

/// Parsed and display-ready JSONL entry.
#[derive(Debug, Clone)]
pub enum ParsedEntry {
    User {
        text: String,
        timestamp: Option<String>,
    },
    Assistant {
        blocks: Vec<ContentBlock>,
        model: Option<String>,
        usage: Option<TokenUsage>,
        timestamp: Option<String>,
    },
    ToolUse {
        name: String,
        summary: String,
        timestamp: Option<String>,
    },
    ToolResult {
        content: String,
        timestamp: Option<String>,
    },
    Thinking {
        text: String,
        timestamp: Option<String>,
    },
    System {
        subtype: String,
        text: String,
        timestamp: Option<String>,
    },
    Progress {
        message: String,
        timestamp: Option<String>,
    },
    Result {
        cost: f64,
        duration_ms: u64,
        turns: u32,
        text: String,
    },
}

#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse { name: String, input_summary: String },
    ToolResult { content: String },
    Thinking(String),
}

// ---------------------------------------------------------------------------
// JSONL parser
// ---------------------------------------------------------------------------

/// Parse an entire JSONL file into display-ready entries.
/// Returns (entries, parse_error_count).
pub fn parse_jsonl_file(path: &Path) -> (Vec<ParsedEntry>, usize) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read JSONL file");
            return (Vec::new(), 0);
        }
    };
    parse_jsonl_lines(&content)
}

/// Parse JSONL content string into entries. Returns (entries, error_count).
pub fn parse_jsonl_lines(content: &str) -> (Vec<ParsedEntry>, usize) {
    let mut entries = Vec::new();
    let mut errors = 0;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match parse_jsonl_line(line) {
            Some(parsed) => entries.extend(parsed),
            None => {
                errors += 1;
                tracing::debug!(line_prefix = &line[..line.len().min(80)], "malformed JSONL line");
            }
        }
    }

    (entries, errors)
}

/// Parse a single JSONL line into zero or more ParsedEntries.
fn parse_jsonl_line(line: &str) -> Option<Vec<ParsedEntry>> {
    let raw: RawJsonlEntry = serde_json::from_str(line).ok()?;

    // Skip sidechain entries by default
    if raw.is_sidechain == Some(true) {
        return Some(Vec::new());
    }

    let ts = raw.timestamp.clone();

    match raw.entry_type.as_str() {
        "user" => {
            let text = extract_message_text(&raw.message?);
            Some(vec![ParsedEntry::User { text, timestamp: ts }])
        }
        "assistant" => {
            let msg = raw.message?;
            let blocks = parse_content_blocks(&msg.content);
            let mut result = Vec::new();

            // Expand blocks into individual entries for better display
            for block in &blocks {
                match block {
                    ContentBlock::ToolUse { name, input_summary } => {
                        result.push(ParsedEntry::ToolUse {
                            name: name.clone(),
                            summary: input_summary.clone(),
                            timestamp: ts.clone(),
                        });
                    }
                    ContentBlock::ToolResult { content } => {
                        result.push(ParsedEntry::ToolResult {
                            content: truncate(content, 120),
                            timestamp: ts.clone(),
                        });
                    }
                    ContentBlock::Thinking(text) => {
                        result.push(ParsedEntry::Thinking {
                            text: text.clone(),
                            timestamp: ts.clone(),
                        });
                    }
                    ContentBlock::Text(_) => {}
                }
            }

            // The main assistant entry with text blocks
            result.insert(0, ParsedEntry::Assistant {
                blocks,
                model: msg.model,
                usage: msg.usage,
                timestamp: ts,
            });

            Some(result)
        }
        "system" => {
            let subtype = raw
                .data
                .as_ref()
                .and_then(|d| d.get("subtype"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let text = raw
                .data
                .as_ref()
                .and_then(|d| d.get("message"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            Some(vec![ParsedEntry::System {
                subtype,
                text,
                timestamp: ts,
            }])
        }
        "progress" => {
            let message = raw
                .data
                .as_ref()
                .and_then(|d| d.get("statusMessage"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if message.is_empty() {
                Some(Vec::new())
            } else {
                Some(vec![ParsedEntry::Progress {
                    message,
                    timestamp: ts,
                }])
            }
        }
        "result" => {
            let text = raw
                .data
                .as_ref()
                .and_then(|d| d.get("result"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            Some(vec![ParsedEntry::Result {
                cost: raw.total_cost_usd.unwrap_or(0.0),
                duration_ms: raw.duration_ms.unwrap_or(0),
                turns: raw.num_turns.unwrap_or(0),
                text,
            }])
        }
        "file-history-snapshot" => Some(Vec::new()), // skip
        _ => {
            tracing::debug!(entry_type = %raw.entry_type, "unknown JSONL entry type, skipping");
            Some(Vec::new())
        }
    }
}

/// Extract plain text from a message's content field.
fn extract_message_text(msg: &MessageData) -> String {
    match &msg.content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => {
            blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type")?.as_str()? == "text" {
                        b.get("text")?.as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => String::new(),
    }
}

/// Parse the content field of an assistant message into ContentBlocks.
fn parse_content_blocks(content: &serde_json::Value) -> Vec<ContentBlock> {
    match content {
        serde_json::Value::String(s) => vec![ContentBlock::Text(s.clone())],
        serde_json::Value::Array(blocks) => {
            blocks
                .iter()
                .filter_map(|b| {
                    let block_type = b.get("type")?.as_str()?;
                    match block_type {
                        "text" => {
                            let text = b.get("text")?.as_str()?.to_string();
                            Some(ContentBlock::Text(text))
                        }
                        "tool_use" => {
                            let name = b
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let input_summary = summarize_tool_input(
                                &name,
                                b.get("input").unwrap_or(&serde_json::Value::Null),
                            );
                            Some(ContentBlock::ToolUse { name, input_summary })
                        }
                        "tool_result" => {
                            let content = b
                                .get("content")
                                .and_then(|c| {
                                    if let Some(s) = c.as_str() {
                                        Some(s.to_string())
                                    } else if let Some(arr) = c.as_array() {
                                        Some(
                                            arr.iter()
                                                .filter_map(|item| {
                                                    item.get("text")
                                                        .and_then(|t| t.as_str())
                                                        .map(|s| s.to_string())
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n"),
                                        )
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_default();
                            Some(ContentBlock::ToolResult { content })
                        }
                        "thinking" => {
                            let text = b
                                .get("thinking")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            Some(ContentBlock::Thinking(text))
                        }
                        _ => None,
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Summarize tool input for one-line display.
fn summarize_tool_input(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Read" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string(),
        "Write" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string(),
        "Edit" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string(),
        "Bash" => input
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| truncate(c, 80))
            .unwrap_or_else(|| "?".to_string()),
        "Glob" => input
            .get("pattern")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string(),
        "Grep" => input
            .get("pattern")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => {
            // Generic: show first string value
            if let Some(obj) = input.as_object() {
                for (_, v) in obj.iter().take(1) {
                    if let Some(s) = v.as_str() {
                        return truncate(s, 60);
                    }
                }
            }
            String::new()
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}...", &s[..end])
    }
}

// ---------------------------------------------------------------------------
// JSONL file tailer (incremental reader)
// ---------------------------------------------------------------------------

/// Incremental JSONL file reader that tracks position and detects rotation.
pub struct JsonlTailer {
    path: PathBuf,
    offset: u64,
    inode: u64,
    partial_buf: String,
    pub parse_errors: usize,
}

impl JsonlTailer {
    /// Create a new tailer for the given file path.
    pub fn new(path: PathBuf) -> Option<Self> {
        let meta = fs::metadata(&path).ok()?;
        Some(Self {
            path,
            offset: 0,
            inode: meta.ino(),
            partial_buf: String::new(),
            parse_errors: 0,
        })
    }

    /// Read all entries from the beginning (initial load).
    pub fn read_all(&mut self) -> (Vec<ParsedEntry>, usize) {
        let (entries, errors) = parse_jsonl_file(&self.path);
        if let Ok(meta) = fs::metadata(&self.path) {
            self.offset = meta.len();
            self.inode = meta.ino();
        }
        self.parse_errors = errors;
        (entries, errors)
    }

    /// Read new entries since last position. Returns new entries and error count delta.
    pub fn read_new(&mut self) -> (Vec<ParsedEntry>, usize) {
        let meta = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(_) => return (Vec::new(), 0),
        };

        // Detect file rotation (inode changed) or truncation (size shrunk).
        if meta.ino() != self.inode || meta.len() < self.offset {
            self.offset = 0;
            self.inode = meta.ino();
            self.partial_buf.clear();
            return self.read_all();
        }

        // No new data.
        if meta.len() == self.offset {
            return (Vec::new(), 0);
        }

        // Read new bytes.
        let mut file = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return (Vec::new(), 0),
        };

        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return (Vec::new(), 0);
        }

        let mut new_data = String::new();
        if file.read_to_string(&mut new_data).is_err() {
            return (Vec::new(), 0);
        }

        self.offset = meta.len();

        // Handle partial lines.
        let combined = format!("{}{}", self.partial_buf, new_data);
        self.partial_buf.clear();

        let mut entries = Vec::new();
        let mut errors = 0;

        let all_lines: Vec<&str> = combined.lines().collect();
        let total_lines = all_lines.len();

        for (i, line) in all_lines.into_iter().enumerate() {
            let is_last = i == total_lines - 1;
            let line = line.trim();

            // If this is the last line and the data doesn't end with newline,
            // it might be a partial line — buffer it.
            if is_last && !combined.ends_with('\n') && !line.is_empty() {
                self.partial_buf = line.to_string();
                continue;
            }

            if line.is_empty() {
                continue;
            }

            match parse_jsonl_line(line) {
                Some(parsed) => entries.extend(parsed),
                None => errors += 1,
            }
        }

        self.parse_errors += errors;
        (entries, errors)
    }
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Aggregated statistics from JSONL entries.
#[derive(Debug, Clone, Default)]
pub struct JsonlStats {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_creation: u64,
    pub total_cache_read: u64,
    pub total_cost_usd: f64,
    pub num_turns: u32,
    pub model: Option<String>,
    pub parse_errors: usize,
    pub version: Option<String>,
}

impl JsonlStats {
    /// Update stats from a batch of new entries.
    pub fn update_from_entries(&mut self, entries: &[ParsedEntry]) {
        for entry in entries {
            match entry {
                ParsedEntry::Assistant { usage, model, .. } => {
                    if let Some(u) = usage {
                        self.total_input_tokens += u.input_tokens.unwrap_or(0);
                        self.total_output_tokens += u.output_tokens.unwrap_or(0);
                        self.total_cache_creation += u.cache_creation_input_tokens.unwrap_or(0);
                        self.total_cache_read += u.cache_read_input_tokens.unwrap_or(0);
                    }
                    if let Some(m) = model {
                        self.model = Some(m.clone());
                    }
                    self.num_turns += 1;
                }
                ParsedEntry::Result { cost, .. } => {
                    self.total_cost_usd = *cost; // Result has the final total cost
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

pub fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => format!("{n}"),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        format!("${:.4}", usd)
    } else {
        format!("${:.2}", usd)
    }
}

pub fn format_duration_ms(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1_000..=59_999 => format!("{}s", ms / 1_000),
        60_000..=3_599_999 => format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1_000),
        _ => format!("{}h {}m", ms / 3_600_000, (ms % 3_600_000) / 60_000),
    }
}

// ---------------------------------------------------------------------------
// JSONL view state (lives in App, independent of SessionViewModel)
// ---------------------------------------------------------------------------

/// JSONL state stored independently of SessionViewModel, keyed by pane_id.
pub struct JsonlSessionState {
    pub entries: Vec<ParsedEntry>,
    pub path: Option<PathBuf>,
    pub tailer: Option<JsonlTailer>,
    pub stats: JsonlStats,
    pub state: JsonlViewState,
    pub scroll_offset: u16,
    pub auto_scroll: bool,
    pub expanded: HashSet<usize>,
}

impl Default for JsonlSessionState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            path: None,
            tailer: None,
            stats: JsonlStats::default(),
            state: JsonlViewState::NotFound,
            scroll_offset: 0,
            auto_scroll: true,
            expanded: HashSet::new(),
        }
    }
}

impl JsonlSessionState {
    /// Append new entries and update stats.
    pub fn append_entries(&mut self, new_entries: Vec<ParsedEntry>, error_delta: usize) {
        self.stats.update_from_entries(&new_entries);
        self.stats.parse_errors += error_delta;
        self.entries.extend(new_entries);

        // Cap to 2000 entries to bound memory.
        if self.entries.len() > 2000 {
            let drain = self.entries.len() - 2000;
            self.entries.drain(..drain);
        }

        if !self.entries.is_empty() {
            self.state = JsonlViewState::Ready;
        }
    }
}

/// Graceful degradation states for the JSONL view.
#[derive(Debug, Clone)]
pub enum JsonlViewState {
    NotFound,
    Empty,
    Loading { count: usize },
    Ready,
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(34_600), "34.6K");
        assert_eq!(format_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.001), "$0.0010");
        assert_eq!(format_cost(0.0099), "$0.0099");
        assert_eq!(format_cost(0.01), "$0.01");
        assert_eq!(format_cost(1.23), "$1.23");
    }

    #[test]
    fn test_format_duration_ms() {
        assert_eq!(format_duration_ms(500), "500ms");
        assert_eq!(format_duration_ms(5_000), "5s");
        assert_eq!(format_duration_ms(90_000), "1m 30s");
        assert_eq!(format_duration_ms(3_700_000), "1h 1m");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn test_parse_user_entry() {
        let line = r#"{"type":"user","uuid":"abc","timestamp":"2026-03-13T10:00:00Z","message":{"role":"user","content":"hello world"}}"#;
        let entries = parse_jsonl_line(line).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ParsedEntry::User { text, .. } => assert_eq!(text, "hello world"),
            _ => panic!("expected User entry"),
        }
    }

    #[test]
    fn test_parse_assistant_with_tool_use() {
        let line = r#"{"type":"assistant","timestamp":"2026-03-13T10:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Let me read that."},{"type":"tool_use","name":"Read","input":{"file_path":"/foo/bar.rs"}}],"model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let entries = parse_jsonl_line(line).unwrap();
        assert!(entries.len() >= 2); // Assistant + ToolUse
        match &entries[0] {
            ParsedEntry::Assistant { model, usage, .. } => {
                assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
                assert_eq!(usage.as_ref().unwrap().input_tokens, Some(100));
            }
            _ => panic!("expected Assistant entry"),
        }
    }

    #[test]
    fn test_parse_result_entry() {
        let line = r#"{"type":"result","totalCostUsd":1.23,"durationMs":90000,"numTurns":5,"data":{"result":"Done!"}}"#;
        let entries = parse_jsonl_line(line).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ParsedEntry::Result { cost, duration_ms, turns, text } => {
                assert!((cost - 1.23).abs() < 0.001);
                assert_eq!(*duration_ms, 90000);
                assert_eq!(*turns, 5);
                assert_eq!(text, "Done!");
            }
            _ => panic!("expected Result entry"),
        }
    }

    #[test]
    fn test_parse_malformed_line() {
        assert!(parse_jsonl_line("not json at all").is_none());
        assert!(parse_jsonl_line("{invalid}").is_none());
    }

    #[test]
    fn test_parse_sidechain_skipped() {
        let line = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"sidechain msg"}}"#;
        let entries = parse_jsonl_line(line).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_unknown_type_skipped() {
        let line = r#"{"type":"file-history-snapshot"}"#;
        let entries = parse_jsonl_line(line).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_jsonl_lines_mixed() {
        let content = r#"{"type":"user","message":{"role":"user","content":"hi"}}
not valid json
{"type":"result","totalCostUsd":0.5,"durationMs":1000,"numTurns":1,"data":{"result":"ok"}}"#;
        let (entries, errors) = parse_jsonl_lines(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(errors, 1);
    }

    #[test]
    fn test_jsonl_stats_update() {
        let mut stats = JsonlStats::default();
        let entries = vec![
            ParsedEntry::Assistant {
                blocks: vec![],
                model: Some("claude-opus-4-6".to_string()),
                usage: Some(TokenUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                }),
                timestamp: None,
            },
            ParsedEntry::Result {
                cost: 0.05,
                duration_ms: 5000,
                turns: 1,
                text: "done".to_string(),
            },
        ];
        stats.update_from_entries(&entries);
        assert_eq!(stats.total_input_tokens, 100);
        assert_eq!(stats.total_output_tokens, 50);
        assert_eq!(stats.num_turns, 1);
        assert!((stats.total_cost_usd - 0.05).abs() < 0.001);
        assert_eq!(stats.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn test_jsonl_session_state_cap() {
        let mut state = JsonlSessionState::default();
        let entries: Vec<ParsedEntry> = (0..2100)
            .map(|i| ParsedEntry::User {
                text: format!("msg {i}"),
                timestamp: None,
            })
            .collect();
        state.append_entries(entries, 0);
        assert_eq!(state.entries.len(), 2000);
    }

    #[test]
    fn test_summarize_tool_input() {
        let input: serde_json::Value =
            serde_json::json!({"file_path": "/src/main.rs"});
        assert_eq!(summarize_tool_input("Read", &input), "/src/main.rs");

        let input: serde_json::Value =
            serde_json::json!({"command": "cargo build"});
        assert_eq!(summarize_tool_input("Bash", &input), "cargo build");

        let input: serde_json::Value =
            serde_json::json!({"pattern": "*.rs"});
        assert_eq!(summarize_tool_input("Glob", &input), "*.rs");
    }
}
