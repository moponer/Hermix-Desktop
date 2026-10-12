//! Website policy engine: whitelist/blacklist rules by domain, path, and URL regex.
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use hermix_core::ToolError;

// ---------------------------------------------------------------------------
// Policy types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PolicyAction {
    #[default]
    Allow,
    Deny,
    Warn,
}

/// A single policy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Domain glob pattern (e.g. "*.malware.com", "example.com").
    #[serde(default)]
    pub domain: Option<String>,
    /// Path regex pattern (e.g. "/admin/.*").
    #[serde(default)]
    pub path_regex: Option<String>,
    /// Full URL regex pattern.
    #[serde(default)]
    pub url_regex: Option<String>,
    /// Action to take when matched.
    pub action: PolicyAction,
    /// Human-readable reason.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Website policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebsitePolicyConfig {
    /// Default action when no rules match.
    #[serde(default)]
    pub default_action: PolicyAction,
    /// Ordered list of rules (first match wins).
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

impl Default for WebsitePolicyConfig {
    fn default() -> Self {
        Self {
            default_action: PolicyAction::Allow,
            rules: vec![
                // Built-in safety rules
                PolicyRule {
                    domain: None,
                    path_regex: None,
                    url_regex: Some(r"^http://".into()),
                    action: PolicyAction::Warn,
                    reason: Some("Non-HTTPS URL".into()),
                },
            ],
        }
    }
}

/// Result of a URL policy check.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyCheckResult {
    pub url: String,
    pub action: PolicyAction,
    pub safe: bool,
    pub reason: String,
    pub matched_rule: Option<usize>,
}

// ---------------------------------------------------------------------------
// WebsitePolicy engine
// ---------------------------------------------------------------------------

/// Website policy engine with hot-reload support.
pub struct WebsitePolicy {
    config: Arc<RwLock<WebsitePolicyConfig>>,
    config_path: Option<PathBuf>,
    last_modified: Arc<RwLock<Option<SystemTime>>>,
}

impl WebsitePolicy {
    /// Create with default built-in rules.
    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(WebsitePolicyConfig::default())),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    /// Create from a config file path. Loads immediately and supports hot-reload.
    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let config = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_yaml::from_str::<WebsitePolicyConfig>(&content) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to parse website policy config at {}: {e}", path.display());
                        WebsitePolicyConfig::default()
                    }
                },
                Err(_) => WebsitePolicyConfig::default(),
            }
        } else {
            WebsitePolicyConfig::default()
        };

        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: Some(path),
            last_modified: Arc::new(RwLock::new(mtime)),
        }
    }

    /// Create from an explicit config (for testing).
    pub fn from_config(config: WebsitePolicyConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if the config file has been modified and reload if so.
    pub fn maybe_reload(&self) {
        let Some(ref path) = self.config_path else {
            return;
        };

        let current_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return,
        };

        let needs_reload = {
            let last = self.last_modified.read().expect("url_safety rwlock not poisoned");
            match *last {
                Some(prev) => current_mtime > prev,
                None => true,
            }
        };

        if needs_reload {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(new_config) = serde_yaml::from_str::<WebsitePolicyConfig>(&content) {
                    if let Ok(mut cfg) = self.config.write() {
                        *cfg = new_config;
                    }
                    if let Ok(mut last) = self.last_modified.write() {
                        *last = Some(current_mtime);
                    }
                    tracing::info!("Reloaded website policy from {:?}", path);
                }
            }
        }
    }

    /// Check a URL against the policy.
    pub fn check_url(&self, url: &str) -> PolicyCheckResult {
        self.maybe_reload();

        let config = self.config.read().expect("url_safety rwlock not poisoned");
        let parsed = url::Url::parse(url);

        for (i, rule) in config.rules.iter().enumerate() {
            if rule_matches(rule, url, parsed.as_ref().ok()) {
                let action = rule.action;
                return PolicyCheckResult {
                    url: url.to_string(),
                    action,
                    safe: action != PolicyAction::Deny,
                    reason: rule
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("Matched rule #{}", i + 1)),
                    matched_rule: Some(i),
                };
            }
        }

        // No rule matched — use default
        PolicyCheckResult {
            url: url.to_string(),
            action: config.default_action,
            safe: config.default_action != PolicyAction::Deny,
            reason: "No matching rule; using default policy".into(),
            matched_rule: None,
        }
    }

    /// Pre-check interface for web tools. Returns `Err` if denied.
    pub fn pre_check(&self, url: &str) -> Result<PolicyCheckResult, ToolError> {
        let result = self.check_url(url);
        if result.action == PolicyAction::Deny {
            return Err(ToolError::ExecutionFailed(format!(
                "URL blocked by policy: {} (reason: {})",
                url, result.reason
            )));
        }
        if result.action == PolicyAction::Warn {
            tracing::warn!(url = url, reason = %result.reason, "URL policy warning");
        }
        Ok(result)
    }
}

impl Default for WebsitePolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a rule matches a URL.
fn rule_matches(rule: &PolicyRule, url: &str, parsed: Option<&url::Url>) -> bool {
    // Check domain pattern
    if let Some(ref domain_pattern) = rule.domain {
        if let Some(parsed) = parsed {
            if let Some(host) = parsed.host_str() {
                if !domain_glob_matches(domain_pattern, host) {
                    return false;
                }
            } else {
                return false;
            }
        } else {
            return false;
        }
        // If only domain is specified and it matched, return true
        if rule.path_regex.is_none() && rule.url_regex.is_none() {
            return true;
        }
    }

    // Check path regex
    if let Some(ref path_pattern) = rule.path_regex {
        if let Some(parsed) = parsed {
            if let Ok(re) = regex::Regex::new(path_pattern) {
                if !re.is_match(parsed.path()) {
                    return false;
                }
            }
        } else {
            return false;
        }
        if rule.url_regex.is_none() {
            return true;
        }
    }

    // Check full URL regex
    if let Some(ref url_pattern) = rule.url_regex {
        if let Ok(re) = regex::Regex::new(url_pattern) {
            return re.is_match(url);
        }
        return false;
    }

    // No patterns specified — rule doesn't match anything
    false
}

/// Simple glob matching for domain patterns.
/// Supports `*` as wildcard for a single domain segment and `*.` prefix for subdomains.
pub(crate) fn domain_glob_matches(pattern: &str, domain: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let domain = domain.to_lowercase();

    if pattern == domain {
        return true;
    }

    // *.example.com matches sub.example.com and deep.sub.example.com
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return domain.ends_with(suffix)
            && (domain.len() > suffix.len())
            && domain.as_bytes()[domain.len() - suffix.len() - 1] == b'.';
    }

    false
}

