//! Data models and row types for the SQLite memory backend.
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::*;

use crate::focus_stack::{FocusFrameId, FrameState};

pub(crate) struct TurnRecord {
    pub(crate) turn_number: u32,
    pub(crate) user: String,
    pub(crate) assistant: String,
}

pub struct StoredFrame {
    pub id: FocusFrameId,
    pub payload: Value,
    pub state: FrameState,
    pub salience: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub parent_id: Option<FocusFrameId>,
    pub tags: Vec<String>,
    pub pin_count: u32,
    pub touch_count: u32,
}

/// (架构-1): a frozen snapshot of a session's recent
/// frames. Written every N turns (default 50) or on
/// `on_session_end`. The agent loop reads the latest snapshot via
/// [`EmbeddedMemoryBackend::latest_session_snapshot`] when starting
/// a new turn so it doesn't have to replay every frame in the
/// session.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub id: i64,
    pub session_id: String,
    pub turn_number: u32,
    pub summary: String,
    pub frame_ids: Vec<String>,
    pub ts: String,
}

/// (E: memory evolution) — reflective note recorded at
/// session end. Written by `reflection_generator` (no LLM) and
/// surfaced as a `StreamEvent::MemoryUpdate { kind: "reflection" }`
/// so the user can see "what the agent thinks it learned" in chat.
/// Distinct from `facts` (atomic user/assistant assertions) and
/// from `session_snapshots` (state freezes for replay). Reflections
/// are **short, meta-cognitive notes** about the session itself.
#[derive(Debug, Clone)]
pub struct Reflection {
    pub id: i64,
    pub session_id: String,
    pub kind: String,
    pub content: String,
    pub salience: f32,
    pub tags: Vec<String>,
    pub source: String,
    pub turn_count: u32,
    pub created_at: String,
}

impl StoredFrame {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let id_s: String = row.get(0)?;
        let payload_s: String = row.get(1)?;
        let state_s: String = row.get(2)?;
        let salience: f64 = row.get(3)?;
        let created_s: String = row.get(4)?;
        let updated_s: String = row.get(5)?;
        let parent_id_s: Option<String> = row.get(6)?;
        let tags_s: String = row.get(7)?;
        let pin_count: i64 = row.get(8)?;
        let touch_count: i64 = row.get(9)?;

        Ok(Self {
            id: parse_uuid(&id_s).unwrap_or_else(Uuid::nil),
            payload: serde_json::from_str(&payload_s).unwrap_or(Value::Null),
            state: str_to_state(&state_s),
            salience: salience as f32,
            created_at: DateTime::parse_from_rfc3339(&created_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            updated_at: DateTime::parse_from_rfc3339(&updated_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            parent_id: parent_id_s.as_ref().and_then(|s| parse_uuid(s)),
            tags: serde_json::from_str(&tags_s).unwrap_or_default(),
            pin_count: pin_count as u32,
            touch_count: touch_count as u32,
        })
    }
}

pub(crate) fn parse_uuid(s: &str) -> Option<FocusFrameId> {
    Uuid::parse_str(s).ok()
}
