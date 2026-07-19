//! SQLite-backed implementation of [`hermix_gateway::groupchat::persistence::RoomPersistence`].
//!
//! # Why this lives in `hermix_server::dashboard` and not `hermix_gateway`
//!
//! The `RoomPersistence` trait is defined in `hermix-gateway` because that
//! is where the `groupchat` domain logic (rooms, agents, rules, settings)
//! lives. However, the **concrete** SQLite persistence implementation
//! deliberately lives in `hermix-server/src/dashboard/` rather than
//! `hermix-gateway/src/groupchat/`. This is a deliberate inversion of the
//! usual layering rule:
//!
//! - **`hermix-gateway`** is intentionally storage-agnostic. It depends on
//!   no SQL driver, no database, and no `rusqlite`. This keeps the gateway
//!   crate lightweight (it can be used in tests, in-memory, or with custom
//!   storage backends the user wires in).
//! - **`hermix-server`** owns the on-disk format. This is the only crate
//!   that knows about `~/.hermix/`, `data/groupchat.db`, schema migrations,
//!   and what indices to create. By keeping the SQLite implementation here,
//!   we make it possible to add alternative backends (Postgres, MySQL,
//!   in-memory) without touching `hermix-gateway` at all — they would each
//!   be sibling implementations in `hermix-server` (or in user code that
//!   only depends on the gateway trait).
//!
//! ## Architectural contract (do not break without coordinating)
//!
//! - `hermix-gateway` MUST NOT take a `rusqlite` dependency.
//! - `hermix-server` is the only place schema migrations and DDL live.
//! - If you add a new persistence backend, add a sibling module here
//!   (e.g. `groupchat_postgres.rs`) and a wiring switch in
//!   `hermix-server/src/dashboard/mod.rs`.
//!
//! # Schema
//!
//! Single SQLite file at `<hermix_home>/data/groupchat.db`. Three tables:
//!
//! - `groupchat_rooms` — room metadata (id, name, leader, agent roster,
//!   settings, rules, timestamps).
//! - `groupchat_messages` — message log (one row per message, indexed by
//!   room + timestamp for paginated reads).
//! - `groupchat_membership_events` — audit log of who joined/left when.
//!
//! See `ensure_db()` for the full DDL.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::params;

use hermix_gateway::groupchat::persistence::*;

/// SQLite implementation of [`RoomPersistence`].
///
/// Construct via [`SqliteRoomPersistence::new(hermix_home)`] — this resolves
/// the database path under `<hermix_home>/data/groupchat.db` and runs
/// `ensure_db()` to create tables/indexes if missing.
pub struct SqliteRoomPersistence {
    conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
}

impl SqliteRoomPersistence {
    pub fn new(hermix_home: impl AsRef<Path>) -> Result<Self, String> {
        let data_dir = hermix_config::paths::data_dir_in(hermix_home.as_ref());
        let db_path = data_dir.join("groupchat.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Retry loop for transient SQLite file locks on Windows.
        // A previous sidecar instance may have left a journal/WAL
        // lock on the file; 3 short retries cover the common case
        // without delaying startup too much.
        let conn = Self::open_with_retry(&db_path, 3, std::time::Duration::from_millis(200))?;
        let conn = Arc::new(std::sync::Mutex::new(conn));
        let sp = Self { conn };
        sp.ensure_db()?;
        Ok(sp)
    }

    /// Open a SQLite connection with retries. On Windows, a stale
    /// `groupchat.db` journal from a crash can briefly hold a file
    /// lock that resolves after a short wait.
    fn open_with_retry(
        db_path: &std::path::Path,
        max_attempts: u32,
        delay: std::time::Duration,
    ) -> Result<rusqlite::Connection, String> {
        let mut last_err = String::new();
        for attempt in 1..=max_attempts {
            match rusqlite::Connection::open(db_path) {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    last_err = e.to_string();
                    if attempt < max_attempts {
                        std::thread::sleep(delay);
                    }
                }
            }
        }
        Err(format!(
            "failed to open {} after {} attempts: {}",
            db_path.display(),
            max_attempts,
            last_err,
        ))
    }

