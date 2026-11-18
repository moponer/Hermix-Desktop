//! Vision plugin toggle — the eye switch above the composer.
//!
//! ## What this switches between
//!
//! Hermix has two independent ways to look at an image:
//!
//! - **ON (plugin path)** — the free vision chain: the image is sent to the
//!   bundled vision backend, which returns a text description. Works with a
//!   text-only chat model because the model never receives pixels.
//! - **OFF (native path)** — the image goes straight into the conversation as
//!   `ContentPart::ImageUrl` and the chat model itself must be multimodal.
//!   A text-only model cannot do this at all, which is why the UI must say so
//!   instead of silently dropping the image.
//!
//! The switch is **per conversation**, not global: one session may be reading
//! screenshots with a text model while another drives a multimodal model
//! directly. A global setting would force the wrong mode on one of them.
//!
//! ## Why a file and not the settings table
//!
//! It is read on every send and written on every toggle — a JSON file under
//! `$HERMIX_HOME` is enough and keeps the switch usable even when the settings
//! DB is unavailable. Losing this file must never block a send, so every read
//! falls back to the default (off) on any error.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Default when nothing has been chosen yet.
///
/// Off by default: the native multimodal path is the one that needs no extra
/// network hop and no third-party vision service. Users opt *in* to the
/// plugin path.
pub const DEFAULT_ENABLED: bool = false;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct VisionState {
    /// session id -> enabled
    sessions: HashMap<String, bool>,
}

fn state_path() -> PathBuf {
    hermix_config::paths::hermix_home().join("vision-plugin.json")
}

fn load() -> VisionState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(state: &VisionState) -> Result<(), String> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Whether the vision plugin is on for `session_id`.
///
/// A missing/blank session id returns the default rather than inventing a
/// shared bucket — two unrelated conversations must not inherit each other's
/// switch.
pub fn is_enabled(session_id: Option<&str>) -> bool {
    let Some(sid) = session_id.filter(|s| !s.is_empty()) else {
        return DEFAULT_ENABLED;
    };
    load().sessions.get(sid).copied().unwrap_or(DEFAULT_ENABLED)
}

/// Set the switch for `session_id` and persist it.
pub fn set_enabled(session_id: &str, enabled: bool) -> Result<(), String> {
    if session_id.is_empty() {
        return Err("session id is required".to_string());
    }
    let mut state = load();
    state.sessions.insert(session_id.to_string(), enabled);
    save(&state)
}

/// Whether a model that declares `supports_vision` can take pixels directly.
///
/// `None` means the upstream said nothing. That is **not** the same as
/// "cannot" — many gateways omit the field for models that do support images.
/// We treat unknown as *allowed* and let the provider reject it: refusing up
/// front would block working setups, while a provider error is accurate and
/// actionable.
pub fn model_accepts_images(supports_vision: Option<bool>) -> bool {
    supports_vision != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_session_uses_default() {
        assert_eq!(is_enabled(None), DEFAULT_ENABLED);
        assert_eq!(is_enabled(Some("")), DEFAULT_ENABLED);
    }

    #[test]
    fn blank_session_is_rejected_on_write() {
        assert!(set_enabled("", true).is_err());
    }

    /// `None` (upstream said nothing) must not be read as "cannot see images".
    #[test]
    fn unknown_capability_is_not_a_refusal() {
        assert!(model_accepts_images(None));
        assert!(model_accepts_images(Some(true)));
        assert!(!model_accepts_images(Some(false)));
    }
}
