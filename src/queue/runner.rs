use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::backend::{ClaudeBackend, ClaudeResponse, ExecOptions};

use super::{JobStatus, PromptQueue, QueueStatus};

/// Executes jobs from a prompt queue using a Claude backend.
pub struct QueueRunner<B: ClaudeBackend> {
    /// Shared queue protected by a mutex.
    queue: Arc<Mutex<PromptQueue>>,
    /// Backend for executing prompts.
    backend: Arc<B>,
}

impl<B: ClaudeBackend> QueueRunner<B> {
    /// Create a new runner with the given queue and backend.
    pub fn new(queue: Arc<Mutex<PromptQueue>>, backend: Arc<B>) -> Self {
        Self { queue, backend }
    }

    /// Take the next pending job, execute it, and mark it completed or failed.
    ///
    /// Returns `None` if no pending jobs are available.
    pub async fn run_next(&self) -> Result<Option<ClaudeResponse>> {
        let (job_id, prompt, session_target) = {
            let mut queue = self.queue.lock().await;
            match queue.next() {
                Some(job) => {
                    job.status = JobStatus::Running;
                    job.started_at = Some(Utc::now());
                    let id = job.id;
                    let prompt = job.prompt.clone();
                    let session_target = job.session_target.clone();
                    queue.save().context("failed to save queue after marking job running")?;
                    (id, prompt, session_target)
                }
                None => return Ok(None),
            }
        };

        let opts = ExecOptions {
            resume_session_id: session_target.map(|s| s.0),
            ..Default::default()
        };

        match self.backend.execute(&prompt, opts).await {
            Ok(response) => {
                let mut queue = self.queue.lock().await;
                queue.complete(job_id, response.clone())?;
                Ok(Some(response))
            }
            Err(e) => {
                let mut queue = self.queue.lock().await;
                queue.fail(job_id, e.to_string())?;
                Err(e)
            }
        }
    }

    /// Run all pending jobs sequentially until the queue is empty or cancellation is requested.
    pub async fn run_all(&self, cancel: CancellationToken) -> Result<QueueStatus> {
        loop {
            if cancel.is_cancelled() {
                tracing::info!("queue runner cancelled");
                break;
            }

            match self.run_next().await {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("job failed: {e}");
                    continue;
                }
            }
        }

        let queue = self.queue.lock().await;
        Ok(queue.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockBackend;
    use crate::queue::job::Job;

    fn setup() -> (Arc<Mutex<PromptQueue>>, QueueRunner<MockBackend>) {
        let path = std::env::temp_dir().join(format!("varre-runner-test-{}.json", uuid::Uuid::new_v4()));
        let queue = Arc::new(Mutex::new(PromptQueue::load(&path).unwrap()));
        let backend = Arc::new(MockBackend::default());
        let runner = QueueRunner::new(queue.clone(), backend);
        (queue, runner)
    }

    #[tokio::test]
    async fn test_run_next_empty() {
        let (_queue, runner) = setup();
        let result = runner.run_next().await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_run_next_success() {
        let (queue, runner) = setup();

        {
            let mut q = queue.lock().await;
            q.add(Job::new("hello".into(), None), false).unwrap();
        }

        let result = runner.run_next().await.unwrap();
        assert!(result.is_some());

        let q = queue.lock().await;
        assert_eq!(q.status().completed, 1);
        assert_eq!(q.status().pending, 0);
    }

    #[tokio::test]
    async fn test_run_all() {
        let (queue, runner) = setup();

        {
            let mut q = queue.lock().await;
            q.add(Job::new("prompt 1".into(), None), false).unwrap();
            q.add(Job::new("prompt 2".into(), None), false).unwrap();
            q.add(Job::new("prompt 3".into(), None), false).unwrap();
        }

        let cancel = CancellationToken::new();
        let status = runner.run_all(cancel).await.unwrap();

        assert_eq!(status.completed, 3);
        assert_eq!(status.pending, 0);
    }

    #[tokio::test]
    async fn test_run_all_cancelled() {
        let (queue, runner) = setup();

        {
            let mut q = queue.lock().await;
            q.add(Job::new("prompt 1".into(), None), false).unwrap();
        }

        let cancel = CancellationToken::new();
        cancel.cancel();

        let status = runner.run_all(cancel).await.unwrap();
        assert_eq!(status.pending, 1);
        assert_eq!(status.completed, 0);
    }
}