    fn ensure_db(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS groupchat_rooms (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                leader_id TEXT,
                agent_ids_json TEXT DEFAULT '[]',
                settings_json TEXT DEFAULT '{}',
                rules_json TEXT DEFAULT '{}',
                announcement TEXT,
                announcement_published_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS groupchat_room_agents (
                room_id TEXT,
                agent_id TEXT,
                role TEXT DEFAULT 'member',
                session_id TEXT,
                model_override TEXT,
                display_name TEXT,
                PRIMARY KEY (room_id, agent_id)
            );
            CREATE TABLE IF NOT EXISTS groupchat_messages (
                id TEXT PRIMARY KEY,
                room_id TEXT NOT NULL,
                sender_id TEXT NOT NULL,
                sender_name TEXT,
                content TEXT,
                message_type TEXT DEFAULT 'chat',
                metadata_json TEXT,
                created_at TEXT
            );
            CREATE TABLE IF NOT EXISTS groupchat_tasks (
                id TEXT PRIMARY KEY,
                room_id TEXT NOT NULL,
                leader_agent_id TEXT NOT NULL,
                original_prompt TEXT,
                plan_json TEXT,
                status TEXT DEFAULT 'pending',
                final_report TEXT,
                approved_at TEXT,
                created_at TEXT,
                completed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS groupchat_subtasks (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                description TEXT,
                assigned_agent_id TEXT,
                session_id TEXT,
                status TEXT DEFAULT 'pending',
                result TEXT,
                started_at TEXT,
                completed_at TEXT,
                review_passed INTEGER,
                review_comment TEXT,
                reviewer_id TEXT,
                reviewed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS groupchat_agent_ranks (
                room_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                score INTEGER NOT NULL DEFAULT 10,
                total_tasks INTEGER NOT NULL DEFAULT 0,
                consecutive_no_violation INTEGER NOT NULL DEFAULT 0,
                consecutive_passes INTEGER NOT NULL DEFAULT 0,
                max_streak INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT,
                PRIMARY KEY (room_id, agent_id)
            );",
        )
        .map_err(|e| e.to_string())?;
        // Idempotent column adds for pre-P2 databases that already
        // created groupchat_rooms without the announcement columns.
        // Each ALTER is wrapped to swallow "duplicate column" errors
        // because the second run will fail harmlessly.
        let _ = conn.execute("ALTER TABLE groupchat_rooms ADD COLUMN announcement TEXT", []);
        let _ = conn.execute("ALTER TABLE groupchat_rooms ADD COLUMN announcement_published_at TEXT", []);
        // P3 display_name: per-room alias for an agent.
        // Idempotent — pre-P3 rooms get the column added on first open.
        let _ = conn.execute("ALTER TABLE groupchat_room_agents ADD COLUMN display_name TEXT", []);
        // Review feedback: subtask review verdict (passed/comment/reviewer/reviewed_at).
        // Idempotent ALTER for pre-P5 databases. Each ALTER swallows
        // "duplicate column" errors harmlessly.
        let _ = conn.execute("ALTER TABLE groupchat_subtasks ADD COLUMN review_passed INTEGER", []);
        let _ = conn.execute("ALTER TABLE groupchat_subtasks ADD COLUMN review_comment TEXT", []);
        let _ = conn.execute("ALTER TABLE groupchat_subtasks ADD COLUMN reviewer_id TEXT", []);
        let _ = conn.execute("ALTER TABLE groupchat_subtasks ADD COLUMN reviewed_at TEXT", []);
        // P1.2 — consecutive-pass streak + lifetime max. Idempotent ALTER
        // for pre-P1.2 databases; second run swallows "duplicate column".
        let _ = conn.execute("ALTER TABLE groupchat_agent_ranks ADD COLUMN consecutive_passes INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE groupchat_agent_ranks ADD COLUMN max_streak INTEGER NOT NULL DEFAULT 0", []);
        // Index on tasks(room_id, created_at) for the "active task" and
        // history queries — both filter by room and sort by created_at.
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_groupchat_tasks_room_created
             ON groupchat_tasks(room_id, created_at DESC)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_groupchat_subtasks_task
             ON groupchat_subtasks(task_id)",
            [],
        );
        // P3 — backfill default rank rows for any pre-existing
        // room_agent row that doesn't have one yet. Idempotent —
        // `INSERT OR IGNORE` skips rows already present.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO groupchat_agent_ranks
                 (room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak, updated_at)
             SELECT room_id, agent_id, 10, 0, 0, 0, 0, ?1
             FROM groupchat_room_agents",
            params![Utc::now().to_rfc3339()],
        );
        Ok(())
    }

    fn parse_dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now())
    }
}

