# varre — Rust CLI Orchestrator for Claude Code

> Multi-session Claude Code orchestrator with dual-mode execution (headless + tmux), queue-based task runner, and interactive TUI.

## Metadata

- **Type**: feat
- **Created**: 2026-03-12
- **Status**: draft (forge-enriched)
- **Language**: Rust (2021 edition, MSRV 1.75+)
- **Target**: macOS/Linux CLI tool
- **Scope**: v0.1 = headless-only CLI + queue; v0.2 = interactive tmux + TUI

## Architecture Decision: Dual-Mode Execution

Research revealed a critical insight: Claude Code's interactive mode uses Ink (React-based terminal UI) with **raw terminal mode** that intercepts programmatic Enter keypresses. This makes `tmux send-keys` unreliable for interactive mode.

**Solution: Dual-mode architecture**

| Mode | Mechanism | Use Case |
|------|-----------|----------|
| **Headless** (primary) | `claude -p` with process spawning | Automation, queues, scripting — clean stdin/stdout, JSON output, exit codes |
| **Interactive** (secondary) | tmux sessions with Escape+delay+Enter workaround | Live monitoring, manual intervention, TUI dashboard |

Headless mode is the **recommended default** for all automated workflows. Interactive/tmux mode is available for users who want to watch Claude Code work in real-time.

### Forge Enrichment: Scope Phasing (from horizon-sage)

**v0.1 ships headless-only** to reduce brittleness and ship faster:
- CLI + headless session engine + queue runner + orchestrator
- No tmux dependency in v0.1 — removes the most fragile code paths

**v0.2 adds interactive mode + TUI**:
- tmux wrapper, detection heuristics, TUI dashboard
- By then, Claude Code CLI stability is better known

### Forge Enrichment: Abstraction Layer (from horizon-sage)

All Claude Code CLI interaction must go through a `ClaudeBackend` trait:

```rust
// Contract — isolates ALL Claude CLI dependencies
pub trait ClaudeBackend {
    async fn execute(&self, prompt: &str, opts: &ExecOptions) -> Result<ClaudeResponse>;
    async fn execute_streaming(&self, prompt: &str, opts: &ExecOptions) -> Result<impl Stream<Item = StreamEvent>>;
    fn version(&self) -> Result<String>;
}

// Real implementation wraps `claude -p`
pub struct CliBackend { /* ... */ }

// Mock for testing without Claude installed
pub struct MockBackend { /* ... */ }
```

**Why**: `claude -p` output format, flags, and behavior are undocumented internal contracts. This layer lets varre: (1) detect CLI version and adapt, (2) test without Claude installed, (3) swap backends if Anthropic ships a Rust SDK.

### Forge Enrichment: Resilience Mechanisms (from horizon-sage)

- **Circuit breaker**: After 3 consecutive Claude invocations fail, pause queue and alert user
- **Output validation**: Verify JSON schema before parsing, graceful fallback on unexpected format
- **Timeout escalation**: SIGTERM → wait 5s → SIGKILL (not just a single timeout)
- **Version detection**: Parse `claude --version` on startup, warn on untested versions

### Forge Enrichment: Sunset Conditions

This tool's lifecycle depends on Claude Code's evolution:
- If Claude Code ships native batch/queue mode → varre becomes a thin TUI wrapper
- If Claude Code ships a Rust SDK → varre migrates from CLI wrapping to SDK calls
- Maintenance commitment: personal tool with potential community adoption

## Prior Art: claude-tmux

