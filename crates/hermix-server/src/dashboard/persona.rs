//! Persona management endpoints — SOUL / system prompt / personalities / memory.
//!
//! The agent's behavior is shaped by a handful of files under `$HERMIX_HOME`
//! (defaults to `~/.hermix`):
//!
//! | Path                            | Loaded by                                 |
//! |---------------------------------|-------------------------------------------|
//! | `SOUL.md`                       | `load_soul_md()` in `hermes-agent`        |
//! | `personalities/<name>.md`       | `resolve_personality(name, _)`            |
//! | `memories/MEMORY.md`            | `load_builtin_memory_snapshot(_)`         |
//! | `memories/USER.md`              | `load_builtin_memory_snapshot(_)`         |
//! | `persona_overrides.json`        | `system_prompt` field read at build time  |
//!
//! These endpoints expose the read/write lifecycle so the desktop UI can
//! let users manage them without dropping to the filesystem. They are
//! intentionally file-backed (no DB), matching the same source-of-truth
//! the agent reads at session start.
//!
//! The `persona_overrides.json` schema is intentionally minimal — only
//! fields the UI knows how to edit. Future fields can be added without
//! breaking older clients (forward-compat).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};

use crate::{HttpError, HttpServerState};

// ── helpers ────────────────────────────────────────────────────────

fn persona_dir(state: &HttpServerState) -> PathBuf {
    state.hermix_home.clone()
}

fn memory_dir(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("memories")
}

fn personalities_dir(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("personalities")
}

fn override_path(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("persona_overrides.json")
}

fn ensure_dir(dir: &FsPath) -> Result<(), HttpError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir).map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("无法创建目录 {}: {e}", dir.display()),
        })?;
    }
    Ok(())
}

fn read_text_file(path: &FsPath) -> Result<Option<String>, HttpError> {
    if !path.exists() {
        return Ok(None);
    }
    let s = std::fs::read_to_string(path).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("读取 {} 失败: {e}", path.display()),
    })?;
    Ok(Some(s))
}

fn write_text_file(path: &FsPath, content: &str) -> Result<(), HttpError> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    std::fs::write(path, content).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("写入 {} 失败: {e}", path.display()),
    })?;
    Ok(())
}

// ── generic text-file resource ────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TextFileResponse {
    /// Absolute path on disk — useful for the UI to surface "已写入 …" hints.
    pub path: String,
    /// File contents. `null` when the file does not yet exist.
    pub content: Option<String>,
    /// Last-modified epoch millis. `null` if the file does not exist.
    pub updated_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TextFileUpdateRequest {
    pub content: String,
}

fn mtime_ms(path: &FsPath) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

fn build_text_response(path: PathBuf) -> Result<TextFileResponse, HttpError> {
    let content = read_text_file(&path)?;
    let updated_at = if content.is_some() { mtime_ms(&path) } else { None };
    Ok(TextFileResponse {
        path: path.to_string_lossy().into_owned(),
        content,
        updated_at,
    })
}

fn handle_text_get(
    path: PathBuf,
) -> Result<Json<TextFileResponse>, HttpError> {
    Ok(Json(build_text_response(path)?))
}

fn handle_text_put(
    path: PathBuf,
    Json(req): Json<TextFileUpdateRequest>,
) -> Result<Json<TextFileResponse>, HttpError> {
    write_text_file(&path, &req.content)?;
    Ok(Json(build_text_response(path)?))
}

// ── SOUL ──────────────────────────────────────────────────────────

pub async fn get_soul(
    State(state): State<HttpServerState>,
) -> Result<Json<TextFileResponse>, HttpError> {
    handle_text_get(persona_dir(&state).join("SOUL.md"))
}

pub async fn put_soul(
    State(state): State<HttpServerState>,
    Json(req): Json<TextFileUpdateRequest>,
) -> Result<Json<TextFileResponse>, HttpError> {
    handle_text_put(persona_dir(&state).join("SOUL.md"), Json(req))
}

// ── MEMORY ────────────────────────────────────────────────────────

pub async fn get_memory(
    State(state): State<HttpServerState>,
) -> Result<Json<TextFileResponse>, HttpError> {
    handle_text_get(memory_dir(&state).join("MEMORY.md"))
}

pub async fn put_memory(
    State(state): State<HttpServerState>,
    Json(req): Json<TextFileUpdateRequest>,
) -> Result<Json<TextFileResponse>, HttpError> {
    handle_text_put(memory_dir(&state).join("MEMORY.md"), Json(req))
}

// ── USER (user profile = preferences) ─────────────────────────────

pub async fn get_user_profile(
    State(state): State<HttpServerState>,
) -> Result<Json<TextFileResponse>, HttpError> {
    handle_text_get(memory_dir(&state).join("USER.md"))
}

pub async fn put_user_profile(
    State(state): State<HttpServerState>,
    Json(req): Json<TextFileUpdateRequest>,
) -> Result<Json<TextFileResponse>, HttpError> {
    handle_text_put(memory_dir(&state).join("USER.md"), Json(req))
}

