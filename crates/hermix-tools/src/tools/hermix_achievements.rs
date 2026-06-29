//! Hermes Achievements — track agent milestones and unlock decorative badges.
//!
//! Persists state to `<HERMIX_HOME>/achievements.json`. The agent calls
//! `check` with a stats object; the backend evaluates rules and returns any
//! newly unlocked achievements plus the full list.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// Built-in badge catalog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Badge {
    pub id: String,
    pub title: String,
    pub description: String,
    pub emoji: String,
    /// Rule description (for UI display)
    pub rule: String,
}

pub fn builtin_badges() -> Vec<Badge> {
    vec![
        Badge {
            id: "first_steps".into(),
            title: "First Steps".into(),
            description: "Sent your first message to the agent.".into(),
            emoji: "🌱".into(),
            rule: "stats.messages >= 1".into(),
        },
        Badge {
            id: "first_1k_tokens".into(),
            title: "Wordy".into(),
            description: "Crossed 1,000 total tokens in a single session.".into(),
            emoji: "📝".into(),
            rule: "stats.session_tokens >= 1000".into(),
        },
        Badge {
            id: "tool_apprentice".into(),
            title: "Tool Apprentice".into(),
            description: "Invoked 10 tools.".into(),
            emoji: "🔧".into(),
            rule: "stats.tool_calls >= 10".into(),
        },
        Badge {
            id: "tool_master".into(),
            title: "Tool Master".into(),
            description: "Invoked 100 tools.".into(),
            emoji: "🛠️".into(),
            rule: "stats.tool_calls >= 100".into(),
        },
        Badge {
            id: "cron_master".into(),
            title: "Cron Master".into(),
            description: "Created 5 cron jobs.".into(),
            emoji: "⏰".into(),
            rule: "stats.cron_jobs >= 5".into(),
        },
        Badge {
            id: "voice_pioneer".into(),
            title: "Voice Pioneer".into(),
            description: "Used voice mode or TTS at least once.".into(),
            emoji: "🎙️".into(),
            rule: "stats.voice_seconds >= 1".into(),
        },
        Badge {
            id: "image_artist".into(),
            title: "Image Artist".into(),
            description: "Generated 3 images.".into(),
            emoji: "🎨".into(),
            rule: "stats.images_generated >= 3".into(),
        },
        Badge {
            id: "marathoner".into(),
            title: "Marathoner".into(),
            description: "Held a single session for 50+ turns.".into(),
            emoji: "🏃".into(),
            rule: "stats.session_turns >= 50".into(),
        },
    ]
}

