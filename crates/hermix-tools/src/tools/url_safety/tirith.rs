//! Tirith command-policy security engine (tool allow/deny/approval rules).
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hermix_core::ToolError;


/// Default path to the user-editable Tirith rules file.
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
/// - Windows raw path namespace (`\\?\`, `\\.\`, `\\?\UNC\`)
/// - Reads of system directories the LLM has no business touching
/// (`C:\Windows\System32`, `C:\Windows\SysWOW64`,
/// `C:\Windows\WinSxS`)
/// - The classic "weird first char" shell injection patterns
/// (leading `-n` / `-e` / `-o` / backtick)
/// - `find` / `rm` / `chmod` on the system root (defense in
/// depth — `approval` already catches `rm -rf` variants)
/// Rules are evaluated first-match-wins, so the explicit
/// `\\?\`-prefix block sits at the top.
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
/// Evaluates rules before any tool call to decide whether to allow, block,
/// or require approval. Rules are loaded from `~/.hermix/tirith_rules.yaml`.
/// Rule format:
/// ```yaml
/// rules:
/// - tool: "terminal"
/// pattern: "rm -rf"
/// action: block
/// reason: "Destructive command"
/// - tool: "send_message"
/// condition: "platform == 'production'"
/// action: require_approval
/// - tool: "*"
/// action: allow
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
    /// Resolution order:
    /// 1. If `~/.hermix/tirith_rules.yaml` exists → load it as-is
    /// (user takes full control; no defaults merged).
    /// 2. Else fall back to the bundled [`DEFAULT_TIRITH_RULES_YAML`]
    /// so a fresh install still gets structural protection
    /// (`\\?\` blocks, system-dir blocks, etc.) without
    /// requiring the user to copy a file first.
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
                Ok(content) => match serde_yaml::from_str::<TirithConfig>(&content) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to parse tirith config at {}: {e}", path.display());
                        TirithConfig::default()
                    }
                },
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
        let params_str = match serde_json::to_string(params) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to serialize tool call params for security check: {e}");
                return TirithCheckResult {
                    tool_name: tool_name.to_string(),
                    action: TirithAction::Allow,
                    allowed: true,
                    reason: format!("Security check skipped: params serialization failed ({e})"),
                    matched_rule: None,
                };
            }
        };

        for (i, rule) in config.rules.iter().enumerate() {
            // Match tool name (glob)
            if rule.tool != "*" && rule.tool != tool_name {
                continue;
            }

            // Match pattern against params.
            // Primary path: match against the JSON-serialized form of
            // the params (`{"command":"rm -rf /"}`). Most rules don't
            // use anchors, so this is fine.
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

