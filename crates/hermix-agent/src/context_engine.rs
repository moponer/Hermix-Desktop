// P8 - Context management & compression algorithm primitives.
// Seven cooperating sub-primitives for the context-engine:
//ContextHealth - degradation diagnosis
//IncrementalSummary - append-style summary deltas
//MaskingStrategy - redact PII / secrets
//FrozenSnapshot - immutable KV-cache snapshot
//ContextFence - protected section markers
//HeaderProtection - lock the first N messages
//FlushIntegration - bind the flush sentinel + diagnostics
// Each is a small data type + a small policy function. The actual
// loader / unloader lives in compression.rs.

use std::collections::VecDeque;

// ===========================================================================
//ContextHealth
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthLevel {
    Green,
    Yellow,
    Red,
    Clamped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextHealth {
    pub level: HealthLevel,
    pub used_tokens: u64,
    pub budget_tokens: u64,
    pub utilization: f32,
    pub reasons: Vec<String>,
}

impl ContextHealth {
    pub fn diagnose(used_tokens: u64, budget_tokens: u64) -> Self {
        let utilization = if budget_tokens == 0 {
            0.0
        } else {
            used_tokens as f32 / budget_tokens as f32
        };
        let (level, mut reasons) = match utilization {
            u if u >= 1.0 => (HealthLevel::Clamped, vec!["used >= budget".to_string()]),
            u if u >= 0.9 => (HealthLevel::Red, vec![">= 90% of budget".to_string()]),
            u if u >= 0.7 => (HealthLevel::Yellow, vec![">= 70% of budget".to_string()]),
            _ => (HealthLevel::Green, vec![]),
        };
        if used_tokens == 0 && budget_tokens > 0 {
            reasons.push("no tokens used (empty session?)".to_string());
        }
        Self {
            level,
            used_tokens,
            budget_tokens,
            utilization,
            reasons,
        }
    }

    pub fn is_actionable(&self) -> bool {
        matches!(self.level, HealthLevel::Yellow | HealthLevel::Red | HealthLevel::Clamped)
    }

    pub fn with_reason(mut self, r: impl Into<String>) -> Self {
        self.reasons.push(r.into());
        self
    }
}

// ===========================================================================
//IncrementalSummary
// ===========================================================================

/// Incremental summarisation: instead of re-summarising the entire
/// history every turn, the agent keeps a running summary and *deltas*
/// of what changed in the latest turn.
#[derive(Debug, Clone, PartialEq)]
pub struct IncrementalSummary {
    /// Running text summary, oldest at index 0, newest last.
    running: String,
    /// Most recent delta to apply on top of `running`.
    delta: String,
    /// Up to N recent deltas for the audit log.
    history: VecDeque<String>,
}

pub const DELTA_HISTORY_CAP: usize = 16;

impl IncrementalSummary {
    pub fn empty() -> Self {
        Self {
            running: String::new(),
            delta: String::new(),
            history: VecDeque::new(),
        }
    }

    pub fn summary(&self) -> &str {
        &self.running
    }

    pub fn last_delta(&self) -> &str {
        &self.delta
    }

    /// Append a delta; the summary regenerates by concatenating. We
    /// do not "rewrite" because the LLM that emitted the delta is
    /// trusted; if you need rewrite semantics, layer it on top.
    pub fn add_delta(&mut self, delta: impl Into<String>) {
        let delta = delta.into();
        if delta.trim().is_empty() {
            return;
        }
        self.delta = delta.clone();
        if !self.running.is_empty() {
            self.running.push(' ');
        }
        self.running.push_str(&delta);
        // Keep up to N deltas; drop the oldest.
        if self.history.len() == DELTA_HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(delta);
    }

    pub fn delta_count(&self) -> usize {
        self.history.len()
    }

    pub fn recent_deltas(&self, n: usize) -> Vec<String> {
        self.history.iter().rev().take(n).cloned().collect()
    }

    /// Trim the running summary to `char_budget` characters, keeping
    /// the *most recent* part (older context is assumed to be already
    /// captured elsewhere).
    pub fn trim_to_budget(&mut self, char_budget: usize) {
        if char_budget == 0 {
            self.running.clear();
            return;
        }
        let total = self.running.chars().count();
        if total <= char_budget {
            return;
        }
        let skip = total - char_budget;
        let mut out = String::with_capacity(char_budget);
        for (i, c) in self.running.chars().enumerate() {
            if i < skip {
                continue;
            }
            out.push(c);
        }
        self.running = out;
    }
}

// ===========================================================================
//MaskingStrategy
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskKind {
    Email,
    IpAddress,
    OpenaiKey,
    AwsAccessKey,
    GithubToken,
    LongHex,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaskedSpan {
    pub kind: MaskKind,
    pub start: usize,
    pub end: usize,
    pub placeholder: String,
}

#[derive(Default, Debug, Clone)]
pub struct MaskingStrategy {
    pub enabled: Vec<MaskKind>,
}

impl MaskingStrategy {
    pub fn new(kinds: &[MaskKind]) -> Self {
        Self {
            enabled: kinds.to_vec(),
        }
    }

    /// Apply masks in-place, returning the spans that were masked.
    pub fn apply(&self, body: &str) -> (String, Vec<MaskedSpan>) {
        if self.enabled.is_empty() {
            return (body.to_string(), Vec::new());
        }
        // Build the regex-equivalent pattern by hand-rolling each
        // kind. We deliberately avoid pulling in a regex crate
        // dependency for this tiny surface.
        let mut spans: Vec<MaskedSpan> = Vec::new();
        let mut hits: Vec<(usize, usize, MaskKind)> = Vec::new();
        for kind in &self.enabled {
            match kind {
                MaskKind::Email => scan(body, b'@', b'.', 3, *kind, &mut hits),
                MaskKind::IpAddress => scan(body, b'.', b'.', 6, *kind, &mut hits),
                MaskKind::OpenaiKey => scan_prefixed(body, "sk-", 30, *kind, &mut hits),
                MaskKind::AwsAccessKey => scan_prefixed(body, "AKIA", 16, *kind, &mut hits),
                MaskKind::GithubToken => scan_prefixed(body, "ghp_", 36, *kind, &mut hits),
                MaskKind::LongHex => scan_long_hex(body, 32, *kind, &mut hits),
            }
        }
        // Sort by start, overlap-merge.
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        let mut merged: Vec<(usize, usize, MaskKind)> = Vec::new();
        for h in hits {
            if let Some(last) = merged.last_mut() {
                if h.0 <= last.1 {
                    last.1 = last.1.max(h.1);
                    continue;
                }
            }
            merged.push(h);
        }
        // Replace from end so byte indices stay valid.
        let mut out = body.to_string();
        for (start, end, kind) in merged.iter().rev() {
            let placeholder = placeholder_for(*kind);
            let span_len = end - start;
            let s = *start;
            if s + span_len <= out.len() {
                out.replace_range(s..s + span_len, &placeholder);
            }
            spans.push(MaskedSpan {
                kind: *kind,
                start: s,
                end: s + span_len,
                placeholder,
            });
        }
        spans.reverse();
        (out, spans)
    }
}

fn scan(
    body: &str,
    delim1: u8,
    delim2: u8,
    min_chunk: usize,
    kind: MaskKind,
    out: &mut Vec<(usize, usize, MaskKind)>,
) {
    let bytes = body.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == delim1 {
            // Look for the second delim within min_chunk..30 chars.
            for j in (i + min_chunk)..bytes.len().min(i + 32) {
                if bytes[j] == delim2 {
                    out.push((i, j + 1, kind));
                    break;
                }
            }
        }
    }
}

fn scan_prefixed(
    body: &str,
    prefix: &str,
    total_len: usize,
    kind: MaskKind,
    out: &mut Vec<(usize, usize, MaskKind)>,
) {
    let bytes = body.as_bytes();
    let pb = prefix.as_bytes();
    let mut i = 0;
    while i + pb.len() <= bytes.len() {
        if &bytes[i..i + pb.len()] == pb {
            // take total_len bytes; clamp at body end.
            let end = (i + total_len).min(bytes.len());
            if end - i >= pb.len() + 4 {
                out.push((i, end, kind));
                i = end;
                continue;
            }
        }
        i += 1;
    }
}

fn scan_long_hex(
    body: &str,
    min_len: usize,
    kind: MaskKind,
    out: &mut Vec<(usize, usize, MaskKind)>,
) {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_hexdigit() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - i >= min_len {
                // Skip masking if this hex run looks like a filename —
                // i.e. it's immediately followed by a dot + 1-5
                // alphanumeric chars (e.g. `ab12cd34...ef.jpg`). Real
                // API keys / tokens never have file extensions after
                // them, but hash-based filenames (MD5=32, SHA1=40)
                // are commonly caught by the 32-char threshold.
                if is_followed_by_file_extension(bytes, j) {
                    i = j;
                    continue;
                }
                out.push((i, j, kind));
                i = j;
                continue;
            }
        }
        i += 1;
    }
}