impl RoomPersistence for SqliteRoomPersistence {
    fn create_room(&self, name: &str) -> Result<GroupChatRoom, String> {
        let room = GroupChatRoom {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            leader_id: None,
            agent_ids: vec![],
            settings: RoomSettings::default(),
            rules: RoomRules::default(),
            announcement: None,
            announcement_published_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let leader_session_id = hermix_gateway::groupchat::persistence::new_session_id();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO groupchat_rooms (id, name, agent_ids_json, settings_json, rules_json, announcement, announcement_published_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, ?7)",
            params![
                room.id, room.name,
                serde_json::to_string(&room.agent_ids).unwrap_or_default(),
                serde_json::to_string(&room.settings).unwrap_or_default(),
                serde_json::to_string(&room.rules).unwrap_or_default(),
                room.created_at.to_rfc3339(), room.updated_at.to_rfc3339(),
            ],
        ).map_err(|e| e.to_string())?;
        // Auto-create the built-in leader (总管) row so the existing
        // leader-driven send_message flow has something to dispatch to.
        // Sentinel LEADER_AGENT_ID is excluded from `room.agent_ids` —
        // it lives only in `groupchat_room_agents` so the rest of the
        // codebase can treat it like any other agent for dispatch, but
        // the user-facing member roster never lists it.
        conn.execute(
            "INSERT INTO groupchat_room_agents (room_id, agent_id, role, session_id, model_override, display_name)
             VALUES (?1, ?2, 'leader', ?3, NULL, NULL)",
            params![room.id, LEADER_AGENT_ID, leader_session_id],
        ).map_err(|e| e.to_string())?;
        // P3 — seed the leader's rank row at the default 10/青铜 so
        // the room's first GET /ranks returns a snapshot for every
        // member (including the synthetic leader).
        conn.execute(
            "INSERT OR IGNORE INTO groupchat_agent_ranks
                 (room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak, updated_at)
             VALUES (?1, ?2, 10, 0, 0, 0, 0, ?3)",
            params![room.id, LEADER_AGENT_ID, Utc::now().to_rfc3339()],
        ).map_err(|e| e.to_string())?;
        Ok(room)
    }

    fn get_room(&self, id: &str) -> Result<Option<GroupChatRoom>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, leader_id, agent_ids_json, settings_json, rules_json, announcement, announcement_published_at, created_at, updated_at FROM groupchat_rooms WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let result = stmt.query_row(params![id], |row| {
            Ok(GroupChatRoom {
                id: row.get(0)?,
                name: row.get(1)?,
                leader_id: row.get(2)?,
                agent_ids: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
                settings: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                rules: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                announcement: row.get(6)?,
                announcement_published_at: row.get::<_, Option<String>>(7)?
                    .map(|s| Self::parse_dt(&s)),
                created_at: Self::parse_dt(&row.get::<_, String>(8)?),
                updated_at: Self::parse_dt(&row.get::<_, String>(9)?),
            })
        });
        match result {
            Ok(room) => Ok(Some(room)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn list_rooms(&self) -> Result<Vec<GroupChatRoom>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, leader_id, agent_ids_json, settings_json, rules_json, announcement, announcement_published_at, created_at, updated_at FROM groupchat_rooms ORDER BY updated_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |row| {
            Ok(GroupChatRoom {
                id: row.get(0)?,
                name: row.get(1)?,
                leader_id: row.get(2)?,
                agent_ids: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
                settings: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                rules: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                announcement: row.get(6)?,
                announcement_published_at: row.get::<_, Option<String>>(7)?
                    .map(|s| Self::parse_dt(&s)),
                created_at: Self::parse_dt(&row.get::<_, String>(8)?),
                updated_at: Self::parse_dt(&row.get::<_, String>(9)?),
            })
        }).map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn update_room(&self, room: &GroupChatRoom) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE groupchat_rooms SET name = ?1, leader_id = ?2, agent_ids_json = ?3,
             settings_json = ?4, rules_json = ?5,
             announcement = ?6, announcement_published_at = ?7,
             updated_at = ?8 WHERE id = ?9",
            params![
                room.name, room.leader_id,
                serde_json::to_string(&room.agent_ids).unwrap_or_default(),
                serde_json::to_string(&room.settings).unwrap_or_default(),
                serde_json::to_string(&room.rules).unwrap_or_default(),
                room.announcement,
                room.announcement_published_at.map(|d| d.to_rfc3339()),
                Utc::now().to_rfc3339(), room.id,
            ],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn delete_room(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM groupchat_subtasks WHERE task_id IN (SELECT id FROM groupchat_tasks WHERE room_id = ?1)", params![id]).map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM groupchat_tasks WHERE room_id = ?1", params![id]).map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM groupchat_messages WHERE room_id = ?1", params![id]).map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM groupchat_room_agents WHERE room_id = ?1", params![id]).map_err(|e| e.to_string())?;
        // P3 — drop rank rows too so the foreign key (room_id, agent_id)
        // doesn't leave orphans pointing at a deleted room.
        conn.execute("DELETE FROM groupchat_agent_ranks WHERE room_id = ?1", params![id]).map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM groupchat_rooms WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn add_agent(&self, room_id: &str, agent_id: &str, role: AgentRole) -> Result<AgentRoomProfile, String> {
        let session_id = hermix_gateway::groupchat::persistence::new_session_id();
        let profile = AgentRoomProfile {
            agent_id: agent_id.to_string(),
            room_id: room_id.to_string(),
            role: role.clone(),
            session_id,
            model_override: None,
            display_name: None,
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO groupchat_room_agents (room_id, agent_id, role, session_id, model_override, display_name)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![room_id, agent_id, role.to_string(), profile.session_id, profile.model_override],
        ).map_err(|e| e.to_string())?;
        // P3 — seed a default rank row for this member if it doesn't
        // already have one. `INSERT OR IGNORE` skips rows already
        // present so re-inviting the same agent does NOT reset their
        // existing score.
        conn.execute(
            "INSERT OR IGNORE INTO groupchat_agent_ranks
                 (room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak, updated_at)
             VALUES (?1, ?2, 10, 0, 0, 0, 0, ?3)",
            params![room_id, agent_id, Utc::now().to_rfc3339()],
        ).map_err(|e| e.to_string())?;
        // Update room agent_ids
        let agent_ids_json: String = conn
            .query_row("SELECT agent_ids_json FROM groupchat_rooms WHERE id = ?1", params![room_id], |row| row.get(0))
            .unwrap_or_else(|_| "[]".to_string());
        let mut agent_ids: Vec<String> = serde_json::from_str(&agent_ids_json).unwrap_or_default();
        if !agent_ids.contains(&agent_id.to_string()) {
            agent_ids.push(agent_id.to_string());
            conn.execute(
                "UPDATE groupchat_rooms SET agent_ids_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![serde_json::to_string(&agent_ids).unwrap_or_default(), Utc::now().to_rfc3339(), room_id],
            ).map_err(|e| e.to_string())?;
        }
        Ok(profile)
    }

