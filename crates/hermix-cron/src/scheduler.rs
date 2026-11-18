//! Cron scheduler engine.
//!
//! The `CronScheduler` manages the lifecycle of cron jobs: creation, listing,
//! updating, pausing, resuming, removal, and execution. It runs a background
//! loop that polls for due jobs (default every 60s, overridable via
//! `HERMES_CRON_TICK_SECS`) and dispatches them to the `CronRunner`.

use std::collections::HashMap;
use hermix_config::env_compat;
use std::sync::Arc;

use chrono::Utc;
use hermix_core::AgentResult;
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::time::{self, Duration};

use crate::completion::CronCompletionEvent;
use crate::file_lock::CronFileLock;
use crate::job::{CronJob, JobStatus};
use crate::persistence::JobPersistence;
use crate::runner::CronRunner;

/// Background poll interval for due jobs. Default **60** seconds.
/// Override with **`HERMES_CRON_TICK_SECS`** (integer **1–300**) for integration tests
/// or local debugging; values outside the range are clamped.
fn cron_poll_interval() -> Duration {
    let secs = env_compat("HERMIX_CRON_TICK_SECS", "HERMES_CRON_TICK_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 300);
    Duration::from_secs(secs)
}

/// Default timeout for the per-write `CronFileLock` acquisition.
/// Three seconds is generous: the only contender is the CLI's
/// `hermix cron add/remove` which finishes in milliseconds, or
/// another scheduler instance which we expect to be rare.
fn default_lock_timeout() -> Duration {
    let secs = env_compat("HERMIX_CRON_LOCK_TIMEOUT_SECS", "HERMES_CRON_LOCK_TIMEOUT_SECS")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, 60);
    Duration::from_secs(secs)
}

// ---------------------------------------------------------------------------
// CronError
// ---------------------------------------------------------------------------

/// Errors that can occur during cron scheduler operations.
#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("Job not found: {0}")]
    JobNotFound(String),

    #[error("Invalid job: {0}")]
    InvalidJob(String),

    #[error("Job already exists: {0}")]
    JobAlreadyExists(String),

    #[error("Job is paused: {0}")]
    JobPaused(String),

    #[error("Job is completed: {0}")]
    JobCompleted(String),

    #[error("Agent error: {0}")]
    Agent(#[from] hermix_core::AgentError),

    #[error("Persistence error: {0}")]
    Persistence(String),

    #[error("Scheduler error: {0}")]
    Scheduler(String),

    #[error("Platform delivery failed: {0}")]
    PlatformDelivery(String),
}

// ---------------------------------------------------------------------------
// CronScheduler
// ---------------------------------------------------------------------------

/// The cron scheduler manages cron jobs and runs a background loop that
/// dispatches due jobs to the runner.
pub struct CronScheduler {
    /// In-memory job store.
    jobs: Arc<Mutex<HashMap<String, CronJob>>>,
    /// Persistence backend.
    persistence: Arc<dyn JobPersistence>,
    /// Job runner.
    runner: Arc<CronRunner>,
    /// Optional broadcast of job completion (e.g. gateway HTTP webhooks).
    completion_tx: Option<broadcast::Sender<CronCompletionEvent>>,
    /// Notification handle to stop the scheduler loop.
    stop_notify: Arc<Notify>,
    /// Whether the scheduler loop is running.
    running: Arc<Mutex<bool>>,
    /// Cross-process file lock that serialises all SQLite writes
    /// against concurrent `hermix cron ...` CLI invocations and
    /// other scheduler instances. `None` only in tests that
    /// explicitly opt out via [`Self::without_file_lock`].
    file_lock: Option<CronFileLock>,
}

