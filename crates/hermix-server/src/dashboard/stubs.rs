//! Stub endpoints for APIs the frontend expects but are not yet fully implemented.
//!
//! These return valid JSON so the SPA doesn't choke on HTML fallback responses.

use std::path::Path;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::HttpServerState;

// ── Dashboard plugins ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardPluginSettings {
    pub mcp_filesystem: bool,
    pub mcp_terminal: bool,
    pub mcp_database: bool,
    pub tool_code_exec: bool,
}

impl Default for DashboardPluginSettings {
    fn default() -> Self {
        Self {
            mcp_filesystem: true,
            mcp_terminal: true,
            mcp_database: false,
            tool_code_exec: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DashboardPluginState {
    plugins: DashboardPluginSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardPluginResponse {
    plugins: DashboardPluginSettings,
    persisted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashboardPluginUpdate {
    pub plugins: DashboardPluginSettings,
}

fn plugin_state_path(state: &HttpServerState) -> std::path::PathBuf {
    state.hermix_home.join("dashboard_plugins.json")
}

fn load_plugin_state(state: &HttpServerState) -> (DashboardPluginState, bool) {
    let path = plugin_state_path(state);
    let data = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return (DashboardPluginState::default(), false),
    };
    match serde_json::from_str::<DashboardPluginState>(&data) {
        Ok(v) => (v, true),
        Err(_) => (DashboardPluginState::default(), false),
    }
}

fn save_plugin_state(state: &HttpServerState, body: &DashboardPluginState) -> Result<(), String> {
    let path = plugin_state_path(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(body).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// GET /api/dashboard/plugins — return persisted plugin settings.
pub async fn get_plugins(State(state): State<HttpServerState>) -> Json<DashboardPluginResponse> {
    let (plugin_state, persisted) = load_plugin_state(&state);
    Json(DashboardPluginResponse {
        plugins: plugin_state.plugins,
        persisted,
    })
}

/// PUT /api/dashboard/plugins — persist plugin settings for remote mode.
pub async fn set_plugins(
    State(state): State<HttpServerState>,
    Json(body): Json<DashboardPluginUpdate>,
) -> Json<serde_json::Value> {
    let payload = DashboardPluginState {
        plugins: body.plugins,
    };
    match save_plugin_state(&state, &payload) {
        Ok(_) => Json(serde_json::json!({
            "ok": true,
            "plugins": payload.plugins,
            "persisted": true
        })),
        Err(err) => Json(serde_json::json!({
            "ok": false,
            "error": err
        })),
    }
}

/// `$HERMIX_HOME/dashboard-plugins` — each subdir may contain `manifest.json` (SPA contract).
fn plugin_bundles_root(hermix_home: &Path) -> std::path::PathBuf {
    hermix_home.join("dashboard-plugins")
}

/// Returns the official NousResearch hermes-agent plugin manifest seeds.
/// Used on first start to populate `$HERMIX_HOME/dashboard-plugins/` so the
/// front-end PluginsView has real content immediately, without depending on
/// the user cloning the official repo or running an installer.
/// This list mirrors `https://github.com/NousResearch/hermes-agent/tree/main/plugins`
/// as of . Each entry corresponds to one `manifest.json` written
/// to `<hermix_home>/dashboard-plugins/<id>/manifest.json` on first start.
/// `availability: "seeded"` marks these as "official catalog, not yet wired
/// into the local Rust backend" — distinct from `availability: "ready"`
/// plugins that have full backend implementations.
fn builtin_plugin_seeds() -> Vec<(&'static str, Value)> {
    vec![
        ("context_engine", json!({
            "name": "context_engine",
            "display_name": "上下文引擎",
            "category": "智能体",
            "description": "Long-context summarization, semantic chunking, and cross-session memory retrieval for the agent loop.",
            "icon": "Layers",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["context", "memory", "retrieval"]
        })),
        ("dashboard_auth", json!({
            "name": "dashboard_auth",
            "display_name": "Dashboard Auth (Nous Portal)",
            "category": "认证",
            "description": "Nous Portal OAuth login + per-user dashboard preferences. Required for the hosted version.",
            "icon": "KeyRound",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["auth", "oauth", "nous-portal"]
        })),
        ("disk_cleanup", json!({
            "name": "disk_cleanup",
            "display_name": "Disk Cleanup",
            "category": "系统",
            "description": "Find and remove old session artifacts, stale caches, and orphaned cron outputs across HERMIX_HOME.",
            "icon": "Trash2",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["disk", "cleanup", "maintenance"]
        })),
        ("google_meet", json!({
            "name": "google_meet",
            "display_name": "Google Meet",
            "category": "集成",
            "description": "Join and record Google Meet sessions, transcribe audio, and surface action items back into the chat.",
            "icon": "Video",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["meet", "transcription", "calendar"]
        })),
        ("hermix_achievements", json!({
            "name": "hermix_achievements",
            "display_name": "Hermes Achievements",
            "category": "游戏化",
            "description": "Track agent milestones and unlock decorative badges (e.g. 'first 1k tokens', 'cron master', 'voice pioneer').",
            "icon": "Trophy",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["achievements", "stats"]
        })),
        ("image_gen", json!({
            "name": "image_gen",
            "display_name": "Image Generation",
            "category": "媒体",
            "description": "Generate images via the configured provider (DALL·E, Flux, SDXL). Surfaces prompts and seeds for reproducibility.",
            "icon": "Image",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["image", "generation", "ai"]
        })),
        ("kanban", json!({
            "name": "kanban",
            "display_name": "看板",
            "category": "生产力",
            "description": "In-chat kanban board: create boards, drag cards across columns, integrate with the todo tool.",
            "icon": "Trello",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["kanban", "tasks", "productivity"]
        })),
        ("memory", json!({
            "name": "memory",
            "display_name": "记忆系统",
            "category": "智能体",
            "description": "Pluggable memory backend: embedded SQLite (memory.db) with time-window filtering and FTS5 recall.",
            "icon": "Brain",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["memory", "honcho", "mem0", "long-term"],
            "backends": ["embedded-sqlite"]
        })),
        ("model_providers", json!({
            "name": "model_providers",
            "display_name": "模型提供商",
            "category": "提供商",
            "description": "Configure upstream LLM providers: OpenAI, Anthropic, Nous Portal, OpenRouter, NVIDIA NIM, Xiaomi MiMo, z.ai/GLM, Kimi/Moonshot, MiniMax, Hugging Face, custom.",
            "icon": "Cpu",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["providers", "llm", "config"],
            "providers": ["openai", "anthropic", "nous_portal", "openrouter", "nvidia_nim", "xiaomi_mimo", "z_ai_glm", "kimi_moonshot", "MiniMax", "huggingface", "custom"]
        })),
        ("observability", json!({
            "name": "observability",
            "display_name": "可观测性",
            "category": "运维",
            "description": "Export every agent turn, tool call, and tool output to Langfuse for trace-level debugging and cost analysis.",
            "icon": "Activity",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["observability", "langfuse", "tracing"]
        })),
        ("platforms", json!({
            "name": "platforms",
            "display_name": "终端平台",
            "category": "系统",
            "description": "Choose where the agent runs commands: local, Docker, SSH, Singularity, Modal, Daytona. Serverless mode sleeps when idle.",
            "icon": "Server",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["sandbox", "docker", "ssh", "singularity", "modal", "daytona"],
            "backends": ["local", "docker", "ssh", "singularity", "modal", "daytona"]
        })),
        ("security_guidance", json!({
            "name": "security_guidance",
            "display_name": "安全指导",
            "category": "安全",
            "description": "Inject up-to-date system prompt guidance for the current threat model: prompt injection, secret leakage, network egress, etc.",
            "icon": "ShieldCheck",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["security", "guardrails", "prompts"]
        })),
        ("spotify", json!({
            "name": "spotify",
            "display_name": "音乐播放",
            "category": "集成",
            "description": "Control Spotify playback and search the catalogue via OAuth. Useful for ambient coding sessions.",
            "icon": "Music",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["spotify", "music", "integration"]
        })),
        ("teams_pipeline", json!({
            "name": "teams_pipeline",
            "display_name": "团队协作",
            "category": "集成",
            "description": "Run the agent inside a Microsoft Teams channel: receive tasks, return results, and ping owners when done.",
            "icon": "Users",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["teams", "microsoft", "pipeline"]
        })),
        ("video_gen", json!({
            "name": "video_gen",
            "display_name": "视频生成",
            "category": "媒体",
            "description": "Generate short videos via the configured provider (Sora, Runway, Veo, Kling). Streams progress in the chat.",
            "icon": "Film",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["video", "generation", "ai"],
            "toolset_alias": "video"
        })),
        ("web", json!({
            "name": "web",
            "display_name": "Web Search",
            "category": "检索",
            "description": "Web search, page fetch, and content extraction with a configurable search provider (Brave, Tavily, SerpAPI).",
            "icon": "Search",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "ready",
            "tags": ["search", "web", "retrieval"]
        })),
        ("example_dashboard", json!({
            "name": "example_dashboard",
            "display_name": "Example Dashboard (Tutorial)",
            "category": "教程",
            "description": "Sample SPA dashboard plugin showing the manifest contract, route table, and tool calls. Use as a starter template.",
            "icon": "BookOpen",
            "version": "0.1.0",
            "author": "NousResearch",
            "source": "official",
            "availability": "seeded",
            "tags": ["tutorial", "example", "starter"]
        })),
    ]
}

/// On first start, write the official plugin manifest seeds into
/// `$HERMIX_HOME/dashboard-plugins/<id>/manifest.json` so the front-end
/// PluginsView has a populated catalog right away. This is idempotent —
/// existing files (i.e. user-customized) are never overwritten.
/// We mark the seeds as `availability: "seeded"` or `"ready"` so the UI can
/// distinguish "official catalog, not yet wired into Rust backend" from
/// "fully usable today".
/// **T1-C **: previously marked dead — but it is called by
/// [`discover_plugin_manifests`]. Restored.
pub fn seed_official_plugin_manifests(hermix_home: &Path) -> std::io::Result<usize> {
    let root = plugin_bundles_root(hermix_home);
    std::fs::create_dir_all(&root)?;

    let mut written = 0;
    for (id, manifest) in builtin_plugin_seeds() {
        let mut manifest = manifest;
        if let Value::Object(ref mut m) = manifest {
            m.entry("id".to_string())
                .or_insert(Value::String(id.to_string()));
        }
        let dir = root.join(id);
        let path = dir.join("manifest.json");
        if path.exists() {
            continue; // user already customised this one
        }
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json)?;
        written += 1;
    }
    Ok(written)
}

/// Read the manifest files under `<hermix_home>/dashboard-plugins/<id>/manifest.json`,
/// merging in the `id` field if missing. Returns manifests sorted by name.
/// **T1-C **: previously marked dead — but it is called by
/// [`rescan_plugins`] and [`get_plugin_manifests`]. Restored.
pub fn discover_plugin_manifests(hermix_home: &Path) -> Vec<Value> {
    let dir = plugin_bundles_root(hermix_home);

    // First-call seeding: idempotent, never overwrites user files. A failure
    // here is non-fatal (e.g. read-only filesystem) — we just return whatever
    // was already on disk.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        if entries.flatten().next().is_none() {
            let _ = seed_official_plugin_manifests(hermix_home);
        }
    } else {
        // Directory doesn't exist yet — try to create it and seed.
        let _ = seed_official_plugin_manifests(hermix_home);
    }

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let plugin_id = entry.file_name().to_string_lossy().to_string();
        if let Value::Object(ref mut m) = v {
            m.entry("id".to_string())
                .or_insert(Value::String(plugin_id.clone()));
            m.entry("name".to_string())
                .or_insert(Value::String(plugin_id));
        }
        out.push(v);
    }
    out.sort_by(|a, b| {
        let sa = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let sb = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
        sa.cmp(sb)
    });
    out
}

/// Resolve the underlying toolset name for a plugin id, honouring the
/// `toolset_alias` field in the manifest. This lets `video_gen` map to
/// the `video` toolset (where the Rust implementation actually lives)
/// without renaming the catalog id the user sees in the UI.
/// Returns the id itself when no alias is set.
/// **T1-C **: previously marked dead — but it is called by
/// [`super::plugins_inspect`]. Restored.
pub fn resolve_toolset_for(id: &str) -> String {
    for (seed_id, manifest) in builtin_plugin_seeds() {
        if seed_id != id {
            continue;
        }
        if let Some(alias) = manifest.get("toolset_alias").and_then(|v| v.as_str()) {
            return alias.to_string();
        }
        return id.to_string();
    }
    id.to_string()
}

/// POST /api/dashboard/plugins/rescan — returns count of discovered local plugin manifests.
pub async fn rescan_plugins(State(state): State<HttpServerState>) -> Json<Value> {
    let count = discover_plugin_manifests(&state.hermix_home).len();
    Json(json!({ "ok": true, "count": count }))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_returns_alias_for_video_gen() {
        // video_gen manifest declares toolset_alias: "video" because
        // the Rust register_builtins.rs uses toolset="video".
        assert_eq!(resolve_toolset_for("video_gen"), "video");
    }

    #[test]
    fn resolve_returns_id_when_no_alias() {
        // kanban has no alias; should resolve to itself.
        assert_eq!(resolve_toolset_for("kanban"), "kanban");
    }

    #[test]
    fn resolve_returns_id_for_unknown_plugin() {
        // Unknown ids are passed through unchanged (the inspect endpoint
        // will then fail to find any tools and return availability="seeded").
        assert_eq!(resolve_toolset_for("totally_made_up"), "totally_made_up");
    }

    #[test]
    fn disk_cleanup_manifest_marked_ready() {
        for (id, manifest) in builtin_plugin_seeds() {
            if id == "disk_cleanup" {
                assert_eq!(
                    manifest.get("availability").and_then(|v| v.as_str()),
                    Some("ready"),
                    "disk_cleanup should be marked ready after Rust implementation"
                );
                return;
            }
        }
        panic!("disk_cleanup seed missing from builtin_plugin_seeds");
    }

    #[test]
    fn achievements_manifest_marked_ready() {
        for (id, manifest) in builtin_plugin_seeds() {
            if id == "hermix_achievements" {
                assert_eq!(
                    manifest.get("availability").and_then(|v| v.as_str()),
                    Some("ready"),
                    "hermix_achievements should be marked ready after Rust implementation"
                );
                return;
            }
        }
        panic!("hermix_achievements seed missing from builtin_plugin_seeds");
    }

    #[test]
    fn memory_marked_ready() {
        for (id, manifest) in builtin_plugin_seeds() {
            if id == "memory" {
                assert_eq!(
                    manifest.get("availability").and_then(|v| v.as_str()),
                    Some("ready"),
                    "{} should be ready (Rust tools are registered)",
                    id
                );
            }
        }
    }
}

/// GET /api/dashboard/plugin-manifests — JSON array from `dashboard-plugins/*/manifest.json`.
pub async fn get_plugin_manifests(State(state): State<HttpServerState>) -> Json<Vec<Value>> {
    Json(discover_plugin_manifests(&state.hermix_home))
}
