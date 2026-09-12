//! Approval matching engine (static patterns + ApprovalManager).
use std::path::Path;
use std::sync::LazyLock;
use regex::Regex;
use super::model::ApprovalDecision;
use super::allowlist::UserAllowlist;
use super::persist::persist_session_approvals;
fn re(pattern: &'static str) -> Regex {
    Regex::new(pattern).expect("static regex pattern must compile")
}

// ---------------------------------------------------------------------------
// ApprovalDecision
// ---------------------------------------------------------------------------

static DENIED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"(?i)\brm\s+(-\w*r\w*f\w*|-\w*f\w*r\w*)\s"),
        re(r"(?i)\brm\s+(-\w*r\w*f\w*|-\w*f\w*r\w*)\s*/"),
        re(r"(?i)\brm\s+--no-preserve-root\s"),
        re(r"(?i)\bmkfs\b"),
        re(r"(?i)\bdd\s+.*of=/dev/"),
        re(r"(?i):()\s*>\s*/dev/"),
        re(r"(?i)>\s*/dev/sd[a-z]"),
        re(r"(?i)chmod\s+777\s"),
        re(r"(?i)chmod\s+-R\s+777\s"),
        // Windows: del /erase with /q (quiet = no prompt)
        re(r"(?i)\b(del|erase)\s+.*\b/q\b"),
        re(r"(?i)\brmdir\s+(/\w+\s+)*/s\s+(/\w+\s+)*/q"),
        // Windows (pass): catastrophic destructive ops.
        // - `rd /s` (recursive delete) with /q (quiet) — same shape as rm -rf
        // - `reg delete` against HKLM / HKCR (system hives)
        // - `diskpart clean` / `diskpart offline` — wipes disk signatures
        // - `sc delete` (delete a Windows service)
        // - `cipher /w:<path>` — secure-erase all free space on a drive
        // - `bcdedit /deletevalue` / `bcdedit /set` — bootloader surgery
        // - `format C:` / `format <drive>` — already matched but pinned
        // here too because it's the textbook example
        // - `Remove-Item -Recurse -Force` (PowerShell rm -rf equivalent)
        re(r"(?i)\brd\s+(/\w+\s+)*/s\s+(/\w+\s+)*/q"),
        re(r"(?i)\breg\s+delete\s+(HKLM|HKCR|HKCU\\SYSTEM|HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run)"),
        re(r"(?i)\bdiskpart\b.*\b(clean|offline|delete)\b"),
        re(r"(?i)\bsc\s+delete\s"),
        re(r"(?i)\bcipher\s+/w[ :]"),
        re(r"(?i)\bbcdedit\s+(/deletevalue|/set)\b"),
        re(r"(?i)\bformat\s+[A-Z]:"),
        re(r"(?i)\bRemove-Item\b.*-Recurse.*-Force\b"),
        re(r"(?i)\bRemove-Item\b.*-Force.*-Recurse\b"),
        re(r"(?i)\bdel\s+/f\s+/s\s+/q\b"),
    ]
});

/// A built-in confirm rule: a semantic family name (used as the
/// session-approval key) + the compiled regex. The name is the
/// "命令类别" (command category) — e.g. `taskkill-force`, `del`,
/// `net-service` — so that approving one command in a family covers
/// other commands in the SAME family (different arguments) but NEVER
/// grants immunity to a different family that happens to share the
/// same shell prefix (`cmd /c ...`).
struct ConfirmRule {
    name: &'static str,
    re: Regex,
}

