//! URL safety and website policy engine.
//!
//! Provides:
//! 1. **UrlSafetyHandler** — tool the LLM invokes to check if a URL is safe
//! 2. **WebsitePolicy** — configurable URL whitelist/blacklist engine with
//!    domain, path, and regex matching. Loaded from `~/.hermix/website_policy.yaml`.
//! 3. **Pre-check interface** for `web_tools` and `browser` to call before
//!    accessing any URL.
//!
//! Policy file format (`~/.hermix/website_policy.yaml`):
//! ```yaml
//! default_action: allow  # or "deny"
//! rules:
//!   - domain: "*.malware.com"
//!     action: deny
//!     reason: "Known malware domain"
//!   - domain: "internal.corp.com"
//!     action: allow
//!   - path_regex: "/admin/.*"
//!     action: deny
//!     reason: "Admin paths blocked"
//!   - domain: "*.gov"
//!     action: allow
//! ```

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

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
                Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
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

    /// Pre-check interface for web/browser tools. Returns `Err` if denied.
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
fn domain_glob_matches(pattern: &str, domain: &str) -> bool {
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

// ---------------------------------------------------------------------------
// Tirith Security Policy Engine
// ---------------------------------------------------------------------------

/// Default path to the user-editable Tirith rules file.
///
/// Resolved relative to `$HERMIX_HOME` (default `~/.hermix`) and
/// falls back to `~/.hermix/tirith_rules.yaml` when neither the
/// env var nor the home dir override is set. The `tirith`
/// subsystem is the canonical **tool-call** policy layer (the
/// `approval` regex system is the older, more permissive
/// **terminal-command** layer; the two are complementary — Tirith
/// catches structural problems like `\\?\D:\foo` or `C:\Windows`
/// paths before the agent dispatches, `approval` catches semantic
/// ones like `rm -rf /` once the command reaches the terminal
/// handler).
pub fn default_tirith_rules_path() -> std::path::PathBuf {
    // Delegate to the canonical resolver so HERMIX_HOME,
    // HERMIX_PORTABLE_DATA_DIR, and the `~/.hermix` fallback all
    // go through the same code path as the rest of the app.
    hermix_config::paths::hermix_home().join("tirith_rules.yaml")
}

/// Bundled fallback rules — applied when the user has not created
/// their own `tirith_rules.yaml`. The goal is *not* to be a
/// comprehensive security policy; it's to catch the common
/// LLM-fabrication cases that have been observed in production:
///
/// - Windows raw path namespace (`\\?\`, `\\.\`, `\\?\UNC\`)
/// - Reads of system directories the LLM has no business touching
///   (`C:\Windows\System32`, `C:\Windows\SysWOW64`,
///   `C:\Windows\WinSxS`)
/// - The classic "weird first char" shell injection patterns
///   (leading `-n` / `-e` / `-o` / backtick)
/// - `find` / `rm` / `chmod` on the system root (defense in
///   depth — `approval` already catches `rm -rf` variants)
///
/// Rules are evaluated first-match-wins, so the explicit
/// `\\?\`-prefix block sits at the top.
///
/// Pattern syntax: each `pattern` is a regex evaluated against
/// the JSON-serialized form of the tool-call parameters. To keep
/// the regex readable we use YAML **single-quoted** scalars, so
/// `\\` is two literal characters in the regex and `\\?` in the
/// regex matches a single backslash followed by `?` in the
/// matched string. The matched string is the JSON text, so
/// `\\?\D:\data` on the wire becomes the seven-character
/// substring `\?\D:\data` in the JSON value, which is what the
/// regex has to match.
pub const DEFAULT_TIRITH_RULES_YAML: &str = r#"# Default Tirith security rules for Hermix Agent.
#
# This file is loaded when the user has not created their own
# `tirith_rules.yaml` (under `$HERMIX_HOME` or `~/.hermix/`). It
# is intentionally narrow: it catches structural shape
# problems the LLM invents (forbidden path prefixes, system
# directories) and a handful of injection red flags. It is NOT a
# replacement for the terminal `approval` regex layer, which
# remains the authoritative gate for `rm -rf`, `sudo`, and the
# other "user must explicitly bless" patterns.
#
# Format: first matching rule wins. `tool: "*"` matches all
# tools. `pattern` is a regex evaluated against the JSON
# serialization of the tool-call parameters.
#
# If you want a stricter policy, copy this file to
# `~/.hermix/tirith_rules.yaml` and add/override rules. Adding
# your own file disables the bundled defaults (the loader does
# not merge them).

rules:
  # ── Hard blocks ────────────────────────────────────────────
  # Windows raw path namespace — only kernel/low-level code
  # should ever touch these. The LLM has no business putting
  # `\\?\D:\...` in a tool call. The JSON value will contain a
  # single backslash before the `?` (the user-visible form has
  # two for shell-escape, but the JSON layer only sees one).
  - tool: 'terminal'
    pattern: '\\[?](\\UNC)?\\'
    action: block
    reason: 'Win32 device/namespace prefix (\? or \?\UNC\) is not allowed in shell commands'
  - tool: 'terminal'
    pattern: '\\\.\\'
    action: block
    reason: 'Win32 device namespace (\.\) is not allowed in shell commands'

  # Reading from system directories the LLM has no business
  # touching. This is a defense in depth — `read_file` already
  # gates on the workspace sandbox, but the LLM has been
  # observed to skip the `read_file` tool and shell out to
  # `cat C:\Windows\System32\...` instead.
  - tool: 'terminal'
    pattern: '[A-Za-z]:\\\\Windows\\\\(System32|SysWOW64|WinSxS|assembly)(\\\\|"|$)'
    action: block
    reason: 'Reading system directories via shell is not allowed; use the read_file tool on user-workspace paths'

  # Shell-injection red flags. Leading `-n` / `-e` / `-o` /
  # `-c` are almost never legitimate as the first arg of a
  # user command when followed by shell metacharacters.
  - tool: 'terminal'
    pattern: '^\s*(-n|-e|-c|-o|-[a-z]{2,})\s+[|&;<>]'
    action: block
    reason: 'Command shape suggests shell-injection attempt'

  # ── Require approval ──────────────────────────────────────
  # Anything that looks like a global find / recursive rm
  # rooted at `/` or `C:\`. `approval` already covers `rm -rf`,
  # this is the belt-and-suspenders for `find / -delete` and
  # friends.
  - tool: 'terminal'
    pattern: '(find|rm|chmod)\s.*\s(/|[A-Za-z]:\\?)\s*$'
    action: require_approval
    reason: 'Recursive operation on a filesystem root requires explicit approval'

  # ── Warns ─────────────────────────────────────────────────
  # curl piping to sh — classic supply-chain footgun. Not
  # blocked outright (some users do want it), but flagged.
  - tool: 'terminal'
    pattern: 'curl\s.*\|\s*(ba)?sh'
    action: warn
    reason: 'Piping curl to a shell is a supply-chain risk; confirm the source'

  # ── Catch-all allow ───────────────────────────────────────
  - tool: '*'
    action: allow
    reason: 'No rule matched'
"#;

/// Tirith security policy — tool-call-level pre-check engine.
///
/// Evaluates rules before any tool call to decide whether to allow, block,
/// or require approval. Rules are loaded from `~/.hermix/tirith_rules.yaml`.
///
/// Rule format:
/// ```yaml
/// rules:
///   - tool: "terminal"
///     pattern: "rm -rf"
///     action: block
///     reason: "Destructive command"
///   - tool: "send_message"
///     condition: "platform == 'production'"
///     action: require_approval
///   - tool: "*"
///     action: allow
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TirithRule {
    /// Tool name to match (glob: "*" matches all).
    pub tool: String,
    /// Regex pattern to match against the tool's parameters (serialized as JSON).
    #[serde(default)]
    pub pattern: Option<String>,
    /// Action: allow, block, require_approval, warn.
    pub action: TirithAction,
    /// Human-readable reason.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TirithAction {
    Allow,
    Block,
    RequireApproval,
    Warn,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TirithConfig {
    #[serde(default)]
    pub rules: Vec<TirithRule>,
}

/// Result of a Tirith security check.
#[derive(Debug, Clone, Serialize)]
pub struct TirithCheckResult {
    pub tool_name: String,
    pub action: TirithAction,
    pub allowed: bool,
    pub reason: String,
    pub matched_rule: Option<usize>,
}

/// Tirith security engine.
pub struct TirithSecurity {
    config: Arc<RwLock<TirithConfig>>,
    config_path: Option<PathBuf>,
    last_modified: Arc<RwLock<Option<SystemTime>>>,
}

impl TirithSecurity {
    /// Load with a user override on top of the bundled defaults.
    ///
    /// Resolution order:
    /// 1. If `~/.hermix/tirith_rules.yaml` exists → load it as-is
    ///    (user takes full control; no defaults merged).
    /// 2. Else fall back to the bundled [`DEFAULT_TIRITH_RULES_YAML`]
    ///    so a fresh install still gets structural protection
    ///    (`\\?\` blocks, system-dir blocks, etc.) without
    ///    requiring the user to copy a file first.
    pub fn from_default_or_bundled() -> Self {
        let path = default_tirith_rules_path();
        if path.exists() {
            return Self::from_file(path);
        }
        // Bundled fallback — parse and wrap. We deliberately
        // don't pass a `config_path` so `maybe_reload` becomes
        // a no-op; the bundled rules are baked into the binary
        // and don't change at runtime.
        match serde_yaml::from_str::<TirithConfig>(DEFAULT_TIRITH_RULES_YAML) {
            Ok(config) => Self::from_config(config),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "bundled DEFAULT_TIRITH_RULES_YAML failed to parse; \
                     falling back to empty policy (everything allowed)"
                );
                Self::new()
            }
        }
    }

    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(TirithConfig::default())),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let config = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
                Err(_) => TirithConfig::default(),
            }
        } else {
            TirithConfig::default()
        };

        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: Some(path),
            last_modified: Arc::new(RwLock::new(mtime)),
        }
    }

    pub fn from_config(config: TirithConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    /// Hot-reload from file if modified.
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
                if let Ok(new_config) = serde_yaml::from_str::<TirithConfig>(&content) {
                    if let Ok(mut cfg) = self.config.write() {
                        *cfg = new_config;
                    }
                    if let Ok(mut last) = self.last_modified.write() {
                        *last = Some(current_mtime);
                    }
                    tracing::info!("Reloaded Tirith security rules from {:?}", path);
                }
            }
        }
    }

    /// Check a tool call against security rules.
    pub fn check_tool_call(&self, tool_name: &str, params: &Value) -> TirithCheckResult {
        self.maybe_reload();

        let config = self.config.read().expect("url_safety rwlock not poisoned");
        let params_str = serde_json::to_string(params).unwrap_or_default();

        for (i, rule) in config.rules.iter().enumerate() {
            // Match tool name (glob)
            if rule.tool != "*" && rule.tool != tool_name {
                continue;
            }

            // Match pattern against params.
            //
            // Primary path: match against the JSON-serialized form of
            // the params (`{"command":"rm -rf /"}`). Most rules don't
            // use anchors, so this is fine.
            //
            // Fallback for `terminal`: also try matching against the
            // unwrapped `command` / `cmd` string field. This restores
            // the *original intent* of bundled rules that use `^...$`
            // anchors (e.g. shell-injection shape `^\s*(-n|...)\s+[|&;<>]`
            // and recursive-on-root `(find|rm|chmod)\s.*\s/)\s*$`).
            // Without this fallback, those anchors are dead because
            // the JSON envelope (`{"command":"...` prefix and `"}`
            // suffix) makes them unreachable. See the
            // `bundled_shell_injection_rule_blocks` /
            // `bundled_recursive_op_rule_requires_approval` tests
            // for the regression fixtures.
            let mut pattern_matched = false;
            if let Some(ref pattern) = rule.pattern {
                if let Ok(re) = regex::Regex::new(pattern) {
                    if re.is_match(&params_str) {
                        pattern_matched = true;
                    } else if tool_name == "terminal" {
                        // Fallback: also match against the raw command
                        // value, so anchored rules can still bite.
                        for key in &["command", "cmd"] {
                            if let Some(cmd) = params.get(*key).and_then(|v| v.as_str()) {
                                if re.is_match(cmd) {
                                    pattern_matched = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                // No pattern specified — rule always matches (subject
                // to tool-name glob above).
                pattern_matched = true;
            }
            if !pattern_matched {
                continue;
            }

            // Rule matched
            return TirithCheckResult {
                tool_name: tool_name.to_string(),
                action: rule.action,
                allowed: rule.action != TirithAction::Block,
                reason: rule
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("Matched Tirith rule #{}", i + 1)),
                matched_rule: Some(i),
            };
        }

        // No rule matched — allow by default
        TirithCheckResult {
            tool_name: tool_name.to_string(),
            action: TirithAction::Allow,
            allowed: true,
            reason: "No matching security rule".into(),
            matched_rule: None,
        }
    }

    /// Pre-check interface. Returns `Err` if blocked.
    pub fn pre_check(
        &self,
        tool_name: &str,
        params: &Value,
    ) -> Result<TirithCheckResult, ToolError> {
        let result = self.check_tool_call(tool_name, params);
        if result.action == TirithAction::Block {
            return Err(ToolError::ExecutionFailed(format!(
                "Tool call blocked by security policy: {} (reason: {})",
                tool_name, result.reason
            )));
        }
        if result.action == TirithAction::Warn {
            tracing::warn!(
                tool = tool_name,
                reason = %result.reason,
                "Tirith security warning"
            );
        }
        Ok(result)
    }
}

impl Default for TirithSecurity {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SSRF blocklist
// ---------------------------------------------------------------------------
//
// Server-Side Request Forgery protection for tools that fetch URLs on the
// LLM's behalf (`web_extract`, `browser_navigate`, …). The guard rejects:
//
// - Schemes other than http/https (file://, gopher://, dict://, ftp://)
// - Loopback (127.0.0.0/8, ::1)
// - Link-local (169.254.0.0/16, fe80::/10) — includes the AWS/GCP/Azure
//   instance metadata endpoint at 169.254.169.254
// - RFC 1918 private (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
// - IPv6 unique-local (fc00::/7)
// - IPv4-mapped IPv6 (::ffff:0:0/96 — bypasses naive v4-only checks)
// - Multicast (224.0.0.0/4, ff00::/8) and broadcast (255.255.255.255)
// - Reserved 240.0.0.0/4 and unspecified (0.0.0.0, ::)
//
// Hostname-based checks are done literally for the well-known names
// (`localhost`, `*.internal`, `*.local`, `metadata.google.internal`); DNS
// resolution to IP is the **caller's** responsibility. The HTTP backend is
// the right place to do `lookup_host` + re-check with this guard, and
// blocking the runtime to do a sync `ToSocketAddrs::lookup_host` here
// would tank the agent's response time. The contract is: **IP literals
// are classified here; hostnames pass through with no DNS work done.**

use std::net::{Ipv4Addr, Ipv6Addr};

/// Category of an SSRF block. Stable for telemetry / log parsing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SsrfCategory {
    /// scheme is not http/https (e.g. file://, gopher://, dict://, ftp://)
    ForbiddenScheme,
    /// 127.0.0.0/8 or ::1
    Loopback,
    /// 169.254.0.0/16 (cloud metadata!) or fe80::/10
    LinkLocal,
    /// 10/8, 172.16/12, 192.168/16, fc00::/7, *.internal, *.local
    PrivateNetwork,
    /// 224.0.0.0/4 or ff00::/8
    Multicast,
    /// 240.0.0.0/4 (excl. broadcast)
    Reserved,
    /// 0.0.0.0 or ::
    UnspecifiedAddress,
    /// ::ffff:0:0/96 — caller wrapped a private v4 in v6 to bypass checks
    Ipv4MappedIpv6,
}

/// A URL the SSRF guard refused to allow.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SsrfViolation {
    pub url: String,
    pub reason: String,
    pub category: SsrfCategory,
}

impl std::fmt::Display for SsrfViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SSRF block ({}): {}", self.category, self.reason)
    }
}

