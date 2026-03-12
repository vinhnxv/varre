use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::ClaudeResponse;
use crate::session::SessionId;

/// Current status of a queued job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Waiting to be executed.
    Pending,
    /// Currently being executed.
    Running,
    /// Successfully completed.
    Completed,
    /// Failed after exhausting retries.
    Failed,
}

/// A single prompt job in the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Unique job identifier.
    pub id: Uuid,
    /// The prompt to send to Claude.
    pub prompt: String,
    /// Optional session to target.
    pub session_target: Option<SessionId>,
    /// Current job status.
    pub status: JobStatus,
    /// Claude response on completion.
    pub output: Option<ClaudeResponse>,
    /// When the job was created.
    pub created_at: DateTime<Utc>,
    /// When the job started executing.
    pub started_at: Option<DateTime<Utc>>,
    /// When the job completed or failed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Number of retry attempts so far.
    pub retry_count: u32,
    /// Maximum number of retries before marking as failed.
    pub max_retries: u32,
    /// Last error message if the job failed.
    pub last_error: Option<String>,
    /// Hash of prompt + session_target for deduplication.
    pub content_hash: String,
}

impl Job {
    /// Create a new job with the given prompt and optional session target.
    pub fn new(prompt: String, session_target: Option<SessionId>) -> Self {
        let content_hash = compute_content_hash(&prompt, &session_target);
        Self {
            id: Uuid::new_v4(),
            prompt,
            session_target,
            status: JobStatus::Pending,
            output: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            retry_count: 0,
            max_retries: 2,
            last_error: None,
            content_hash,
        }
    }

    /// Check if this job is a duplicate of another by comparing content hashes.
    pub fn is_duplicate_of(&self, other: &Job) -> bool {
        self.content_hash == other.content_hash
    }
}

/// Compute a deterministic hash string from prompt and session target.
fn compute_content_hash(prompt: &str, session_target: &Option<SessionId>) -> String {
    let mut hasher = DefaultHasher::new();
    let input = format!("{}{:?}", prompt, session_target);
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_job_defaults() {
        let job = Job::new("hello".into(), None);
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.retry_count, 0);
        assert_eq!(job.max_retries, 2);
        assert!(job.output.is_none());
        assert!(job.started_at.is_none());
        assert!(job.completed_at.is_none());
        assert!(!job.content_hash.is_empty());
    }

    #[test]
    fn test_duplicate_detection() {
        let a = Job::new("hello".into(), None);
        let b = Job::new("hello".into(), None);
        let c = Job::new("different".into(), None);

        assert!(a.is_duplicate_of(&b));
        assert!(!a.is_duplicate_of(&c));
    }

    #[test]
    fn test_duplicate_with_session() {
        let sid = SessionId::from_string("test-session".into());
        let a = Job::new("hello".into(), Some(sid.clone()));
        let b = Job::new("hello".into(), Some(sid));
        let c = Job::new("hello".into(), None);

        assert!(a.is_duplicate_of(&b));
        assert!(!a.is_duplicate_of(&c));
    }
}
