//! SQLite-backed CRUD for the automation three-table model.
//!
//! Shares the same `sessions.db` file as `TaskRepository` (铁律 D/WAL).
//! All operations go through [`open_with_wal`] to ensure WAL mode +
//! busy_timeout are set on every connection.
//!
//! (Phase 5 — P5-1)

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;

use crate::{
    Automation, AutomationRun, AutomationRuntimeState, AutomationStatus, ScheduleType,
};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AutomationRepositoryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Automation not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// Connection helper (mirrors TaskRepository::open_with_wal)
// ---------------------------------------------------------------------------

fn open_with_wal(path: &Path) -> Result<rusqlite::Connection, AutomationRepositoryError> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| AutomationRepositoryError::Io(format!("Failed to open automations db: {e}")))?;
    if let Err(e) =
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
    {
        tracing::warn!(
            "automation_repository: WAL/busy_timeout PRAGMA failed ({e}); continuing"
        );
    }
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
    Ok(conn)
}

// ---------------------------------------------------------------------------
// AutomationRepository
// ---------------------------------------------------------------------------

/// SQLite-backed repository for automation entities.
/// /// Stateless — each method opens a fresh connection to the DB.
pub struct AutomationRepository {
    db_path: PathBuf,
}

impl AutomationRepository {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    /// Ensure the automation tables exist (idempotent).
    pub fn ensure_schema(&self) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS automations (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                prompt        TEXT NOT NULL,
                schedule_type TEXT NOT NULL,
                rrule         TEXT,
                scheduled_at  TEXT,
                status        TEXT NOT NULL DEFAULT 'ACTIVE',
                valid_from    TEXT,
                valid_until   TEXT,
                cwds          TEXT,
                connector_ids_json TEXT,
                expert_id     TEXT,
                model_id      TEXT,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automations_status ON automations(status);

            CREATE TABLE IF NOT EXISTS automation_runtime_state (
                automation_id TEXT PRIMARY KEY,
                last_run_at   INTEGER,
                next_run_at   INTEGER,
                run_count     INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY(automation_id) REFERENCES automations(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS automation_runs (
                id            TEXT PRIMARY KEY,
                automation_id TEXT NOT NULL,
                started_at    INTEGER NOT NULL,
                finished_at   INTEGER,
                status        TEXT NOT NULL,
                task_id       TEXT,
                error_text    TEXT,
                FOREIGN KEY(automation_id) REFERENCES automations(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_automation_runs_automation ON automation_runs(automation_id);"#,
        )?;
        Ok(())
    }

    // ── Automation CRUD ────────────────────────────────────

    /// Insert a new automation definition.
    pub fn create(&self, auto: &Automation) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        conn.execute(
            "INSERT INTO automations (id, name, prompt, schedule_type, rrule, scheduled_at,
                status, valid_from, valid_until, cwds, connector_ids_json, expert_id, model_id,
                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                auto.id,
                auto.name,
                auto.prompt,
                auto.schedule_type.to_string(),
                auto.rrule,
                auto.scheduled_at,
                auto.status.to_string(),
                auto.valid_from,
                auto.valid_until,
                auto.cwds,
                auto.connector_ids_json,
                auto.expert_id,
                auto.model_id,
                auto.created_at,
                auto.updated_at,
            ],
        )?;

        // Initialize runtime state
        conn.execute(
            "INSERT INTO automation_runtime_state (automation_id, last_run_at, next_run_at, run_count)
             VALUES (?1, NULL, NULL, 0)",
            [&auto.id],
        )?;
        Ok(())
    }

    /// Fetch an automation by ID.
    pub fn get(&self, id: &str) -> Result<Automation, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let auto = conn
            .query_row(
                "SELECT id, name, prompt, schedule_type, rrule, scheduled_at,
                        status, valid_from, valid_until, cwds, connector_ids_json,
                        expert_id, model_id, created_at, updated_at
                 FROM automations WHERE id = ?1",
                [id],
                row_to_automation,
            )
            .optional()?;
        auto.ok_or(AutomationRepositoryError::NotFound(id.to_string()))
    }

    /// List all automations, optionally filtered by status.
    pub fn list(
        &self,
        status_filter: Option<AutomationStatus>,
    ) -> Result<Vec<Automation>, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut autos = Vec::new();
        if let Some(status) = status_filter {
            let status_str = status.to_string();
            let mut stmt = conn.prepare(
                "SELECT id, name, prompt, schedule_type, rrule, scheduled_at,
                        status, valid_from, valid_until, cwds, connector_ids_json,
                        expert_id, model_id, created_at, updated_at
                 FROM automations WHERE status = ?1
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([&status_str], row_to_automation)?;
            for row in rows {
                autos.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, name, prompt, schedule_type, rrule, scheduled_at,
                        status, valid_from, valid_until, cwds, connector_ids_json,
                        expert_id, model_id, created_at, updated_at
                 FROM automations
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_automation)?;
            for row in rows {
                autos.push(row?);
            }
        }
        Ok(autos)
    }

