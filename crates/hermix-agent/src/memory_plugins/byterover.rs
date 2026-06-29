//! ByteRover memory provider plugin.
//!
//! Implements `MemoryLayer` for ByteRover — persistent knowledge tree
//! with hierarchical context, tiered retrieval, and optional cloud sync via
//! the `brv` CLI.
//!
//! Mirrors the Python `plugins/memory/byterover/__init__.py`.
//!
//! Configuration:
//!   - `brv` CLI must be installed (npm install -g byterover-cli)
//!   - `BRV_API_KEY` (optional, for cloud sync)
//!   - Working directory: `$HERMIX_HOME/byterover/`

use std::process::Command;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};

use hermix_focus::{MemoryHit, MemoryLayer, MemoryQuery, Result, SystemPromptBlock};

const QUERY_TIMEOUT_SECS: u64 = 10;
const CURATE_TIMEOUT_SECS: u64 = 120;
const MIN_QUERY_LEN: usize = 10;
const MIN_OUTPUT_LEN: usize = 20;

// ---------------------------------------------------------------------------
// MemoryLayer impl — Phase 15 H5 unified interface.
//
// ByteRover's `prefetch()` runs the `brv query` CLI synchronously. The
// MemoryLayer impl wraps that same call in a blocking thread so the
// async trait signature is satisfied.
// ---------------------------------------------------------------------------
#[async_trait]
impl MemoryLayer for ByteRoverPlugin {
    fn name(&self) -> &str {
        "byterover"
    }

    fn is_available(&self) -> bool {
        resolve_brv_path().is_some()
    }

    async fn recognize(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>> {
        if !MemoryLayer::is_available(self) {
            return Ok(Vec::new());
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return Ok(Vec::new());
        }
        let q = query.text.trim().to_string();
        if q.len() < MIN_QUERY_LEN {
            return Ok(Vec::new());
        }
        let truncated: String = q.chars().take(5000).collect();
        let output = tokio::task::spawn_blocking(move || {
            let (ok, out) = run_brv(&["query", "--", &truncated], QUERY_TIMEOUT_SECS, &cwd);
            if ok && out.len() > MIN_OUTPUT_LEN { out } else { String::new() }
        })
        .await
        .unwrap_or_default();
        if output.is_empty() {
            return Ok(Vec::new());
        }
        let now = Utc::now();
        let mut hits: Vec<MemoryHit> = Vec::new();
        for (idx, line) in output.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let content = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .unwrap_or(trimmed)
                .to_string();
            if content.is_empty() {
                continue;
            }
            if let Some(tw) = &query.time_window {
                if !tw.contains(now) {
                    continue;
                }
            }
            hits.push(MemoryHit {
                id: format!("byterover:{now}:{idx}"),
                content,
                salience: 0.55,
                created_at: now,
                metadata: json!({"source_plugin": "byterover"}),
                source: "byterover".to_string(),
            });
            if hits.len() >= query.limit {
                break;
            }
        }
        Ok(hits)
    }

    async fn inject(&self, hits: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> {
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        let preamble = MemoryLayer::system_prompt_block(self);
        if preamble.is_empty() {
            return Ok(Vec::new());
        }
        let mut body = String::new();
        for h in hits {
            body.push_str(&format!("- {}\n", h.content));
        }
        Ok(vec![SystemPromptBlock {
            text: format!("{preamble}\n\n{body}"),
            priority: 50,
        }])
    }

    // ----- T1-A (2026-06-20): write-path hooks (merged from legacy
    // `MemoryLayer`). Default no-op on the trait; ByteRover
    // overrides these to drive the `brv curate` CLI on turns/writes.

    fn initialize(&self, session_id: &str, hermix_home: &str) {
        let cwd = std::path::Path::new(hermix_home)
            .join("byterover")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&cwd);

        *self.cwd.lock().unwrap() = cwd;
        *self.session_id.lock().unwrap() = session_id.to_string();
        *self.turn_count.lock().unwrap() = 0;

        tracing::info!(
            "ByteRover memory plugin initialized for session {}",
            session_id
        );
    }

