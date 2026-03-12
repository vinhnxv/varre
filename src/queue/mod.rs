pub mod job;
pub mod runner;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::ClaudeResponse;

pub use job::{Job, JobStatus};

/// Summary counts of queue state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStatus {
    /// Number of pending jobs.
    pub pending: usize,
    /// Number of running jobs.
    pub running: usize,
    /// Number of completed jobs.
    pub completed: usize,
    /// Number of failed jobs.
    pub failed: usize,
}

/// Persistent data layout for the queue file.
#[derive(Debug, Serialize, Deserialize)]
struct QueueData {
    jobs: VecDeque<Job>,
    completed: Vec<Job>,
    failed: Vec<Job>,
}

/// A persistent prompt queue backed by a JSON file.
pub struct PromptQueue {
    /// Active and pending jobs.
    jobs: VecDeque<Job>,
    /// Successfully completed jobs.
    completed: Vec<Job>,
    /// Jobs that exhausted retries.
    failed: Vec<Job>,
    /// Path to the queue JSON file.
    data_path: PathBuf,
}

impl PromptQueue {
    /// Load a queue from a JSON file, or create an empty queue if the file is missing or corrupt.
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .context("failed to read queue file");

            match content {
                Ok(text) => {
                    match serde_json::from_str::<QueueData>(&text) {
                        Ok(data) => Ok(Self {
                            jobs: data.jobs,
                            completed: data.completed,
                            failed: data.failed,
                            data_path: path.to_path_buf(),
                        }),
                        Err(e) => {
                            tracing::warn!("corrupt queue file, starting fresh: {e}");
                            Ok(Self::empty(path))
                        }
                    }
                }
                Err(_) => Ok(Self::empty(path)),
            }
        } else {
            Ok(Self::empty(path))
        }
    }

    /// Create an empty queue at the given path.
    fn empty(path: &Path) -> Self {
        Self {
            jobs: VecDeque::new(),
            completed: Vec::new(),
            failed: Vec::new(),
            data_path: path.to_path_buf(),
        }
    }

    /// Persist the queue to disk atomically (write to .tmp → fsync → rename).
    pub fn save(&self) -> Result<()> {
        use std::io::Write;

        let data = QueueData {
            jobs: self.jobs.clone(),
            completed: self.completed.clone(),
            failed: self.failed.clone(),
        };

        let json = serde_json::to_string_pretty(&data)
            .context("failed to serialize queue")?;

        let tmp_path = self.data_path.with_extension("json.tmp");

        if let Some(parent) = self.data_path.parent() {
            std::fs::create_dir_all(parent)
                .context("failed to create queue directory")?;
        }

        let mut file = std::fs::File::create(&tmp_path)
            .context("failed to create temp queue file")?;
        file.write_all(json.as_bytes())
            .context("failed to write temp queue file")?;
        file.sync_all()
            .context("failed to fsync queue file")?;
        std::fs::rename(&tmp_path, &self.data_path)
            .context("failed to rename queue file")?;

        Ok(())
    }

    /// Add a job to the queue.
    ///
    /// When `force` is false, rejects duplicates found in the last 100 jobs.
    pub fn add(&mut self, job: Job, force: bool) -> Result<()> {
        if !force {
            let recent = self.jobs.iter()
                .chain(self.completed.iter())
                .chain(self.failed.iter())
                .rev()
                .take(100);

            for existing in recent {
                if job.is_duplicate_of(existing) {
                    anyhow::bail!(
                        "duplicate job detected (hash {}); use force=true to override",
                        job.content_hash
                    );
                }
            }
        }

        self.jobs.push_back(job);
        self.save()?;
        Ok(())
    }

    /// Get the next pending job as a mutable reference.
    pub fn next(&mut self) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.status == JobStatus::Pending)
    }

    /// Mark a job as completed with the given output.
    pub fn complete(&mut self, job_id: Uuid, output: ClaudeResponse) -> Result<()> {
        let pos = self.jobs.iter().position(|j| j.id == job_id)
            .context("job not found in queue")?;

        let mut job = self.jobs.remove(pos)
            .expect("position was valid");
        job.status = JobStatus::Completed;
        job.output = Some(output);
        job.completed_at = Some(Utc::now());
        self.completed.push(job);
        self.save()?;
        Ok(())
    }

    /// Record a job failure. Re-queues as Pending if retries remain, otherwise moves to failed.
    pub fn fail(&mut self, job_id: Uuid, error: String) -> Result<()> {
        let pos = self.jobs.iter().position(|j| j.id == job_id)
            .context("job not found in queue")?;

        let job = &mut self.jobs[pos];
        job.retry_count += 1;
        job.last_error = Some(error);

        if job.retry_count > job.max_retries {
            let mut job = self.jobs.remove(pos)
                .expect("position was valid");
            job.status = JobStatus::Failed;
            job.completed_at = Some(Utc::now());
            self.failed.push(job);
        } else {
            job.status = JobStatus::Pending;
            job.started_at = None;
        }

        self.save()?;
        Ok(())
    }

    /// Move a failed job back to the queue as Pending.
    pub fn retry(&mut self, job_id: Uuid) -> Result<()> {
        let pos = self.failed.iter().position(|j| j.id == job_id)
            .context("job not found in failed list")?;

        let mut job = self.failed.remove(pos);
        job.status = JobStatus::Pending;
        job.started_at = None;
        job.completed_at = None;
        self.jobs.push_back(job);
        self.save()?;
        Ok(())
    }

    /// Return summary counts of queue state.
    pub fn status(&self) -> QueueStatus {
        let pending = self.jobs.iter().filter(|j| j.status == JobStatus::Pending).count();
        let running = self.jobs.iter().filter(|j| j.status == JobStatus::Running).count();

        QueueStatus {
            pending,
            running,
            completed: self.completed.len(),
            failed: self.failed.len(),
        }
    }

    /// Remove all completed and failed jobs.
    pub fn clear_finished(&mut self) {
        self.completed.clear();
        self.failed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("varre-queue-test-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_load_missing_file() {
        let path = PathBuf::from("/tmp/varre-test-nonexistent.json");
        let q = PromptQueue::load(&path).unwrap();
        assert_eq!(q.status().pending, 0);
    }

    #[test]
    fn test_add_and_next() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let job = Job::new("test prompt".into(), None);
        let id = job.id;
        q.add(job, false).unwrap();

        let next = q.next().unwrap();
        assert_eq!(next.id, id);
        assert_eq!(next.status, JobStatus::Pending);
    }

    #[test]
    fn test_duplicate_rejection() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let a = Job::new("same prompt".into(), None);
        let b = Job::new("same prompt".into(), None);

        q.add(a, false).unwrap();
        assert!(q.add(b, false).is_err());
    }

    #[test]
    fn test_duplicate_force() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let a = Job::new("same prompt".into(), None);
        let b = Job::new("same prompt".into(), None);

        q.add(a, false).unwrap();
        q.add(b, true).unwrap();
        assert_eq!(q.status().pending, 2);
    }

    #[test]
    fn test_complete() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let job = Job::new("test".into(), None);
        let id = job.id;
        q.add(job, false).unwrap();

        let response = ClaudeResponse {
            result: "done".into(),
            session_id: "s1".into(),
            cost_usd: None,
            duration_ms: None,
            stderr: None,
            truncated: false,
            model: None,
        };

        q.complete(id, response).unwrap();
        assert_eq!(q.status().completed, 1);
        assert_eq!(q.status().pending, 0);
    }

    #[test]
    fn test_fail_with_retry() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let job = Job::new("test".into(), None);
        let id = job.id;
        q.add(job, false).unwrap();

        // First failure — should re-queue as Pending
        q.fail(id, "error 1".into()).unwrap();
        assert_eq!(q.status().pending, 1);
        assert_eq!(q.status().failed, 0);

        // Second failure — still within retries
        q.fail(id, "error 2".into()).unwrap();
        assert_eq!(q.status().pending, 1);

        // Third failure — exceeds max_retries (2), moves to failed
        q.fail(id, "error 3".into()).unwrap();
        assert_eq!(q.status().pending, 0);
        assert_eq!(q.status().failed, 1);
    }

    #[test]
    fn test_retry_from_failed() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let mut job = Job::new("test".into(), None);
        job.max_retries = 0; // fail immediately
        let id = job.id;
        q.add(job, false).unwrap();

        q.fail(id, "error".into()).unwrap();
        assert_eq!(q.status().failed, 1);

        q.retry(id).unwrap();
        assert_eq!(q.status().failed, 0);
        assert_eq!(q.status().pending, 1);
    }

    #[test]
    fn test_save_and_reload() {
        let path = temp_path();

        {
            let mut q = PromptQueue::load(&path).unwrap();
            q.add(Job::new("prompt 1".into(), None), false).unwrap();
            q.add(Job::new("prompt 2".into(), None), false).unwrap();
        }

        let q = PromptQueue::load(&path).unwrap();
        assert_eq!(q.status().pending, 2);
    }

    #[test]
    fn test_clear_finished() {
        let path = temp_path();
        let mut q = PromptQueue::load(&path).unwrap();

        let job = Job::new("test".into(), None);
        let id = job.id;
        q.add(job, false).unwrap();

        let response = ClaudeResponse {
            result: "done".into(),
            session_id: "s1".into(),
            cost_usd: None,
            duration_ms: None,
            stderr: None,
            truncated: false,
            model: None,
        };
        q.complete(id, response).unwrap();
        assert_eq!(q.status().completed, 1);

        q.clear_finished();
        assert_eq!(q.status().completed, 0);
        assert_eq!(q.status().failed, 0);
    }
}
