//! SQLite-backed CRUD for the automation three-table model.
//!
//! Shares the same `sessions.db` file as `TaskRepository` (铁律 D/WAL).
//! All operations go through [`open_with_wal`] to ensure WAL mode +
//! busy_timeout are set on every connection.
//!
//!

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;

use crate::{
    Automation, AutomationRun, AutomationRuntimeState, AutomationStatus, RunStatus, RunTrigger,
    ScheduleType,
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

/// Whether `s` is a bare SQL identifier: ASCII letters, digits and
/// underscores, not starting with a digit.
///
/// Used by [`AutomationRepository::ensure_column`]'s debug assertion.
fn is_sql_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

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
/// Stateless — each method opens a fresh connection to the DB.
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
                anchor_session_id TEXT,
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
        // 列迁移：`CREATE TABLE IF NOT EXISTS` 对**已存在**的库不补列，
        // 老用户磁盘上的 automations 表没有 anchor_session_id。查
        // table_info 后按需 ALTER，避免 "duplicate column" 报错。
        Self::ensure_column(
            &conn,
            "automations",
            "anchor_session_id",
            "TEXT",
        )?;
        // run 表的状态机列。同样只对老库补。
        for (col, ty) in [
            ("scheduled_at", "INTEGER"),
            ("trigger", "TEXT"),
            ("claimed_at", "INTEGER"),
            ("dispatched_at", "INTEGER"),
            ("attempts", "INTEGER"),
            ("retry_at", "INTEGER"),
        ] {
            Self::ensure_column(&conn, "automation_runs", col, ty)?;
        }
        // 在途查重要按 (automation_id, status) 走；`list_runs` 的
        // automation_id 前缀索引已覆盖前者的一半，状态在行数少时收益
        // 有限，故不额外建复合索引。
        Ok(())
    }

    /// `ALTER TABLE … ADD COLUMN` 的幂等包装：列已存在则跳过。
    ///
    /// 用 `pragma_table_info` 表值函数而非 `PRAGMA table_info(…)` 拼串：
    /// 表名与列名都走绑定参数，不存在标识符插值。`table`/`column` 目前
    /// 只有字面量调用点，但迁移脚本是复制粘贴的高发地，别给后人留坑。
    fn ensure_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
        ty: &str,
    ) -> Result<(), AutomationRepositoryError> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
                rusqlite::params![table, column],
                |row| row.get(0),
            )
            .ok();
        if exists.is_some() {
            return Ok(());
        }
        // `table` / `column` / `ty` 只能是调用点处的字面量——SQL 的
        // 标识符与类型名不支持绑定参数，sqlite 的 `format!` 只能作用于
        // 值。这个签名把「必须字面量」这件事写进了类型，而把拼串
        // 责任留在唯一的实现里，调用点无法各自拼错。
        debug_assert!(
            is_sql_identifier(table) && is_sql_identifier(column) && is_sql_identifier(ty),
            "ensure_column 的标识符必须是字面量：{table}.{column} {ty}"
        );
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {ty}"
        ))?;
        Ok(())
    }

    // ── Automation CRUD ────────────────────────────────────

    /// Insert a new automation definition.
    pub fn create(&self, auto: &Automation) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        conn.execute(
            "INSERT INTO automations (id, name, prompt, schedule_type, rrule, scheduled_at,
                status, valid_from, valid_until, cwds, connector_ids_json, expert_id, model_id,
                anchor_session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                auto.anchor_session_id,
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
                        expert_id, model_id, anchor_session_id, created_at, updated_at
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
                        expert_id, model_id, anchor_session_id, created_at, updated_at
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
                        expert_id, model_id, anchor_session_id, created_at, updated_at
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
                status, task_id, error_text, scheduled_at, trigger, claimed_at,
                dispatched_at, attempts, retry_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                run.id,
                run.automation_id,
                run.started_at,
                run.finished_at,
                run.status.as_str(),
                run.task_id,
                run.error_text,
                run.scheduled_at,
                run.trigger.as_deref(),
                run.claimed_at,
                run.dispatched_at,
                run.attempts,
                run.retry_at,
            ],
        )?;
        Ok(())
    }

    /// Whether a run with this id already exists.
    ///
    /// The first half of the runId contract's dedup. A `true` here means
    /// this `(automation, trigger, scheduled_at)` triple was already
    /// claimed — clock jump, restart, or a second tick inside one
    /// interval — and must not fire again.
    pub fn run_exists(&self, run_id: &str) -> Result<bool, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM automation_runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// Number of runs for this automation still holding the single-flight slot.
    ///
    /// Must stay in lockstep with [`RunStatus::is_in_flight`] — see that
    /// method's note on why the two callers agreeing is load-bearing.
    pub fn count_in_flight(&self, automation_id: &str) -> Result<u64, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM automation_runs
             WHERE automation_id = ?1
               AND status IN ('claimed', 'dispatched')
               AND finished_at IS NULL",
            [automation_id],
            |row| row.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Consecutive `failed_to_dispatch` runs for this automation,
    /// counting back from the newest terminal run.
    ///
    /// The backoff exponent. Stops at the first run that either dispatched
    /// successfully or reached a non-dispatch terminal state — a success
    /// in between means the streak is broken, and retrying forever across
    /// a recovered outage would be wrong.
    pub fn consecutive_dispatch_failures(&self, automation_id: &str) -> Result<i64, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT status FROM automation_runs
             WHERE automation_id = ?1 AND finished_at IS NOT NULL
             ORDER BY started_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([automation_id], |row| row.get::<_, String>(0))?;
        let mut streak = 0i64;
        for row in rows {
            let status = row?;
            match RunStatus::parse(&status) {
                Some(RunStatus::FailedToDispatch) => streak += 1,
                // Pre-state-machine rows: a dispatch-layer failure used to
                // be recorded as plain `failed`, indistinguishable from an
                // agent failure. Count it — over-retrying one legacy run
                // beats silently dropping it.
                None => streak += 1,
                _ => break,
            }
        }
        Ok(streak)
    }

    /// Reap runs left in flight by a previous process.
    ///
    /// The runId contract prevents duplicates but **cannot** reclaim rows
    /// whose owning process died between claim and dispatch. Without this
    /// sweep such a row is permanent, and — because `count_active_runs`
    /// reads the same predicate — the machine would never be allowed to
    /// sleep again, with nothing in the UI to explain why.
    ///
    /// Returns the number of rows reaped.
    pub fn reap_abandoned_runs(&self) -> Result<usize, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let now = chrono::Utc::now().timestamp_millis();
        let affected = conn.execute(
            "UPDATE automation_runs
             SET status = 'abandoned',
                 finished_at = ?1,
                 error_text = 'process restarted while in flight'
             WHERE status IN ('claimed', 'dispatched') AND finished_at IS NULL",
            [now],
        )?;
        Ok(affected)
    }

    /// Update a run record (e.g. set finished_at + status).
    pub fn update_run(
        &self,
        run_id: &str,
        finished_at: i64,
        status: RunStatus,
        task_id: Option<&str>,
        error_text: Option<&str>,
    ) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let affected = conn.execute(
            "UPDATE automation_runs
             SET finished_at = ?2, status = ?3, task_id = ?4, error_text = ?5, retry_at = NULL
             WHERE id = ?1",
            rusqlite::params![run_id, finished_at, status.as_str(), task_id, error_text],
        )?;
        if affected == 0 {
            return Err(AutomationRepositoryError::NotFound(run_id.to_string()));
        }
        Ok(())
    }

    /// Mark a run as handed to the agent, stamping `dispatched_at`.
    pub fn mark_run_dispatched(&self, run_id: &str) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let now = chrono::Utc::now().timestamp_millis();
        let affected = conn.execute(
            "UPDATE automation_runs
             SET status = 'dispatched', dispatched_at = ?2
             WHERE id = ?1 AND status = 'claimed'",
            rusqlite::params![run_id, now],
        )?;
        if affected == 0 {
            return Err(AutomationRepositoryError::NotFound(run_id.to_string()));
        }
        Ok(())
    }

    /// Schedule a retry for a transiently-failed dispatch.
    ///
    /// Leaves the row non-terminal (`retry_at` set, `finished_at` NULL)
    /// only if the caller wants the same run retried; here we close the
    /// attempt and record when the next one may start, keeping the
    /// previous run's row as history.
    pub fn schedule_dispatch_retry(
        &self,
        run_id: &str,
        retry_at: i64,
        attempts: i64,
        error_text: &str,
    ) -> Result<(), AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let affected = conn.execute(
            "UPDATE automation_runs
             SET status = 'failed_to_dispatch', finished_at = ?2,
                 attempts = ?3, retry_at = ?4, error_text = ?5
             WHERE id = ?1",
            rusqlite::params![run_id, chrono::Utc::now().timestamp_millis(), attempts, retry_at, error_text],
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
            "SELECT id, automation_id, started_at, finished_at, status, task_id, error_text,
                    scheduled_at, trigger, claimed_at, dispatched_at, attempts, retry_at
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

    /// Count runs currently in flight across all automations.
    ///
    /// Drives the desktop "keep the machine awake while work is running"
    /// signal: a background automation has no streaming session, so the
    /// front-end cannot infer activity from its own session state.
    ///
    /// "In flight" is derived, not a stored flag: the row is written at
    /// claim time with a `claimed`/`dispatched` status and
    /// `finished_at = NULL`, and every terminal transition fills
    /// `finished_at`. Counting on `finished_at IS NULL` alone would also
    /// match rows written by a future migration that forgot the status,
    /// so both halves of the invariant are checked.
    pub fn count_active_runs(&self) -> Result<u64, AutomationRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let n = conn.query_row(
            "SELECT COUNT(*) FROM automation_runs
             WHERE status IN ('claimed', 'dispatched') AND finished_at IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(n.max(0) as u64)
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
        anchor_session_id: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
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
    let status_str: String = row.get(4)?;
    let trigger_str: Option<String> = row.get(8)?;
    Ok(AutomationRun {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        // An unrecognised status is data corruption, not a legacy label —
        // `parse` already maps the two legacy spellings. Surfacing it as
        // SQL `InvalidColumnType` beats inventing a status that would then
        // drive retry/skip decisions.
        status: RunStatus::parse(&status_str).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(
                4,
                "automation_runs.status".to_string(),
                rusqlite::types::Type::Text,
            )
        })?,
        task_id: row.get(5)?,
        error_text: row.get(6)?,
        scheduled_at: row.get(7)?,
        trigger: trigger_str
            .as_deref()
            .and_then(RunTrigger::parse)
            .map(|t| t.as_str().to_string()),
        claimed_at: row.get(9)?,
        dispatched_at: row.get(10)?,
        attempts: row.get(11)?,
        retry_at: row.get(12)?,
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


    /// 构造一条 run 记录，状态机字段取合理的默认值。
    /// 测试里手写 13 个字段既啰嗦又容易在加列时漏改，集中在这里。
    fn make_run(automation_id: &str, status: RunStatus) -> AutomationRun {
        let now = chrono::Utc::now().timestamp_millis();
        AutomationRun {
            id: uuid::Uuid::new_v4().to_string(),
            automation_id: automation_id.to_string(),
            started_at: now,
            finished_at: Some(now),
            status,
            task_id: None,
            error_text: None,
            scheduled_at: None,
            trigger: Some("scheduled".to_string()),
            claimed_at: Some(now),
            dispatched_at: None,
            attempts: Some(1),
            retry_at: None,
        }
    }

    /// 构造一条在途（未收尾）的 run。
    fn make_in_flight_run(automation_id: &str, status: RunStatus) -> AutomationRun {
        AutomationRun {
            finished_at: None,
            ..make_run(automation_id, status)
        }
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
        repo.create_run(&make_in_flight_run(&auto.id, RunStatus::Dispatched))
            .unwrap();

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

        repo.create_run(&make_in_flight_run(&auto.id, RunStatus::Dispatched))
            .unwrap();

        let mut done = make_run(&auto.id, RunStatus::Succeeded);
        done.task_id = Some("task-123".to_string());
        done.started_at += 1000;
        repo.create_run(&done).unwrap();

        let runs = repo.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs.len(), 2);
        // Most recent first
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        assert_eq!(runs[1].status, RunStatus::Dispatched);
    }

    #[test]
    fn test_update_run() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Test");
        repo.create(&auto).unwrap();

        let run = make_in_flight_run(&auto.id, RunStatus::Claimed);
        repo.create_run(&run).unwrap();

        let finished_at = chrono::Utc::now().timestamp_millis();
        repo.update_run(
            &run.id,
            finished_at,
            RunStatus::Succeeded,
            Some("task-456"),
            None,
        )
        .unwrap();

        let runs = repo.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        assert_eq!(runs[0].task_id.as_deref(), Some("task-456"));
        assert_eq!(runs[0].finished_at, Some(finished_at));
    }

    #[test]
    fn test_count_active_runs_tracks_in_flight_only() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Nightly");
        repo.create(&auto).unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 0);

        let running = make_in_flight_run(&auto.id, RunStatus::Dispatched);
        repo.create_run(&running).unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 1);

        // A second concurrent run must not be shadowed by the first.
        let also_running = make_in_flight_run(&auto.id, RunStatus::Claimed);
        repo.create_run(&also_running).unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 2);

        let finished_at = chrono::Utc::now().timestamp_millis();
        repo.update_run(&running.id, finished_at, RunStatus::Succeeded, None, None)
            .unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 1);

        // A failed run is still finished — it must not keep the machine awake.
        repo.update_run(
            &also_running.id,
            finished_at,
            RunStatus::Failed,
            None,
            Some("boom"),
        )
        .unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 0);
    }

    #[test]
    fn test_count_active_runs_ignores_finished_at_without_running_status() {
        // Guards the "both halves of the invariant" rule: a row that has a
        // finished_at but was never marked running (partial write, future
        // migration) must not be counted as active work.
        let (repo, _tmp) = repo();
        let auto = make_automation("Partial");
        repo.create(&auto).unwrap();
        // `claimed` 但已 finished：两个条件只满足一半，不该计入在途。
        let odd = make_run(&auto.id, RunStatus::Claimed);
        repo.create_run(&odd).unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 0);
    }

    // --- anchor_session_id round-trip ---------------------------

    #[test]
    fn test_anchor_session_id_round_trips() {
        let (repo, _tmp) = repo();
        let mut auto = make_automation("Anchored");
        auto.anchor_session_id = Some("sess-42".into());
        repo.create(&auto).unwrap();

        let fetched = repo.get(&auto.id).unwrap();
        assert_eq!(fetched.anchor_session_id.as_deref(), Some("sess-42"));
        // list 走另一条 SELECT，索引错位只会在其中一条上暴露。
        let listed = repo.list(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].anchor_session_id.as_deref(), Some("sess-42"));
        assert_eq!(listed[0].created_at, auto.created_at, "列序错位会污染 created_at");
    }

    #[test]
    fn test_anchor_session_id_defaults_to_none() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Unanchored");
        assert!(auto.anchor_session_id.is_none());
        repo.create(&auto).unwrap();
        assert!(repo.get(&auto.id).unwrap().anchor_session_id.is_none());
    }

    /// 老库升级：`CREATE TABLE IF NOT EXISTS` 不给已存在的表补列，必须
    /// 靠 `ensure_column` 的 ALTER。构造一个缺列的旧 schema 再 ensure。
    #[test]
    fn test_ensure_schema_migrates_legacy_table_without_anchor_column() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            // 旧版表结构：14 列，无 anchor_session_id。
            conn.execute_batch(
                "CREATE TABLE automations (
                    id TEXT PRIMARY KEY, name TEXT NOT NULL, prompt TEXT NOT NULL,
                    schedule_type TEXT NOT NULL, rrule TEXT, scheduled_at INTEGER,
                    status TEXT NOT NULL, valid_from INTEGER, valid_until INTEGER,
                    cwds TEXT, connector_ids_json TEXT, expert_id TEXT, model_id TEXT,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
            // 塞一条旧数据，迁移后必须仍可读出。
            conn.execute(
                "INSERT INTO automations (id, name, prompt, schedule_type, status, created_at, updated_at)
                 VALUES ('old-1', 'Old', 'p', 'recurring', 'active', 1, 2)",
                [],
            )
            .unwrap();
        }

        let repo = AutomationRepository::new(path);
        repo.ensure_schema().unwrap();

        let old = repo.get("old-1").unwrap();
        assert_eq!(old.name, "Old");
        assert!(old.anchor_session_id.is_none(), "旧行迁移后无锚点");

        // 幂等：第二次 ensure 不许报 duplicate column。
        repo.ensure_schema().unwrap();
        assert!(repo.get("old-1").is_ok());
    }

    // --- run 状态机 ---------------------------------------------

    /// run 表的 6 个新列也必须能被补上，且旧行读得出。
    #[test]
    fn test_ensure_schema_migrates_legacy_run_table() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy_runs.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            // 旧版 run 表：7 列，无状态机列；status 用旧的字面量。
            conn.execute_batch(
                "CREATE TABLE automations (
                    id TEXT PRIMARY KEY, name TEXT NOT NULL, prompt TEXT NOT NULL,
                    schedule_type TEXT NOT NULL, rrule TEXT, scheduled_at INTEGER,
                    status TEXT NOT NULL, valid_from INTEGER, valid_until INTEGER,
                    cwds TEXT, connector_ids_json TEXT, expert_id TEXT, model_id TEXT,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                );
                CREATE TABLE automation_runs (
                    id TEXT PRIMARY KEY,
                    automation_id TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER,
                    status TEXT NOT NULL,
                    task_id TEXT,
                    error_text TEXT
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO automations (id, name, prompt, schedule_type, status, created_at, updated_at)
                 VALUES ('a1', 'A', 'p', 'recurring', 'active', 1, 2)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO automation_runs (id, automation_id, started_at, finished_at, status)
                 VALUES ('r1', 'a1', 10, 20, 'success')",
                [],
            )
            .unwrap();
        }

        let repo = AutomationRepository::new(path);
        repo.ensure_schema().unwrap();

        // 旧 `success` 状态映射到 Succeeded，不该因改名读不出来。
        let runs = repo.list_runs("a1", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        assert_eq!(runs[0].scheduled_at, None, "新列应为 NULL");
        assert_eq!(runs[0].attempts, None);
    }

    /// 收割：进程重启留下的在途行必须变成 abandoned。
    ///
    /// 这条测试的存在理由是保活信号——`count_active_runs` 读的是同一
    /// 组状态，孤儿行若不被收割，它会永久非零，机器永远不休眠。
    #[test]
    fn test_reap_abandoned_runs_closes_in_flight_rows() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Nightly");
        repo.create(&auto).unwrap();

        repo.create_run(&make_in_flight_run(&auto.id, RunStatus::Claimed))
            .unwrap();
        repo.create_run(&make_in_flight_run(&auto.id, RunStatus::Dispatched))
            .unwrap();
        assert_eq!(repo.count_active_runs().unwrap(), 2);

        let reaped = repo.reap_abandoned_runs().unwrap();
        assert_eq!(reaped, 2);

        // 收割后不再占用单飞槽，保活信号也归零。
        assert_eq!(repo.count_active_runs().unwrap(), 0);
        assert_eq!(repo.count_in_flight(&auto.id).unwrap(), 0);
        let runs = repo.list_runs(&auto.id, 10).unwrap();
        assert!(runs.iter().all(|r| r.status == RunStatus::Abandoned));
        assert!(runs.iter().all(|r| r.finished_at.is_some()));
    }

    /// 收割不该碰到已收尾的行。
    #[test]
    fn test_reap_abandoned_runs_leaves_terminal_rows_alone() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Nightly");
        repo.create(&auto).unwrap();
        let done = make_run(&auto.id, RunStatus::Succeeded);
        repo.create_run(&done).unwrap();

        assert_eq!(repo.reap_abandoned_runs().unwrap(), 0);
        let runs = repo.list_runs(&auto.id, 10).unwrap();
        assert_eq!(runs[0].status, RunStatus::Succeeded);
    }

    /// 同一 slot 的 runId 只能存在一条——去重的前半段。
    #[test]
    fn test_run_exists_detects_duplicate_slot() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Nightly");
        repo.create(&auto).unwrap();

        let run_id = crate::compose_run_id(&auto.id, crate::RunTrigger::Scheduled, Some(1_700_000_000_000));
        assert!(!repo.run_exists(&run_id).unwrap());
        repo.create_run(&AutomationRun {
            id: run_id.clone(),
            ..make_in_flight_run(&auto.id, RunStatus::Claimed)
        })
        .unwrap();
        assert!(repo.run_exists(&run_id).unwrap());
    }

    /// 连续派发失败计数：遇到一次成功就断链。
    #[test]
    fn test_consecutive_dispatch_failures_stops_at_success() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Flaky");
        repo.create(&auto).unwrap();

        // 按时间升序插入：两次派发失败 → 一次成功 → 一次派发失败。
        let mut base = 1_000_000i64;
        let mut add = |status: RunStatus, repo: &AutomationRepository| {
            let mut r = make_run(&auto.id, status);
            r.started_at = base;
            base += 1000;
            repo.create_run(&r).unwrap();
        };
        add(RunStatus::FailedToDispatch, &repo);
        add(RunStatus::FailedToDispatch, &repo);
        add(RunStatus::Succeeded, &repo);
        add(RunStatus::FailedToDispatch, &repo);

        // 最新那条是 FailedToDispatch，往回遇到 Succeeded 就停。
        assert_eq!(repo.consecutive_dispatch_failures(&auto.id).unwrap(), 1);
    }

    /// 退避间隔随尝试次数指数增长，且有上限。
    #[test]
    fn test_backoff_is_exponential_and_capped() {
        use crate::{backoff_delay_ms, MAX_DISPATCH_ATTEMPTS};
        let d1 = backoff_delay_ms(1);
        let d2 = backoff_delay_ms(2);
        let d3 = backoff_delay_ms(3);
        assert!(d2 > d1, "第 2 次必须比第 1 次久");
        assert!(d3 > d2, "第 3 次必须比第 2 次久");
        // 封顶后不再增长。
        assert_eq!(backoff_delay_ms(30), backoff_delay_ms(20));
        // 极大 attempt 不能溢出成负数。
        assert!(backoff_delay_ms(i64::MAX) > 0);
        assert!(MAX_DISPATCH_ATTEMPTS >= 1);
    }

    /// 错误分类：能自己好的归 transient，要改配置才好的归 permanent。
    #[test]
    fn test_classify_dispatch_failure() {
        use crate::{classify_dispatch_failure, FailureClass};
        use hermix_core::AgentError;
        let transient = [
            AgentError::LlmApi("503 overloaded".into()),
            AgentError::Gateway("502".into()),
            AgentError::Timeout("no response".into()),
            AgentError::RateLimited { retry_after_secs: Some(30) },
            AgentError::Io("connection reset".into()),
        ];
        for e in transient {
            assert_eq!(
                classify_dispatch_failure(&e),
                FailureClass::Transient,
                "{e:?} 应判为 transient"
            );
        }
        let permanent = [
            AgentError::Config("missing provider".into()),
            AgentError::AuthFailed("bad api key".into()),
            AgentError::InvalidToolCall("no such tool".into()),
        ];
        for e in permanent {
            assert_eq!(
                classify_dispatch_failure(&e),
                FailureClass::Permanent,
                "{e:?} 应判为 permanent"
            );
        }
    }

    /// is_in_flight 与 SQL 谓词必须一致——两处调用点（单飞 + 保活）
    /// 靠它对齐。
    #[test]
    fn test_in_flight_predicate_matches_count_query() {
        let (repo, _tmp) = repo();
        let auto = make_automation("Nightly");
        repo.create(&auto).unwrap();

        for status in [
            RunStatus::Claimed,
            RunStatus::Dispatched,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::FailedToDispatch,
            RunStatus::Skipped,
            RunStatus::Abandoned,
        ] {
            let fresh = make_automation(&format!("A-{status:?}"));
            repo.create(&fresh).unwrap();
            repo.create_run(&make_in_flight_run(&fresh.id, status))
                .unwrap();
            let from_enum = status.is_in_flight() as u64;
            let from_sql = repo.count_in_flight(&fresh.id).unwrap();
            assert_eq!(
                from_enum, from_sql,
                "{status:?}: 枚举判据与 SQL 谓词不一致"
            );
        }
    }
}