    /// Update an automation's status (ACTIVE/PAUSED).
    pub fn update_status(
        &self,
        id: &str,
        status: AutomationStatus,
    ) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let now = chrono::Utc::now().timestamp_millis();
        let status_str = status.to_string();
        let affected = conn.execute(
            "UPDATE automations SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status_str, now],
        )?;
        if affected == 0 {
            return Err(AutomationRepositoryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Delete an automation and its runtime state + runs (cascade).
    pub fn delete(&self, id: &str) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        // Foreign key cascade handles runtime_state and runs
        let affected = conn.execute("DELETE FROM automations WHERE id = ?1", [id])?;
        if affected == 0 {
            return Err(AutomationRepositoryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    // ── Runtime state ───────────────────────────────────────

    /// Get the runtime state for an automation.
    pub fn get_runtime_state(
        &self,
        automation_id: &str,
    ) -> Result<AutomationRuntimeState, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let state = conn
            .query_row(
                "SELECT automation_id, last_run_at, next_run_at, run_count
                 FROM automation_runtime_state WHERE automation_id = ?1",
                [automation_id],
                row_to_runtime_state,
            )
            .optional()?;
        state.ok_or(AutomationRepositoryError::NotFound(automation_id.to_string()))
    }

    /// Update the runtime state (last_run, next_run, run_count).
    pub fn update_runtime_state(
        &self,
        automation_id: &str,
        last_run_at: Option<i64>,
        next_run_at: Option<i64>,
        run_count: i64,
    ) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let affected = conn.execute(
            "UPDATE automation_runtime_state
             SET last_run_at = ?2, next_run_at = ?3, run_count = ?4
             WHERE automation_id = ?1",
            rusqlite::params![automation_id, last_run_at, next_run_at, run_count],
        )?;
        if affected == 0 {
            return Err(AutomationRepositoryError::NotFound(automation_id.to_string()));
        }
        Ok(())
    }

    // ── Run history ─────────────────────────────────────────

    /// Insert a new run record.
    pub fn create_run(&self, run: &AutomationRun) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        conn.execute(
            "INSERT INTO automation_runs (id, automation_id, started_at, finished_at,
                status, task_id, error_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                run.id,
                run.automation_id,
                run.started_at,
                run.finished_at,
                run.status,
                run.task_id,
                run.error_text,
            ],
        )?;
        Ok(())
    }

    /// Update a run record (e.g. set finished_at + status).
    pub fn update_run(
        &self,
        run_id: &str,
        finished_at: i64,
        status: &str,
        task_id: Option<&str>,
        error_text: Option<&str>,
    ) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let affected = conn.execute(
            "UPDATE automation_runs
             SET finished_at = ?2, status = ?3, task_id = ?4, error_text = ?5
             WHERE id = ?1",
            rusqlite::params![run_id, finished_at, status, task_id, error_text],
        )?;
        if affected == 0 {
            return Err(AutomationRepositoryError::NotFound(run_id.to_string()));
        }
        Ok(())
    }

    /// List runs for an automation, most recent first.
    pub fn list_runs(
        &self,
        automation_id: &str,
        limit: usize,
    ) -> Result<Vec<AutomationRun>, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, automation_id, started_at, finished_at, status, task_id, error_text
             FROM automation_runs WHERE automation_id = ?1
             ORDER BY started_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![automation_id, limit as i64],
            row_to_run,
        )?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }
}

// ---------------------------------------------------------------------------
// Row mappers
// ---------------------------------------------------------------------------