fn evaluate_rule(rule: &str, stats: &Value) -> bool {
    // Mini DSL: `stats.<field> >= <int>`
    let parts: Vec<&str> = rule.split_whitespace().collect();
    if parts.len() != 3 || !parts[0].starts_with("stats.") || parts[1] != ">=" {
        return false;
    }
    let field = parts[0].trim_start_matches("stats.");
    if field.is_empty() {
        return false;
    }
    let threshold: f64 = match parts[2].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let actual = stats.get(field).and_then(|v| v.as_f64()).unwrap_or(0.0);
    actual >= threshold
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AchievementState {
    /// Badges that have already been unlocked, keyed by badge id.
    pub unlocked: IndexMap<String, UnlockedAt>,
    /// Cumulative stats used to evaluate rules. Reset only via `reset`.
    #[serde(default)]
    pub stats: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnlockedAt {
    pub at: u64,
    pub session_id: Option<String>,
}

fn state_path() -> PathBuf {
    // Delegate to the canonical resolver so HERMIX_HOME,
    // HERMIX_PORTABLE_DATA_DIR, and the `~/.hermix` fallback all
    // share the same lookup logic as the rest of the app.
    let home = hermix_config::paths::hermix_home();
    let _ = std::fs::create_dir_all(&home);
    home.join("achievements.json")
}

fn load_state() -> AchievementState {
    let p = state_path();
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &AchievementState) {
    let p = state_path();
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&p, json);
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub struct HermixAchievementsHandler {
    state: Mutex<AchievementState>,
}

impl Default for HermixAchievementsHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl HermixAchievementsHandler {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(load_state()),
        }
    }

    fn list(&self) -> Result<String, ToolError> {
        let state = self.state.lock().unwrap();
        let badges = builtin_badges();
        let unlocked = state
            .unlocked
            .iter()
            .map(|(id, ts)| {
                let badge = badges.iter().find(|b| &b.id == id);
                json!({
                    "id": id,
                    "title": badge.map(|b| b.title.as_str()).unwrap_or(""),
                    "emoji": badge.map(|b| b.emoji.as_str()).unwrap_or("🏅"),
                    "description": badge.map(|b| b.description.as_str()).unwrap_or(""),
                    "unlocked_at": ts.at,
                    "session_id": ts.session_id,
                })
            })
            .collect::<Vec<_>>();
        let total = badges.len();
        let report = json!({
            "unlocked_count": unlocked.len(),
            "total_count": total,
            "completion": if total > 0 { unlocked.len() as f64 / total as f64 } else { 0.0 },
            "unlocked": unlocked,
            "all_badges": badges,
        });
        serde_json::to_string_pretty(&report)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    fn check(&self, params: &Value) -> Result<String, ToolError> {
        let incoming_stats = params.get("stats").cloned().unwrap_or(json!({}));
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut state = self.state.lock().unwrap();

        // Merge stats (take max of numeric fields) so cumulative progress
        // is preserved across sessions.
        if let Some(obj) = incoming_stats.as_object() {
            let merged = state.stats.as_object().cloned().unwrap_or_default();
            let mut next = merged;
            for (k, v) in obj {
                let new_num = v.as_f64().unwrap_or(0.0);
                let old_num = next.get(k.as_str()).and_then(|x| x.as_f64()).unwrap_or(0.0);
                next.insert(k.clone(), json!(new_num.max(old_num)));
            }
            state.stats = Value::Object(next);
        }

        let badges = builtin_badges();
        let mut newly_unlocked: Vec<Value> = vec![];
        for badge in &badges {
            if state.unlocked.contains_key(&badge.id) {
                continue;
            }
            if evaluate_rule(&badge.rule, &state.stats) {
                state.unlocked.insert(
                    badge.id.clone(),
                    UnlockedAt {
                        at: now_unix(),
                        session_id: session_id.clone(),
                    },
                );
                newly_unlocked.push(json!({
                    "id": badge.id,
                    "title": badge.title,
                    "emoji": badge.emoji,
                    "description": badge.description,
                }));
            }
        }
        save_state(&state);

        let report = json!({
            "newly_unlocked": newly_unlocked,
            "total_unlocked": state.unlocked.len(),
            "total_badges": badges.len(),
            "stats": state.stats,
        });
        serde_json::to_string_pretty(&report)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    fn reset(&self) -> Result<String, ToolError> {
        let mut state = self.state.lock().unwrap();
        state.unlocked.clear();
        state.stats = json!({});
        save_state(&state);
        Ok(json!({ "ok": true, "reset": true }).to_string())
    }
}

#[async_trait]
impl ToolHandler for HermixAchievementsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;
        match action {
            "list" => self.list(),
            "check" => self.check(&params),
            "reset" => self.reset(),
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'list', 'check', or 'reset'.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action: list (show unlocked + catalog), check (evaluate stats and unlock new), reset (clear all)",
                "enum": ["list", "check", "reset"]
            }),
        );
        props.insert(
            "stats".into(),
            json!({
                "type": "object",
                "description": "Cumulative stats for evaluation. Numeric fields are merged with max-of-merge. Example: {\"messages\": 1, \"tool_calls\": 10, \"session_tokens\": 1500, \"cron_jobs\": 0, \"voice_seconds\": 0, \"images_generated\": 0, \"session_turns\": 5}",
            }),
        );
        props.insert(
            "session_id".into(),
            json!({
                "type": "string",
                "description": "Optional session id to record alongside the unlock timestamp."
            }),
        );
        tool_schema(
            "hermix_achievements",
            "Track agent milestones (first message, 1k tokens, tool-master, cron-master, voice-pioneer, etc.) and unlock decorative badges. State is persisted to HERMIX_HOME/achievements.json.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// `HERMIX_HOME` is process-global env state. Tests in this
    /// module (and others in the same crate) read/write it; without
    /// a lock, parallel tests race and assert on the wrong state.
    static HERMIX_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn set_home(tmp: &TempDir) {
        std::env::set_var("HERMIX_HOME", tmp.path());
    }

    #[test]
    fn rule_evaluates_threshold() {
        assert!(evaluate_rule(
            "stats.tool_calls >= 10",
            &json!({ "tool_calls": 10 })
        ));
        assert!(evaluate_rule(
            "stats.tool_calls >= 10",
            &json!({ "tool_calls": 15 })
        ));
        assert!(!evaluate_rule(
            "stats.tool_calls >= 10",
            &json!({ "tool_calls": 9 })
        ));
        assert!(!evaluate_rule("stats.tool_calls >= 10", &json!({})));
    }

    #[test]
    fn builtin_badges_have_unique_ids() {
        let badges = builtin_badges();
        let mut ids: Vec<_> = badges.iter().map(|b| b.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), badges.len(), "badge ids must be unique");
    }

    #[test]
    fn check_unlocks_first_1k_tokens() {
        let _g = HERMIX_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        set_home(&tmp);
        let handler = HermixAchievementsHandler::new();
        let out = handler
            .check(&json!({
                "action": "check",
                "stats": { "session_tokens": 1500, "tool_calls": 0 }
            }))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let unlocked = v["newly_unlocked"].as_array().unwrap();
        assert!(
            unlocked.iter().any(|b| b["id"] == "first_1k_tokens"),
            "expected first_1k_tokens to be unlocked, got {:?}",
            unlocked
        );
    }

    #[test]
    fn check_does_not_double_unlock() {
        let _g = HERMIX_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        set_home(&tmp);
        let handler = HermixAchievementsHandler::new();
        handler
            .check(&json!({ "action": "check", "stats": { "session_tokens": 5000 } }))
            .unwrap();
        // second check with even higher stats should not return newly_unlocked
        let out = handler
            .check(&json!({ "action": "check", "stats": { "session_tokens": 9999 } }))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["newly_unlocked"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn reset_clears_state() {
        let _g = HERMIX_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        set_home(&tmp);
        let handler = HermixAchievementsHandler::new();
        handler
            .check(&json!({ "action": "check", "stats": { "session_tokens": 5000 } }))
            .unwrap();
        handler.reset().unwrap();
        // fresh handler re-reads from disk
        let handler2 = HermixAchievementsHandler::new();
        let out = handler2.list().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["unlocked_count"].as_u64().unwrap(), 0);
    }
}