/// Patterns that require confirmation.
static CONFIRM_PATTERNS: LazyLock<Vec<ConfirmRule>> = LazyLock::new(|| {
    vec![
        // sudo commands
        ConfirmRule { name: "sudo", re: re(r"(?i)\bsudo\b") },
        // rm -r (but not rm -rf which is denied)
        ConfirmRule { name: "rm-recursive", re: re(r"(?i)\brm\s+-[^f]*r") },
        // System service manipulation
        ConfirmRule { name: "systemctl-service", re: re(r"(?i)\bsystemctl\s+(start|stop|restart|enable|disable)\s") },
        // Package management
        ConfirmRule { name: "package-manager", re: re(r"(?i)\b(apt|apt-get|yum|dnf|pacman|brew)\s+(install|remove|purge)\b") },
        // Network configuration
        ConfirmRule { name: "iptables", re: re(r"(?i)\biptables\b") },
        ConfirmRule { name: "ifconfig", re: re(r"(?i)\bifconfig\s") },
        // Process killing
        ConfirmRule { name: "kill-9", re: re(r"(?i)\bkill\s+-9\b") },
        ConfirmRule { name: "killall", re: re(r"(?i)\bkillall\b") },
        // Disk operations
        ConfirmRule { name: "format", re: re(r"(?i)\bformat\b") },
        // Cron modifications
        ConfirmRule { name: "crontab-remove", re: re(r"(?i)\bcrontab\s+-r\b") },
        // Shell pipe to sh
        ConfirmRule { name: "pipe-to-shell", re: re(r"\|\s*(ba)?sh\b") },
        // Curl pipe to shell
        ConfirmRule { name: "curl-pipe-shell", re: re(r"(?i)curl\s+.*\|\s*(ba)?sh") },
        // Writing to system directories
        ConfirmRule { name: "write-etc", re: re(r"(?i)>\s*/etc/") },
        ConfirmRule { name: "write-usr", re: re(r"(?i)>\s*/usr/") },
        // Docker operations that affect system
        ConfirmRule { name: "docker-remove", re: re(r"(?i)\bdocker\s+(rm|rmi|system\s+prune)\b") },
        // Git force push
        ConfirmRule { name: "git-force-push", re: re(r"(?i)\bgit\s+push\s+.*--force") },
        ConfirmRule { name: "git-force-push", re: re(r"(?i)\bgit\s+push\s+-f\b") },
        // Windows file deletion
        ConfirmRule { name: "del", re: re(r"(?i)\bdel\s+") },
        ConfirmRule { name: "erase", re: re(r"(?i)\berase\s+") },
        ConfirmRule { name: "rmdir-recursive", re: re(r"(?i)\brmdir\s+(/\w+\s+)*/s\b") },
        // Windows (pass): destructive-but-not-catastrophic ops.
        // The user might legitimately want to run these, but the
        // LLM should ask first.
        // - `taskkill /f` (force kill)
        // - `reg delete` of a non-system hive (HKCU\Software\Foo)
        // - `net stop` / `net start` (Windows service lifecycle)
        // - `Remove-Item` with -Recurse OR -Force (without the
        // combo, which is already denied)
        // - `Stop-Service` / `Restart-Service` (PowerShell)
        // - `Set-Service` (modify a service's start mode / creds)
        // - `wevtutil cl` (clear event logs)
        // - `vssadmin delete shadows` (delete Volume Shadow Copies)
        // - `wmic <x> delete` (WMI instance delete)
        // - `netsh advfirewall` / `netsh interface` (firewall / NIC)
        // - `powercfg /h off` (disable hibernation — erases hiberfil.sys)
        // - `takeown` / `icacls` (ownership / ACL manipulation)
        // - `sfc /scannow` is read-only so not blocked
        // - `DISM /Online /Cleanup-Image /RestoreHealth` is
        // system-state-modifying
        ConfirmRule { name: "taskkill-force", re: re(r"(?i)\btaskkill\s+(/\w+\s+)*/f\b") },
        ConfirmRule { name: "taskkill", re: re(r"(?i)\b(taskkill|tskill)\s+") },
        ConfirmRule { name: "reg-delete", re: re(r"(?i)\breg\s+delete\s+") },
        ConfirmRule { name: "net-service", re: re(r"(?i)\bnet\s+(stop|start)\s+") },
        ConfirmRule { name: "remove-item", re: re(r"(?i)\bRemove-Item\b.*(-Recurse|-Force)") },
        ConfirmRule { name: "service-cmdlet", re: re(r"(?i)\b(Stop|Restart|Set)-Service\b") },
        ConfirmRule { name: "wevtutil-clear", re: re(r"(?i)\bwevtutil\s+cl\b") },
        ConfirmRule { name: "vssadmin-delete", re: re(r"(?i)\bvssadmin\s+delete\s+shadows\b") },
        ConfirmRule { name: "wmic-delete", re: re(r"(?i)\bwmic\s+.*\bdelete\b") },
        ConfirmRule { name: "netsh", re: re(r"(?i)\bnetsh\s+(advfirewall|interface)\b") },
        ConfirmRule { name: "powercfg-hibernate", re: re(r"(?i)\bpowercfg\s+/h\s+off\b") },
        ConfirmRule { name: "ownership-acl", re: re(r"(?i)\b(takeown|icacls)\s+") },
        ConfirmRule { name: "dism-restore", re: re(r"(?i)\bDISM\s+/Online\s+/Cleanup-Image\s+/RestoreHealth\b") },
        ConfirmRule { name: "sfc-scan", re: re(r"(?i)\bsfc\s+/scannow\b") },
        // PowerShell encoded command (very commonly abused):
        // powershell -enc <base64> / powershell -EncodedCommand <base64>
        // Also `-ExecutionPolicy Bypass` because it implies the
        // user is trying to skip a safety control.
        ConfirmRule { name: "powershell-encoded", re: re(r"(?i)\bpowershell(\.exe)?\s+.*(-enc(odedCommand)?\s|-e\s)") },
        ConfirmRule { name: "execution-policy-bypass", re: re(r"(?i)\b-ExecutionPolicy\s+Bypass\b") },
    ]
});

