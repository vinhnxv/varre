pub mod event;
pub mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::ClaudeConfig;
use crate::error::VarreError;
use crate::session::state::{SessionEvent, SessionState};

/// Unique identifier for a session.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    /// Generate a new random session ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Create a session ID from an existing string.
    pub fn from_string(id: String) -> Self {
        Self(id)
    }

    /// Return the inner string reference.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The kind of session — currently only headless is supported.
#[derive(Debug)]
pub enum SessionKind {
    /// A headless (non-interactive) Claude Code session.
    Headless(HeadlessSession),
    // Interactive added in v0.2
}

/// A headless Claude Code session managed by varre.
#[derive(Debug)]
pub struct HeadlessSession {
    /// Unique session identifier.
    pub id: SessionId,
    /// Current lifecycle state (behind RwLock for async safety).
    state: RwLock<SessionState>,
    /// Working directory for the session.
    pub working_dir: PathBuf,
    /// Previous session ID for continuation (if any).
    pub last_session_id: Option<String>,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// Claude configuration snapshot for this session.
    pub config: ClaudeConfig,
}

impl HeadlessSession {
    /// Create a new headless session with the given working directory and config.
    pub fn new(working_dir: PathBuf, config: ClaudeConfig) -> Self {
        Self {
            id: SessionId::new(),
            state: RwLock::new(SessionState::Creating),
            working_dir,
            last_session_id: None,
            created_at: Utc::now(),
            config,
        }
    }

    /// Read the current session state.
    pub async fn state(&self) -> SessionState {
        self.state.read().await.clone()
    }

    /// Return a reference to the session ID.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Send an event to transition the session state.
    ///
    /// Returns the new state on success, or an error if the transition is invalid.
    pub async fn send_event(
        &self,
        event: &SessionEvent,
        max_retries: u32,
    ) -> Result<SessionState, VarreError> {
        let mut state = self.state.write().await;
        let new_state = state.transition(event, max_retries)?;
        *state = new_state.clone();
        Ok(new_state)
    }
}

/// Serializable representation of a headless session (for persistence).
#[derive(Debug, Serialize, Deserialize)]
struct HeadlessSessionData {
    id: SessionId,
    state: SessionState,
    working_dir: PathBuf,
    last_session_id: Option<String>,
    created_at: DateTime<Utc>,
    config: ClaudeConfig,
}

/// Serializable wrapper for session kinds (for persistence).
#[derive(Debug, Serialize, Deserialize)]
enum SessionKindData {
    Headless(HeadlessSessionData),
}

impl From<&HeadlessSession> for HeadlessSessionData {
    fn from(session: &HeadlessSession) -> Self {
        // Use try_read to avoid async in a sync context; the store holds the write lock.
        let state = session
            .state
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(SessionState::Creating);
        Self {
            id: session.id.clone(),
            state,
            working_dir: session.working_dir.clone(),
            last_session_id: session.last_session_id.clone(),
            created_at: session.created_at,
            config: session.config.clone(),
        }
    }
}

impl From<HeadlessSessionData> for HeadlessSession {
    fn from(data: HeadlessSessionData) -> Self {
        Self {
            id: data.id,
            state: RwLock::new(data.state),
            working_dir: data.working_dir,
            last_session_id: data.last_session_id,
            created_at: data.created_at,
            config: data.config,
        }
    }
}

/// Manages a collection of sessions with persistence to disk.
pub struct SessionStore {
    /// Path to the sessions.json file.
    path: PathBuf,
    /// In-memory session map.
    sessions: HashMap<SessionId, SessionKind>,
}

impl SessionStore {
    /// Load sessions from a JSON file, or create an empty store if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content =
                std::fs::read_to_string(path).context("failed to read sessions file")?;
            let data: HashMap<String, SessionKindData> =
                serde_json::from_str(&content).context("failed to parse sessions file")?;

            let sessions = data
                .into_iter()
                .map(|(key, kind_data)| {
                    let id = SessionId::from_string(key);
                    let kind = match kind_data {
                        SessionKindData::Headless(d) => SessionKind::Headless(d.into()),
                    };
                    (id, kind)
                })
                .collect();

            Ok(Self {
                path: path.to_path_buf(),
                sessions,
            })
        } else {
            Ok(Self {
                path: path.to_path_buf(),
                sessions: HashMap::new(),
            })
        }
    }

    /// Persist sessions to disk atomically (write .tmp, fsync, rename).
    pub fn save(&self) -> Result<()> {
        let data: HashMap<String, SessionKindData> = self
            .sessions
            .iter()
            .map(|(id, kind)| {
                let kind_data = match kind {
                    SessionKind::Headless(s) => SessionKindData::Headless(s.into()),
                };
                (id.0.clone(), kind_data)
            })
            .collect();

        let json = serde_json::to_string_pretty(&data).context("failed to serialize sessions")?;

        let tmp_path = self.path.with_extension("json.tmp");

        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).context("failed to create sessions directory")?;
        }

        // Write to temp file, fsync, then rename for atomicity.
        {
            use std::io::Write;
            let mut file =
                std::fs::File::create(&tmp_path).context("failed to create temp sessions file")?;
            file.write_all(json.as_bytes())
                .context("failed to write sessions")?;
            file.sync_all().context("failed to fsync sessions file")?;
        }
        std::fs::rename(&tmp_path, &self.path).context("failed to rename sessions file")?;

        Ok(())
    }

    /// Add a session to the store.
    pub fn add(&mut self, id: SessionId, session: SessionKind) {
        self.sessions.insert(id, session);
    }

    /// Remove a session from the store.
    pub fn remove(&mut self, id: &SessionId) -> Option<SessionKind> {
        self.sessions.remove(id)
    }

    /// Get a reference to a session by ID.
    pub fn get(&self, id: &SessionId) -> Option<&SessionKind> {
        self.sessions.get(id)
    }

    /// List all session IDs.
    pub fn list(&self) -> Vec<&SessionId> {
        self.sessions.keys().collect()
    }
}
