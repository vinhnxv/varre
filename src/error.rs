use std::fmt;

#[derive(Debug)]
pub enum VarreError {
    SessionNotFound(String),
    SessionBusy(String),
    SessionLocked(String),
    InvalidTransition { from: String, event: String },
    ClaudeNotFound,
    TmuxNotFound,
    Timeout { seconds: u64 },
    QueueEmpty,
    CircuitBreakerOpen { consecutive_failures: u32 },
    Config(String),
}

impl fmt::Display for VarreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound(name) => write!(f, "session not found: {name}"),
            Self::SessionBusy(name) => write!(f, "session is busy: {name} (use queue instead)"),
            Self::SessionLocked(name) => write!(f, "session is locked by another varre instance: {name}"),
            Self::InvalidTransition { from, event } => {
                write!(f, "invalid state transition: {from} -> {event}")
            }
            Self::ClaudeNotFound => write!(f, "claude CLI not found in PATH"),
            Self::TmuxNotFound => write!(f, "tmux not found in PATH"),
            Self::Timeout { seconds } => write!(f, "operation timed out after {seconds}s"),
            Self::QueueEmpty => write!(f, "queue is empty"),
            Self::CircuitBreakerOpen { consecutive_failures } => {
                write!(f, "circuit breaker open: {consecutive_failures} consecutive failures")
            }
            Self::Config(msg) => write!(f, "config error: {msg}"),
        }
    }
}

impl std::error::Error for VarreError {}