// ── PERSONALITIES (named persona files) ───────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PersonalitySummary {
    /// Slug (filename without `.md`).
    pub name: String,
    /// First non-blank line — a friendly one-line summary if present.
    pub summary: Option<String>,
    /// File size in bytes.
    pub size: u64,
    /// Last-modified epoch millis.
    pub updated_at: Option<i64>,
    /// `true` if this is a built-in persona (`coder` / `writer` / `analyst`).
    /// Built-ins live in code, not in the user's `personalities/` directory.
    pub is_builtin: bool,
}

pub async fn list_personalities(
    State(state): State<HttpServerState>,
) -> Result<Json<Vec<PersonalitySummary>>, HttpError> {
    let mut out: Vec<PersonalitySummary> = Vec::new();

    // 1) Built-in personas — hard-coded; surfaced as read-only.
    for (name, body) in BUILTIN_PERSONAS {
        out.push(PersonalitySummary {
            name: name.to_string(),
            summary: body.lines().find(|l| !l.trim().is_empty()).map(|s| s.trim().to_string()),
            size: body.len() as u64,
            updated_at: None,
            is_builtin: true,
        });
    }

    // 2) User-defined personas on disk.
    let dir = personalities_dir(&state);
    if dir.exists() {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(it) => it.flatten().map(|e| e.path()).collect(),
            Err(e) => {
                return Err(HttpError {
                    status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    message: format!("读取 {} 失败: {e}", dir.display()),
                });
            }
        };
        entries.sort();

        for path in entries {
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext != "md" && ext != "markdown" {
                continue;
            }
            let name = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            let size = raw.len() as u64;
            let summary = raw.lines().find(|l| !l.trim().is_empty()).map(|s| s.trim().to_string());
            out.push(PersonalitySummary {
                name,
                summary,
                size,
                updated_at: mtime_ms(&path),
                is_builtin: false,
            });
        }
    }

    Ok(Json(out))
}

pub async fn get_personality(
    State(state): State<HttpServerState>,
    Path(name): Path<String>,
) -> Result<Json<TextFileResponse>, HttpError> {
    let slug = name.trim().to_ascii_lowercase();
    if slug.is_empty() || !is_safe_slug(&slug) {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "人设名只能包含字母、数字、下划线、连字符".into(),
        });
    }

    // 1) Built-in wins if it shadows the same name on disk.
    if let Some(body) = builtin_personality_body(&slug) {
        return Ok(Json(TextFileResponse {
            path: format!("<builtin:{}>", slug),
            content: Some(body.to_string()),
            updated_at: None,
        }));
    }

    handle_text_get(personalities_dir(&state).join(format!("{slug}.md")))
}

pub async fn put_personality(
    State(state): State<HttpServerState>,
    Path(name): Path<String>,
    Json(req): Json<TextFileUpdateRequest>,
) -> Result<Json<TextFileResponse>, HttpError> {
    let slug = name.trim().to_ascii_lowercase();
    if slug.is_empty() || !is_safe_slug(&slug) {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "人设名只能包含字母、数字、下划线、连字符".into(),
        });
    }
    if builtin_personality_body(&slug).is_some() {
        return Err(HttpError {
            status: axum::http::StatusCode::FORBIDDEN,
            message: format!("「{slug}」是内置人设,不可编辑 — 换个名字保存为你的自定义版本"),
        });
    }
    handle_text_put(personalities_dir(&state).join(format!("{slug}.md")), Json(req))
}

pub async fn delete_personality(
    State(state): State<HttpServerState>,
    Path(name): Path<String>,
) -> Result<Json<DeletePersonalityResponse>, HttpError> {
    let slug = name.trim().to_ascii_lowercase();
    if slug.is_empty() || !is_safe_slug(&slug) {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "人设名只能包含字母、数字、下划线、连字符".into(),
        });
    }
    if builtin_personality_body(&slug).is_some() {
        return Err(HttpError {
            status: axum::http::StatusCode::FORBIDDEN,
            message: format!("「{slug}」是内置人设,不可删除"),
        });
    }
    let path = personalities_dir(&state).join(format!("{slug}.md"));
    if !path.exists() {
        return Err(HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("人设「{slug}」不存在"),
        });
    }
    std::fs::remove_file(&path).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("删除 {} 失败: {e}", path.display()),
    })?;
    Ok(Json(DeletePersonalityResponse {
        deleted: slug,
    }))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeletePersonalityResponse {
    pub deleted: String,
}

fn is_safe_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ── BUILT-IN PERSONALITY MIRROR ───────────────────────────────────
//
// Mirrors the `BUILTIN_PERSONALITY_*` constants in
// `crates/hermes-agent/src/context.rs`. The agent's runtime reads
// those constants directly via `resolve_personality`; we duplicate
// them here so the dashboard can render the catalog and content.
// When you edit one, edit both.

const BUILTIN_PERSONA_CODER: &str = "You are operating in the `coder` persona.\n\
Focus on correctness, idiomatic style, and minimal surface area.\n\
Prefer simple, debuggable solutions over clever ones; explain trade-offs only when the choice is non-obvious.\n\
Format code with the project's existing style; never introduce new dependencies without flagging the cost.\n\
When you need to verify behavior, run the code or call the relevant tool — don't guess from memory.";