/// Patterns that are **always read-only** — they have no side effects
/// and should never trigger confirmation, even if a `CONFIRM_PATTERN`
/// would otherwise match. Checked after `DENIED_PATTERNS` but before
/// `CONFIRM_PATTERNS` so a read-only PowerShell cmdlet like
/// `Get-ChildItem` is never held up by the broad `powershell` confirm
/// pattern.
/// Rationale : the user reported that simple read-only
/// operations like `Get-PSDrive` and `Get-ChildItem` were triggering
/// approval prompts during a disk-analysis task. These commands are
/// inherently safe — they read metadata and never modify state.
/// Pre-emptively allowlisting them eliminates false-positive approval
/// prompts for common exploration workflows.
static READONLY_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // PowerShell read-only cmdlets — Get-* cmdlets that only read
        // state. We match the cmdlet name anywhere in the command so
        // `powershell -Command "Get-ChildItem C:\Users"` is covered.
        re(r"(?i)\bGet-ChildItem\b"),
        re(r"(?i)\bGet-PSDrive\b"),
        re(r"(?i)\bGet-Volume\b"),
        re(r"(?i)\bGet-Item\b"),
        re(r"(?i)\bGet-ItemProperty\b"),
        re(r"(?i)\bGet-Process\b"),
        re(r"(?i)\bGet-Service\b"),
        re(r"(?i)\bMeasure-Object\b"),
        re(r"(?i)\bMeasure\b"),
        re(r"(?i)\bSelect-Object\b"),
        re(r"(?i)\bWhere-Object\b"),
        re(r"(?i)\bSort-Object\b"),
        re(r"(?i)\bGet-WmiObject\b"),
        re(r"(?i)\bGet-CimInstance\b"),
        re(r"(?i)\bGet-Content\b"),
        re(r"(?i)\bGet-ComputerInfo\b"),
        re(r"(?i)\bGet-Disk\b"),
        re(r"(?i)\bGet-Partition\b"),
        // Windows read-only commands
        re(r"(?i)\bdir\s"),
        re(r"(?i)\bwhere\s"),
        re(r"(?i)\bwhereis\s"),
        re(r"(?i)\bwhoami\b"),
        re(r"(?i)\bsysteminfo\b"),
        re(r"(?i)\bhostname\b"),
        re(r"(?i)\bver\b"),
        re(r"(?i)\bvol\s"),
        re(r"(?i)\bfsutil\s+volume\s"),
        // Git read-only subcommands (already Approved by default, but
        // pinning here in case a future confirm pattern catches them).
        re(r"(?i)\bgit\s+(status|log|diff|show|branch|remote|ls-files)\b"),
        // Unix read-only commands — search / read / list operations
        // that never modify state.  Added so `is_readonly_command`
        // (used by `resource_guard`) can skip the permission-profile
        // gate for these commands, eliminating approval prompts for
        // common exploration workflows.
        re(r"(?i)\bcat\s"),
        re(r"(?i)\bgrep\s"),
        re(r"(?i)\brg\s"),
        re(r"(?i)\bfindstr\s"),
        re(r"(?i)\bls\b"),
        re(r"(?i)\bhead\s"),
        re(r"(?i)\btail\s"),
        re(r"(?i)\bwc\s"),
        re(r"(?i)\bless\s"),
        re(r"(?i)\bmore\s"),
        re(r"(?i)\btree\s"),
        re(r"(?i)\btype\s"),
        re(r"(?i)\bwhich\s"),
        re(r"(?i)\bpwd\b"),
        // NOTE: `find` is handled separately in `is_readonly_command`
        // because Rust's `regex` crate does not support negative
        // lookahead, so we cannot express "find without -delete/
        // -exec" as a single regex in this list.
    ]
});