/// Check if `bytes[pos]` starts a file-extension pattern: a literal `.`
/// followed by 1-5 ASCII alphanumeric characters, then a non-alphanumeric
/// boundary (or end of input). Examples that match: `.jpg`, `.png`,
/// `.webp`, `.json`. This prevents `scan_long_hex` from redacting
/// hash-based filenames like `D:\data\a1b2c3d4...d4.jpg`.
fn is_followed_by_file_extension(bytes: &[u8], pos: usize) -> bool {
    if pos >= bytes.len() || bytes[pos] != b'.' {
        return false;
    }
    let mut k = pos + 1;
    let ext_start = k;
    while k < bytes.len() && bytes[k].is_ascii_alphanumeric() {
        k += 1;
    }
    let ext_len = k - ext_start;
    // Extension must be 1-5 chars and followed by a boundary
    // (end of input, quote, whitespace, path separator, etc.)
    ext_len >= 1 && ext_len <= 5
}

fn placeholder_for(kind: MaskKind) -> String {
    match kind {
        MaskKind::Email => "[REDACTED:email]".to_string(),
        MaskKind::IpAddress => "[REDACTED:ip]".to_string(),
        MaskKind::OpenaiKey => "[REDACTED:openai-key]".to_string(),
        MaskKind::AwsAccessKey => "[REDACTED:aws-key]".to_string(),
        MaskKind::GithubToken => "[REDACTED:gh-token]".to_string(),
        MaskKind::LongHex => "[REDACTED:hex]".to_string(),
    }
}

