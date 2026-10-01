//! Platform-specific configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// UnauthorizedDmBehavior
// ---------------------------------------------------------------------------

/// How to handle unauthorized direct messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum UnauthorizedDmBehavior {
    /// Pair the user with the bot (create a session).
    Pair,
    /// Silently ignore the message.
    #[default]
    Ignore,
}

// ---------------------------------------------------------------------------
// PlatformConfig
// ---------------------------------------------------------------------------

/// Configuration for a specific platform (e.g. discord, slack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PlatformConfig {
    /// Whether this platform adapter is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Bot authentication token (may be an env-var reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    /// Optional webhook URL for incoming events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,

    /// Whether the bot must be @mentioned in group channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_mention: Option<bool>,

    /// How to handle DMs from unauthorized users.
    #[serde(default)]
    pub unauthorized_dm_behavior: UnauthorizedDmBehavior,

    /// Whether each user gets their own group session context.
    #[serde(default)]
    pub group_sessions_per_user: bool,

    /// Home / landing channel for the bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_channel: Option<String>,

    /// Users explicitly allowed to interact with the bot.
    #[serde(default)]
    pub allowed_users: Vec<String>,

    /// Users with administrative privileges.
    #[serde(default)]
    pub admin_users: Vec<String>,

    /// Keys not declared above (Python Hermes platform blocks often use many
    /// adapter-specific names); they deserialize here so YAML stays compatible.
    #[serde(default, flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl PlatformConfig {
    /// Deep-merge a JSON overlay into this configuration.
    /// Values present in `overlay` take precedence; missing keys in the
    /// overlay are left unchanged. The `extra` map is merged recursively.
    pub fn merge_with_json(&mut self, overlay: &serde_json::Value) {
        if let serde_json::Value::Object(map) = overlay {
            // Booleans accept the string forms a model (or a hand-edited
            // config) can produce: `as_bool()` only reads a JSON boolean,
            // so `"enabled": "true"` used to leave the channel silently
            // DISABLED — the setting looked present in the file but had
            // no effect. Same "explicit affirmative only" rule as
            // `hermix_tools::param_coercion`; duplicated rather than
            // imported because `hermix-config` sits below `hermix-tools`
            // in the dependency chain.
            fn flag(v: Option<&serde_json::Value>) -> Option<bool> {
                match v? {
                    serde_json::Value::Bool(b) => Some(*b),
                    serde_json::Value::String(s) => {
                        let t = s.trim();
                        if t.eq_ignore_ascii_case("true") || t == "1" {
                            Some(true)
                        } else if t.eq_ignore_ascii_case("false") || t == "0" {
                            Some(false)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            // Standard fields
            if let Some(v) = flag(map.get("enabled")) {
                self.enabled = v;
            }
            if let Some(v) = map.get("token").and_then(|v| v.as_str()) {
                self.token = Some(v.to_string());
            }
            if let Some(v) = map.get("webhook_url").and_then(|v| v.as_str()) {
                self.webhook_url = Some(v.to_string());
            }
            if let Some(v) = flag(map.get("require_mention")) {
                self.require_mention = Some(v);
            }
            if let Some(v) = map.get("unauthorized_dm_behavior").and_then(|v| v.as_str()) {
                if let Ok(behavior) = serde_json::from_value::<UnauthorizedDmBehavior>(
                    serde_json::Value::String(v.to_string()),
                ) {
                    self.unauthorized_dm_behavior = behavior;
                }
            }
            if let Some(v) = flag(map.get("group_sessions_per_user")) {
                self.group_sessions_per_user = v;
            }
            if let Some(v) = map.get("home_channel").and_then(|v| v.as_str()) {
                self.home_channel = Some(v.to_string());
            }
            if let Some(v) = map.get("allowed_users") {
                if let Ok(users) = serde_json::from_value::<Vec<String>>(v.clone()) {
                    self.allowed_users = users;
                }
            }
            if let Some(v) = map.get("admin_users") {
                if let Ok(users) = serde_json::from_value::<Vec<String>>(v.clone()) {
                    self.admin_users = users;
                }
            }
            // Merge extra map recursively
            if let Some(serde_json::Value::Object(extra_overlay)) = map.get("extra") {
                for (k, v) in extra_overlay {
                    self.extra.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared parsing helpers (`extra` map + token; Python / YAML–compatible)
// ---------------------------------------------------------------------------

/// `extra[key]` as a trimmed, non-empty string when the value is a JSON string.
pub fn extra_string(cfg: &PlatformConfig, key: &str) -> Option<String> {
    cfg.extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Bot token from [`PlatformConfig::token`] or `extra["token"]`, trimmed.
pub fn platform_token_or_extra(cfg: &PlatformConfig) -> Option<String> {
    cfg.token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| extra_string(cfg, "token"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extra_string_trims_and_skips_empty() {
        let mut pc = PlatformConfig::default();
        pc.extra.insert("k".to_string(), json!("  hi  "));
        assert_eq!(extra_string(&pc, "k").as_deref(), Some("hi"));
        pc.extra.insert("empty".to_string(), json!("   "));
        assert!(extra_string(&pc, "empty").is_none());
    }

    #[test]
    fn platform_token_or_extra_prefers_top_level_token() {
        let mut pc = PlatformConfig::default();
        pc.token = Some("  top  ".to_string());
        pc.extra.insert("token".to_string(), json!("extra"));
        assert_eq!(platform_token_or_extra(&pc).as_deref(), Some("top"));
    }

    #[test]
    fn platform_token_or_extra_falls_back_to_extra() {
        let mut pc = PlatformConfig::default();
        pc.extra.insert("token".to_string(), json!("  from_extra "));
        assert_eq!(platform_token_or_extra(&pc).as_deref(), Some("from_extra"));
    }

    #[test]
    fn platform_config_default() {
        let pc = PlatformConfig::default();
        assert!(!pc.enabled);
        assert!(pc.token.is_none());
        assert_eq!(pc.unauthorized_dm_behavior, UnauthorizedDmBehavior::Ignore);
    }

    #[test]
    fn unauthorized_dm_behavior_serde() {
        let b = UnauthorizedDmBehavior::Pair;
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, "\"pair\"");
        let back: UnauthorizedDmBehavior = serde_json::from_str(&json).unwrap();
        assert_eq!(back, UnauthorizedDmBehavior::Pair);
    }

    #[test]
    fn merge_with_json_overlay() {
        let mut pc = PlatformConfig::default();
        let overlay = serde_json::json!({
            "enabled": true,
            "token": "abc123",
            "extra": {
                "custom_field": "value"
            }
        });
        pc.merge_with_json(&overlay);
        assert!(pc.enabled);
        assert_eq!(pc.token.as_deref(), Some("abc123"));
        assert_eq!(
            pc.extra.get("custom_field").unwrap().as_str().unwrap(),
            "value"
        );
    }

    #[test]
    fn merge_accepts_string_booleans() {
        // Regression: `"enabled": "true"` used to be read as absent, so
        // the channel stayed DISABLED while the config file visibly
        // said it was enabled. Same class as the `storage_clean.confirm`
        // loop, but with the opposite failure direction: the setting
        // silently never took effect.
        for (raw, want) in [
            (serde_json::json!("true"), true),
            (serde_json::json!("TRUE"), true),
            (serde_json::json!(" true "), true),
            (serde_json::json!("1"), true),
            (serde_json::json!("false"), false),
            (serde_json::json!("0"), false),
        ] {
            // Start from the opposite of what we expect, so a merge that
            // silently did nothing would fail the assertion.
            let mut pc = PlatformConfig {
                enabled: !want,
                require_mention: Some(!want),
                group_sessions_per_user: !want,
                ..Default::default()
            };
            pc.merge_with_json(&serde_json::json!({
                "enabled": raw,
                "require_mention": raw,
                "group_sessions_per_user": raw,
            }));
            assert_eq!(pc.enabled, want, "enabled from {:?}", raw);
            assert_eq!(
                pc.require_mention,
                Some(want),
                "require_mention from {:?}",
                raw
            );
            assert_eq!(
                pc.group_sessions_per_user, want,
                "group_sessions_per_user from {:?}",
                raw
            );
        }
    }

    #[test]
    fn merge_ignores_unparseable_booleans() {
        // Present but meaningless must NOT flip the flag — the channel
        // has to keep whatever it had, not silently enable or disable.
        let mut pc = PlatformConfig {
            enabled: false,
            ..Default::default()
        };
        pc.merge_with_json(&serde_json::json!({
            "enabled": "yes",
            "require_mention": 1,
        }));
        assert!(!pc.enabled, "\"yes\" is not an affirmative");
        assert_eq!(pc.require_mention, None, "1 is not a boolean");
    }
}