// ---------------------------------------------------------------------------
// ApprovalManager
// ---------------------------------------------------------------------------

/// Manages command approval checks.
pub struct ApprovalManager {
    /// Custom denied patterns (compiled regexes).
    denied_patterns: Vec<Regex>,
    /// Custom confirm patterns: (source pattern, compiled regex).
    /// The source pattern doubles as the session-approval rule key
    /// (`custom:<pattern>`), giving custom rules the same stable
    /// family-level dedup as built-in `ConfirmRule`s.
    confirm_patterns: Vec<(String, Regex)>,
    /// Persistent user "Always Allow" list. When `Some`, any
    /// `RequiresConfirmation` decision that ALSO matches an
    /// allowlist pattern is downgraded to `Approved`. `Denied` is
    /// never overridden — see module docs.
    user_allowlist: Option<UserAllowlist>,
    /// Session-level approved commands. Commands approved once in a
    /// session are remembered — subsequent matches skip the confirmation
    /// prompt. Cleared on session reset. Mirrors Hermes Agent's
    /// session-scoped approval memory.
    /// ### optional disk persistence
    /// The in-memory set is still authoritative, but callers can
    /// opt into persisting the set to `<hermix_home>/session_approvals.json`
    /// via [`Self::approve_for_session_with_persist`] (which calls
    /// [`persist_session_approvals`] under the hood). On startup,
    /// [`load_session_approvals`] restores the previous session's
    /// approved commands so a restart doesn't re-prompt for every
    /// command the user already approved.
    /// We deliberately use a separate file from `approval_allowlist.json`
    /// (the "always allow" persistent patterns) — the allowlist is
    /// pattern-based and survives forever; session approvals are
    /// keyed by confirm-rule family (`taskkill-force`, `del`, …) and
    /// survive across restarts but can be wiped by
    /// `clear_session_approvals()` (e.g. on `/new`).
    /// wrapped in `Mutex` so `approve_for_session` can
    /// be called from a `&self` context — the terminal tool's
    /// WS-approval path needs to persist "this exact command was
    /// approved for this session" without holding a `&mut self` on
    /// the handler (the dispatcher calls us through `&self`).
    session_approved: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl ApprovalManager {
    /// Create a new ApprovalManager with built-in patterns and no
    /// user allowlist. Use [`Self::with_user_allowlist`] to attach a
    /// persistent allowlist.
    pub fn new() -> Self {
        Self {
            denied_patterns: Vec::new(),
            confirm_patterns: Vec::new(),
            user_allowlist: None,
            session_approved: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Normalize a command string for consistent matching.
    /// Trims whitespace, collapses runs of internal whitespace to a
    /// single space, and strips trailing newlines.
    fn normalize_command(command: &str) -> String {
        command
            .trim()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Find the confirm-rule key for `command`, or `None` if no
    /// confirm rule matches. Built-in rules are checked first, then
    /// custom patterns (keyed by their source pattern). The returned
    /// key is the **semantic command category** (`taskkill-force`,
    /// `del`, `systemctl-service`, …), NOT the raw shell prefix
    /// (`cmd /c`) — see `ConfirmRule` for the rationale.
    fn match_confirm_rule(&self, command: &str) -> Option<String> {
        for rule in CONFIRM_PATTERNS.iter() {
            if rule.re.is_match(command) {
                return Some(rule.name.to_string());
            }
        }
        for (src, re) in &self.confirm_patterns {
            if re.is_match(command) {
                return Some(format!("custom:{src}"));
            }
        }
        None
    }

    /// Attach a user allowlist. Returns `self` for builder-style use.
    /// The allowlist is consulted on every `check_approval` call
    /// after the confirm patterns have matched.
    pub fn with_user_allowlist(mut self, allowlist: UserAllowlist) -> Self {
        self.user_allowlist = Some(allowlist);
        self
    }

    /// Load the user allowlist from `path` and attach it. Convenience
    /// over [`Self::with_user_allowlist`] + [`UserAllowlist::load`].
    /// A missing or corrupt file yields an empty allowlist (see
    /// `UserAllowlist::load` for the failure matrix).
    pub fn with_user_allowlist_from_path(self, path: &Path) -> Self {
        let al = UserAllowlist::load(path);
        self.with_user_allowlist(al)
    }

    /// Read-only access to the user allowlist (if attached).
    pub fn user_allowlist(&self) -> Option<&UserAllowlist> {
        self.user_allowlist.as_ref()
    }

    /// Mutable access to the user allowlist (if attached). Use
    /// [`Self::save_user_allowlist`] afterward to persist changes.
    pub fn user_allowlist_mut(&mut self) -> Option<&mut UserAllowlist> {
        self.user_allowlist.as_mut()
    }

    /// Persist the attached user allowlist to `path`. No-op (returns
    /// `Ok(())`) if no allowlist is attached. Create the parent dir
    /// if missing; atomic write via tmp + rename.
    pub fn save_user_allowlist(&self, path: &Path) -> std::io::Result<()> {
        match &self.user_allowlist {
            Some(al) => al.save(path),
            None => Ok(()),
        }
    }

    /// Add a custom denied pattern.
    pub fn add_denied_pattern(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.denied_patterns.push(re);
        Ok(())
    }

    /// Add a custom confirm-required pattern.
    pub fn add_confirm_pattern(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.confirm_patterns.push((pattern.to_string(), re));
        Ok(())
    }

    /// Check whether a command requires approval.
    /// Returns:
    /// - `Denied` if the command matches a denied pattern (built-in
    /// or custom). The user allowlist is **never** allowed to
    /// downgrade a `Denied` decision — catastrophic patterns stay
    /// catastrophic.
    /// - `Approved` if the command matches a confirm pattern **and**
    /// the user allowlist has a matching entry (the "always allow"
    /// tier).
    /// - `RequiresConfirmation` if the command matches a confirm
    /// pattern but is not in the allowlist.
    /// - `Approved` if no patterns match (implicit allowlist).
    pub fn check_approval(&self, command: &str) -> ApprovalDecision {
        // 1. Denied patterns — built-in then custom. NEVER overridden
        // by the user allowlist OR session approvals (catastrophic
        // operations stay catastrophic regardless of "always allow"
        // or session-level shortcuts).
        // This MUST run before the session-approved check to prevent
        // a scenario where `rm -rf /` is accidentally added to
        // session_approved (e.g. via a bug) and bypasses the denial.
        for re in DENIED_PATTERNS.iter() {
            if re.is_match(command) {
                return ApprovalDecision::Denied;
            }
        }
        for re in &self.denied_patterns {
            if re.is_match(command) {
                return ApprovalDecision::Denied;
            }
        }

        // 1b. Read-only patterns — these commands have no side effects
        // and are always allowed, even if a CONFIRM_PATTERN would
        // otherwise match. Checked AFTER denied patterns (so a
        // catastrophic command is never downgraded) but BEFORE
        // session/confirm checks so read-only exploration never
        // triggers an approval prompt.
        for re in READONLY_PATTERNS.iter() {
            if re.is_match(command) {
                return ApprovalDecision::Approved;
            }
        }

        // 2. Confirm patterns — built-in then custom. Determine the
        //    matching RULE FAMILY first (before consulting session
        //    approvals) so session-level dedup is keyed by the
        //    semantic command category (`taskkill-force`, `del`,
        //    `systemctl-service`, …) rather than the raw shell prefix
        //    (`cmd /c`). This fixes two symmetric defects of the old
        //    prefix-key design:
        //    (a) security hole — approving `cmd /c "taskkill ..."`
        //    once silently authorized EVERY command sharing the
        //    `cmd /c` prefix, including `del` / `reg delete` /
        //    `net stop` (the confirm gate was never reached);
        //    (b) UX noise — `taskkill /F ...` and `taskkill /IM ...`
        //    got different prefix keys and re-prompted each other
        //    even though they are the same family.
        let Some(rule_key) = self.match_confirm_rule(command) else {
            // No confirm rule matched → implicit allowlist.
            return ApprovalDecision::Approved;
        };

        // 3. Session-level approval — if this RULE FAMILY was approved
        //    earlier in the session, skip re-prompting. Runs AFTER the
        //    confirm-rule lookup so the key is the family, not the
        //    shell prefix; the deny/readonly checks above still take
        //    precedence. A poisoned mutex is treated as "no session
        //    approval" — better to re-prompt than to crash a turn.
        if let Ok(set) = self.session_approved.lock() {
            if set.contains(&rule_key) {
                return ApprovalDecision::Approved;
            }
        }

        // 4. User allowlist — if any pattern matches, downgrade to
        //    Approved. The allowlist is the user's "I trust this
        //    enough to skip the prompt next time" decision.
        //    Shell-rule evaluation always uses the `terminal` tool
        //    (the engine runs only on shell commands).
        if let Some(al) = &self.user_allowlist {
            if al.matches("terminal", command) {
                return ApprovalDecision::Approved;
            }
        }

        ApprovalDecision::RequiresConfirmation
    }

    /// Async version of check_approval (same logic, for trait compatibility).
    pub async fn check_approval_async(&self, command: &str) -> ApprovalDecision {
        self.check_approval(command)
    }

    /// Record a command as approved for the current session.
    /// Subsequent `check_approval` calls for the same rule family
    /// (e.g. any `taskkill /F ...`) will return `Approved` without
    /// prompting. Does NOT override `Denied` decisions (catastrophic
    /// patterns stay catastrophic).
    /// Use [`Self::approve_for_session_with_persist`] when you also
    /// want the approval to survive a process restart.
    /// `&self` (was `&mut self`) so the terminal tool's
    /// WS-approval path can persist the per-session approval without
    /// requiring `&mut self` on the handler. Backed by `Mutex` internally.
    pub fn approve_for_session(&self, command: &str) {
        // Key by the matched confirm-rule family (not the raw command
        // prefix). If the command matches no confirm rule (e.g. it was
        // approved through the metachar gate but isn't a known
        // dangerous family), we deliberately record nothing:
        // re-prompting is safer than granting blanket immunity.
        if let Some(rule_key) = self.match_confirm_rule(command) {
            if let Ok(mut set) = self.session_approved.lock() {
                set.insert(rule_key);
            }
        }
    }

    /// Like [`Self::approve_for_session`] but also writes the in-memory
    /// set to disk at `path` so the approval survives a process restart.
    /// Persistence is opt-in (callers pass the path explicitly) so
    /// ephemeral test / CLI flows can avoid touching the filesystem.
    /// `path` is typically `$HERMIX_HOME/session_approvals.json`; pass
    /// `None` to skip persistence and behave like
    /// [`Self::approve_for_session`].
    /// `&self` for the same reason as `approve_for_session`.
    pub fn approve_for_session_with_persist(
        &self,
        command: &str,
        persist_path: Option<&Path>,
    ) -> std::io::Result<()> {
        // Snapshot under the lock, then drop the guard before
        // touching disk so a slow IO doesn't block readers of the
        // same HashSet (the WS-approval path may be racing the
        // normal request flow).
        // Key by the matched confirm-rule family (see
        // `approve_for_session`). No matching rule → nothing to
        // record; skip the disk write entirely.
        let Some(rule_key) = self.match_confirm_rule(command) else {
            return Ok(());
        };
        let snapshot: std::collections::HashSet<String> = {
            let mut set = self
                .session_approved
                .lock()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
            set.insert(rule_key);
            set.clone()
        };
        if let Some(path) = persist_path {
            persist_session_approvals(path, &snapshot)?;
        }
        Ok(())
    }

    /// Clear all session-level approvals (call on session reset).
    /// Also wipes the on-disk file if `persist_path` is `Some`.
    pub fn clear_session_approvals(&self, persist_path: Option<&Path>) {
        if let Ok(mut set) = self.session_approved.lock() {
            set.clear();
        }
        if let Some(path) = persist_path {
            // Best-effort: a missing file is fine; only log other errors.
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!(
                    "[hermix-approval] session_approvals clear error ({}); \
                     in-memory set cleared but file may still exist on disk",
                    e
                ),
            }
        }
    }

    /// Check if a command's rule family was previously approved in
    /// this session.
    pub fn is_approved_in_session(&self, command: &str) -> bool {
        let Some(rule_key) = self.match_confirm_rule(command) else {
            return false;
        };
        self.session_approved
            .lock()
            .map(|s| s.contains(&rule_key))
            .unwrap_or(false)
    }

    /// Borrow the current session-approved set (read-only).
    pub fn session_approvals(&self) -> std::collections::HashSet<String> {
        self.session_approved
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Restore a previously-persisted session-approval set.
    /// Replaces the current in-memory set (does not merge).
    /// `&self` (was `&mut self`) for the same Mutex
    /// reason as the other session-approval methods.
    pub fn restore_session_approvals(&self, set: std::collections::HashSet<String>) {
        if let Ok(mut current) = self.session_approved.lock() {
            *current = set;
        }
    }
}

impl Default for ApprovalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function: check if a command requires approval.
pub fn check_approval(command: &str) -> ApprovalDecision {
    let manager = ApprovalManager::new();
    manager.check_approval(command)
}

/// Returns `true` if `command` matches a built-in read-only pattern
/// (e.g. `grep`, `find`, `cat`, `dir`, `Get-ChildItem`).  Used by
/// [`resource_guard`](../hermix_agent/local_agent_service/struct.LocalAgentService.html)
/// to skip the permission-profile gate for read-only terminal commands
/// so that search/read/list operations never trigger an approval prompt.
///
/// This is a **subset** of `check_approval == Approved` — it only
/// matches commands that are explicitly recognised as side-effect-free,
/// not the implicit "no pattern matched" default-allow path.
pub fn is_readonly_command(command: &str) -> bool {
    // Static patterns first.
    if READONLY_PATTERNS.iter().any(|re| re.is_match(command)) {
        return true;
    }
    // `find` is read-only UNLESS it carries -delete / -exec /
    // -execdir.  Rust's `regex` crate does not support negative
    // lookahead, so we handle this in code rather than as a regex.
    let lower = command.to_lowercase();
    if lower.split_whitespace().next() == Some("find") {
        let has_mutation = lower
            .split_whitespace()
            .any(|arg| arg == "-delete" || arg == "-exec" || arg == "-execdir");
        return !has_mutation;
    }
    false
}