impl CronScheduler {
    /// Create a new cron scheduler.
    pub fn new(persistence: Arc<dyn JobPersistence>, runner: Arc<CronRunner>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            persistence,
            runner,
            completion_tx: None,
            stop_notify: Arc::new(Notify::new()),
            running: Arc::new(Mutex::new(false)),
            file_lock: None,
        }
    }

    /// Enable the cross-process file lock. `cron_dir` is the
    /// directory that contains the SQLite `cron_jobs.db` (or
    /// the per-job JSON files for `FileJobPersistence`). The
    /// lock file is created at `<cron_dir>/.tick.lock`.
    /// Without a file lock, the scheduler is vulnerable to
    /// `SQLITE_BUSY` when two processes touch the same DB
    /// concurrently. The default behaviour is to **not** take
    /// a lock so unit tests that share a tempdir don't
    /// serialize. Production code paths in `cli_support` and
    /// `lib::cron_scheduler_for_data_dir` call this
    /// automatically.
    pub fn with_file_lock(mut self, cron_dir: impl AsRef<std::path::Path>) -> Self {
        self.file_lock = Some(CronFileLock::at(cron_dir));
        self
    }

    /// Disable the file lock. Test-only convenience.
    #[cfg(test)]
    pub fn without_file_lock(mut self) -> Self {
        self.file_lock = None;
        self
    }

    /// Take the file lock if configured, run `f`, release the
    /// lock when `f` returns. Returns the result of `f` (or
    /// the lock-acquisition error). The lock is *advisory* —
    /// both sides must call this for the protection to be
    /// effective. See [`crate::file_lock`] for details.
    async fn with_lock<F, Fut, T>(&self, f: F) -> Result<T, CronError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, CronError>>,
    {
        let Some(lock) = self.file_lock.clone() else {
            return f().await;
        };
        let timeout = default_lock_timeout();
        // Spawn a blocking task to take the OS-level lock,
        // then run `f` while holding it. Releasing the lock
        // happens when `guard` drops at the end of this
        // function.
        let guard = tokio::task::spawn_blocking(move || lock.acquire(timeout))
            .await
            .map_err(|e| CronError::Scheduler(format!("file lock task panicked: {e}")))?
            .map_err(|e| CronError::Scheduler(format!("file lock acquire failed: {e}")))?;
        let result = f().await;
        // Explicit release on the blocking thread pool so
        // we don't hold it across the await point.
        tokio::task::spawn_blocking(move || guard.release()).await.ok();
        result
    }

    /// Receive [`CronCompletionEvent`] on every finished run (scheduled or manual).
    pub fn set_completion_broadcast(&mut self, tx: broadcast::Sender<CronCompletionEvent>) {
        self.completion_tx = Some(tx);
    }

    fn emit_completion(
        tx: &Option<broadcast::Sender<CronCompletionEvent>>,
        job: &CronJob,
        trigger: &'static str,
        outcome: Result<&AgentResult, String>,
    ) {
        let Some(sender) = tx else {
            return;
        };
        let ev = CronCompletionEvent::new(job, trigger, outcome);
        if let Err(e) = sender.send(ev) {
            tracing::debug!("cron completion broadcast dropped: {}", e);
        }
    }

    /// Load persisted jobs into the scheduler.
    /// Call this before `start()` to re-register jobs from the previous session.
    pub async fn load_persisted_jobs(&self) -> Result<(), CronError> {
        let jobs = self
            .with_lock(|| async {
                self.persistence
                    .load_jobs()
                    .await
                    .map_err(|e| CronError::Persistence(e.to_string()))
            })
            .await?;

        let mut guard = self.jobs.lock().await;
        for job in jobs {
            tracing::info!(
                "Loaded persisted job '{}' ({})",
                job.name.as_deref().unwrap_or(&job.id),
                job.id
            );
            guard.insert(job.id.clone(), job);
        }

        tracing::info!("Loaded {} persisted cron jobs", guard.len());
        Ok(())
    }

    /// Start the scheduler loop in the background.
    /// The loop sleeps (see [`cron_poll_interval`]) then dispatches due jobs.
    /// Returns immediately; the loop runs as a spawned tokio task.
    pub async fn start(&self) {
        let mut running = self.running.lock().await;
        if *running {
            tracing::warn!("Scheduler is already running");
            return;
        }
        *running = true;
        drop(running);

        tracing::info!("Starting cron scheduler");

        let jobs = self.jobs.clone();
        let runner = self.runner.clone();
        let persistence = self.persistence.clone();
        let completion_tx = self.completion_tx.clone();
        let stop_notify = self.stop_notify.clone();
        let running_flag = self.running.clone();
        let file_lock = self.file_lock.clone();

        tokio::spawn(async move {
            loop {
                // Check if we should stop
                if !*running_flag.lock().await {
                    break;
                }

                // Wait for next poll tick or stop signal
                tokio::select! {
                    _ = time::sleep(cron_poll_interval()) => {
                        // Tick: check for due jobs
                    }
                    _ = stop_notify.notified() => {
                        tracing::info!("Scheduler received stop signal");
                        break;
                    }
                }

                let now = Utc::now();
                let mut guard = jobs.lock().await;
                let due_job_ids: Vec<String> = guard
                    .iter()
                    .filter(|(_, job)| job.is_due(now))
                    .map(|(id, _)| id.clone())
                    .collect();

                for job_id in due_job_ids {
                    let job = guard.get(&job_id).cloned();
                    if let Some(mut job) = job {
                        drop(&mut guard);

                        // Run the job
                        tracing::info!(
                            "Executing cron job '{}' ({})",
                            job.name.as_deref().unwrap_or(&job.id),
                            job.id
                        );
                        match runner.run_job(&job).await {
                            Ok(result) => {
                                tracing::info!(
                                    "Cron job '{}' completed successfully (turns: {})",
                                    job.id,
                                    result.total_turns
                                );
                                Self::emit_completion(
                                    &completion_tx,
                                    &job,
                                    "schedule",
                                    Ok(&result),
                                );
                                job.mark_executed(now);
                            }
                            Err(e) => {
                                tracing::error!("Cron job '{}' failed: {}", job.id, e);
                                Self::emit_completion(
                                    &completion_tx,
                                    &job,
                                    "schedule",
                                    Err(e.to_string()),
                                );
                                job.mark_failed();
                            }
                        }

                        // Update the job in memory and persist
                        guard = jobs.lock().await;
                        guard.insert(job.id.clone(), job.clone());
                        drop(&mut guard);

                        // Persist (under file lock). We use
                        // `with_lock` indirectly through a
                        // dedicated helper because we don't
                        // have `&self` here — we have
                        // `persistence` (cloned) and `file_lock`
                        // (cloned).
                        let save_result = if let Some(lock) = file_lock.as_ref() {
                            let lock = lock.clone();
                            let persistence = persistence.clone();
                            let job_for_save = job.clone();
                            tokio::task::spawn_blocking(move || lock.acquire(default_lock_timeout()))
                                .await
                                .ok()
                                .and_then(|r| r.ok())
                                .map(|guard| {
                                    let persistence = persistence.clone();
                                    let job_for_save = job_for_save.clone();
                                    async move {
                                        let r = persistence.save_job(&job_for_save).await;
                                        drop(guard);
                                        r
                                    }
                                })
                        } else {
                            None
                        };
                        let save_result = match save_result {
                            Some(fut) => fut.await,
                            None => persistence.save_job(&job).await,
                        };
                        if let Err(e) = save_result {
                            tracing::error!("Failed to persist job '{}': {}", job.id, e);
                        }

                        guard = jobs.lock().await;
                    }
                }
            }

            tracing::info!("Scheduler loop exited");
        });
    }

    /// Stop the scheduler loop.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        if !*running {
            tracing::warn!("Scheduler is not running");
            return;
        }
        *running = false;
        drop(running);

        self.stop_notify.notify_waiters();
        tracing::info!("Cron scheduler stopped");
    }

    /// Check if the scheduler is currently running.
    pub async fn is_running(&self) -> bool {
        *self.running.lock().await
    }

    /// Create a new cron job.
    /// Validates the job, generates a UUID if needed, computes the next run
    /// time, persists it, and registers it in memory.
    pub async fn create_job(&self, mut job: CronJob) -> Result<String, CronError> {
        // Validate
        job.validate().map_err(CronError::InvalidJob)?;

        // Generate UUID if empty
        if job.id.is_empty() {
            job.id = uuid::Uuid::new_v4().to_string();
        }

        let id = job.id.clone();
        let schedule = job.schedule.clone();

        // Check for duplicate
        {
            let guard = self.jobs.lock().await;
            if guard.contains_key(&id) {
                return Err(CronError::JobAlreadyExists(id));
            }
        }

        // Compute next_run if not set
        if job.next_run.is_none() && job.status == JobStatus::Active {
            job.next_run = job.compute_next_run(Utc::now());
        }

        // Persist (under file lock so concurrent CLI
        // invocations don't hit SQLITE_BUSY)
        self.with_lock(|| async {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))
        })
        .await?;

        // Register in memory
        {
            let mut guard = self.jobs.lock().await;
            guard.insert(id.clone(), job);
        }

        tracing::info!("Created cron job '{}' ({})", id, schedule);
        Ok(id)
    }

    /// List all registered cron jobs.
    pub async fn list_jobs(&self) -> Vec<CronJob> {
        let guard = self.jobs.lock().await;
        guard.values().cloned().collect()
    }

    /// Get a specific job by ID.
    pub async fn get_job(&self, id: &str) -> Option<CronJob> {
        let guard = self.jobs.lock().await;
        guard.get(id).cloned()
    }

    /// Update an existing cron job.
    pub async fn update_job(&self, id: &str, updated: CronJob) -> Result<(), CronError> {
        // Validate the updated job
        updated.validate().map_err(CronError::InvalidJob)?;

        let mut guard = self.jobs.lock().await;
        if !guard.contains_key(id) {
            return Err(CronError::JobNotFound(id.to_string()));
        }

        // Ensure the ID stays consistent
        let mut job = updated;
        job.id = id.to_string();

        // Recompute next_run if schedule changed
        if job.status == JobStatus::Active && job.next_run.is_none() {
            job.next_run = job.compute_next_run(Utc::now());
        }

        // Persist (under file lock)
        self.with_lock(|| async {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))
        })
        .await?;

        guard.insert(id.to_string(), job);
        Ok(())
    }

    /// Pause a cron job.
    pub async fn pause_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        let job = guard
            .get_mut(id)
            .ok_or_else(|| CronError::JobNotFound(id.to_string()))?;

        if job.status == JobStatus::Paused {
            return Err(CronError::JobPaused(id.to_string()));
        }
        if job.status == JobStatus::Completed {
            return Err(CronError::JobCompleted(id.to_string()));
        }

        job.status = JobStatus::Paused;
        drop(guard);

        // Persist the change (under file lock)
        if let Some(job) = self.get_job(id).await {
            self.with_lock(|| async {
                self.persistence
                    .save_job(&job)
                    .await
                    .map_err(|e| CronError::Persistence(e.to_string()))
            })
            .await?;
        }

        tracing::info!("Paused cron job '{}'", id);
        Ok(())
    }

    /// Resume a paused cron job.
    pub async fn resume_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        let job = guard
            .get_mut(id)
            .ok_or_else(|| CronError::JobNotFound(id.to_string()))?;

        if job.status != JobStatus::Paused {
            return Err(CronError::Scheduler(format!(
                "Job '{}' is not paused (status: {})",
                id, job.status
            )));
        }

        job.status = JobStatus::Active;
        job.next_run = job.compute_next_run(Utc::now());
        drop(guard);

        // Persist the change (under file lock)
        if let Some(job) = self.get_job(id).await {
            self.with_lock(|| async {
                self.persistence
                    .save_job(&job)
                    .await
                    .map_err(|e| CronError::Persistence(e.to_string()))
            })
            .await?;
        }

        tracing::info!("Resumed cron job '{}'", id);
        Ok(())
    }

    /// Remove a cron job.
    pub async fn remove_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        if guard.remove(id).is_none() {
            return Err(CronError::JobNotFound(id.to_string()));
        }
        drop(guard);

        // Delete from persistence (under file lock)
        self.with_lock(|| async {
            self.persistence
                .delete_job(id)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))
        })
        .await?;

        tracing::info!("Removed cron job '{}'", id);
        Ok(())
    }

    /// Remove every job bound to a workbench (`CronJob.workbench_id`).
    /// Called when a workbench is deleted so its scheduled tasks don't
    /// outlive it (workbench design note, plan D.4 adversarial review).
    /// Idempotent: a workbench with no jobs returns an empty Vec.
    pub async fn remove_jobs_for_workbench(&self, workbench_id: &str) -> Result<Vec<String>, CronError> {
        let ids: Vec<String> = {
            let guard = self.jobs.lock().await;
            guard
                .values()
                .filter(|j| j.workbench_id.as_deref() == Some(workbench_id))
                .map(|j| j.id.clone())
                .collect()
        };
        for id in &ids {
            // remove_job errors only on JobNotFound — impossible here since
            // we just listed them under the lock; ignore defensively anyway.
            let _ = self.remove_job(id).await;
        }
        if !ids.is_empty() {
            tracing::info!(
                "Removed {} cron job(s) bound to workbench '{}'",
                ids.len(),
                workbench_id
            );
        }
        Ok(ids)
    }

    /// Manually trigger a cron job to run immediately.
    /// The job is executed regardless of its schedule or status (except
    /// Completed jobs). The run count is not incremented for manual triggers.
    pub async fn run_job(&self, id: &str) -> Result<AgentResult, CronError> {
        let job = {
            let guard = self.jobs.lock().await;
            guard
                .get(id)
                .cloned()
                .ok_or_else(|| CronError::JobNotFound(id.to_string()))?
        };

        if job.status == JobStatus::Completed {
            return Err(CronError::JobCompleted(id.to_string()));
        }

        tracing::info!("Manually triggering cron job '{}'", id);
        let run_result = self.runner.run_job(&job).await;
        match &run_result {
            Ok(result) => Self::emit_completion(&self.completion_tx, &job, "manual", Ok(result)),
            Err(e) => {
                Self::emit_completion(&self.completion_tx, &job, "manual", Err(e.to_string()))
            }
        }
        let result = run_result?;

        // Update last_run but don't increment run_count for manual triggers
        {
            let mut guard = self.jobs.lock().await;
            if let Some(j) = guard.get_mut(id) {
                j.last_run = Some(Utc::now());
                if j.status == JobStatus::Failed {
                    // Reset status to Active on successful manual run
                    j.status = JobStatus::Active;
                }
            }
        }

        // Persist the updated job
        if let Some(job) = self.get_job(id).await {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    use crate::cli_support::cron_scheduler_for_data_dir;
    use crate::job::CronJob;

    fn make_test_scheduler() -> CronScheduler {
        let dir = tempdir().expect("tempdir");
        cron_scheduler_for_data_dir(dir.path().to_path_buf())
    }

    #[tokio::test]
    async fn test_create_job_validates() {
        let mut job = CronJob::new("", "");
        job.prompt = "".to_string();
        // Validation should fail for empty schedule
        assert!(job.validate().is_err());
    }

    #[tokio::test]
    async fn test_job_status_active_by_default() {
        let job = CronJob::new("* * * * *", "test");
        assert_eq!(job.status, JobStatus::Active);
    }

    #[tokio::test]
    async fn test_scheduler_create_job_roundtrip() {
        let sched = make_test_scheduler();
        let job = CronJob::new("0 * * * *", "hello");
        let id = sched.create_job(job).await.expect("create");
        let loaded = sched.get_job(&id).await.expect("get");
        assert_eq!(loaded.prompt, "hello");
        assert_eq!(sched.list_jobs().await.len(), 1);
    }

    #[tokio::test]
    async fn test_remove_jobs_for_workbench_cascades_and_is_idempotent() {
        let sched = make_test_scheduler();

        // 两个任务绑定同一工作台，一个普通任务不绑定。
        let mut wb_job_a = CronJob::new("0 8 * * *", "daily summary");
        wb_job_a.workbench_id = Some("wb-x".into());
        let mut wb_job_b = CronJob::new("0 9 * * *", "weekly report");
        wb_job_b.workbench_id = Some("wb-x".into());
        let plain_job = CronJob::new("0 10 * * *", "not workbench");

        sched.create_job(wb_job_a).await.expect("create a");
        sched.create_job(wb_job_b).await.expect("create b");
        sched.create_job(plain_job).await.expect("create plain");

        let removed = sched.remove_jobs_for_workbench("wb-x").await.expect("cascade");
        assert_eq!(removed.len(), 2, "both workbench-bound jobs removed");

        // 绑定工作台的任务没了，普通任务留下。
        let remaining = sched.list_jobs().await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].prompt, "not workbench");

        // 幂等：再删一次返回空，不报错。
        let again = sched.remove_jobs_for_workbench("wb-x").await.expect("idempotent");
        assert!(again.is_empty());
    }
}