// ===========================================================================
//FrozenSnapshot
// ===========================================================================

/// Immutable snapshot of the messages vec at a known instant.
/// Useful for KV-cache continuity: a frozen snapshot can be revived
/// even if the live history has diverged since.
#[derive(Debug, Clone, PartialEq)]
pub struct FrozenSnapshot {
    pub version: u64,
    pub captured_at: u64,
    /// Cheap fingerprint (FNV-1a over joined roles).
    pub fingerprint: String,
    /// Message count, not content, to keep the snapshot cheap.
    pub message_count: usize,
    /// First non-system role fingerprint, if any.
    pub first_meaningful_index: Option<usize>,
}

pub fn fingerprint_messages(roles: &[&str]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for r in roles {
        for b in r.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// The default redaction policy used at the runtime boundary
/// (`execute_tool_calls`): API keys, tokens, IPs, emails, long-hex
/// blobs. Centralised here so callers don't have to remember which
/// kinds are safe to ship to the LLM.
pub fn default_redaction_strategy() -> MaskingStrategy {
    MaskingStrategy::new(&[
        MaskKind::OpenaiKey,
        MaskKind::AwsAccessKey,
        MaskKind::GithubToken,
        MaskKind::LongHex,
        MaskKind::IpAddress,
        MaskKind::Email,
    ])
}

/// wire convenience: redact secret-shaped strings out of an
/// arbitrary tool result body. Returns the rewritten text + the spans
/// that were masked (for audit / trace panel rendering).
pub fn redact_tool_result(body: &str) -> (String, Vec<MaskedSpan>) {
    default_redaction_strategy().apply(body)
}

#[derive(Default, Debug)]
pub struct SnapshotStore {
    next_version: u64,
    snapshots: Vec<FrozenSnapshot>,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn freeze(&mut self, roles: &[&str], captured_at: u64) -> FrozenSnapshot {
        self.next_version = self.next_version.saturating_add(1);
        let f = FrozenSnapshot {
            version: self.next_version,
            captured_at,
            fingerprint: fingerprint_messages(roles),
            message_count: roles.len(),
            first_meaningful_index: roles.iter().position(|r| !matches!(r, &"system" | &"")),
        };
        self.snapshots.push(f.clone());
        f
    }

    pub fn latest(&self) -> Option<&FrozenSnapshot> {
        self.snapshots.last()
    }

    pub fn get(&self, version: u64) -> Option<&FrozenSnapshot> {
        self.snapshots.iter().find(|s| s.version == version)
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }
}

// ===========================================================================
//ContextFence
// ===========================================================================

/// Range [start, end) of message indices that MUST survive any
/// compression. Fences are *additive* — multiple fences can be open
/// at once (different roles, different policies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence {
    pub start: usize,
    pub end: usize,
    pub reason: FenceReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceReason {
    ActiveTask,
    PendingTool,
    OperatorPinned,
    AuditRequired,
}

#[derive(Default, Debug)]
pub struct ContextFence {
    fences: Vec<Fence>,
}

impl ContextFence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, f: Fence) {
        if f.start >= f.end {
            return;
        }
        self.fences.push(f);
        // Sort + merge overlapping.
        self.fences.sort_by_key(|f| f.start);
        self.merge_overlaps();
    }

    fn merge_overlaps(&mut self) {
        let mut merged: Vec<Fence> = Vec::new();
        for f in self.fences.drain(..) {
            if let Some(last) = merged.last_mut() {
                if f.start <= last.end {
                    last.end = last.end.max(f.end);
                    continue;
                }
            }
            merged.push(f);
        }
        self.fences = merged;
    }

    pub fn is_protected(&self, idx: usize) -> bool {
        // binary-search lower bound.
        let mut lo = 0;
        let mut hi = self.fences.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.fences[mid].start <= idx {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        // lo is the first fence with start > idx; the previous one (if
        // any) is the candidate.
        if lo == 0 {
            return false;
        }
        let f = &self.fences[lo - 1];
        idx >= f.start && idx < f.end
    }

    pub fn fences(&self) -> &[Fence] {
        &self.fences
    }
}

// ===========================================================================
//HeaderProtection
// ===========================================================================

/// Always keep the first N messages untouched. Use this for the
/// system prompt and the very first user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderProtection(pub usize);

impl HeaderProtection {
    pub fn new(n: usize) -> Self {
        Self(n)
    }

    pub fn keep_count(&self) -> usize {
        self.0
    }

    pub fn is_protected(&self, idx: usize) -> bool {
        idx < self.0
    }
}

// ===========================================================================
//FlushIntegration
// ===========================================================================

/// Cohesive view that binds the flush sentinel context and
/// the context-health diagnostic . The agent loop can call
/// `should_flush_before_compress(health)` and decide on one knob.
#[derive(Debug, Clone, PartialEq)]
pub struct FlushIntegration {
    pub min_health_to_skip: HealthLevel,
    pub flush_min_turns: usize,
    pub flush_already_requested: bool,
}

impl Default for FlushIntegration {
    fn default() -> Self {
        Self {
            min_health_to_skip: HealthLevel::Red,
            flush_min_turns: 6,
            flush_already_requested: false,
        }
    }
}

impl FlushIntegration {
    pub fn should_flush(
        &self,
        health: &ContextHealth,
        message_count: usize,
    ) -> bool {
        if self.flush_already_requested {
            return false;
        }
        if message_count < self.flush_min_turns {
            return false;
        }
        // Health must be at or beyond the configured threshold to
        // warrant the cost of a flush turn.
        let health_passes = matches!(
            health.level,
            HealthLevel::Red | HealthLevel::Clamped
        );
        health_passes && matches!(self.min_health_to_skip, HealthLevel::Red | HealthLevel::Clamped)
    }

    pub fn mark_flushed(&mut self) {
        self.flush_already_requested = true;
    }

    pub fn reset(&mut self) {
        self.flush_already_requested = false;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    //
    #[test]
    fn context_health_levels() {
        assert_eq!(
            ContextHealth::diagnose(0, 1000).level,
            HealthLevel::Green
        );
        let y = ContextHealth::diagnose(750, 1000);
        assert_eq!(y.level, HealthLevel::Yellow);
        assert!(y.is_actionable());
        let r = ContextHealth::diagnose(950, 1000);
        assert_eq!(r.level, HealthLevel::Red);
        let c = ContextHealth::diagnose(1500, 1000);
        assert_eq!(c.level, HealthLevel::Clamped);
    }

    #[test]
    fn context_health_zero_budget_avoids_divzero() {
        let h = ContextHealth::diagnose(0, 0);
        assert_eq!(h.utilization, 0.0);
        assert_eq!(h.level, HealthLevel::Green);
    }

    #[test]
    fn context_health_reasons_appendable() {
        let h = ContextHealth::diagnose(800, 1000).with_reason("user explicitly invoked");
        assert!(h.reasons.len() >= 2);
    }

    //
    #[test]
    fn incremental_summary_accumulates() {
        let mut s = IncrementalSummary::empty();
        s.add_delta("first");
        s.add_delta("second");
        s.add_delta("third");
        assert_eq!(s.summary(), "first second third");
        assert_eq!(s.last_delta(), "third");
        assert_eq!(s.delta_count(), 3);
    }

    #[test]
    fn incremental_summary_skip_empty() {
        let mut s = IncrementalSummary::empty();
        s.add_delta("");
        s.add_delta("   ");
        assert_eq!(s.delta_count(), 0);
        assert_eq!(s.summary(), "");
    }

    #[test]
    fn incremental_summary_trim_keeps_recent() {
        let mut s = IncrementalSummary::empty();
        for i in 0..100 {
            s.add_delta(format!("delta{i}"));
        }
        s.trim_to_budget(20);
        let total = s.summary().chars().count();
        assert!(total <= 20);
        // The newest tail should remain.
        assert!(s.summary().contains("delta99"));
    }

    #[test]
    fn incremental_summary_recent_deltas_in_reverse() {
        let mut s = IncrementalSummary::empty();
        s.add_delta("a");
        s.add_delta("b");
        s.add_delta("c");
        let recent = s.recent_deltas(2);
        assert_eq!(recent, vec!["c", "b"]);
    }

    #[test]
    fn incremental_summary_history_capped() {
        let mut s = IncrementalSummary::empty();
        for i in 0..(DELTA_HISTORY_CAP + 5) {
            s.add_delta(format!("d{i}"));
        }
        assert_eq!(s.delta_count(), DELTA_HISTORY_CAP);
    }

    //
    #[test]
    fn masking_disabled_passthrough() {
        let m = MaskingStrategy::default();
        let (out, spans) = m.apply("emails a@b.c are easy");
        assert_eq!(out, "emails a@b.c are easy");
        assert!(spans.is_empty());
    }

    #[test]
    fn masking_email_basic() {
        let m = MaskingStrategy::new(&[MaskKind::Email]);
        let (out, _) = m.apply("ping alice@example.com please");
        assert!(!out.contains("alice@example.com"));
        assert!(out.contains("ping"));
        assert!(out.contains("[REDACTED:email]"));
    }

    #[test]
    fn masking_email_excludes_too_short() {
        let m = MaskingStrategy::new(&[MaskKind::Email]);
        // 'a@b' has no TLD; should NOT match (we require two '.'s).
        let (out, _) = m.apply("see a@b for info");
        // We only emit a span when both '@' and '.' are within a
        // short window. 'a@b' has no '.' so the email regex skips.
        assert!(out.contains("a@b"));
    }

    #[test]
    fn masking_ip_basic() {
        let m = MaskingStrategy::new(&[MaskKind::IpAddress]);
        let (out, _) = m.apply("server is 192.168.1.42 now");
        assert!(!out.contains("192.168.1.42"));
        assert!(out.contains("[REDACTED:ip]"));
    }

    #[test]
    fn masking_openai_key_starts_with_sk() {
        let m = MaskingStrategy::new(&[MaskKind::OpenaiKey]);
        let (out, _) = m.apply("sk-test1234567890ABCDEFGHIJKLMNOPQRSTUV");
        assert!(!out.contains("sk-test1234567890ABCDEFGHIJKLMNOPQRSTUV"));
    }

    #[test]
    fn masking_overlap_merges() {
        let m = MaskingStrategy::new(&[MaskKind::LongHex]);
        let (out, _) =
            m.apply("here is a 0123456789abcdef0123456789abcdef01234567 hex token");
        assert!(!out.contains("0123456789abcdef0123456789abcdef01234567"));
        assert!(out.contains("[REDACTED:hex]"));
    }

    //
    #[test]
    fn fingerprint_messages_changes_on_role_change() {
        let a = fingerprint_messages(&["user", "assistant", "user"]);
        let b = fingerprint_messages(&["user", "assistant", "tool"]);
        assert_ne!(a, b);
    }

    #[test]
    fn snapshot_store_freeze_increments_version() {
        let mut s = SnapshotStore::new();
        let f1 = s.freeze(&["user", "assistant"], 1);
        let f2 = s.freeze(&["user", "assistant", "tool"], 2);
        assert!(f2.version > f1.version);
        assert_eq!(f2.message_count, 3);
        assert!(s.get(f2.version).is_some());
        assert!(s.latest().is_some());
    }

    #[test]
    fn snapshot_first_meaningful_index_skips_system_and_empty() {
        let mut s = SnapshotStore::new();
        let f = s.freeze(&["system", "", "user", "assistant"], 0);
        assert_eq!(f.first_meaningful_index, Some(2));
    }

    //
    #[test]
    fn fence_protects_in_range() {
        let mut fs = ContextFence::new();
        fs.add(Fence { start: 5, end: 10, reason: FenceReason::ActiveTask });
        fs.add(Fence { start: 20, end: 30, reason: FenceReason::PendingTool });
        assert!(fs.is_protected(6));
        assert!(!fs.is_protected(4));
        assert!(fs.is_protected(25));
        assert!(!fs.is_protected(30));
    }

    #[test]
    fn fence_overlaps_merge() {
        let mut fs = ContextFence::new();
        fs.add(Fence { start: 5, end: 10, reason: FenceReason::ActiveTask });
        fs.add(Fence { start: 8, end: 14, reason: FenceReason::OperatorPinned });
        assert_eq!(fs.fences().len(), 1);
        assert_eq!(fs.fences()[0].start, 5);
        assert_eq!(fs.fences()[0].end, 14);
    }

    #[test]
    fn fence_zero_range_ignored() {
        let mut fs = ContextFence::new();
        fs.add(Fence { start: 10, end: 10, reason: FenceReason::ActiveTask });
        assert!(fs.fences().is_empty());
    }

    //
    #[test]
    fn header_protection_is_inclusive_of_first_n() {
        let h = HeaderProtection::new(3);
        assert!(h.is_protected(0));
        assert!(h.is_protected(2));
        assert!(!h.is_protected(3));
    }

    //
    #[test]
    fn flush_integration_default_skips_under_threshold() {
        let fi = FlushIntegration::default();
        let h = ContextHealth::diagnose(800, 1000); // Yellow
        assert!(!fi.should_flush(&h, 100));
    }

    #[test]
    fn flush_integration_decides_red_does_flush() {
        let mut fi = FlushIntegration::default();
        let h = ContextHealth::diagnose(980, 1000); // Red
        assert!(fi.should_flush(&h, 100));
        fi.mark_flushed();
        assert!(!fi.should_flush(&h, 100)); // second attempt is skipped
        fi.reset();
        assert!(fi.should_flush(&h, 100));
    }

    #[test]
    fn flush_integration_short_session_skipped() {
        let fi = FlushIntegration::default();
        let h = ContextHealth::diagnose(980, 1000);
        assert!(!fi.should_flush(&h, 4));
    }
}