    fn remove_agent(&self, room_id: &str, agent_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM groupchat_room_agents WHERE room_id = ?1 AND agent_id = ?2", params![room_id, agent_id]).map_err(|e| e.to_string())?;
        // P3 — drop the member's rank row too so /ranks stays in sync.
        conn.execute("DELETE FROM groupchat_agent_ranks WHERE room_id = ?1 AND agent_id = ?2", params![room_id, agent_id]).map_err(|e| e.to_string())?;
        if let Ok(json) = conn.query_row("SELECT agent_ids_json FROM groupchat_rooms WHERE id = ?1", params![room_id], |row| row.get::<_, String>(0)) {
            let mut agent_ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
            agent_ids.retain(|a| a != agent_id);
            conn.execute(
                "UPDATE groupchat_rooms SET agent_ids_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![serde_json::to_string(&agent_ids).unwrap_or_default(), Utc::now().to_rfc3339(), room_id],
            ).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn get_agents(&self, room_id: &str) -> Result<Vec<AgentRoomProfile>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT room_id, agent_id, role, session_id, model_override, display_name FROM groupchat_room_agents WHERE room_id = ?1").map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![room_id], |row| {
            let role_str: String = row.get(2)?;
            let model_override: Option<String> = row.get(4)?;
            let display_name: Option<String> = row.get(5)?;
            Ok(AgentRoomProfile {
                room_id: row.get(0)?,
                agent_id: row.get(1)?,
                role: role_str.parse().unwrap_or(AgentRole::Member),
                session_id: row.get(3)?,
                model_override,
                display_name,
            })
        }).map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn set_member_model(
        &self,
        room_id: &str,
        agent_id: &str,
        model: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE groupchat_room_agents SET model_override = ?1
             WHERE room_id = ?2 AND agent_id = ?3",
            params![model, room_id, agent_id],
        ).map_err(|e| e.to_string())?;
        if rows == 0 {
            return Err(format!("agent {} not in room {}", agent_id, room_id));
        }
        Ok(())
    }

    fn set_member_display_name(
        &self,
        room_id: &str,
        agent_id: &str,
        display_name: Option<&str>,
    ) -> Result<(), String> {
        // Treat empty / whitespace-only as "clear alias" so the API
        // layer can stay naive (clients send "" → we store NULL).
        let normalized: Option<&str> = display_name.map(str::trim).filter(|s| !s.is_empty());
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE groupchat_room_agents SET display_name = ?1
             WHERE room_id = ?2 AND agent_id = ?3",
            params![normalized, room_id, agent_id],
        ).map_err(|e| e.to_string())?;
        if rows == 0 {
            return Err(format!("agent {} not in room {}", agent_id, room_id));
        }
        Ok(())
    }

    fn list_room_ranks(&self, room_id: &str) -> Result<Vec<RankSnapshot>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak
             FROM groupchat_agent_ranks WHERE room_id = ?1
             ORDER BY score DESC, agent_id ASC",
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![room_id], |row| {
            let score: i32 = row.get(2)?;
            let total_tasks: u32 = row.get(3)?;
            let consec: u32 = row.get(4)?;
            let streak: u32 = row.get(5)?;
            let max_streak: u32 = row.get(6)?;
            let mut snap = RankSnapshot {
                agent_id: row.get(1)?,
                room_id: row.get(0)?,
                score,
                tier: String::new(),
                tier_cn: String::new(),
                badge_emoji: String::new(),
                total_tasks,
                consecutive_no_violation: consec,
                consecutive_passes: streak,
                max_streak,
                previous_tier: None,
            };
            snap.refresh_tier();
            Ok(snap)
        }).map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn adjust_member_score(
        &self,
        room_id: &str,
        agent_id: &str,
        delta: i32,
    ) -> Result<RankSnapshot, String> {
        let conn = self.conn.lock().unwrap();
        // Upsert — create the row at the default score first if
        // missing, then apply the delta in a second statement so the
        // increment / decrement is computed by SQLite, not us.
        conn.execute(
            "INSERT OR IGNORE INTO groupchat_agent_ranks
                 (room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak, updated_at)
             VALUES (?1, ?2, 10, 0, 0, 0, 0, ?3)",
            params![room_id, agent_id, Utc::now().to_rfc3339()],
        ).map_err(|e| e.to_string())?;
        if delta > 0 {
            conn.execute(
                "UPDATE groupchat_agent_ranks
                 SET score = MAX(0, score + ?1),
                     total_tasks = total_tasks + 1,
                     consecutive_no_violation = consecutive_no_violation + 1,
                     updated_at = ?2
                 WHERE room_id = ?3 AND agent_id = ?4",
                params![delta, Utc::now().to_rfc3339(), room_id, agent_id],
            ).map_err(|e| e.to_string())?;
        } else if delta < 0 {
            conn.execute(
                "UPDATE groupchat_agent_ranks
                 SET score = MAX(0, score + ?1),
                     consecutive_no_violation = 0,
                     updated_at = ?2
                 WHERE room_id = ?3 AND agent_id = ?4",
                params![delta, Utc::now().to_rfc3339(), room_id, agent_id],
            ).map_err(|e| e.to_string())?;
        } else {
            conn.execute(
                "UPDATE groupchat_agent_ranks SET updated_at = ?1
                 WHERE room_id = ?2 AND agent_id = ?3",
                params![Utc::now().to_rfc3339(), room_id, agent_id],
            ).map_err(|e| e.to_string())?;
        }
        // Read back the updated row.
        let snap = conn.query_row(
            "SELECT room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak
             FROM groupchat_agent_ranks WHERE room_id = ?1 AND agent_id = ?2",
            params![room_id, agent_id],
            |row| {
                let score: i32 = row.get(2)?;
                let total_tasks: u32 = row.get(3)?;
                let consec: u32 = row.get(4)?;
                let streak: u32 = row.get(5)?;
                let max_streak: u32 = row.get(6)?;
                let mut s = RankSnapshot {
                    agent_id: row.get(1)?,
                    room_id: row.get(0)?,
                    score,
                    tier: String::new(),
                    tier_cn: String::new(),
                    badge_emoji: String::new(),
                    total_tasks,
                    consecutive_no_violation: consec,
                    consecutive_passes: streak,
                    max_streak,
                    previous_tier: None,
                };
                s.refresh_tier();
                Ok(s)
            },
        ).map_err(|e| e.to_string())?;
        Ok(snap)
    }

    fn get_agent_score(&self, room_id: &str, agent_id: &str) -> Result<Option<i32>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT score FROM groupchat_agent_ranks WHERE room_id = ?1 AND agent_id = ?2")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query(params![room_id, agent_id])
            .map_err(|e| e.to_string())?;
        match rows.next().map_err(|e| e.to_string())? {
            Some(row) => {
                let score: i32 = row.get(0).map_err(|e| e.to_string())?;
                Ok(Some(score))
            }
            None => Ok(None),
        }
    }

    fn set_agent_score(&self, room_id: &str, agent_id: &str, new_score: i32) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let clamped = new_score.max(-5);
        // Upsert: insert default row if missing, then force-set the score.
        conn.execute(
            "INSERT OR IGNORE INTO groupchat_agent_ranks
                 (room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak, updated_at)
             VALUES (?1, ?2, 10, 0, 0, 0, 0, ?3)",
            params![room_id, agent_id, now],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE groupchat_agent_ranks
             SET score = ?1, updated_at = ?2
             WHERE room_id = ?3 AND agent_id = ?4",
            params![clamped, now, room_id, agent_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn bump_streak(
        &self,
        room_id: &str,
        agent_id: &str,
        passed: bool,
    ) -> Result<u32, String> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        // Upsert: ensure the row exists at default values first.
        // The 0/0 default is fine for both pass and fail paths.
        conn.execute(
            "INSERT OR IGNORE INTO groupchat_agent_ranks
                 (room_id, agent_id, score, total_tasks, consecutive_no_violation, consecutive_passes, max_streak, updated_at)
             VALUES (?1, ?2, 10, 0, 0, 0, 0, ?3)",
            params![room_id, agent_id, now],
        ).map_err(|e| e.to_string())?;
        if passed {
            // saturating_add at the SQL layer so a malicious caller
            // can't wrap u32::MAX back to 0. max_streak uses MAX()
            // so a later pass never lowers the all-time high.
            conn.execute(
                "UPDATE groupchat_agent_ranks
                 SET consecutive_passes = MIN(consecutive_passes + 1, ?1),
                     max_streak = MAX(max_streak, MIN(consecutive_passes + 1, ?1)),
                     updated_at = ?2
                 WHERE room_id = ?3 AND agent_id = ?4",
                params![u32::MAX, now, room_id, agent_id],
            ).map_err(|e| e.to_string())?;
        } else {
            conn.execute(
                "UPDATE groupchat_agent_ranks
                 SET consecutive_passes = 0, updated_at = ?1
                 WHERE room_id = ?2 AND agent_id = ?3",
                params![now, room_id, agent_id],
            ).map_err(|e| e.to_string())?;
        }
        // Read back the new streak value so the caller can branch on
        // milestone boundaries (every 2/4/6...) without a second query.
        let new_streak: u32 = conn
            .query_row(
                "SELECT consecutive_passes FROM groupchat_agent_ranks
                 WHERE room_id = ?1 AND agent_id = ?2",
                params![room_id, agent_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(new_streak)
    }

    fn set_leader(&self, room_id: &str, agent_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        // Update leader_id on room
        conn.execute("UPDATE groupchat_rooms SET leader_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![agent_id, now, room_id]).map_err(|e| e.to_string())?;
        // Demote any previous leader to member
        conn.execute(
            "UPDATE groupchat_room_agents SET role = 'member' WHERE room_id = ?1 AND role = 'leader'",
            params![room_id],
        ).map_err(|e| e.to_string())?;
        // Promote new leader
        conn.execute(
            "UPDATE groupchat_room_agents SET role = 'leader' WHERE room_id = ?1 AND agent_id = ?2",
            params![room_id, agent_id],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn update_settings(&self, room_id: &str, settings: &RoomSettings) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE groupchat_rooms SET settings_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![serde_json::to_string(settings).unwrap_or_default(), Utc::now().to_rfc3339(), room_id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn save_message(&self, msg: &ChatMessage) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO groupchat_messages
             (id, room_id, sender_id, sender_name, content, message_type, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![msg.id, msg.room_id, msg.sender_id, msg.sender_name, msg.content,
                msg.message_type.to_string(), msg.metadata, msg.created_at.to_rfc3339()],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn get_messages(&self, room_id: &str, limit: u32) -> Result<Vec<ChatMessage>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, room_id, sender_id, sender_name, content, message_type, metadata_json, created_at
             FROM groupchat_messages WHERE room_id = ?1 ORDER BY created_at ASC LIMIT ?2"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![room_id, limit], |row| {
            let msg_type_str: String = row.get(5)?;
            Ok(ChatMessage {
                id: row.get(0)?,
                room_id: row.get(1)?,
                sender_id: row.get(2)?,
                sender_name: row.get(3)?,
                content: row.get(4)?,
                message_type: msg_type_str.parse().unwrap_or(MessageType::Chat),
                metadata: row.get(6)?,
                created_at: Self::parse_dt(&row.get::<_, String>(7)?),
            })
        }).map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// M4 — paginated message loading. Returns up to `limit` messages
    /// older than the message identified by `before_id` (using created_at).
    fn get_messages_paginated(
        &self,
        room_id: &str,
        before_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ChatMessage>, String> {
        let conn = self.conn.lock().unwrap();

        let mut msgs: Vec<ChatMessage> = if let Some(before) = before_id {
            // Find the timestamp of the cursor message.
            let cursor_ts: Option<String> = conn
                .query_row(
                    "SELECT created_at FROM groupchat_messages WHERE id = ?1",
                    params![before],
                    |row| row.get(0),
                )
                .ok();
            match cursor_ts {
                Some(ts) => {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, room_id, sender_id, sender_name, content, message_type, metadata_json, created_at
                             FROM groupchat_messages
                             WHERE room_id = ?1 AND created_at < ?2
                             ORDER BY created_at DESC LIMIT ?3",
                        )
                        .map_err(|e| e.to_string())?;
                    let rows = stmt
                        .query_map(params![room_id, ts, limit], |row| parse_message_row(row))
                        .map_err(|e| e.to_string())?;
                    rows.filter_map(|r| r.ok()).collect()
                }
                None => return self.get_messages(room_id, limit),
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, room_id, sender_id, sender_name, content, message_type, metadata_json, created_at
                     FROM groupchat_messages
                     WHERE room_id = ?1
                     ORDER BY created_at DESC LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![room_id, limit], |row| parse_message_row(row))
                .map_err(|e| e.to_string())?;
            rows.filter_map(|r| r.ok()).collect()
        };
        // Reverse to chronological order (ASC)
        msgs.reverse();
        Ok(msgs)
    }

    fn save_task(&self, task: &GroupTask) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO groupchat_tasks
             (id, room_id, leader_agent_id, original_prompt, plan_json, status,
              final_report, approved_at, created_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task.id, task.room_id, task.leader_agent_id, task.original_prompt,
                task.plan_json, task.status.to_string(), task.final_report,
                task.approved_at.map(|d| d.to_rfc3339()),
                task.created_at.to_rfc3339(),
                task.completed_at.map(|d| d.to_rfc3339()),
            ],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn get_task(&self, id: &str) -> Result<Option<GroupTask>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, room_id, leader_agent_id, original_prompt, plan_json, status,
             final_report, approved_at, created_at, completed_at FROM groupchat_tasks WHERE id = ?1"
        ).map_err(|e| e.to_string())?;
        let result = stmt.query_row(params![id], |row| {
            Ok(GroupTask {
                id: row.get(0)?,
                room_id: row.get(1)?,
                leader_agent_id: row.get(2)?,
                original_prompt: row.get(3)?,
                plan_json: row.get(4)?,
                status: row.get::<_, String>(5)?.parse().unwrap_or(TaskStatus::Pending),
                final_report: row.get(6)?,
                approved_at: row.get::<_, Option<String>>(7)?.map(|s| Self::parse_dt(&s)),
                created_at: Self::parse_dt(&row.get::<_, String>(8)?),
                completed_at: row.get::<_, Option<String>>(9)?.map(|s| Self::parse_dt(&s)),
            })
        });
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn save_subtask(&self, subtask: &SubTask) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO groupchat_subtasks
             (id, task_id, description, assigned_agent_id, session_id, status,
              result, started_at, completed_at,
              review_passed, review_comment, reviewer_id, reviewed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                     ?10, ?11, ?12, ?13)",
            params![
                subtask.id, subtask.task_id, subtask.description,
                subtask.assigned_agent_id, subtask.session_id,
                subtask.status.to_string(), subtask.result,
                subtask.started_at.map(|d| d.to_rfc3339()),
                subtask.completed_at.map(|d| d.to_rfc3339()),
                subtask.review.as_ref().map(|r| if r.passed { 1 } else { 0 }),
                subtask.review.as_ref().map(|r| r.comment.clone()),
                subtask.review.as_ref().map(|r| r.reviewer_id.clone()),
                subtask.review.as_ref().map(|r| r.reviewed_at.to_rfc3339()),
            ],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn get_subtasks(&self, task_id: &str) -> Result<Vec<SubTask>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, description, assigned_agent_id, session_id, status,
             result, started_at, completed_at,
             review_passed, review_comment, reviewer_id, reviewed_at
             FROM groupchat_subtasks WHERE task_id = ?1"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![task_id], |row| {
            // Review columns: NULL when not yet reviewed. We treat
            // any row missing `review_passed` as "no review" rather
            // than as a real verdict, so a half-populated row never
            // fools the UI into thinking the agent was approved.
            let review = {
                let passed_opt: Option<i64> = row.get(9).ok();
                let comment_opt: Option<String> = row.get(10).ok();
                let reviewer_opt: Option<String> = row.get(11).ok();
                let reviewed_at_opt: Option<String> = row.get(12).ok();
                match (passed_opt, comment_opt, reviewer_opt, reviewed_at_opt) {
                    (Some(p), comment, Some(reviewer), Some(when)) => {
                        Some(hermix_gateway::groupchat::task_types::ReviewResult {
                            passed: p != 0,
                            comment: comment.unwrap_or_default(),
                            reviewer_id: reviewer,
                            reviewed_at: Self::parse_dt(&when),
                        })
                    }
                    _ => None,
                }
            };
            Ok(SubTask {
                id: row.get(0)?,
                task_id: row.get(1)?,
                description: row.get(2)?,
                assigned_agent_id: row.get(3)?,
                session_id: row.get(4)?,
                status: row.get::<_, String>(5)?.parse().unwrap_or(SubTaskStatus::Pending),
                result: row.get(6)?,
                started_at: row.get::<_, Option<String>>(7)?.map(|s| Self::parse_dt(&s)),
                completed_at: row.get::<_, Option<String>>(8)?.map(|s| Self::parse_dt(&s)),
                review,
            })
        }).map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn get_active_task_for_room(&self, room_id: &str) -> Result<Option<GroupTask>, String> {
        // The 5 in-flight statuses per the persistence-layer contract.
        // We can't use a binding here because rusqlite's `params!` macro
        // requires literal types; an IN-list with literal strings is
        // safe and self-contained.
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, room_id, leader_agent_id, original_prompt, plan_json, status,
             final_report, approved_at, created_at, completed_at FROM groupchat_tasks
             WHERE room_id = ?1
               AND status IN ('pending','decomposing','executing','aggregating','awaiting_approval')
             ORDER BY created_at DESC
             LIMIT 1"
        ).map_err(|e| e.to_string())?;
        let result = stmt.query_row(params![room_id], |row| {
            Ok(GroupTask {
                id: row.get(0)?,
                room_id: row.get(1)?,
                leader_agent_id: row.get(2)?,
                original_prompt: row.get(3)?,
                plan_json: row.get(4)?,
                status: row.get::<_, String>(5)?.parse().unwrap_or(TaskStatus::Pending),
                final_report: row.get(6)?,
                approved_at: row.get::<_, Option<String>>(7)?.map(|s| Self::parse_dt(&s)),
                created_at: Self::parse_dt(&row.get::<_, String>(8)?),
                completed_at: row.get::<_, Option<String>>(9)?.map(|s| Self::parse_dt(&s)),
            })
        });
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn list_tasks_for_room(&self, room_id: &str) -> Result<Vec<GroupTask>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, room_id, leader_agent_id, original_prompt, plan_json, status,
             final_report, approved_at, created_at, completed_at FROM groupchat_tasks
             WHERE room_id = ?1
             ORDER BY created_at DESC"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![room_id], |row| {
            Ok(GroupTask {
                id: row.get(0)?,
                room_id: row.get(1)?,
                leader_agent_id: row.get(2)?,
                original_prompt: row.get(3)?,
                plan_json: row.get(4)?,
                status: row.get::<_, String>(5)?.parse().unwrap_or(TaskStatus::Pending),
                final_report: row.get(6)?,
                approved_at: row.get::<_, Option<String>>(7)?.map(|s| Self::parse_dt(&s)),
                created_at: Self::parse_dt(&row.get::<_, String>(8)?),
                completed_at: row.get::<_, Option<String>>(9)?.map(|s| Self::parse_dt(&s)),
            })
        }).map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn update_subtask(&self, subtask: &SubTask) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE groupchat_subtasks
             SET description = ?1, assigned_agent_id = ?2, session_id = ?3,
                 status = ?4, result = ?5, started_at = ?6, completed_at = ?7
             WHERE id = ?8",
            params![
                subtask.description, subtask.assigned_agent_id, subtask.session_id,
                subtask.status.to_string(), subtask.result,
                subtask.started_at.map(|d| d.to_rfc3339()),
                subtask.completed_at.map(|d| d.to_rfc3339()),
                subtask.id,
            ],
        ).map_err(|e| e.to_string())?;
        if rows == 0 {
            Err(format!("subtask {} not found", subtask.id))
        } else {
            Ok(())
        }
    }

    fn set_announcement(
        &self,
        room_id: &str,
        text: Option<&str>,
        published_at: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        // We update only when the room exists — if no row matches,
        // surface a clear error so the API layer can return 404.
        let rows = conn.execute(
            "UPDATE groupchat_rooms
             SET announcement = ?1,
                 announcement_published_at = ?2,
                 updated_at = ?3
             WHERE id = ?4",
            params![
                text,
                published_at.map(|d| d.to_rfc3339()),
                Utc::now().to_rfc3339(),
                room_id,
            ],
        ).map_err(|e| e.to_string())?;
        if rows == 0 {
            Err(format!("room {} not found", room_id))
        } else {
            Ok(())
        }
    }
}

/// M4 — shared row parser for message queries.
fn parse_message_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ChatMessage> {
    let msg_type_str: String = row.get(5)?;
    Ok(ChatMessage {
        id: row.get(0)?,
        room_id: row.get(1)?,
        sender_id: row.get(2)?,
        sender_name: row.get(3)?,
        content: row.get(4)?,
        message_type: msg_type_str.parse().unwrap_or(MessageType::Chat),
        metadata: row.get::<_, Option<String>>(6)?,
        created_at: {
            let s: String = row.get(7)?;
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now())
        },
    })
}
