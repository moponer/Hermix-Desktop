//! Background job supervisor.
//!
//! Each backgrounded command gets a UUID and a directory under
//! `~/.hermix/jobs/<id>/` containing:
//!
//! ```text
//! meta.json     {"id","command","cwd","started_at","finished_at","exit_code","status","pid"}
//! stdout.log    appended raw bytes
//! stderr.log    appended raw bytes
//! status.json   {"status":"running|exited|failed|stopped","exit_code":i32,"pid":u32}
//! ```
//!
//! ## Lifetime
//!
//! The supervisor owns a `tokio::sync::Mutex<HashMap<id, Child>>`
//! *behind* `Arc<Self>`. The supervisor task clones the Arc and uses
//! it to read/mutate the registry; when the child exits it removes
//! itself from the map. `jobs_stop` takes the same mutex and calls
//! `Child::start_kill`; the supervisor then observes the exit and
//! writes the final status.
//!
//! ## Restart recovery
//!
//! `jobs_list` reads `meta.json` from disk for every job it sees.
//! Live handles whose underlying PID has died are filtered out and
//! marked `exited` so the LLM can pick up the picture without
//! needing the in-memory state to survive a process restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

/// On-disk status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Exited,
    Failed,
    Stopped,
}

/// Job metadata persisted in `meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMeta {
    pub id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub status: JobStatus,
    pub pid: Option<u32>,
}

/// Lightweight summary for `jobs_list`.
#[derive(Debug, Clone, Serialize)]
pub struct JobSummary {
    pub id: String,
    pub command: String,
    pub status: JobStatus,
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
}

/// Stdout or stderr log selector for `jobs_log`.
#[derive(Debug, Clone, Copy)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StatusRecord {
    status: JobStatus,
    exit_code: Option<i32>,
    pid: Option<u32>,
}

/// Live handles are kept here so `jobs_stop` can kill them by id.
type Handles = Mutex<HashMap<String, Child>>;

/// The registry owns the live-handle map. Wrap it in `Arc<JobRegistry>`
/// at the call site (typically inside `LocalBackend::with_jobs`) and
/// share the same Arc with supervisor tasks.
pub struct JobRegistry {
    jobs_dir: PathBuf,
    handles: Handles,
}

