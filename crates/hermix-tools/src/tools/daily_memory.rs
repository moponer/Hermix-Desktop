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
        let conn = hermix_store::open_connection(&path, hermix_store::OpenMode::Write).map_err(|e| {
            ToolError::ExecutionFailed(format!("daily_memory: open {} failed: {e}", path.display()))
        })?;
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

/// 把长文本裁到 `max` 字符（按 char 边界），并留一句可操作的提示。
/// 目的是别让一次 `read` 把上下文吃满。
fn clip_context(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n…（已截断，用 `days` 缩小范围）")
}

fn today_local() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::clip_context;

    /// 短文本原样返回，不加任何尾巴。
    #[test]
    fn clip_leaves_short_text_untouched() {
        let s = "## 2026-10-21\n- conclusions: 做完了";
        assert_eq!(clip_context(s, 16_000), s);
    }

    /// 截断必须落在 char 边界上（中文多字节，按字节切会 panic 或乱码）。
    #[test]
    fn clip_truncates_multibyte_at_char_boundary() {
        let s = "汉".repeat(40);
        let out = clip_context(&s, 10);
        assert!(out.starts_with(&"汉".repeat(10)), "应保留前 10 个汉字");
        assert!(out.contains("已截断"), "截断必须留一句可操作的提示");
        // 不能出现替换字符（乱码的典型表现）
        assert!(!out.contains('\u{FFFD}'));
    }
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
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("write")
            .trim()
            .to_ascii_lowercase();

        // `read` 分支：把最近的日记读回来。此前这个工具只能写、不能读，
        // 于是「跨天回忆任务进展」只能靠 memory_prefetch 的侧面注入 ——
        // 助理型 agent 需要自己主动回看，这是它的输入源。
        if action == "read" {
            let days = params
                .get("days")
                .and_then(|v| v.as_u64())
                .unwrap_or(7)
                .clamp(1, 30) as u32;
            let store = DailyMemoryStore::open()?;
            let entries = store.list_days(days)?;
            if entries.is_empty() {
                return Ok("daily_memory 还没有任何记录。".into());
            }
            let mut out = String::new();
            for e in &entries {
                out.push_str(&format!("## {}\n", e.date));
                for (label, body) in [
                    ("goals", &e.goals),
                    ("progress", &e.progress),
                    ("conclusions", &e.conclusions),
                ] {
                    let body = body.trim();
                    if !body.is_empty() {
                        out.push_str(&format!("- {label}: {body}\n"));
                    }
                }
                out.push('\n');
            }
            return Ok(clip_context(&out.trim_end().to_string(), 16_000));
        }

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
            "action".into(),
            serde_json::json!({
                "type": "string",
                "enum": ["write", "read"],
                "description": "`write` (default) appends a note to today; `read` returns the last `days` entries."
            }),
        );
        props.insert(
            "section".into(),
            serde_json::json!({
                "type": "string",
                "enum": ["goals", "progress", "conclusions"],
                "description": "Which part of the day's working log to append to. Required for action=write."
            }),
        );
        props.insert(
            "content".into(),
            serde_json::json!({
                "type": "string",
                "description": "The concise note to record (one bullet or short paragraph). Required for action=write."
            }),
        );
        props.insert(
            "days".into(),
            serde_json::json!({
                "type": "integer",
                "description": "How many recent days to return (read only). Default 7, max 30."
            }),
        );
        tool_schema(
            "daily_memory",
            "Read or append the daily working memory (SQLite, one row per day). \
             `action=write` (default): use `goals` at the start of any substantive task, \
             `progress` as work proceeds, `conclusions` at wrap-up. \
             `action=read` + `days`: read back the last N days — use it to recall what \
             was in progress before a restart, or to review recent outcomes.",
            JsonSchema::object(props, vec![]),
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
        "Read or append the daily working memory in SQLite (one row per day). \
        `action=write` (default) records goals at task start, progress as you go, conclusions at wrap-up. \
        `action=read` + `days` reads back the last N days — then you actually need it, not as a habit.\n\n\
        BOUNDARY (three memory layers, pick one): `daily_memory` = this task's timeline \
        (what I decided, what is left) — read it back after a restart; `memory` = durable facts \
        injected every turn (preferences/environment) — do not park task progress there; \
        `session_search` = read-only search over past conversations, cannot write.",
        "📅",
        None,
        RiskLevel::Safe,
        false,
    );
}
