// Daily memory tool + store.
//
// Persists a per-day working log in SQLite so task goals, progress and
// conclusions survive across turns and sessions (architecture-unified with
// the rest of Hermix SQLite state). The agent is required by the forced
// Operating Principles block in prompt_assembly to record the day's goals
// at the first step of any substantive task, update progress as it goes,
// and append conclusions at wrap-up. ensure_today also runs at session
// start so the skeleton always exists.

use std::sync::{Arc, Mutex};

use chrono::Local;
use hermix_config::paths::hermix_home;
use hermix_core::traits::RiskLevel;
use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::Value;

use crate::ToolRegistry;

// Daily memory store: one row per calendar day.
pub struct DailyMemoryStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyMemoryEntry {
    pub date: String,
    pub goals: String,
    pub progress: String,
    pub conclusions: String,
    pub updated_at: i64,
}

impl DailyMemoryStore {
    // Open (creating if needed) the daily-memory DB under the Hermix home.
    pub fn open() -> Result<Self, ToolError> {
        Self::open_at(hermix_home().join("daily_memory.db"))
    }

    // Open at an explicit path — the testable seam; `open` delegates
    // with the resolved hermix home.
    pub fn open_at(path: std::path::PathBuf) -> Result<Self, ToolError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&path).map_err(|e| {
            ToolError::ExecutionFailed(format!("daily_memory: open {} failed: {e}", path.display()))
        })?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory pragma: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS daily_memory (
                date TEXT PRIMARY KEY,
                goals TEXT NOT NULL DEFAULT '',
                progress TEXT NOT NULL DEFAULT '',
                conclusions TEXT NOT NULL DEFAULT '',
                updated_at INTEGER NOT NULL DEFAULT 0
            );",
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory migrate: {e}")))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    // Ensure today's skeleton row exists. Idempotent.
    pub fn ensure_today(&self) -> Result<(), ToolError> {
        let today = today_local();
        let now = now_ms();
        let conn = self
            .conn
            .lock()
            .map_err(|_| ToolError::ExecutionFailed("daily_memory: connection lock poisoned".into()))?;
        conn.execute(
            "INSERT OR IGNORE INTO daily_memory (date, goals, progress, conclusions, updated_at) VALUES (?1, '', '', '', ?2)",
            params![today, now],
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory ensure: {e}")))?;
        Ok(())
    }

    // Append content to one of the three sections for a given day.
    pub fn append_section(&self, date: &str, section: &str, content: &str) -> Result<(), ToolError> {
        let col = match section {
            "goals" => "goals",
            "progress" => "progress",
            "conclusions" => "conclusions",
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "section must be one of goals|progress|conclusions, got `{other}`"
                )))
            }
        };
        let conn = self
            .conn
            .lock()
            .map_err(|_| ToolError::ExecutionFailed("daily_memory: connection lock poisoned".into()))?;
        let existing: String = conn
            .query_row(
                &format!("SELECT {col} FROM daily_memory WHERE date = ?1"),
                params![date],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default();
        let merged = if existing.trim().is_empty() {
            content.to_string()
        } else {
            format!("{existing}\n{content}")
        };
        conn.execute(
            &format!("UPDATE daily_memory SET {col} = ?1, updated_at = ?2 WHERE date = ?3"),
            params![merged, now_ms(), date],
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory append: {e}")))?;
        Ok(())
    }

    // Read a day's entry.
    pub fn get(&self, date: &str) -> Result<DailyMemoryEntry, ToolError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| ToolError::ExecutionFailed("daily_memory: connection lock poisoned".into()))?;
        conn.query_row(
            "SELECT date, goals, progress, conclusions, updated_at FROM daily_memory WHERE date = ?1",
            params![date],
            |r| {
                Ok(DailyMemoryEntry {
                    date: r.get(0)?,
                    goals: r.get(1)?,
                    progress: r.get(2)?,
                    conclusions: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            },
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory get: {e}")))
    }

    // List the most recent day entries, newest first. `limit == 0`
    // returns an empty vec (same contract as the memory-ui listings).
    pub fn list_days(&self, limit: u32) -> Result<Vec<DailyMemoryEntry>, ToolError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| ToolError::ExecutionFailed("daily_memory: connection lock poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT date, goals, progress, conclusions, updated_at
                 FROM daily_memory ORDER BY date DESC LIMIT ?1",
            )
            .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory list: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(DailyMemoryEntry {
                    date: r.get(0)?,
                    goals: r.get(1)?,
                    progress: r.get(2)?,
                    conclusions: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })
            .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory list: {e}")))?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| ToolError::ExecutionFailed(format!("daily_memory list: {e}")))?)
    }
}

fn today_local() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// Tool handler: writes a structured note into today's daily memory.
struct DailyMemoryHandler;

#[async_trait::async_trait]
impl ToolHandler for DailyMemoryHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let section = params
            .get("section")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParams("`section` is required (goals|progress|conclusions)".into())
            })?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("`content` is required (string)".into()))?;
        if content.trim().is_empty() {
            return Err(ToolError::InvalidParams("`content` must be non-empty".into()));
        }
        let store = DailyMemoryStore::open()?;
        store.ensure_today()?;
        let today = today_local();
        store.append_section(&today, section, content)?;
        Ok(format!("Recorded to daily_memory[{today}].{section}."))
    }

    fn schema(&self) -> ToolSchema {
        let mut props: IndexMap<String, Value> = IndexMap::new();
        props.insert(
            "section".into(),
            serde_json::json!({
                "type": "string",
                "enum": ["goals", "progress", "conclusions"],
                "description": "Which part of the day's working log to append to."
            }),
        );
        props.insert(
            "content".into(),
            serde_json::json!({
                "type": "string",
                "description": "The concise note to record (one bullet or short paragraph)."
            }),
        );
        tool_schema(
            "daily_memory",
            "Append a structured note to today's daily working memory (SQLite). \
             Use `goals` at the start of any substantive task to capture objectives, \
             `progress` to record status/checkpoints as work proceeds, and \
             `conclusions` at wrap-up for results and decisions. This makes task \
             goals, progress and outcomes survive across turns and sessions.",
            JsonSchema::object(props, vec!["section".into(), "content".into()]),
        )
    }
}

// Register the daily_memory tool (no backend needed; it owns its SQLite store).
pub fn register(registry: &ToolRegistry) {
    let h = Arc::new(DailyMemoryHandler);
    let schema = h.schema();
    registry.register_with_risk(
        "daily_memory",
        "memory",
        schema,
        h,
        Arc::new(|| true),
        vec![],
        false,
        "Append a structured note (goals|progress|conclusions) to today's daily working memory in SQLite. Required by the operating-principles workflow: record goals at task start, progress as you go, conclusions at wrap-up.",
        "📅",
        None,
        RiskLevel::Safe,
        false,
    );
}