impl JobRegistry {
    /// Create a registry rooted at `~/.hermix/jobs/`. Creates the
    /// directory if missing.
    pub fn new() -> Result<Arc<Self>, std::io::Error> {
        let base = dirs_home().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not resolve user home for jobs dir",
            )
        })?;
        let jobs_dir = base.join(".hermix").join("jobs");
        std::fs::create_dir_all(&jobs_dir)?;
        Ok(Arc::new(Self {
            jobs_dir,
            handles: Mutex::new(HashMap::new()),
        }))
    }

    /// For tests / injection — point at an arbitrary directory.
    pub fn with_dir(jobs_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            jobs_dir,
            handles: Mutex::new(HashMap::new()),
        })
    }

    pub fn jobs_dir(&self) -> &Path {
        &self.jobs_dir
    }

    /// Spawn `command` as a background job. Writes `meta.json` and
    /// `status.json`, opens stdout/stderr log files, and starts a
    /// supervisor task. Returns the new job id.
    pub async fn start(self: Arc<Self>, command: &str, cwd: &Path) -> Result<String, std::io::Error> {
        let id = Uuid::new_v4().to_string();
        let job_dir = self.jobs_dir.join(&id);
        std::fs::create_dir_all(&job_dir)?;

        let now = Utc::now();
        let meta = JobMeta {
            id: id.clone(),
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            started_at: now,
            finished_at: None,
            exit_code: None,
            status: JobStatus::Running,
            pid: None,
        };
        write_json(&job_dir.join("meta.json"), &meta)?;

        // Wrap the command in a shell so quoting and operators are
        // honoured by the OS layer (we already vetted the program
        // isn't ours via the dispatch fast path in LocalBackend).
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd.exe");
            c.arg("/c").arg(command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        };
        cmd.current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(append_file(&job_dir.join("stdout.log"))?))
            .stderr(Stdio::from(append_file(&job_dir.join("stderr.log"))?));
        #[cfg(windows)]
        {
            // tokio::process::Command 的 creation_flags 是 Windows 固有方法，
            // 无需 std::os::windows::process::CommandExt trait 导入
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd.spawn()?;
        let pid = child.id();
        let mut meta = meta;
        meta.pid = pid;
        write_json(&job_dir.join("meta.json"), &meta)?;
        write_json(
            &job_dir.join("status.json"),
            &StatusRecord {
                status: JobStatus::Running,
                exit_code: None,
                pid,
            },
        )?;

        self.handles.lock().await.insert(id.clone(), child);

        // Spawn the supervisor task on this Arc so it survives the
        // current call frame.
        let registry = self.clone();
        let job_dir_for_task = job_dir.clone();
        let id_for_task = id.clone();
        tokio::spawn(async move {
            registry
                .supervise(id_for_task.clone(), job_dir_for_task)
                .await;
        });

        Ok(id)
    }

    /// Polling loop: check every 500 ms whether the child has
    /// exited; when it has, write the final status and remove the
    /// live handle.
    async fn supervise(self: Arc<Self>, id: String, job_dir: PathBuf) {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let mut guard = self.handles.lock().await;
            let Some(child) = guard.get_mut(&id) else {
                // Reaped by jobs_stop or external killer — leave
                // whatever status the caller wrote.
                break;
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = status.code();
                    let final_status = if status.success() {
                        JobStatus::Exited
                    } else {
                        JobStatus::Failed
                    };
                    let _ = write_json(
                        &job_dir.join("status.json"),
                        &StatusRecord {
                            status: final_status.clone(),
                            exit_code,
                            pid: child.id(),
                        },
                    );
                    if let Err(e) = update_meta_finished(&job_dir, final_status.clone(), exit_code) {
                        tracing::warn!(job_id = %id, error = %e, "update_meta_finished failed");
                    }
                    guard.remove(&id);
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(job_id = %id, error = %e, "supervisor try_wait failed");
                    let _ = write_json(
                        &job_dir.join("status.json"),
                        &StatusRecord {
                            status: JobStatus::Failed,
                            exit_code: Some(-1),
                            pid: None,
                        },
                    );
                    let _ = update_meta_finished(&job_dir, JobStatus::Failed, Some(-1));
                    guard.remove(&id);
                    break;
                }
            }
        }
    }

    /// List all known jobs (live + on-disk). Live entries whose
    /// child has died are dropped and rewritten as `exited` so the
    /// caller sees a coherent picture.
    pub async fn list(&self) -> Result<Vec<JobSummary>, std::io::Error> {
        // 1. Drop dead live entries and update their on-disk
        //    status so a `jobs_log` afterwards sees the final state.
        let mut stale_ids: Vec<String> = Vec::new();
        {
            let mut guard = self.handles.lock().await;
            for (id, child) in guard.iter_mut() {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    stale_ids.push(id.clone());
                }
            }
            for id in &stale_ids {
                guard.remove(id);
            }
        }
        for id in &stale_ids {
            let job_dir = self.jobs_dir.join(id);
            let _ = write_json(
                &job_dir.join("status.json"),
                &StatusRecord {
                    status: JobStatus::Exited,
                    exit_code: None,
                    pid: None,
                },
            );
            // Mirror onto meta.json so list() (which reads meta.json)
            // sees the new status without depending on the supervisor
            // task to race us to the same write.
            if let Ok(text) = std::fs::read_to_string(job_dir.join("meta.json")) {
                if let Ok(mut m) = serde_json::from_str::<JobMeta>(&text) {
                    m.status = JobStatus::Exited;
                    m.finished_at = Some(chrono::Utc::now());
                    m.exit_code = None;
                    let _ = write_json(&job_dir.join("meta.json"), &m);
                }
            }
        }

        // 2. Read meta.json for every on-disk job dir.
        let mut out: Vec<JobSummary> = Vec::new();
        let entries = match std::fs::read_dir(&self.jobs_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for ent in entries {
            let ent = ent?;
            if !ent.file_type()?.is_dir() {
                continue;
            }
            let meta_path = ent.path().join("meta.json");
            let Ok(text) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            if let Ok(meta) = serde_json::from_str::<JobMeta>(&text) {
                out.push(JobSummary {
                    id: meta.id,
                    command: meta.command,
                    status: meta.status,
                    pid: meta.pid,
                    started_at: meta.started_at,
                    finished_at: meta.finished_at,
                    exit_code: meta.exit_code,
                });
            }
        }
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    }

    /// Stop a running job by id. Kills the live handle and writes
    /// `stopped` to status files.
    pub async fn stop(&self, id: &str) -> Result<JobSummary, std::io::Error> {
        let job_dir = self.jobs_dir.join(id);
        if !job_dir.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("job id {} unknown", id),
            ));
        }
        {
            let mut guard = self.handles.lock().await;
            if let Some(child) = guard.get_mut(id) {
                let pid = child.id();
                let _ = child.start_kill();
                let _ = write_json(
                    &job_dir.join("status.json"),
                    &StatusRecord {
                        status: JobStatus::Stopped,
                        exit_code: Some(-1),
                        pid,
                    },
                );
                let _ = update_meta_finished(&job_dir, JobStatus::Stopped, Some(-1));
                guard.remove(id);
            } else {
                // Not in the live map. If meta.json says it ran,
                // surface a clear "already finished" message.
                let meta = read_meta(&job_dir.join("meta.json"))?;
                if meta.status != JobStatus::Running {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("job {} already finished: {:?}", id, meta.status),
                    ));
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("job {} not in live registry; restart may be required", id),
                ));
            }
        }
        let meta = read_meta(&job_dir.join("meta.json"))?;
        Ok(JobSummary {
            id: meta.id,
            command: meta.command,
            status: meta.status,
            pid: meta.pid,
            started_at: meta.started_at,
            finished_at: meta.finished_at,
            exit_code: meta.exit_code,
        })
    }

    /// Resolve the path to a job's stdout / stderr log.
    pub fn log_path(&self, id: &str, stream: LogStream) -> Result<PathBuf, std::io::Error> {
        let job_dir = self.jobs_dir.join(id);
        if !job_dir.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("job id {} unknown", id),
            ));
        }
        let name = match stream {
            LogStream::Stdout => "stdout.log",
            LogStream::Stderr => "stderr.log",
        };
        Ok(job_dir.join(name))
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), std::io::Error> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, text)
}