fn row_to_automation(row: &rusqlite::Row) -> rusqlite::Result<Automation> {
    let schedule_type_str: String = row.get(3)?;
    let status_str: String = row.get(6)?;

    let schedule_type = match schedule_type_str.as_str() {
        "recurring" => ScheduleType::Recurring,
        "once" => ScheduleType::Once,
        _ => ScheduleType::Recurring,
    };
    let status = match status_str.as_str() {
        "ACTIVE" => AutomationStatus::Active,
        "PAUSED" => AutomationStatus::Paused,
        _ => AutomationStatus::Active,
    };

    Ok(Automation {
        id: row.get(0)?,
        name: row.get(1)?,
        prompt: row.get(2)?,
        schedule_type,
        rrule: row.get(4)?,
        scheduled_at: row.get(5)?,
        status,
        valid_from: row.get(7)?,
        valid_until: row.get(8)?,
        cwds: row.get(9)?,
        connector_ids_json: row.get(10)?,
        expert_id: row.get(11)?,
        model_id: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn row_to_runtime_state(row: &rusqlite::Row) -> rusqlite::Result<AutomationRuntimeState> {
    Ok(AutomationRuntimeState {
        automation_id: row.get(0)?,
        last_run_at: row.get(1)?,
        next_run_at: row.get(2)?,
        run_count: row.get(3)?,
    })
}

fn row_to_run(row: &rusqlite::Row) -> rusqlite::Result<AutomationRun> {
    Ok(AutomationRun {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        status: row.get(4)?,
        task_id: row.get(5)?,
        error_text: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CreateAutomationParams;

    fn repo() -> (AutomationRepository, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = AutomationRepository::new(tmp.path().join("test.db"));
        repo.ensure_schema().unwrap();
        (repo, tmp)
    }

    fn make_automation(name: &str) -> Automation {
        Automation::new(CreateAutomationParams {
            name: name.into(),
            prompt: "Test prompt".into(),
            schedule_type: ScheduleType::Recurring,
            rrule: Some("0 8 * * *".into()),
            ..Default::default()
        })
    }

    #[test]
    fn test_create_and_get() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Daily Brief");
        repo.create(&auto).unwrap();
        let fetched = repo.get(&auto.id).unwrap();
        assert_eq!(fetched, auto);
    }

    #[test]
    fn test_get_not_found() {
        let (repo, _tmp) = repo();
        let err = repo.get("nonexistent").unwrap_err();
        assert!(matches!(err, AutomationRepositoryError::NotFound(_)));
    }

    #[test]
    fn test_list_all() {
        let (repo, _tmp) = repo();
        repo.create(&make_automation("A1")).unwrap();
        repo.create(&make_automation("A2")).unwrap();
        let list = repo.list(None).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_by_status() {
        let (repo, _tmp) = repo();
        let a1 = make_automation("Active");
        let a2 = make_automation("Paused");
        repo.create(&a1).unwrap();
        repo.create(&a2).unwrap();
        repo.update_status(&a2.id, AutomationStatus::Paused).unwrap();

        let active = repo.list(Some(AutomationStatus::Active)).unwrap();
        assert_eq!(active.len(), 1);
        let paused = repo.list(Some(AutomationStatus::Paused)).unwrap();
        assert_eq!(paused.len(), 1);
    }

    #[test]
    fn test_update_status() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Test");
        repo.create(&auto).unwrap();
        repo.update_status(&auto.id, AutomationStatus::Paused).unwrap();
        let fetched = repo.get(&auto.id).unwrap();
        assert_eq!(fetched.status, AutomationStatus::Paused);
    }

    #[test]
    fn test_delete_cascades() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Test");
        repo.create(&auto).unwrap();

        // Create a run record
        let run = AutomationRun {
            id: uuid::Uuid::new_v4().to_string(),
            automation_id: auto.id.clone(),
            started_at: chrono::Utc::now().timestamp_millis(),
            finished_at: None,
            status: "running".to_string(),
            task_id: None,
            error_text: None,
        };
        repo.create_run(&run).unwrap();

        // Delete should cascade
        repo.delete(&auto.id).unwrap();

        // Runtime state should be gone
        let state = repo.get_runtime_state(&auto.id);
        assert!(state.is_err());

        // Runs should be gone
        let runs = repo.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs.len(), 0);
    }

    #[test]
    fn test_runtime_state() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Test");
        repo.create(&auto).unwrap();

        let state = repo.get_runtime_state(&auto.id).unwrap();
        assert_eq!(state.run_count, 0);
        assert!(state.last_run_at.is_none());

        let now = chrono::Utc::now().timestamp_millis();
        repo.update_runtime_state(&auto.id, Some(now), Some(now + 3600000), 1)
            .unwrap();
        let state = repo.get_runtime_state(&auto.id).unwrap();
        assert_eq!(state.run_count, 1);
        assert_eq!(state.last_run_at, Some(now));
    }

    #[test]
    fn test_create_and_list_runs() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Test");
        repo.create(&auto).unwrap();

        let run1 = AutomationRun {
            id: uuid::Uuid::new_v4().to_string(),
            automation_id: auto.id.clone(),
            started_at: chrono::Utc::now().timestamp_millis(),
            finished_at: None,
            status: "running".to_string(),
            task_id: None,
            error_text: None,
        };
        repo.create_run(&run1).unwrap();

        let run2 = AutomationRun {
            id: uuid::Uuid::new_v4().to_string(),
            automation_id: auto.id.clone(),
            started_at: chrono::Utc::now().timestamp_millis() + 1000,
            finished_at: Some(chrono::Utc::now().timestamp_millis() + 2000),
            status: "success".to_string(),
            task_id: Some("task-123".to_string()),
            error_text: None,
        };
        repo.create_run(&run2).unwrap();

        let runs = repo.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs.len(), 2);
        // Most recent first
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[1].status, "running");
    }

    #[test]
    fn test_update_run() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Test");
        repo.create(&auto).unwrap();

        let run = AutomationRun {
            id: uuid::Uuid::new_v4().to_string(),
            automation_id: auto.id.clone(),
            started_at: chrono::Utc::now().timestamp_millis(),
            finished_at: None,
            status: "running".to_string(),
            task_id: None,
            error_text: None,
        };
        repo.create_run(&run).unwrap();

        let finished_at = chrono::Utc::now().timestamp_millis();
        repo.update_run(&run.id, finished_at, "success", Some("task-456"), None)
            .unwrap();

        let runs = repo.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].task_id.as_deref(), Some("task-456"));
        assert_eq!(runs[0].finished_at, Some(finished_at));
    }
}