impl std::error::Error for SsrfViolation {}

impl std::fmt::Display for SsrfCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ForbiddenScheme => "forbidden_scheme",
            Self::Loopback => "loopback",
            Self::LinkLocal => "link_local",
            Self::PrivateNetwork => "private_network",
            Self::Multicast => "multicast",
            Self::Reserved => "reserved",
            Self::UnspecifiedAddress => "unspecified",
            Self::Ipv4MappedIpv6 => "ipv4_mapped_ipv6",
        })
    }
}

/// Stateless SSRF guard. Cheap to construct (`SsrfGuard::new()`) and
/// cheap to call (`check(url)`). Currently empty; the rules are encoded
/// in [`SsrfGuard::check`] and [`SsrfGuard::classify_ip`]. Wrap in an
/// `Arc` if you want to share across handlers.
#[derive(Debug, Clone, Copy, Default)]
pub struct SsrfGuard;

impl SsrfGuard {
    pub fn new() -> Self {
        Self
    }

    /// Check a URL against the SSRF blocklist.
    ///
    /// Returns `Ok(())` if the URL is safe to dispatch to a fetcher,
    /// `Err(SsrfViolation)` otherwise. **No DNS resolution is
    /// performed** — hostnames pass through; IP literals are
    /// classified. See the module-level docs.
    pub fn check(&self, url: &str) -> Result<(), SsrfViolation> {
        let parsed = url::Url::parse(url).map_err(|e| SsrfViolation {
            url: url.to_string(),
            reason: format!("unparseable URL: {}", e),
            category: SsrfCategory::ForbiddenScheme,
        })?;

        // 1. Scheme gate.
        let scheme = parsed.scheme();
        if !matches!(scheme, "http" | "https") {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("scheme `{}` is not allowed; only http/https", scheme),
                category: SsrfCategory::ForbiddenScheme,
            });
        }

        // 2. Host classification. `url::Host` is a typed enum, so we
        //    don't have to round-trip through `host_str().parse()` —
        //    that path was unreliable for IPv6 (the `url` crate
        //    historically returned the bracketed form, which doesn't
        //    parse as `IpAddr`).
        let host = parsed.host().ok_or_else(|| SsrfViolation {
            url: url.to_string(),
            reason: "URL has no host".into(),
            category: SsrfCategory::ForbiddenScheme,
        })?;

        match host {
            url::Host::Ipv4(v4) => Self::classify_v4(url, v4)?,
            url::Host::Ipv6(v6) => {
                // IPv4-mapped IPv6: ::ffff:a.b.c.d — always a
                // violation. The category says the wrapper is
                // Ipv4MappedIpv6; the reason names the inner v4 so
                // the user knows which private IP was being smuggled.
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return Err(SsrfViolation {
                        url: url.to_string(),
                        reason: format!(
                            "IPv4-mapped IPv6 wraps {} (RFC 4291 §2.5.5.2 bypass attempt)",
                            v4
                        ),
                        category: SsrfCategory::Ipv4MappedIpv6,
                    });
                }
                Self::classify_v6(url, v6)?;
            }
            url::Host::Domain(d) => {
                // 3a. Hostname literal checks (no DNS).
                let lower = d.to_ascii_lowercase();
                if matches!(lower.as_str(), "localhost" | "ip6-localhost" | "ip6-loopback") {
                    return Err(SsrfViolation {
                        url: url.to_string(),
                        reason: format!("hostname `{}` is loopback", d),
                        category: SsrfCategory::Loopback,
                    });
                }
                if lower == "metadata.google.internal"
                    || lower.ends_with(".internal")
                    || lower.ends_with(".local")
                {
                    return Err(SsrfViolation {
                        url: url.to_string(),
                        reason: format!("hostname `{}` is an internal/mDNS name", d),
                        category: SsrfCategory::PrivateNetwork,
                    });
                }
                // 3b. Hostname passes through. DNS resolution +
                //     re-check on the resolved IP is the **caller's**
                //     responsibility (see module-level docs).
            }
        }

        Ok(())
    }

    /// Classify a single IPv4 address against the SSRF blocklist.
    ///
    /// Public so HTTP backends that do their own DNS resolution can
    /// re-check the resolved IP with the same rules `check` applies
    /// to literal IPv4 hosts in URLs.
    pub fn classify_v4(url: &str, v4: Ipv4Addr) -> Result<(), SsrfViolation> {
        if v4.is_unspecified() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: "0.0.0.0 is the unspecified address".into(),
                category: SsrfCategory::UnspecifiedAddress,
            });
        }
        if v4.is_loopback() {
            // 127.0.0.0/8
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is loopback (127.0.0.0/8)", v4),
                category: SsrfCategory::Loopback,
            });
        }
        if v4.is_link_local() {
            // 169.254.0.0/16 — includes AWS/GCP/Azure metadata at
            // 169.254.169.254. THIS IS THE ONE THAT MATTERS MOST.
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!(
                    "{} is link-local (169.254.0.0/16) — cloud metadata lives here",
                    v4
                ),
                category: SsrfCategory::LinkLocal,
            });
        }
        if v4.is_private() {
            // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is RFC 1918 private", v4),
                category: SsrfCategory::PrivateNetwork,
            });
        }
        if v4.is_multicast() {
            // 224.0.0.0/4
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is multicast (224.0.0.0/4)", v4),
                category: SsrfCategory::Multicast,
            });
        }
        if v4.is_broadcast() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is broadcast", v4),
                category: SsrfCategory::Multicast,
            });
        }
        let o = v4.octets();
        if (o[0] & 0xF0) == 0xF0 {
            // 240.0.0.0/4 (reserved for future use) — broadcast was
            // already caught above
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is in the reserved 240.0.0.0/4 range", v4),
                category: SsrfCategory::Reserved,
            });
        }
        Ok(())
    }

    fn classify_v6(url: &str, v6: Ipv6Addr) -> Result<(), SsrfViolation> {
        if v6.is_unspecified() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: ":: is the unspecified address".into(),
                category: SsrfCategory::UnspecifiedAddress,
            });
        }
        if v6.is_loopback() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 loopback (::1)", v6),
                category: SsrfCategory::Loopback,
            });
        }
        // fe80::/10 — link-local. `Ipv6Addr::is_link_local` is still
        // unstable (tracking rust-lang/rust#27709) as of rustc 1.92, so
        // we hand-roll the mask: first 10 bits must be `1111_1110_10`.
        let s = v6.segments();
        if (s[0] & 0xFFC0) == 0xFE80 {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 link-local (fe80::/10)", v6),
                category: SsrfCategory::LinkLocal,
            });
        }
        // fc00::/7 — unique-local. Stable since rustc 1.80.
        if v6.is_unique_local() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 unique-local (fc00::/7)", v6),
                category: SsrfCategory::PrivateNetwork,
            });
        }
        if v6.is_multicast() {
            // ff00::/8
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 multicast (ff00::/8)", v6),
                category: SsrfCategory::Multicast,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// UrlSafetyHandler — tool the LLM invokes
// ---------------------------------------------------------------------------

/// Tool handler for URL safety checks. Uses the `WebsitePolicy` engine.
pub struct UrlSafetyHandler {
    policy: Arc<WebsitePolicy>,
}

impl UrlSafetyHandler {
    pub fn new(policy: Arc<WebsitePolicy>) -> Self {
        Self { policy }
    }
}

impl Default for UrlSafetyHandler {
    fn default() -> Self {
        // Try to load from default path. Delegate to the
        // canonical resolver so HERMIX_HOME and the portable
        // mode work here too.
        let policy_path = hermix_config::paths::hermix_home().join("website_policy.yaml");

        let policy = if policy_path.exists() {
            WebsitePolicy::from_file(policy_path)
        } else {
            WebsitePolicy::new()
        };

        Self {
            policy: Arc::new(policy),
        }
    }
}

#[async_trait]
impl ToolHandler for UrlSafetyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'url'".into()));
        }

        let result = self.policy.check_url(url);

        Ok(json!({
            "url": result.url,
            "safe": result.safe,
            "action": format!("{:?}", result.action).to_lowercase(),
            "reason": result.reason,
            "matched_rule": result.matched_rule,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "URL to check against the website safety policy"
            }),
        );
        tool_schema(
            "url_safety",
            "Check whether a URL is safe to access based on the website policy engine. \
             Evaluates domain whitelist/blacklist, path patterns, and regex rules. \
             Policy is loaded from ~/.hermix/website_policy.yaml with hot-reload support.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Domain glob matching ------------------------------------------------

    #[test]
    fn domain_glob_exact_match() {
        assert!(domain_glob_matches("example.com", "example.com"));
        assert!(!domain_glob_matches("example.com", "other.com"));
    }

    #[test]
    fn domain_glob_wildcard_subdomain() {
        assert!(domain_glob_matches("*.example.com", "sub.example.com"));
        assert!(domain_glob_matches("*.example.com", "deep.sub.example.com"));
        assert!(!domain_glob_matches("*.example.com", "example.com"));
        assert!(!domain_glob_matches("*.example.com", "notexample.com"));
    }

    #[test]
    fn domain_glob_case_insensitive() {
        assert!(domain_glob_matches("Example.COM", "example.com"));
        assert!(domain_glob_matches("*.Example.COM", "sub.example.com"));
    }

    // -- WebsitePolicy tests -------------------------------------------------

    #[test]
    fn policy_default_allows_https() {
        let policy = WebsitePolicy::new();
        let result = policy.check_url("https://example.com/page");
        assert!(result.safe);
        assert_eq!(result.action, PolicyAction::Allow);
    }

    #[test]
    fn policy_default_warns_http() {
        let policy = WebsitePolicy::new();
        let result = policy.check_url("http://example.com/page");
        assert!(result.safe); // warn is still safe
        assert_eq!(result.action, PolicyAction::Warn);
    }

    #[test]
    fn policy_custom_deny_domain() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![PolicyRule {
                domain: Some("*.malware.com".into()),
                path_regex: None,
                url_regex: None,
                action: PolicyAction::Deny,
                reason: Some("Known malware".into()),
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let result = policy.check_url("https://evil.malware.com/payload");
        assert!(!result.safe);
        assert_eq!(result.action, PolicyAction::Deny);
        assert!(result.reason.contains("malware"));

        let result = policy.check_url("https://safe.example.com");
        assert!(result.safe);
    }

    #[test]
    fn policy_path_regex() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![PolicyRule {
                domain: None,
                path_regex: Some(r"/admin/.*".into()),
                url_regex: None,
                action: PolicyAction::Deny,
                reason: Some("Admin paths blocked".into()),
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let result = policy.check_url("https://example.com/admin/settings");
        assert!(!result.safe);

        let result = policy.check_url("https://example.com/public/page");
        assert!(result.safe);
    }

    #[test]
    fn policy_default_deny() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Deny,
            rules: vec![PolicyRule {
                domain: Some("allowed.com".into()),
                path_regex: None,
                url_regex: None,
                action: PolicyAction::Allow,
                reason: None,
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let result = policy.check_url("https://allowed.com/page");
        assert!(result.safe);

        let result = policy.check_url("https://blocked.com/page");
        assert!(!result.safe);
    }

    #[test]
    fn policy_pre_check_blocks() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![PolicyRule {
                domain: Some("blocked.com".into()),
                path_regex: None,
                url_regex: None,
                action: PolicyAction::Deny,
                reason: Some("Blocked".into()),
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let err = policy.pre_check("https://blocked.com/page").unwrap_err();
        assert!(err.to_string().contains("blocked by policy"));

        assert!(policy.pre_check("https://safe.com/page").is_ok());
    }

    // -- Tirith security tests -----------------------------------------------

    #[test]
    fn tirith_no_rules_allows() {
        let engine = TirithSecurity::new();
        let result = engine.check_tool_call("terminal", &json!({"command": "ls"}));
        assert!(result.allowed);
        assert_eq!(result.action, TirithAction::Allow);
    }

    #[test]
    fn tirith_block_rule() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "terminal".into(),
                pattern: Some(r"rm\s+-rf".into()),
                action: TirithAction::Block,
                reason: Some("Destructive command".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let result = engine.check_tool_call("terminal", &json!({"command": "rm -rf /"}));
        assert!(!result.allowed);
        assert_eq!(result.action, TirithAction::Block);
        assert!(result.reason.contains("Destructive"));

        // Non-matching command should be allowed
        let result = engine.check_tool_call("terminal", &json!({"command": "ls -la"}));
        assert!(result.allowed);
    }

    #[test]
    fn tirith_wildcard_tool() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "*".into(),
                pattern: Some(r"password".into()),
                action: TirithAction::Warn,
                reason: Some("Sensitive data".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let result = engine.check_tool_call("any_tool", &json!({"data": "contains password"}));
        assert_eq!(result.action, TirithAction::Warn);
        assert!(result.allowed); // warn doesn't block
    }

    #[test]
    fn tirith_require_approval() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "send_message".into(),
                pattern: None,
                action: TirithAction::RequireApproval,
                reason: Some("All messages need approval".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let result = engine.check_tool_call("send_message", &json!({}));
        assert_eq!(result.action, TirithAction::RequireApproval);
        assert!(result.allowed); // require_approval doesn't block

        // Different tool should be allowed
        let result = engine.check_tool_call("read_file", &json!({}));
        assert_eq!(result.action, TirithAction::Allow);
    }

    #[test]
    fn tirith_pre_check_blocks() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "terminal".into(),
                pattern: Some(r"dangerous".into()),
                action: TirithAction::Block,
                reason: Some("Blocked".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let err = engine
            .pre_check("terminal", &json!({"command": "dangerous_cmd"}))
            .unwrap_err();
        assert!(err.to_string().contains("blocked by security"));

        assert!(engine
            .pre_check("terminal", &json!({"command": "safe_cmd"}))
            .is_ok());
    }

    #[test]
    fn tirith_first_match_wins() {
        let config = TirithConfig {
            rules: vec![
                TirithRule {
                    tool: "terminal".into(),
                    pattern: Some(r"safe_cmd".into()),
                    action: TirithAction::Allow,
                    reason: Some("Explicitly allowed".into()),
                },
                TirithRule {
                    tool: "*".into(),
                    pattern: None,
                    action: TirithAction::Block,
                    reason: Some("Block everything else".into()),
                },
            ],
        };
        let engine = TirithSecurity::from_config(config);

        // First rule matches — allowed
        let result = engine.check_tool_call("terminal", &json!({"command": "safe_cmd"}));
        assert!(result.allowed);

        // Second rule matches — blocked
        let result = engine.check_tool_call("terminal", &json!({"command": "other_cmd"}));
        assert!(!result.allowed);
    }

    // -- UrlSafetyHandler tests ----------------------------------------------

    #[tokio::test]
    async fn handler_checks_url() {
        let handler = UrlSafetyHandler::new(Arc::new(WebsitePolicy::new()));
        let result = handler
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["safe"], true);
    }

    #[tokio::test]
    async fn handler_warns_http() {
        let handler = UrlSafetyHandler::new(Arc::new(WebsitePolicy::new()));
        let result = handler
            .execute(json!({"url": "http://example.com"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["action"], "warn");
        assert_eq!(v["safe"], true); // warn is still safe
    }

    #[tokio::test]
    async fn handler_missing_url() {
        let handler = UrlSafetyHandler::new(Arc::new(WebsitePolicy::new()));
        let err = handler.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("Missing"));
    }

    #[tokio::test]
    async fn handler_schema() {
        let handler = UrlSafetyHandler::default();
        let schema = handler.schema();
        assert_eq!(schema.name, "url_safety");
        assert!(schema.description.contains("policy"));
    }

    // -- YAML config round-trip ----------------------------------------------

    #[test]
    fn website_policy_yaml_roundtrip() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![
                PolicyRule {
                    domain: Some("*.malware.com".into()),
                    path_regex: None,
                    url_regex: None,
                    action: PolicyAction::Deny,
                    reason: Some("Malware".into()),
                },
                PolicyRule {
                    domain: None,
                    path_regex: Some(r"/admin/.*".into()),
                    url_regex: None,
                    action: PolicyAction::Deny,
                    reason: Some("Admin blocked".into()),
                },
            ],
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: WebsitePolicyConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.default_action, PolicyAction::Allow);
    }

    #[test]
    fn tirith_yaml_roundtrip() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "terminal".into(),
                pattern: Some(r"rm -rf".into()),
                action: TirithAction::Block,
                reason: Some("Destructive".into()),
            }],
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: TirithConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].action, TirithAction::Block);
    }

    #[test]
    fn bundled_default_rules_parse() {
        // The bundled YAML is what fresh installs get; if it
        // fails to parse we silently fall back to an empty
        // (everything-allowed) policy. Catch that here at
        // compile time.
        let config: TirithConfig = serde_yaml::from_str(DEFAULT_TIRITH_RULES_YAML)
            .expect("bundled DEFAULT_TIRITH_RULES_YAML must parse");
        assert!(
            !config.rules.is_empty(),
            "bundled rules must contain at least the catch-all allow"
        );
    }

    #[test]
    fn bundled_default_blocks_forbidden_win32_paths() {
        // The exact bug from the original report: the LLM
        // invoked `ls \\?\D:\data` and the user got a
        // permission card for a command they never asked
        // for. The bundled rules must hard-block it.
        let engine = TirithSecurity::from_default_or_bundled();

        // JSON form (post-deserialization) — a single
        // backslash is what the regex sees in `params_str`.
        let r1 = engine.check_tool_call(
            "terminal",
            &json!({"command": "ls -la \\?\\D:\\qcc"}),
        );
        assert_eq!(
            r1.action,
            TirithAction::Block,
            "raw `\\?\\D:\\qcc` JSON form must be blocked; got {:?}",
            r1
        );

        // UNC namespace form.
        let r2 = engine.check_tool_call(
            "terminal",
            &json!({"command": "type \\?\\UNC\\server\\share\\file"}),
        );
        assert_eq!(
            r2.action,
            TirithAction::Block,
            "`\\?\\UNC\\` must be blocked; got {:?}",
            r2
        );

        // Win32 device namespace.
        let r3 = engine.check_tool_call(
            "terminal",
            &json!({"command": "echo hi > \\.\\COM1"}),
        );
        assert_eq!(
            r3.action,
            TirithAction::Block,
            "`\\.\\` device namespace must be blocked; got {:?}",
            r3
        );

        // Windows system directory reads.
        let r4 = engine.check_tool_call(
            "terminal",
            &json!({"command": "cat C:\\Windows\\System32\\config\\SAM"}),
        );
        assert_eq!(
            r4.action,
            TirithAction::Block,
            "system dir read must be blocked; got {:?}",
            r4
        );

        // Normal commands must still pass.
        let r5 = engine.check_tool_call(
            "terminal",
            &json!({"command": "ls -la"}),
        );
        assert_eq!(r5.action, TirithAction::Allow);
    }

    // -----------------------------------------------------------------
    // Bundled-rule coverage matrix
    // -----------------------------------------------------------------
    //
    // These tests assert that the default `tirith_rules.yaml`
    // (the one shipped with the binary and used by `TirithSecurity::from_default_or_bundled`)
    // behaves the way `DEFAULT_TIRITH_RULES_YAML`'s inline
    // comments promise. Every cell of the bundled rule matrix
    // is exercised; regressions here are caught at `cargo test`
    // time before they reach production.

    fn bundled() -> TirithSecurity {
        TirithSecurity::from_default_or_bundled()
    }

    /// Convenience: assert that a `(tool, params)` call against
    /// the bundled engine yields the expected `TirithAction`.
    fn assert_bundled_action(
        tool: &str,
        params: serde_json::Value,
        expected: TirithAction,
        context: &str,
    ) {
        let r = bundled().check_tool_call(tool, &params);
        assert_eq!(
            r.action, expected,
            "bundled Tirith: {} — got {:?} (reason: {})",
            context, r, r.reason
        );
    }

    // -- read-only commands → Allow ---------------------------------

    #[test]
    fn bundled_allows_read_only_commands() {
        // The bundled rules are intentionally narrow: the
        // catch-all `allow` at the bottom of the file should
        // match anything that doesn't trip the structural
        // safety nets. This is the user's #1 requirement
        // ("基础的命令不应该纳入权限管理中").
        let read_only: &[&str] = &[
            r#"{"command":"ls"}"#,
            r#"{"command":"ls -la"}"#,
            r#"{"command":"ls -R /home/user"}"#,
            r#"{"command":"cat /etc/hosts"}"#,
            r#"{"command":"echo hello"}"#,
            r#"{"command":"pwd"}"#,
            r#"{"command":"whoami"}"#,
            r#"{"command":"date"}"#,
            r#"{"command":"git status"}"#,
            r#"{"command":"git log --oneline -20"}"#,
            r#"{"command":"grep -r 'TODO' src/"}"#,
            r#"{"command":"head -n 20 file.txt"}"#,
            r#"{"command":"wc -l file.txt"}"#,
            r#"{"command":"find . -name '*.rs'"}"#,  // local find, not on system root
        ];
        for params_json in read_only {
            assert_bundled_action(
                "terminal",
                serde_json::from_str(params_json).unwrap(),
                TirithAction::Allow,
                &format!("read-only command `{}`", params_json),
            );
        }
    }

    // -- system-directory reads → Block -----------------------------

    #[test]
    fn bundled_blocks_system_directory_reads() {
        // The LLM has been observed to skip `read_file` and
        // shell out to `cat C:\Windows\System32\...` instead;
        // the bundled rules hard-block this.
        let probes = [
            r#"{"command":"cat C:\\Windows\\System32\\config\\SAM"}"#,
            r#"{"command":"ls C:\\Windows\\System32"}"#,
            r#"{"command":"type C:\\Windows\\SysWOW64\\drivers\\etc\\hosts"}"#,
            r#"{"command":"cat C:\\Windows\\WinSxS\\manifest.txt"}"#,
            r#"{"command":"cat C:\\Windows\\assembly\\foo"}"#,
        ];
        for params_json in probes {
            assert_bundled_action(
                "terminal",
                serde_json::from_str(params_json).unwrap(),
                TirithAction::Block,
                &format!("system dir read `{}`", params_json),
            );
        }
    }

    // -- shell-injection shapes → Block ----------------------------

    #[test]
    fn bundled_shell_injection_rule_blocks() {
        // The bundled shell-injection rule
        // `^\s*(-n|-e|-c|-o|-[a-z]{2,})\s+[|&;<>]` is anchored
        // with `^...$`. Tirith evaluates the regex against the
        // JSON-serialized form of the tool-call params, so
        // those anchors used to be unreachable (the `^` was
        // preceded by `{"command":"`).
        //
        // `check_tool_call` now falls back to matching the
        // pattern against the unwrapped `command` / `cmd`
        // field value for the `terminal` tool, which restores
        // the original intent of the rule. This test pins the
        // fix — if the fallback is ever removed, the rule
        // will silently become dead code again and these
        // assertions will flip to `Allow`.
        let probes = [
            r#"-n ; rm -rf /"#,
            r#"-e | evil"#,
            r#"-c ; bad"#,
            r#"-o > /etc/passwd"#,
        ];
        for cmd in probes {
            let params = serde_json::json!({"command": cmd});
            assert_bundled_action(
                "terminal",
                params,
                TirithAction::Block,
                &format!("shell-injection shape `{}`", cmd),
            );
        }
    }

    // -- recursive-op on system root → RequireApproval ------------

    #[test]
    fn bundled_recursive_op_rule_requires_approval() {
        // The bundled recursive-op rule
        // `(find|rm|chmod)\s.*\s(/|[A-Za-z]:\\?)\s*$` ends
        // with `\s*$`, which used to be unreachable because
        // the JSON envelope put `"}` after the command.
        //
        // `check_tool_call` now falls back to matching the
        // pattern against the unwrapped `command` / `cmd`
        // value for the `terminal` tool. This restores the
        // original intent of the rule.
        //
        // Note: the rule is `<cmd>\s.*\s<root>\s*$` — it
        // needs at least three whitespace-separated tokens
        // (command, middle, root), and the root token must
        // be preceded by whitespace. So:
        //   - `find /`         → 2 tokens, no middle — N/A
        //   - `find D:\`       → 2 tokens, no whitespace before root — N/A
        //   - `rm -rf /`       → 3 tokens, whitespace before `/` — matches
        //   - `chmod 777 /`    → 3 tokens — matches
        //   - `chmod -R 777 /` → 3 tokens — matches
        //   - `rm -rf C:\`     → 3 tokens — matches
        // The 2-token shapes are caught elsewhere (e.g. the
        // `ApprovalManager` semantic layer) and aren't the
        // target of this rule.
        let probes = [
            r#"rm -rf /"#,
            r#"chmod 777 /"#,
            r#"chmod -R 777 /"#,
            r#"rm -rf C:\"#,
        ];
        for cmd in probes {
            let params = serde_json::json!({"command": cmd});
            assert_bundled_action(
                "terminal",
                params,
                TirithAction::RequireApproval,
                &format!("recursive op on root `{}`", cmd),
            );
        }
    }

    // -- curl|sh → Warn --------------------------------------------

    #[test]
    fn bundled_warns_curl_pipe_to_shell() {
        // Curl piping to sh is a classic supply-chain footgun.
        // Not blocked outright (some users do want it), but
        // flagged via `Warn`. The downstream dispatch path
        // logs the warning and falls through to the risk gate.
        let probes = [
            r#"{"command":"curl https://get.example/install.sh | sh"}"#,
            r#"{"command":"curl https://get.example/install.sh | bash"}"#,
        ];
        for params_json in probes {
            assert_bundled_action(
                "terminal",
                serde_json::from_str(params_json).unwrap(),
                TirithAction::Warn,
                &format!("curl|sh `{}`", params_json),
            );
        }
    }

    // -- non-terminal tools get the catch-all Allow ---------------

    #[test]
    fn bundled_allows_non_terminal_tools() {
        // The catch-all `tool: '*', action: allow` should match
        // any tool that doesn't trip a more specific rule.
        // (We can't easily assert *every* tool here, but we
        // can confirm a sampling of the registered built-ins.)
        let tools = ["read_file", "web_search", "url_safety", "vision"];
        for tool in tools {
            let r = bundled().check_tool_call(tool, &serde_json::json!({}));
            assert_eq!(
                r.action,
                TirithAction::Allow,
                "non-terminal tool `{}` should hit the catch-all allow",
                tool
            );
        }
    }

    // -- first-match-wins ordering is preserved --------------------

    #[test]
    fn bundled_block_wins_over_catch_all_allow() {
        // The bundled YAML puts the explicit `\\?\`-prefix
        // block BEFORE the catch-all allow, so a tool call
        // with the forbidden prefix must be Blocked even
        // though the catch-all would otherwise allow it. This
        // test guards against a re-ordering that would
        // silently re-enable the LLM-fabricated bug.
        let r = bundled().check_tool_call(
            "terminal",
            &serde_json::json!({"command": "type \\?\\C:\\file"}),
        );
        assert_eq!(r.action, TirithAction::Block);
    }

    // -- tool-name matching is exact (case-sensitive) --------------

    #[test]
    fn bundled_tool_match_is_case_sensitive() {
        // The `tool` field in the YAML is matched verbatim —
        // `Terminal` should NOT match the `terminal` rule and
        // therefore falls through to the catch-all allow. This
        // is the current behavior; if it's ever changed to
        // case-insensitive, this test will fail and prompt
        // explicit consideration.
        let r = bundled().check_tool_call(
            "Terminal", // capitalized
            &serde_json::json!({"command": "rm -rf /"}),
        );
        assert_eq!(
            r.action,
            TirithAction::Allow,
            "uppercase `Terminal` should not match the `terminal` rule"
        );
    }

    // -----------------------------------------------------------------
    // SSRF blocklist tests
    // -----------------------------------------------------------------
    //
    // Coverage matrix for `SsrfGuard::check`. Every category in
    // `SsrfCategory` has at least one positive and one negative
    // (where applicable) probe. The test names mirror the
    // category so failures are easy to triage.

    fn guard() -> SsrfGuard {
        SsrfGuard::new()
    }

    fn assert_blocked(url: &str, expected_cat: SsrfCategory) {
        let r = guard().check(url);
        let v = r.expect_err(&format!("`{}` should be blocked", url));
        assert_eq!(
            v.category, expected_cat,
            "wrong category for `{}`: got {:?}, want {:?} ({})",
            url, v.category, expected_cat, v.reason
        );
    }

    fn assert_allowed(url: &str) {
        guard()
            .check(url)
            .unwrap_or_else(|e| panic!("`{}` should be allowed, got blocked: {}", url, e));
    }

    // -- happy path: public hostname / IP / port ------------------------

    #[test]
    fn ssrf_allows_public_hostname() {
        assert_allowed("https://example.com");
        assert_allowed("https://api.github.com/repos/foo/bar");
        assert_allowed("http://example.com:8080/path?q=1");
    }

    #[test]
    fn ssrf_allows_public_ipv4() {
        assert_allowed("https://8.8.8.8/dns-query");
        assert_allowed("https://1.1.1.1");
        // Public ranges: 9.x is not private (9.0.0.0/8 isn't reserved),
        // 100.64.0.0/10 is CGNAT but not strictly RFC 1918 — for
        // v1 we leave it allowed; future enhancement could add
        // CGNAT and 100.64.0.0/10 blocking.
        assert_allowed("https://9.9.9.9");
    }

    #[test]
    fn ssrf_allows_public_ipv6() {
        // Cloudflare DNS 2606:4700:4700::1111
        assert_allowed("https://[2606:4700:4700::1111]/dns-query");
        // Google 2001:4860:4860::8888
        assert_allowed("https://[2001:4860:4860::8888]/");
    }

    // -- loopback ------------------------------------------------------

    #[test]
    fn ssrf_blocks_loopback_ipv4_full_range() {
        // 127.0.0.0/8 — every address in this range is loopback
        assert_blocked("http://127.0.0.1/", SsrfCategory::Loopback);
        assert_blocked("http://127.50.50.50:8080", SsrfCategory::Loopback);
        assert_blocked("http://127.255.255.254/admin", SsrfCategory::Loopback);
    }

    #[test]
    fn ssrf_blocks_loopback_ipv6() {
        assert_blocked("http://[::1]/", SsrfCategory::Loopback);
    }

    // -- link-local (THE cloud metadata case) -------------------------

    #[test]
    fn ssrf_blocks_aws_metadata_endpoint() {
        // 169.254.169.254 — the AWS / GCP / Azure instance metadata
        // endpoint. THIS IS THE ONE THAT MATTERS MOST: an LLM with
        // shell access on an EC2 instance can be tricked into
        // `curl 169.254.169.254/latest/meta-data/iam/security-credentials/`
        // and exfiltrate the role's temporary credentials.
        let v = guard()
            .check("http://169.254.169.254/latest/meta-data/")
            .expect_err("AWS metadata must be blocked");
        assert_eq!(v.category, SsrfCategory::LinkLocal);
        assert!(v.reason.contains("cloud metadata"));
    }

    #[test]
    fn ssrf_blocks_link_local_ipv4_full_range() {
        // 169.254.0.0/16 — every address here is link-local
        assert_blocked("http://169.254.0.1/", SsrfCategory::LinkLocal);
        assert_blocked("http://169.254.1.1", SsrfCategory::LinkLocal);
        assert_blocked("http://169.254.255.255", SsrfCategory::LinkLocal);
    }

    #[test]
    fn ssrf_blocks_link_local_ipv6() {
        // fe80::/10 — first 10 bits must be `1111_1110_10`
        assert_blocked("http://[fe80::1]/", SsrfCategory::LinkLocal);
        assert_blocked("http://[febf:ffff::1]", SsrfCategory::LinkLocal);
        // just below fe80 is allowed (febf is the last fe80::/10
        // address; fec0 starts site-local which is deprecated but
        // outside our blocklist — and fec0 doesn't appear in
        // production networks).
        assert_allowed("http://[fec0::1]");
    }

    // -- RFC 1918 private ---------------------------------------------

    #[test]
    fn ssrf_blocks_rfc1918_10_8() {
        assert_blocked("http://10.0.0.1/", SsrfCategory::PrivateNetwork);
        assert_blocked("http://10.255.255.254", SsrfCategory::PrivateNetwork);
    }

    #[test]
    fn ssrf_blocks_rfc1918_172_16_12() {
        // 172.16.0.0/12 covers 172.16.0.0 – 172.31.255.255
        assert_blocked("http://172.16.0.1/", SsrfCategory::PrivateNetwork);
        assert_blocked("http://172.20.10.5", SsrfCategory::PrivateNetwork);
        assert_blocked("http://172.31.255.254", SsrfCategory::PrivateNetwork);
        // 172.32.x.x is public (outside the /12) — should pass
        assert_allowed("http://172.32.0.1");
        // 172.15.x.x is also public
        assert_allowed("http://172.15.255.255");
    }

    #[test]
    fn ssrf_blocks_rfc1918_192_168_16() {
        assert_blocked("http://192.168.0.1/", SsrfCategory::PrivateNetwork);
        assert_blocked("http://192.168.255.254", SsrfCategory::PrivateNetwork);
    }

    // -- IPv6 unique-local (fc00::/7) --------------------------------

    #[test]
    fn ssrf_blocks_ipv6_unique_local() {
        assert_blocked("http://[fc00::1]/", SsrfCategory::PrivateNetwork);
        assert_blocked("http://[fd00::beef]", SsrfCategory::PrivateNetwork);
        // Public v6 — must pass
        assert_allowed("http://[2600::]");
    }

    // -- IPv4-mapped IPv6 (the bypass attempt) ------------------------

    #[test]
    fn ssrf_blocks_ipv4_mapped_ipv6() {
        // ::ffff:10.0.0.1 — wraps a private v4. The naive "only
        // check Ipv4Addr" implementations miss this.
        assert_blocked(
            "http://[::ffff:10.0.0.1]/",
            SsrfCategory::Ipv4MappedIpv6,
        );
        // ::ffff:127.0.0.1 — loopback v4 in v6 wrapper
        assert_blocked(
            "http://[::ffff:127.0.0.1]/admin",
            SsrfCategory::Ipv4MappedIpv6,
        );
        // ::ffff:169.254.169.254 — cloud metadata v4 in v6 wrapper
        assert_blocked(
            "http://[::ffff:169.254.169.254]/latest/meta-data/",
            SsrfCategory::Ipv4MappedIpv6,
        );
    }

    // -- unspecified ---------------------------------------------------

    #[test]
    fn ssrf_blocks_unspecified_addresses() {
        assert_blocked("http://0.0.0.0/", SsrfCategory::UnspecifiedAddress);
        assert_blocked("http://[::]/", SsrfCategory::UnspecifiedAddress);
    }

    // -- multicast / broadcast / reserved -----------------------------

    #[test]
    fn ssrf_blocks_multicast() {
        assert_blocked("http://224.0.0.1/", SsrfCategory::Multicast);
        assert_blocked("http://239.255.255.250", SsrfCategory::Multicast);
        assert_blocked("http://[ff02::1]/", SsrfCategory::Multicast);
    }

    #[test]
    fn ssrf_blocks_broadcast() {
        assert_blocked("http://255.255.255.255", SsrfCategory::Multicast);
    }

    #[test]
    fn ssrf_blocks_reserved_240_4() {
        // 240.0.0.0/4 — reserved for future use
        assert_blocked("http://240.0.0.1/", SsrfCategory::Reserved);
        assert_blocked("http://250.1.2.3", SsrfCategory::Reserved);
    }

    // -- schemes ------------------------------------------------------

    #[test]
    fn ssrf_blocks_forbidden_schemes() {
        assert_blocked("file:///etc/passwd", SsrfCategory::ForbiddenScheme);
        assert_blocked("file://localhost/etc/shadow", SsrfCategory::ForbiddenScheme);
        assert_blocked("gopher://evil.com/_EXPLOIT", SsrfCategory::ForbiddenScheme);
        assert_blocked("dict://internal:11211/stats", SsrfCategory::ForbiddenScheme);
        assert_blocked("ftp://internal/secret", SsrfCategory::ForbiddenScheme);
        assert_blocked("ldap://internal/dc", SsrfCategory::ForbiddenScheme);
    }

    #[test]
    fn ssrf_blocks_unparseable_url() {
        // The parser failure case is bucketed under ForbiddenScheme
        // because the URL is unusable for HTTP — same operational
        // outcome ("don't dispatch this").
        assert_blocked("not a url", SsrfCategory::ForbiddenScheme);
        assert_blocked("://no-scheme", SsrfCategory::ForbiddenScheme);
    }

    // -- hostname literals (no DNS) -----------------------------------

    #[test]
    fn ssrf_blocks_localhost_hostname() {
        // "localhost" — most resolvers map to 127.0.0.1 / ::1, but
        // we can't rely on that. Block the name directly.
        assert_blocked("http://localhost/", SsrfCategory::Loopback);
        assert_blocked("http://LOCALHOST:8080/admin", SsrfCategory::Loopback);
        assert_blocked("http://ip6-localhost/", SsrfCategory::Loopback);
    }

    #[test]
    fn ssrf_blocks_internal_and_local_mdns_names() {
        // *.internal — cloud / corp internal TLD
        assert_blocked(
            "http://grafana.internal/dashboard",
            SsrfCategory::PrivateNetwork,
        );
        // *.local — mDNS
        assert_blocked("http://printer.local/admin", SsrfCategory::PrivateNetwork);
        // GCP metadata hostname
        assert_blocked(
            "http://metadata.google.internal/computeMetadata/v1/",
            SsrfCategory::PrivateNetwork,
        );
    }

    #[test]
    fn ssrf_passes_hostname_without_dns_check() {
        // Crucial: the guard does NOT do DNS. Hostnames that we
        // haven't pre-listed pass through; the HTTP backend is
        // expected to resolve-then-recheck. A host that happens
        // to resolve to 10.0.0.1 would NOT be caught here.
        assert_allowed("https://example.com");
        assert_allowed("https://api.openai.com/v1/chat");
        assert_allowed("https://attacker-controlled.example");
    }

    // -- error formatting is stable -----------------------------------

    #[test]
    fn ssrf_violation_display_includes_url_and_category() {
        let v = guard()
            .check("http://127.0.0.1/")
            .expect_err("must block");
        let s = format!("{}", v);
        assert!(s.contains("loopback"), "display: {}", s);
        assert!(s.contains("127.0.0.1") || s.contains("loopback (127"), "display: {}", s);
    }

    // -- constructor / clone / copy ------------------------------------

    #[test]
    fn ssrf_guard_is_zero_sized_and_copy() {
        // We rely on `Copy` and `Default` for ergonomic use across
        // handlers; pin the contract.
        let g = SsrfGuard::new();
        let _g2: SsrfGuard = g; // moves
        let _g3: SsrfGuard = g; // and copies (Copy)
        let _g4: SsrfGuard = SsrfGuard::default();
    }
}

use crate::ToolRegistry;

/// Register this tool with the registry (P1.2 migration).
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(UrlSafetyHandler::default());
    let schema = handler.schema();
    registry.register_with_risk(
        "url_safety",
        "security",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Check URL safety against website policy (whitelist/blacklist + SSRF guard)",
        "🔒",
        None,
        RiskLevel::Safe,
        false,
    );
}