[nielsgroen/claude-tmux](https://github.com/nielsgroen/claude-tmux) (v0.3.0, 55 stars) is an existing Rust TUI for managing Claude Code tmux sessions. Key learnings adopted into varre:

| Pattern from claude-tmux | Adoption in varre |
|--------------------------|-------------------|
| **Status detection via pane content** — looks for `❯` prompt marker with `─` border line above it, then checks for "ctrl+c to interrupt" text to distinguish Working vs Idle | Adopt same heuristic for interactive mode completion detection |
| **ANSI color preservation** — uses `ansi-to-tui` crate to render captured pane output with original terminal colors | Add `ansi-to-tui` to TUI dependencies for live preview |
| **"No server running" graceful handling** — `list_sessions()` returns empty vec instead of error when tmux server isn't running | Adopt same pattern in tmux wrapper |
| **Pane process detection** — finds Claude Code by scanning pane commands for "claude" | Adopt for orphan session discovery |
| **Lean dependency set** — only 7 deps (ratatui, crossterm, anyhow, dirs, unicode-width, ansi-to-tui, git2) | Follow same minimal approach; skip heavy crates |

**Where varre goes beyond claude-tmux**:
- Headless mode (`claude -p`) as primary execution engine — claude-tmux is interactive-only
- Queue-based task runner with persistence
- Multi-session orchestrator with dispatch strategies
- Session state machine with crash recovery
- Cost tracking from JSON output

## Tech Stack

| Concern | Crate | Version | Rationale |
|---------|-------|---------|-----------|
| CLI framework | `clap` | 4.x | derive macros, subcommands, shell completions |
| Async runtime | `tokio` | 1.x | process spawning, MPSC channels, timers |
| TUI framework | `ratatui` + `crossterm` | 0.29+ | actively maintained, rich widget ecosystem (same as claude-tmux) |
| ANSI rendering | `ansi-to-tui` | 7.0 | preserve terminal colors in captured pane output (from claude-tmux) — v0.2 |
| Serialization | `serde` + `serde_json` | 1.x | Claude JSON output parsing |
| Error handling | `anyhow` | 1.0 | lightweight error context (same as claude-tmux; simpler than color-eyre) |
| Logging | `tracing` + `tracing-subscriber` | 0.1+ | async-aware, file logging (not stdout — TUI owns stdout) |
| tmux control | thin `Command` wrapper | — | direct `Command::new("tmux")` with `.context()` — v0.2 |
| Directories | `dirs` | 5.0 | platform-specific paths (same as claude-tmux) |
| Unicode | `unicode-width` | 0.2 | character width for TUI layout (same as claude-tmux) — v0.2 |
| Git | `git` CLI via `Command` | — | shell out to `git` for worktree/branch detection (drop `git2` — C binding overkill) |
| Shutdown | `tokio-util` | 0.7+ | `CancellationToken` for graceful shutdown coordination |
| Job IDs | `uuid` | 1.x | v4 UUIDs for job identity, with `serde` feature |
| Timestamps | `chrono` | 0.4+ | `DateTime<Utc>` for job/session timestamps, with `serde` feature |
| Config parsing | `toml` | 0.8+ | deserialize `config.toml` (serde_json alone can't parse TOML) |

## Project Structure

```
varre/
├── Cargo.toml
├── src/
│   ├── main.rs                 # Entry point, clap CLI dispatch
│   ├── cli.rs                  # Clap derive structs & subcommands
│   ├── config.rs               # Config file loading (~/.config/varre/config.toml)
│   ├── backend/
│   │   ├── mod.rs              # ClaudeBackend trait (abstraction layer for CLI)
│   │   ├── cli.rs              # CliBackend — wraps `claude -p` process
│   │   └── mock.rs             # MockBackend — for testing without Claude installed
│   ├── session/
│   │   ├── mod.rs              # SessionKind enum (NOT trait objects) + SessionId type
│   │   ├── headless.rs         # HeadlessSession — claude -p via ClaudeBackend
│   │   ├── interactive.rs      # InteractiveSession — tmux-based (v0.2)
│   │   ├── state.rs            # SessionState enum + guarded transitions
│   │   └── event.rs            # SessionEvent enum + MPSC channel model
│   ├── tmux/
│   │   ├── mod.rs              # Tmux command wrapper
│   │   ├── send.rs             # send-keys with Escape+delay+Enter
│   │   ├── capture.rs          # capture-pane output parsing
│   │   ├── detection.rs        # Claude status detection (idle/working/waiting) via pane content
│   │   └── session.rs          # tmux session lifecycle (new, kill, list)
│   ├── queue/
│   │   ├── mod.rs              # PromptQueue struct
│   │   ├── runner.rs           # QueueRunner — sequential execution with completion detection
│   │   └── job.rs              # Job struct (prompt, session target, status, output)
│   ├── orchestrator/
│   │   ├── mod.rs              # Orchestrator — manages multiple sessions
│   │   ├── pool.rs             # Session pool with concurrency limits
│   │   └── dispatch.rs         # Route prompts to sessions (round-robin, named, load-based)
│   ├── tui/
│   │   ├── mod.rs              # TUI app struct + event loop
│   │   ├── app.rs              # App state (selected session, view mode)
│   │   ├── ui.rs               # Layout rendering (session list, output pane, status bar)
│   │   ├── widgets/
│   │   │   ├── session_list.rs # Left panel: session names + status indicators
│   │   │   ├── output_view.rs  # Right panel: captured output with scrollback
│   │   │   ├── prompt_input.rs # Bottom: prompt input bar
│   │   │   └── queue_view.rs   # Queue status display
│   │   └── event.rs            # Keyboard/mouse event handling
│   └── error.rs                # Error types
├── tests/
│   ├── integration/
│   │   ├── headless_test.rs    # Headless session lifecycle tests
│   │   ├── tmux_test.rs        # tmux wrapper tests (requires tmux)
│   │   └── queue_test.rs       # Queue execution tests
│   └── unit/
│       ├── state_test.rs       # State machine transition tests
│       └── dispatch_test.rs    # Dispatch strategy tests
└── README.md
```

## Phase 1: Core Foundation

### 1.1 Project Scaffold & CLI

Set up Cargo workspace, clap CLI with subcommands.

```
varre new <name> [--mode headless|interactive]   # Create session
varre send <name> <prompt>                        # Send prompt
varre capture <name>                              # Get output
varre list                                        # List sessions
varre kill <name>                                  # Kill session
varre queue add <prompt>... [--session <name>]    # Add to queue
varre queue run [--concurrency N]                 # Execute queue
varre queue status                                # Show queue state
varre tui                                          # Launch TUI
```

**Acceptance criteria**:
- `varre --help` shows all subcommands
- `varre --version` shows version
- Shell completions generated for bash/zsh/fish

### 1.2 Session State Machine

```
            ┌──────────┐
            │ Creating │
            └────┬─────┘
                 │ success              │ failure
                 ▼                      ▼
            ┌──────────┐          ┌──────────┐
     ┌──────│  Ready   │◄────┐   │  Error   │
     │      └────┬─────┘     │   └────┬─────┘
     │           │ prompt    │        │ retry (count < max_retries)
     │           ▼           │        ▼
     │      ┌──────────┐    │   ┌──────────┐
     │      │   Busy   │────┘   │  Ready   │
     │      └────┬─────┘        └──────────┘
     │           │ permission prompt         │ retries exhausted
     │           ▼                           ▼
     │      ┌──────────────┐           ┌──────────┐
     │      │ WaitingInput │           │   Dead   │
     │      └──────┬───────┘           └──────────┘
     │             │ response / timeout
     │             ▼
     │      ┌──────────┐
     │      │   Busy   │
     │      └──────────┘
     │
     └─────► Dead (kill / crash / external process death)
```

States: `Creating → Ready → Busy → Ready` (happy path), `Busy → WaitingInput → Busy` (permission prompt), `Busy → Error → Ready` (recovery with retry counter), `Error → Dead` (max retries exceeded), `* → Dead` (kill/crash).

#### Forge Enrichment: State Design (from decree-arbiter + depth-seer)

**Added states**:
- `WaitingInput` — Claude is asking for permission (`[y/n]`). Blocks queue runner. Can auto-respond based on config or timeout to Error.
- `Creating → Error` — process spawn fails, claude binary not found

**Concurrency safety**: Use `tokio::sync::RwLock<SessionState>` for interior mutability. Transitions are a method on state itself:

```rust
// Contract — typestate-lite pattern
impl SessionState {
    fn transition(&self, event: SessionEvent) -> Result<SessionState, InvalidTransition>
}
```

**Unified event model**: Both headless and interactive modes emit events into a single `mpsc::Receiver<SessionEvent>`:
- Headless: `tokio::process::Child::wait()` → `SessionEvent::Completed` / `SessionEvent::Failed`
- Interactive: polling task → `SessionEvent::BecameReady` / `SessionEvent::BecameBusy` / `SessionEvent::WaitingInput`
- Orchestrator listens on one channel regardless of mode

**Persistence**: Atomic write pattern — write to `sessions.json.tmp`, `fsync`, then `rename` atomically. Debounce writes to max 1/sec under load.

**Acceptance criteria**:
- Invalid transitions return `Err` (e.g., `send` on `Busy` session)
- `WaitingInput` state handled (auto-respond or timeout)
- Retry counter on `Error` state (default max_retries = 3, then → Dead)
- State persisted atomically to `~/.local/share/varre/sessions.json`
- Orphan process detection on startup (both tmux sessions and `claude -p` processes via `pgrep`)

### 1.3 Tmux Wrapper Module

Thin wrapper around `std::process::Command` for tmux operations:

```rust
// Contract — not implementation code
pub struct Tmux;
impl Tmux {
    fn new_session(name: &str, command: &str) -> Result<()>;
    fn kill_session(name: &str) -> Result<()>;
    fn list_sessions() -> Result<Vec<TmuxSession>>;
    fn send_keys(target: &str, keys: &[&str]) -> Result<()>;
    fn capture_pane(target: &str, lines: Option<i32>) -> Result<String>;
    fn has_session(name: &str) -> Result<bool>;
}
```

**The send-keys workaround** for interactive mode (from research):
1. Send text content
2. Sleep 300ms (let autocomplete engage)
3. Send `Escape` (dismiss autocomplete)
4. Sleep 100ms
5. Send `Enter` (submit)

**Acceptance criteria**:
- Graceful error when tmux is not installed
- All tmux commands timeout after 5s
- Session names sanitized (alphanumeric + hyphens only)

## Phase 2: Headless Session Engine

### 2.1 HeadlessSession

Spawn `claude -p` via `ClaudeBackend` trait with JSON output:

```rust
// Contract
pub struct HeadlessSession {
    id: SessionId,
    state: SessionState,
    working_dir: PathBuf,
    last_session_id: Option<String>,  // Claude session ID for --resume
    backend: Arc<dyn ClaudeBackend>,
    event_tx: mpsc::Sender<SessionEvent>,
}
```

Key flags per invocation:
- `--output-format json` (or `stream-json` for streaming)
- `--resume <session_id>` for multi-turn conversations
- `--allowedTools` configurable per session
- `--max-turns` and `--max-budget-usd` as safety limits

**Completion detection**: Process exit. `claude -p` exits when done — monitor child process with `tokio::process::Command`. Set `kill_on_drop(true)` on child process.

#### Forge Enrichment: Process Safety (from depth-seer)

**[DEEP-001] Kill-signal escalation** (P1): On timeout, send SIGTERM → wait 5s → SIGKILL. Set `kill_on_drop(true)` on `tokio::process::Child`. On startup, scan for orphan `claude` processes via `pgrep -f "claude.*-p"` and offer cleanup.

**[DEEP-002] Stdout buffering limit** (P1): Stream stdout line-by-line rather than collecting into String. Enforce max output size (default 50MB, configurable). If limit exceeded, truncate and set `truncated: true` flag on `ClaudeResponse`.

**[DEEP-005] Malformed JSON recovery** (P2): If JSON parse fails, include first 500 chars of raw output in error context. Use `serde_json::from_str` with descriptive error, not `unwrap`.

**[DEEP-006] Stderr capture** (P2): Capture stderr via `Stdio::piped()`. Include in `ClaudeResponse` as `stderr: Option<String>`. Surface auth failures, rate limits, and CLI errors to user.

**[DEEP-007] Invalid --resume recovery** (P2): If `--resume` fails (detected via exit code + stderr), retry WITHOUT `--resume` (start fresh). Log warning that conversation continuity was lost. Clear `last_session_id`.

**Acceptance criteria**:
- Session tracks Claude `session_id` for conversation continuity
- Timeout configurable (default 5 minutes) with SIGTERM → SIGKILL escalation
- `kill_on_drop(true)` set on all child processes
- Output streamed line-by-line with 50MB size cap
- Stderr captured and surfaced in errors
- `--resume` failure auto-recovers to fresh conversation
- Output parsed into structured `ClaudeResponse` (result text, session_id, cost, duration, stderr, truncated flag)

### 2.2 Interactive Session (tmux)

For users who want to watch Claude work:

```rust
pub struct InteractiveSession {
    id: SessionId,
    state: SessionState,
    tmux_target: String,  // tmux session:window.pane
}

impl InteractiveSession {
    async fn send(&mut self, prompt: &str) -> Result<()>;
    async fn capture(&self) -> Result<String>;
    async fn wait_ready(&self, timeout: Duration) -> Result<()>;
}
```

**Completion detection** (adopted from claude-tmux's proven heuristic):
1. Capture pane content, strip empty lines
2. Find `❯` prompt marker with `─` border line directly above it → confirms input field is present
3. If input field found AND content contains "ctrl+c" + "to interrupt" → **Working** (busy)
4. If input field found WITHOUT interrupt text → **Idle** (ready for input)
5. If content contains `[y/n]` or `[Y/n]` → **WaitingInput** (permission prompt)
6. Otherwise → **Unknown**

This content-inspection approach avoids tight coupling to Claude's implementation.

**Acceptance criteria**:
- Uses Escape+delay+Enter send pattern
- Detection uses claude-tmux's 3-step heuristic (input field → interrupt check → permission check)
- Configurable poll interval (default 1s) and prompt marker
- Timeout on completion detection (default 10 minutes)
- ANSI colors preserved in captured output via `ansi-to-tui`

## Phase 3: Queue & Orchestrator

### 3.1 Prompt Queue

```rust
pub struct PromptQueue {
    jobs: VecDeque<Job>,
    completed: Vec<Job>,
    failed: Vec<Job>,
}

pub struct Job {
    id: Uuid,
    prompt: String,
    session_target: Option<SessionId>,  // None = any available
    status: JobStatus,  // Pending | Running | Completed | Failed
    output: Option<ClaudeResponse>,
    created_at: DateTime<Utc>,
}
```

Queue persistence: atomic write to `~/.local/share/varre/queue.json` on every mutation.

#### Forge Enrichment: Queue Safety (from depth-seer)

**[DEEP-003] Atomic writes** (P1): Write to `queue.json.tmp` → `fsync` → `rename` atomically. Prevents corruption on crash mid-write.

**[DEEP-010] Job deduplication** (P2): Add content hash (SHA-256 of prompt + session_target). On `queue add`, warn if duplicate found within last 100 jobs. `--force` flag to bypass.

**[DEEP-011] Mid-job session death recovery** (P2): Orchestrator heartbeat polls session state every 5s while job is Running. If session transitions to Dead/Error, re-queue job with `retry_count + 1`. Max retries (default 2) before marking permanently Failed. Store `last_error` on Job.

**Acceptance criteria**:
- Queue survives varre restart (atomic writes)
- Failed jobs can be retried (`varre queue retry <job-id>`)
- Duplicate prompts detected and warned
- Jobs auto-re-queued on session death (up to max_retries)
- Queue progress shown (`3/10 completed, 1 running, 6 pending`)

### 3.2 Orchestrator (Multi-Session)

Manages a pool of sessions, dispatches jobs from queue:

```rust
pub struct Orchestrator {
    sessions: HashMap<SessionId, Box<dyn Session>>,
    queue: PromptQueue,
    max_concurrency: usize,  // default 3
}
```

Dispatch strategies:
- **Named**: Job specifies exact session target
- **Round-robin**: Distribute across available sessions
- **Least-busy**: Send to session with shortest queue

#### Forge Enrichment: Orchestrator Safety (from depth-seer)

**[DEEP-008] Multi-instance locking** (P2): File-based advisory lock per session (`~/.local/share/varre/locks/<session>.lock`). `flock` with non-blocking try — return `SessionLocked` error on contention.

**[DEEP-009] Session name prefixing** (P2): Prefix all varre-managed tmux sessions with `varre-` (e.g., `varre-worker-1`). On `list_sessions`, filter by prefix. Prevents collision with user's existing tmux sessions.

**[DEEP-013] Shutdown ordering** (P3): (1) Stop accepting new jobs → (2) Wait for Running jobs with timeout → (3) Re-queue incomplete jobs → (4) Persist final state → (5) Kill sessions → (6) Remove lock files.

**[DEEP-015] Signal handling** (P3): Install `tokio::signal` handler for SIGTERM/SIGINT. On signal: persist queue, persist session state, clean up lock files, exit. Use `CancellationToken` from `tokio-util` to coordinate shutdown across all tasks.

**Circuit breaker** (from horizon-sage): After 3 consecutive Claude invocations fail, pause queue and surface error to user. Prevents runaway API budget burn on auth failures or rate limits.

**Acceptance criteria**:
- Respects max concurrency limit
- New sessions auto-created when pool is exhausted (up to max)
- Graceful shutdown via CancellationToken: wait for busy sessions, persist state, then kill
- Session names prefixed with `varre-` to avoid tmux collisions
- File locks prevent multi-instance races
- Circuit breaker pauses after 3 consecutive failures

## Phase 4: Interactive TUI

### 4.1 TUI Layout

```
┌─────────────────────────────────────────────────────────┐
│ varre v0.1.0                          3 sessions  │ F1 │
├──────────────┬──────────────────────────────────────────┤
│ Sessions     │ Output: worker-1                        │
│              │                                          │
│ ● worker-1   │ > Analyzing src/main.rs...              │
│ ○ worker-2   │   Found 3 issues:                       │
│ ◌ worker-3   │   1. Missing error handling in parse()  │
│              │   2. Unused import on line 15            │
│──────────────│   3. TODO on line 42                     │
│ Queue: 3/10  │                                          │
│ ██████░░░░   │                                          │
├──────────────┴──────────────────────────────────────────┤
│ > Send prompt: _                                        │
└─────────────────────────────────────────────────────────┘
```

Status indicators: `●` Busy, `○` Ready, `◌` Creating, `✕` Error, `✕` Dead

**Key bindings**:
- `↑/↓` or `j/k`: Navigate sessions
- `Enter`: Focus session / send prompt
- `q`: Quit
- `n`: New session
- `d`: Kill selected session
- `Tab`: Switch between output view / queue view

### 4.2 Event Loop

```rust
// Conceptual pattern — not implementation
loop {
    tokio::select! {
        // TUI keyboard/mouse events (crossterm)
        event = event_stream.next() => handle_input(event),

        // Session output updates (polled or streamed)
        update = session_rx.recv() => update_session_output(update),

        // Queue job completion
        result = queue_rx.recv() => handle_job_result(result),

        // Periodic tick for UI refresh
        _ = tick_interval.tick() => render_frame(),
    }
}
```

**Acceptance criteria**:
- TUI renders at 10fps (100ms tick)
- No blocking the render loop — all I/O is async
- Terminal restored on panic (crossterm alternate screen cleanup)
- Responsive to Ctrl+C (graceful shutdown)

## Phase 5: Configuration & Polish

### 5.1 Config File

`~/.config/varre/config.toml`:

```toml
[defaults]
mode = "headless"              # headless | interactive
max_concurrency = 3
timeout_seconds = 300

[claude]
allowed_tools = ["Read", "Edit", "Bash"]
max_turns = 50
max_budget_usd = 5.0
model = "sonnet"

[tmux]
prompt_marker = "❯"
poll_interval_ms = 1000
send_delay_ms = 300

[tui]
refresh_rate_ms = 100
```

### 5.2 Session Persistence & Recovery

On startup:
1. Load `~/.local/share/varre/sessions.json`
2. For each saved session, check if tmux session still exists
3. Orphan sessions: offer to adopt or kill
4. Queue: resume pending jobs

**Acceptance criteria**:
- `varre list` shows recovered sessions after crash
- No data loss on SIGTERM/SIGINT

## Implementation Order

### v0.1 — Headless CLI + Queue (ship first)

| Order | Phase | Tasks | Dependencies |
|-------|-------|-------|--------------|
| 1 | 1.1 Project scaffold + CLI + config | 4 | None |
| 2 | 1.2 ClaudeBackend trait + CliBackend + MockBackend | 3 | 1.1 |
| 3 | 1.3 Session state machine (with WaitingInput, events, atomic persistence) | 3 | 1.1 |
| 4 | 2.1 HeadlessSession (process safety, stderr, kill escalation) | 5 | 1.2, 1.3 |
| 5 | 3.1 Prompt queue (atomic writes, dedup, retry) | 4 | 1.3 |
| 6 | 3.2 Orchestrator (locking, circuit breaker, signal handling, shutdown) | 4 | 2.1, 3.1 |
| 7 | 5.1 Config file loading | 2 | 1.1 |

### v0.2 — Interactive tmux + TUI (ship second)

| Order | Phase | Tasks | Dependencies |
|-------|-------|-------|--------------|
| 8 | 1.4 Tmux wrapper (version check, prefix, graceful errors) | 4 | v0.1 |
| 9 | 2.2 InteractiveSession (detection heuristic, send workaround) | 4 | 1.4, 1.3 |
| 10 | 4.1-4.2 TUI (session list, output view, prompt input, queue view) | 6 | 3.2, 2.2 |
| 11 | 5.2 Session persistence & recovery (orphan detection) | 3 | All above |

## Non-Goals

- **Not a Claude Code replacement** — varre orchestrates Claude Code, it doesn't replicate its functionality
- **No web UI** — terminal-only, TUI is the visual interface
- **No Windows support** — tmux is Unix-only; macOS/Linux target
- **No custom AI models** — Claude Code only (uses `claude` CLI)
- **No distributed execution** — single machine, local tmux sessions

## Open Questions (from flow-seer)

1. **Prompt collision**: What happens when user sends to a busy session? → **Answer**: Return error with `SessionBusy` status, suggest queue instead
2. **Crash recovery**: How to handle orphan tmux sessions? → **Answer**: Detect on startup via `tmux list-sessions`, offer adopt/kill
3. **Output size**: Claude can produce very long output → **Answer**: Stream to file, TUI shows tail with scrollback
4. **Cost tracking**: Should varre track API costs? → **Answer**: Yes, parse `cost_usd` from JSON output, show in TUI status bar

## Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| Claude Code CLI changes break automation | High | Pin to known-good CLI behavior, test against `-p` contract |
| tmux send-keys workaround breaks | Medium | Headless mode is primary; interactive is best-effort |
| Large output overwhelms TUI | Medium | Stream to file, virtual scrolling in ratatui |
| Session state desync | Medium | Heartbeat checks, state reconciliation on startup |