const BUILTIN_PERSONA_WRITER: &str = "You are operating in the `writer` persona.\n\
Prioritize clarity, structure, and audience-aware phrasing.\n\
Use concise sections, strong topic sentences, and remove unnecessary repetition.\n\
When asked to revise, preserve meaning while improving readability and flow.";

const BUILTIN_PERSONA_ANALYST: &str = "You are operating in the `analyst` persona.\n\
Prioritize evidence, explicit reasoning, and clear uncertainty bounds.\n\
Break complex problems into assumptions, observations, and conclusions.\n\
When data is missing, state what is unknown and propose the next highest-value check.";

const BUILTIN_PERSONAS: &[(&str, &str)] = &[
    ("coder", BUILTIN_PERSONA_CODER),
    ("writer", BUILTIN_PERSONA_WRITER),
    ("analyst", BUILTIN_PERSONA_ANALYST),
];

fn builtin_personality_body(slug: &str) -> Option<&'static str> {
    BUILTIN_PERSONAS
        .iter()
        .find(|(name, _)| *name == slug)
        .map(|(_, body)| *body)
}

// ── PERSONA OVERRIDES (system_prompt + preferences) ──────────────
//
// Stored at `$HERMIX_HOME/persona_overrides.json`. Currently the agent
// loop reads `system_prompt` from `HermesConfig` rather than this
// override file — that wiring is intentionally left as a follow-up.
// We expose the API now so the UI can land and the agent can read it
// in a focused commit later.

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PersonaOverrides {
    /// Full system-prompt override. When set, the agent loop uses this
    /// instead of any persona-derived text. Empty string clears it.
    #[serde(default)]
    pub system_prompt: String,
    /// Free-form key-value preferences — surface hints that the agent
    /// loop can read into the system prompt. Kept open-ended so the
    /// UI can grow the schema without breaking older clients.
    #[serde(default)]
    pub preferences: serde_json::Value,
}

pub async fn get_overrides(
    State(state): State<HttpServerState>,
) -> Result<Json<PersonaOverrides>, HttpError> {
    let path = override_path(&state);
    if !path.exists() {
        return Ok(Json(PersonaOverrides::default()));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("读取 {} 失败: {e}", path.display()),
    })?;
    let parsed: PersonaOverrides = serde_json::from_str(&raw).unwrap_or_default();
    Ok(Json(parsed))
}

pub async fn put_overrides(
    State(state): State<HttpServerState>,
    Json(req): Json<PersonaOverrides>,
) -> Result<Json<PersonaOverrides>, HttpError> {
    let path = override_path(&state);
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let serialized = serde_json::to_string_pretty(&req).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("序列化失败: {e}"),
    })?;
    std::fs::write(&path, serialized).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("写入 {} 失败: {e}", path.display()),
    })?;
    Ok(Json(req))
}

// ── tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_slug() {
        assert!(is_safe_slug("coder"));
        assert!(is_safe_slug("code-reviewer"));
        assert!(is_safe_slug("finance_2026"));
        assert!(is_safe_slug("a"));
        assert!(!is_safe_slug(""));
        assert!(!is_safe_slug("../etc"));
        assert!(!is_safe_slug("with space"));
        assert!(!is_safe_slug("with/slash"));
        assert!(!is_safe_slug("with.dot"));
    }

    #[test]
    fn test_builtin_personality_lookup() {
        assert!(builtin_personality_body("coder").is_some());
        assert!(builtin_personality_body("writer").is_some());
        assert!(builtin_personality_body("analyst").is_some());
        assert!(builtin_personality_body("unknown").is_none());
    }

    #[test]
    fn test_persona_overrides_default_serde() {
        let json = "{}";
        let parsed: PersonaOverrides = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.system_prompt, "");
        assert_eq!(parsed.preferences, serde_json::Value::Null);
    }

    #[test]
    fn test_persona_overrides_roundtrip_with_preferences() {
        let original = PersonaOverrides {
            system_prompt: "You are a careful assistant.".into(),
            preferences: serde_json::json!({
                "default_language": "zh-CN",
                "verbosity": "concise",
            }),
        };
        let serialized = serde_json::to_string(&original).unwrap();
        let parsed: PersonaOverrides = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.system_prompt, original.system_prompt);
        assert_eq!(parsed.preferences["default_language"], "zh-CN");
        assert_eq!(parsed.preferences["verbosity"], "concise");
    }

    #[test]
    fn test_text_file_response_serde() {
        let resp = TextFileResponse {
            path: "/tmp/SOUL.md".into(),
            content: Some("# Soul".into()),
            updated_at: Some(1717_000_000_000),
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let parsed: TextFileResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.path, "/tmp/SOUL.md");
        assert_eq!(parsed.content.as_deref(), Some("# Soul"));
        assert_eq!(parsed.updated_at, Some(1717_000_000_000));
    }
}