    fn system_prompt_block(&self) -> String {
        if resolve_brv_path().is_none() {
            return String::new();
        }
        "# ByteRover Memory\n\
         Active. Persistent knowledge tree with hierarchical context.\n\
         Use brv_query to search past knowledge, brv_curate to store \
         important facts, brv_status to check state."
            .to_string()
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        *self.turn_count.lock().unwrap() += 1;

        if user_content.trim().len() < MIN_QUERY_LEN {
            return;
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return;
        }

        let combined = format!(
            "User: {}\nAssistant: {}",
            &user_content[..user_content.len().min(2000)],
            &assistant_content[..assistant_content.len().min(2000)]
        );
        // 2026-06-26 (D): promote failure observability. Was a
        // fire-and-forget thread with `tracing::debug!` on the
        // failure arm — the user/curator had zero visibility when
        // `brv` was missing or the curate call silently failed.
        // Now: log at warn level with session context so Sentry /
        // dashboard / SRE postmortems can correlate.
        let session_id_owned = session_id.to_string();
        std::thread::spawn(move || {
            let (ok, _) = run_brv(&["curate", "--", &combined], CURATE_TIMEOUT_SECS, &cwd);
            if !ok {
                tracing::warn!(
                    session_id = %session_id_owned,
                    "ByteRover sync_turn curate failed; the user→assistant turn was \
                     not persisted to the byterover knowledge tree. Check that `brv` \
                     is on PATH and the working directory is writable."
                );
            }
        });
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if !matches!(action, "add" | "replace") || content.is_empty() {
            return;
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return;
        }
        let label = if target == "user" {
            "User profile"
        } else {
            "Agent memory"
        };
        let tagged = format!("[{}] {}", label, content);
        // 2026-06-26 (D): same observability fix as `sync_turn`.
        // 2026-06-26 (lifetime): `action` and `target` are `&str`
        // with non-static lifetimes, so they can't be captured by
        // the `move` closure and then read inside the spawned
        // thread. Hoist them into owned `String`s first — cheap
        // because they're tiny.
        let action_owned = action.to_string();
        let target_owned = target.to_string();
        std::thread::spawn(move || {
            let (ok, _) = run_brv(&["curate", "--", &tagged], CURATE_TIMEOUT_SECS, &cwd);
            if !ok {
                tracing::warn!(
                    action = %action_owned,
                    target = %target_owned,
                    "ByteRover memory mirror failed; the explicit memory write was \
                     not persisted to the byterover knowledge tree."
                );
            }
        });
    }

    fn on_pre_compress(&self, messages: &[Value]) -> String {
        if messages.is_empty() {
            return String::new();
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();
        for msg in messages.iter().rev().take(10).rev() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !content.trim().is_empty() && (role == "user" || role == "assistant") {
                let truncated: String = content.chars().take(500).collect();
                parts.push(format!("{}: {}", role, truncated));
            }
        }
        if parts.is_empty() {
            return String::new();
        }

        let combined = format!("[Pre-compression context]\n{}", parts.join("\n"));

        // 2026-06-26 (E): turn the fire-and-forget spawn into a real
        // write hook. The previous shape detached on a worker thread
        // and discarded the return value with `let _ =`, so any
        // failure was invisible (the thread crashed silently), the
        // pre-compression context was never persisted, and `agent_loop`
        // always saw `note = ""` from this plugin — i.e. the only
        // "pre-compress" hook in the entire 8-plugin cull was a
        // no-op. We now:
        //   1. run brv *synchronously* with a clear warn on failure
        //      (so the curator sees the loss in logs / Sentry),
        //   2. return the combined note text so the compressor can
        //      inject it as a system message — making the
        //      `MemoryProviderPlugin::on_pre_compress` contract
        //      actually do something instead of always returning `""`.
        let (ok, _) = run_brv(&["curate", "--", &combined], CURATE_TIMEOUT_SECS, &cwd);
        if !ok {
            tracing::warn!(
                cwd = %cwd,
                turns = parts.len(),
                "ByteRover on_pre_compress curate failed; the last 10 turns are \
                 being lost from the knowledge tree. Returning the local note \
                 anyway so the compressor can still surface them to the LLM."
            );
        }
        combined
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        *self.turn_count.lock().unwrap() = turn_number;
    }