fn read_meta(path: &Path) -> Result<JobMeta, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn update_meta_finished(
    job_dir: &Path,
    status: JobStatus,
    exit_code: Option<i32>,
) -> Result<(), std::io::Error> {
    let path = job_dir.join("meta.json");
    let text = std::fs::read_to_string(&path)?;
    let mut meta: JobMeta = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    meta.finished_at = Some(Utc::now());
    meta.exit_code = exit_code;
    meta.status = status;
    write_json(&path, &meta)
}

fn dirs_home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn append_file(path: &Path) -> Result<std::fs::File, std::io::Error> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_then_list_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "hermix_jobs_test_{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let reg = JobRegistry::with_dir(tmp.clone());

        let cmd = if cfg!(windows) { "echo hello" } else { "echo hello" };
        let id = reg.clone().start(cmd, &tmp).await.unwrap();
        // Poll for completion. CI machines can be slow at spawning the
        // supervisor task, so we err on the generous side: 200 × 100 ms
        // = 20 s budget.
        let mut last_status = None;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let list = reg.list().await.unwrap();
            if let Some(j) = list.iter().find(|j| j.id == id) {
                last_status = Some(j.status.clone());
                if j.status != JobStatus::Running {
                    return;
                }
            }
        }
        panic!(
            "job did not finish in time: id={} last_status={:?}",
            id, last_status
        );
    }

    #[tokio::test]
    async fn stop_kills_long_running() {
        let tmp = std::env::temp_dir().join(format!(
            "hermix_jobs_stop_{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let reg = JobRegistry::with_dir(tmp.clone());

        let cmd = if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let id = reg.clone().start(cmd, &tmp).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let summary = reg.stop(&id).await.unwrap();
        assert_eq!(summary.id, id);
        assert_eq!(summary.status, JobStatus::Stopped);
    }
}