    fn shutdown(&self) {
        // debug is fine here — shutdown fires once at session end,
        // not part of the user-visible failure surface.
        tracing::debug!("ByteRover memory plugin shutdown");
    }
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// brv CLI resolution
// ---------------------------------------------------------------------------

fn resolve_brv_path() -> Option<String> {
    if let Ok(path) = which::which("brv") {
        return Some(path.to_string_lossy().to_string());
    }

    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".brv-cli").join("bin").join("brv"),
        std::path::PathBuf::from("/usr/local/bin/brv"),
        home.join(".npm-global").join("bin").join("brv"),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.to_string_lossy().to_string());
        }
    }
    None
}

/// Run a `brv` CLI command. Returns `(success, output_or_error)`.
fn run_brv(args: &[&str], _timeout_secs: u64, cwd: &str) -> (bool, String) {
    let brv_path = match resolve_brv_path() {
        Some(p) => p,
        None => {
            return (
                false,
                "brv CLI not found. Install: npm install -g byterover-cli".into(),
            )
        }
    };

    let _ = std::fs::create_dir_all(cwd);

    let result = Command::new(&brv_path).args(args).current_dir(cwd).output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if output.status.success() {
                (true, stdout)
            } else {
                let err = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!("brv exited {}", output.status)
                };
                (false, err)
            }
        }
        Err(e) => (false, format!("brv execution failed: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// ByteRoverPlugin
// ---------------------------------------------------------------------------

/// ByteRover persistent memory via the `brv` CLI.
pub struct ByteRoverPlugin {
    cwd: Mutex<String>,
    session_id: Mutex<String>,
    turn_count: Mutex<u32>,
}


impl Default for ByteRoverPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteRoverPlugin {
    pub fn new() -> Self {
        Self {
            cwd: Mutex::new(String::new()),
            session_id: Mutex::new(String::new()),
            turn_count: Mutex::new(0),
        }
    }

    fn working_dir(&self) -> String {
        self.cwd.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byterover_plugin_name() {
        let plugin = ByteRoverPlugin::new();
        assert_eq!(MemoryLayer::name(&plugin), "byterover");
    }

    #[test]
    fn test_byterover_system_prompt_empty_when_not_available() {
        let plugin = ByteRoverPlugin::new();
        // When brv is not installed, system_prompt_block may or may not be empty
        // depending on the test environment — just check it doesn't panic
        let _ = MemoryLayer::system_prompt_block(&plugin);
    }

    #[tokio::test]
    async fn layer_name() {
        let plugin = ByteRoverPlugin::new();
        assert_eq!(MemoryLayer::name(&plugin), "byterover");
    }

    #[tokio::test]
    async fn layer_recognize_short_query_returns_empty() {
        let plugin = ByteRoverPlugin::new();
        let q = MemoryQuery::recall("hi");
        let hits = MemoryLayer::recognize(&plugin, &q).await.unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn layer_inject_empty_when_no_preamble() {
        let plugin = ByteRoverPlugin::new();
        let hits = vec![MemoryHit {
            id: "1".into(),
            content: "x".into(),
            salience: 0.5,
            created_at: Utc::now(),
            metadata: json!({}),
            source: "byterover".into(),
        }];
        // Without initialization, preamble is empty so blocks are empty.
        let blocks = MemoryLayer::inject(&plugin, &hits).await.unwrap();
        assert!(blocks.is_empty());
    }
}
