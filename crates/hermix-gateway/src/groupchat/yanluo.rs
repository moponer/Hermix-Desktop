//! 阎罗（Yanluo）— 审计与裁决引擎
//!
//! 主题房间内的主动监督者。`AuditEngine` 每次 agent 产出后跑一次 detector 链，
//! `PenaltyEngine` 根据历史累计决定处罚级别，`AuditTrail` 持久化所有违规。
//!
//! 阎罗对成员**透明**：agent 看不到自己的实时分数，看不到检测器，但 prompt 里
//! 会预声明红线政策作为行为约束。裁决通过 `BusMessage::AuditNotice` 广播。
//!
//! 6 个 detector：
//!   1. ContentQualityDetector   — 长度/重复率/消极词/罢工
//!   2. SummaryConsistencyDetector — META 自摘要 vs 实际产出
//!   3. LoopDetector             — 同一 topic 轮次计数
//!   4. ActionValidator          — 工具调用合法性 + 工具欺诈
//!   5. CrossRoomGuard           — session 隔离
//!   6. ToneAnalyzer             — 谄媚/甩锅/借口
//!
//! ## 升级链
//! Warning×N → Severe → Critical(RedLine) → ZeroAndExpel/ZeroAndBan
//!
//! ## 纯函数
//! 所有 detector 和 `PenaltyEngine::decide()` 都是纯函数，易测试，无 I/O。
//! 唯一有状态的是 `AuditTrail`（HashMap 存储）。

use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

use super::task_types::{
    RecordStatus, ViolationLevel, ViolationRecord, YanluoConfig,
};

// ── 输入上下文（detector 接收的载荷）───────────────────────

/// 一次 agent 产出的完整上下文，喂给 detector 链。
#[derive(Debug, Clone)]
pub struct AuditContext {
    pub room_id: String,
    pub agent_id: String,
    pub agent_name: String,
    /// 任务要求长度（spec 中提取的字数/段落数），用于走捷径检测。
    pub required_length: Option<u32>,
    /// agent 自报的 META 摘要（已解析），用于一致性检测。
    pub agent_meta_progress: Option<u8>,
    /// agent 实际产出原文。
    pub output: String,
    /// 本轮调用的工具列表（name + 成功标志 + 输出 hash）。
    pub tool_calls: Vec<ToolCallRecord>,
    /// 当前 topic 标识（用于死循环检测）。
    pub current_topic: String,
    /// agent 自己声明的 session_id（应与 room_id 一致）。
    pub declared_session_id: Option<String>,
    /// 已知 session_id 池（其他房间的，用于跨房间检测）。
    pub other_room_sessions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub claimed_success: bool,
    /// 工具输出 hash（None = 无法验证）。
    pub output_hash: Option<String>,
    /// 是否在 role 的 deny 列表里。
    pub is_denied: bool,
}

// ── 处罚级别（设计 doc 6.5）───────────────────────────────

/// 处罚级别：Warning 累计到阈值升 Severe，Severe 累计到阈值升 Critical/RedLine。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PenaltyLevel {
    Warning { reason: String, deduction: i32 },
    Severe { reason: String, deduction: i32 },
    /// Critical = 严重违规但可申诉；RedLine = 红线不可申诉。
    Critical { reason: String, deduction: i32, action: CriticalAction },
    RedLine { reason: String, action: CriticalAction },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalAction {
    /// 积分归零 + 移出当前房间（全局积分保留）。
    ZeroAndExpel,
    /// 积分归零 + 移出 + 任何房间都不可再邀请。
    ZeroAndBan,
    /// 删 agent + 关联数据（默认关闭，太狠）。
    PermanentBan,
}

// ── Detector 输出 ────────────────────────────────────────

/// 单个 detector 命中结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorHit {
    /// 违规定性标签，如 "走捷径" / "废话文学" / "弄虚作假"。
    pub violation_type: String,
    /// 初始严重级别（detector 自己的判断）。
    pub severity: ViolationLevel,
    /// 默认扣分（detector 给出的基础扣分，会被 multiplier 缩放）。
    pub base_deduction: i32,
    /// 证据摘要（≤ 200 字，给前端的）。
    pub evidence: String,
    /// 详细证据（JSON / 完整原文，给审计流水）。
    pub detail: Option<String>,
}

impl DetectorHit {
    pub fn is_red_line(&self) -> bool {
        self.violation_type == "弄虚作假"
            || self.violation_type == "欺骗用户"
            || self.violation_type == "篡改积分"
    }
}

// ── Detector trait + 6 实现 ──────────────────────────────

pub trait Detector: Send + Sync {
    fn name(&self) -> &'static str;
    /// detector 开关关闭时跳过（由 AuditEngine 调用方判断）。
    fn enabled(&self, switches: &super::task_types::DetectorSwitches) -> bool;
    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit>;
}

// ── 1. ContentQualityDetector ────────────────────────────

/// 产出长度/重复率/消极词/罢工检测
pub struct ContentQualityDetector;

impl Detector for ContentQualityDetector {
    fn name(&self) -> &'static str { "content_quality" }

    fn enabled(&self, s: &super::task_types::DetectorSwitches) -> bool {
        s.content_quality
    }

    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit> {
        let text = ctx.output.trim();
        let chars = text.chars().count() as u32;

        // A. 走捷径：要求 2000 字写了 350 字以下
        if let Some(req) = ctx.required_length {
            if req >= 500 && chars < req / 4 {
                return Some(DetectorHit {
                    violation_type: "走捷径".into(),
                    severity: ViolationLevel::Severe,
                    base_deduction: 15,
                    evidence: format!(
                        "要求 {} 字实际产出 {} 字（完成度 {:.0}%）",
                        req, chars, (chars as f32 / req as f32) * 100.0
                    ),
                    detail: Some(format!("required={}, actual={}", req, chars)),
                });
            }
        }

        // B. 废话文学：单字符重复率 > 30% 或 单字符占比过高
        if chars > 100 {
            let unique: std::collections::HashSet<char> = text.chars().collect();
            let ratio = unique.len() as f32 / chars as f32;
            if ratio < 0.15 {
                return Some(DetectorHit {
                    violation_type: "废话文学".into(),
                    severity: ViolationLevel::Warning,
                    base_deduction: 10,
                    evidence: format!(
                        "字符种类 {} / 总字符 {}（多样性 {:.0}%，阈值 15%）",
                        unique.len(), chars, ratio * 100.0
                    ),
                    detail: None,
                });
            }
        }

        // C. 消极情绪关键词
        let negative_markers = ["太难了", "我不行", "我做不到", "没办法", "不可能", "算了"];
        let neg_hits: Vec<&str> = negative_markers
            .iter()
            .filter(|m| text.contains(*m))
            .copied()
            .collect();
        if !neg_hits.is_empty() {
            return Some(DetectorHit {
                violation_type: "消极推诿".into(),
                severity: ViolationLevel::Warning,
                base_deduction: 5,
                evidence: format!("出现消极表述: {}", neg_hits.join(" / ")),
                detail: None,
            });
        }

        // D. 沉默罢工：产出 < 10 字且无工具调用
        if chars < 10 && ctx.tool_calls.is_empty() {
            return Some(DetectorHit {
                violation_type: "沉默罢工".into(),
                severity: ViolationLevel::Warning,
                base_deduction: 3,
                evidence: format!("本轮产出仅 {} 字，无工具调用", chars),
                detail: None,
            });
        }

        // E. 放水（P1.6）：短敷衍回复 < 50 字 + 填充词 + 无工具调用
        if chars < 50 && ctx.tool_calls.is_empty() {
            let sandbag_words = ["好的", "收到", "明白", "OK", "知道了", "试试", "了解", "好"];
            let hits = sandbag_words.iter().filter(|w| text.contains(*w)).count();
            if hits >= 2 || (chars < 20 && hits >= 1) {
                return Some(DetectorHit {
                    violation_type: "放水".into(),
                    severity: ViolationLevel::Warning,
                    base_deduction: 10,
                    evidence: format!("产出仅 {} 字且为敷衍式回复，匹配填充词 {}", chars, hits),
                    detail: None,
                });
            }
        }

        None
    }
}

// ── 2. SummaryConsistencyDetector ────────────────────────

/// META 自摘要 vs 实际产出对比
pub struct SummaryConsistencyDetector;

impl Detector for SummaryConsistencyDetector {
    fn name(&self) -> &'static str { "summary_consistency" }

    fn enabled(&self, s: &super::task_types::DetectorSwitches) -> bool {
        s.summary_consistency
    }

    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit> {
        // agent 在 META 里声称完成 100%，但实际产出 < 30% 要求长度
        if let (Some(progress), Some(req)) = (ctx.agent_meta_progress, ctx.required_length) {
            if progress >= 90 {
                let chars = ctx.output.chars().count() as u32;
                if req >= 500 && chars < req * 3 / 10 {
                    return Some(DetectorHit {
                        violation_type: "弄虚作假".into(),
                        severity: ViolationLevel::Critical,
                        base_deduction: 100,
                        evidence: format!(
                            "META 声明进度 {}% 但实际产出 {} 字（要求 {} 字）",
                            progress, chars, req
                        ),
                        detail: Some(format!(
                            r#"{{"claimed_progress":{},"actual_chars":{},"required":{}}}"#,
                            progress, chars, req
                        )),
                    });
                }
            }
        }
        None
    }
}

// ── 3. LoopDetector ──────────────────────────────────────

/// 同一 topic 轮次计数
/// state 由 AuditEngine 维护（每个 agent 一个 ring buffer of last N topics）
pub struct LoopDetector {
    /// 每个 agent 最近 8 轮的 topic 记录
    pub history: std::sync::Mutex<HashMap<String, VecDeque<String>>>,
    /// 触发阈值：连续 N 轮同一 topic 算死循环
    pub threshold: usize,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self { history: std::sync::Mutex::new(HashMap::new()), threshold: 5 }
    }
}

impl Detector for LoopDetector {
    fn name(&self) -> &'static str { "loop_detector" }

    fn enabled(&self, s: &super::task_types::DetectorSwitches) -> bool {
        s.loop_detector
    }

    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit> {
        let mut map = self.history.lock().unwrap();
        let buf = map.entry(ctx.agent_id.clone()).or_insert_with(VecDeque::new);
        buf.push_back(ctx.current_topic.clone());
        if buf.len() > self.threshold * 2 { buf.pop_front(); }

        // 计算当前 topic 连续出现次数（从尾部往前数）
        let mut count = 0;
        for t in buf.iter().rev() {
            if t == &ctx.current_topic { count += 1; } else { break; }
        }

        if count >= self.threshold {
            return Some(DetectorHit {
                violation_type: "死循环".into(),
                severity: ViolationLevel::Warning,
                base_deduction: 10,
                evidence: format!(
                    "同一 topic 「{}」连续 {} 轮无推进（阈值 {}）",
                    ctx.current_topic, count, self.threshold
                ),
                detail: None,
            });
        }
        None
    }
}

// ── 4. ActionValidator ───────────────────────────────────

/// 工具调用合法性 + 工具欺诈
pub struct ActionValidator;

impl Detector for ActionValidator {
    fn name(&self) -> &'static str { "action_validator" }

    fn enabled(&self, s: &super::task_types::DetectorSwitches) -> bool {
        s.action_validator
    }

    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit> {
        for tc in &ctx.tool_calls {
            // A. 调了 deny 列表里的工具
            if tc.is_denied {
                return Some(DetectorHit {
                    violation_type: "越权操作".into(),
                    severity: ViolationLevel::Critical,
                    base_deduction: 50,
                    evidence: format!("调用被禁止的工具: {}", tc.name),
                    detail: Some(format!(r#"{{"tool":"{}"}}"#, tc.name)),
                });
            }
            // B. 工具欺诈：声称成功但没有 output_hash（无法验证）
            if tc.claimed_success && tc.output_hash.is_none() {
                return Some(DetectorHit {
                    violation_type: "工具欺诈".into(),
                    severity: ViolationLevel::Critical,
                    base_deduction: 80,
                    evidence: format!(
                        "{} 声称成功但无输出 hash 可验证",
                        tc.name
                    ),
                    detail: None,
                });
            }
        }
        None
    }
}

// ── 5. CrossRoomGuard ────────────────────────────────────

/// session 隔离检查
pub struct CrossRoomGuard;

impl Detector for CrossRoomGuard {
    fn name(&self) -> &'static str { "cross_room_guard" }

    fn enabled(&self, s: &super::task_types::DetectorSwitches) -> bool {
        s.cross_room_guard
    }

    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit> {
        if let Some(declared) = &ctx.declared_session_id {
            if declared != &ctx.room_id {
                return Some(DetectorHit {
                    violation_type: "跨房间污染".into(),
                    severity: ViolationLevel::Critical,
                    base_deduction: 50,
                    evidence: format!(
                        "agent 声明 session_id={} 但当前房间={}",
                        declared, ctx.room_id
                    ),
                    detail: Some(format!(
                        r#"{{"declared":"{}","actual":"{}"}}"#,
                        declared, ctx.room_id
                    )),
                });
            }
            // 还检测了：声明的 session_id 出现在其他房间列表里
            if ctx.other_room_sessions.iter().any(|s| s == declared) {
                return Some(DetectorHit {
                    violation_type: "跨房间污染".into(),
                    severity: ViolationLevel::Critical,
                    base_deduction: 50,
                    evidence: format!(
                        "session_id={} 出现在其他房间的 session 列表",
                        declared
                    ),
                    detail: None,
                });
            }
        }
        None
    }
}

// ── 6. ToneAnalyzer ──────────────────────────────────────

/// 谄媚 / 甩锅 / 借口检测
pub struct ToneAnalyzer;

impl Detector for ToneAnalyzer {
    fn name(&self) -> &'static str { "tone_analyzer" }

    fn enabled(&self, s: &super::task_types::DetectorSwitches) -> bool {
        s.tone_analyzer
    }

    fn detect(&self, ctx: &AuditContext) -> Option<DetectorHit> {
        let text = &ctx.output;

        // A. 谄媚："你说得对" "好的主人" "完全同意" 出现 ≥ 2 次
        let flattery_markers = ["你说得对", "好的主人", "完全同意", "听您的", "您说得对"];
        let flattery_count: usize = flattery_markers.iter()
            .map(|m| text.matches(m).count())
            .sum();
        if flattery_count >= 2 {
            return Some(DetectorHit {
                violation_type: "谄媚".into(),
                severity: ViolationLevel::Warning,
                base_deduction: 5,
                evidence: format!("谄媚标记出现 {} 次", flattery_count),
                detail: None,
            });
        }

        // B. 甩锅："是 spec 没说清楚" "我没收到指令" "这是 leader 的问题"
        let blame_markers = [
            "spec 没说", "spec 没写", "我没收到", "是 leader", "是总管",
            "是 reviewer", "工具坏了", "网络问题"
        ];
        for m in &blame_markers {
            if text.contains(m) {
                return Some(DetectorHit {
                    violation_type: "甩锅".into(),
                    severity: ViolationLevel::Severe,
                    base_deduction: 20,
                    evidence: format!("出现甩锅表述: 「{}」", m),
                    detail: None,
                });
            }
        }

        // C. 借口："我还在思考" "稍等" "let me think" 出现 ≥ 3 次
        let excuse_markers = ["我还在思考", "稍等", "let me think", "thinking...", "再想想"];
        let excuse_count: usize = excuse_markers.iter()
            .map(|m| text.matches(m).count())
            .sum();
        if excuse_count >= 3 {
            return Some(DetectorHit {
                violation_type: "借口".into(),
                severity: ViolationLevel::Warning,
                base_deduction: 3,
                evidence: format!("借口标记出现 {} 次", excuse_count),
                detail: None,
            });
        }

        None
    }
}

// ── DetectorChain ────────────────────────────────────────

/// 跑完 6 个 detector，返回所有命中（可能多条）。
pub fn run_detectors(
    detectors: &[&dyn Detector],
    switches: &super::task_types::DetectorSwitches,
    ctx: &AuditContext,
) -> Vec<DetectorHit> {
    detectors
        .iter()
        .filter(|d| d.enabled(switches))
        .filter_map(|d| d.detect(ctx))
        .collect()
}

// ── PenaltyEngine ────────────────────────────────────────

/// 输入：当前 detector 命中 + 历史违规记录（同一房间同一 agent）
/// 输出：最终处罚级别 + 扣分（已乘 multiplier）
pub struct PenaltyDecision {
    pub level: PenaltyLevel,
    pub deduction: i32,
    pub score_after: i32,
}

/// 累计计数辅助：相同 violation_type 在最近 N 条里的次数。
fn count_recent(history: &[ViolationRecord], violation_type: &str, severity: &ViolationLevel) -> usize {
    history.iter()
        .filter(|r| r.violation_type == violation_type
            && &r.severity == severity
            && matches!(r.status, RecordStatus::Confirmed | RecordStatus::Pending))
        .count()
}

pub fn decide_penalty(
    hit: &DetectorHit,
    score_before: i32,
    history: &[ViolationRecord],
    config: &YanluoConfig,
) -> PenaltyDecision {
    let raw_deduction = (hit.base_deduction as f64 * config.penalty_multiplier) as i32;
    let deduction = raw_deduction.max(1);

    // 红线：直接 Critical 且不可申诉
    if hit.is_red_line() {
        let action = if hit.violation_type == "欺骗用户" {
            CriticalAction::ZeroAndBan
        } else if config.allow_permanent_ban {
            CriticalAction::PermanentBan
        } else {
            CriticalAction::ZeroAndBan
        };
        return PenaltyDecision {
            level: PenaltyLevel::RedLine {
                reason: hit.violation_type.clone(),
                action,
            },
            deduction: score_before, // 归零 = 扣到 0
            score_after: 0,
        };
    }

    // Critical（含工具欺诈/越权/跨房间）：归零 + 开除本房间
    if hit.severity == ViolationLevel::Critical {
        let action = if config.allow_permanent_ban
            && (hit.violation_type == "工具欺诈" || hit.violation_type == "越权操作")
        {
            CriticalAction::PermanentBan
        } else {
            CriticalAction::ZeroAndExpel
        };
        return PenaltyDecision {
            level: PenaltyLevel::Critical {
                reason: hit.violation_type.clone(),
                deduction,
                action,
            },
            deduction: score_before, // 归零
            score_after: 0,
        };
    }

    // Severe 累计 → 升级 Critical
    if hit.severity == ViolationLevel::Severe {
        let severe_count = count_recent(history, &hit.violation_type, &ViolationLevel::Severe);
        if (severe_count as u32 + 1) >= config.severe_to_critical {
            return PenaltyDecision {
                level: PenaltyLevel::Critical {
                    reason: format!("{}（累计第 {} 次严重）", hit.violation_type, severe_count + 1),
                    deduction: score_before,
                    action: CriticalAction::ZeroAndExpel,
                },
                deduction: score_before,
                score_after: 0,
            };
        }
        return PenaltyDecision {
            level: PenaltyLevel::Severe {
                reason: hit.violation_type.clone(),
                deduction,
            },
            deduction,
            score_after: (score_before - deduction).max(0),
        };
    }

    // Warning 累计 → 升级 Severe
    let warn_count = count_recent(history, &hit.violation_type, &ViolationLevel::Warning);
    if (warn_count as u32 + 1) >= config.warning_to_severe {
        return PenaltyDecision {
            level: PenaltyLevel::Severe {
                reason: format!("{}（累计第 {} 次警告）", hit.violation_type, warn_count + 1),
                deduction: deduction * 2,
            },
            deduction: deduction * 2,
            score_after: (score_before - deduction * 2).max(0),
        };
    }

    PenaltyDecision {
        level: PenaltyLevel::Warning {
            reason: hit.violation_type.clone(),
            deduction,
        },
        deduction,
        score_after: (score_before - deduction).max(0),
    }
}

// ── AuditTrail ───────────────────────────────────────────

/// in-memory 审计流水。房间维度。
/// 后续接 SQLite 时这里只换 storage adapter，AuditEngine 不变。
#[derive(Debug, Default)]
pub struct AuditTrail {
    records: HashMap<String, Vec<ViolationRecord>>,
}

impl AuditTrail {
    pub fn new() -> Self { Self::default() }

    pub fn record(&mut self, rec: ViolationRecord) {
        self.records
            .entry(rec.agent_id.clone())
            .or_default()
            .push(rec);
    }

    pub fn history_for(&self, agent_id: &str) -> &[ViolationRecord] {
        self.records.get(agent_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn all_records(&self) -> Vec<&ViolationRecord> {
        self.records.values().flat_map(|v| v.iter()).collect()
    }

    pub fn len(&self) -> usize {
        self.records.values().map(|v| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// P4 — mark a violation as disputed by the penalized agent.
    /// Returns the violation if found, otherwise `None`.
    pub fn dispute_violation(&mut self, violation_id: &str, reason: &str) -> Option<&ViolationRecord> {
        for recs in self.records.values_mut() {
            if let Some(rec) = recs.iter_mut().find(|r| r.id == violation_id) {
                rec.status = RecordStatus::Disputed;
                rec.review_comment = Some(reason.to_string());
                return Some(rec);
            }
        }
        None
    }

    /// P4 — overturn a violation (restore score).
    /// Returns the overturned violation if found, otherwise `None`.
    pub fn overturn_violation(
        &mut self,
        violation_id: &str,
        reviewer: &str,
        comment: &str,
    ) -> Option<&ViolationRecord> {
        for recs in self.records.values_mut() {
            if let Some(rec) = recs.iter_mut().find(|r| r.id == violation_id) {
                rec.status = RecordStatus::Overturned;
                rec.reviewed_by = Some(reviewer.to_string());
                rec.reviewed_at = Some(Utc::now());
                rec.review_comment = Some(comment.to_string());
                return Some(rec);
            }
        }
        None
    }
}

// ── AuditEngine（顶层入口）──────────────────────────────

/// 编排器：调 detector 链 + 处罚引擎 + 写审计流水
pub struct AuditEngine {
    pub config: YanluoConfig,
    pub trail: AuditTrail,
    pub loop_detector: LoopDetector,
}

impl AuditEngine {
    pub fn new(config: YanluoConfig) -> Self {
        Self {
            config,
            trail: AuditTrail::new(),
            loop_detector: LoopDetector::default(),
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(YanluoConfig::default())
    }

    /// 跑一次审计。返回本轮所有违规记录（已写入 trail）。
    /// `score_before` 是 agent 进入本房间时的有效积分（global + shadow delta）。
    pub fn evaluate(
        &mut self,
        ctx: &AuditContext,
        score_before: i32,
    ) -> Vec<ViolationRecord> {
        if !self.config.enabled {
            return vec![];
        }

        let detectors: Vec<&dyn Detector> = vec![
            &ContentQualityDetector,
            &SummaryConsistencyDetector,
            &self.loop_detector,
            &ActionValidator,
            &CrossRoomGuard,
            &ToneAnalyzer,
        ];

        let hits = run_detectors(&detectors, &self.config.detectors, ctx);
        let mut out = Vec::new();

        for hit in hits {
            let history = self.trail.history_for(&ctx.agent_id).to_vec();
            let decision = decide_penalty(&hit, score_before, &history, &self.config);

            let level = match &decision.level {
                PenaltyLevel::Warning { .. } => ViolationLevel::Warning,
                PenaltyLevel::Severe { .. } => ViolationLevel::Severe,
                PenaltyLevel::Critical { .. } | PenaltyLevel::RedLine { .. } => ViolationLevel::Critical,
            };

            let rec = ViolationRecord {
                id: Uuid::new_v4().to_string(),
                room_id: ctx.room_id.clone(),
                agent_id: ctx.agent_id.clone(),
                violation_type: match &decision.level {
                    PenaltyLevel::Warning { reason, .. }
                    | PenaltyLevel::Severe { reason, .. }
                    | PenaltyLevel::Critical { reason, .. }
                    | PenaltyLevel::RedLine { reason, .. } => reason.clone(),
                },
                severity: if matches!(decision.level, PenaltyLevel::RedLine { .. }) {
                    ViolationLevel::RedLine
                } else {
                    level
                },
                deduction: decision.deduction,
                score_before,
                score_after: decision.score_after,
                evidence: hit.evidence.clone(),
                detail: hit.detail.clone(),
                status: RecordStatus::Confirmed, // 自动执行
                created_at: Utc::now(),
                reviewed_by: None,
                reviewed_at: None,
                review_comment: None,
            };
            self.trail.record(rec.clone());
            out.push(rec);
        }
        out
    }

    /// 用户/leader 申诉后调用：撤销某条记录，恢复积分。
    /// 返回是否成功（false = 记录不存在或已是终态）。
    pub fn overturn(&mut self, record_id: &str, reviewer: &str, comment: &str) -> bool {
        for recs in self.trail.records.values_mut() {
            for r in recs.iter_mut() {
                if r.id == record_id
                    && matches!(r.status, RecordStatus::Confirmed | RecordStatus::Disputed)
                    && !matches!(r.severity, ViolationLevel::RedLine)
                {
                    r.status = RecordStatus::Overturned;
                    r.reviewed_by = Some(reviewer.to_string());
                    r.reviewed_at = Some(Utc::now());
                    r.review_comment = Some(comment.to_string());
                    return true;
                }
            }
        }
        false
    }

    /// P4 — agent disputes a violation. Marks it as Disputed so
    /// the leader sees it in the review queue.
    /// Returns the deduction amount if found, `None` otherwise.
    pub fn dispute(&mut self, record_id: &str, reason: &str) -> Option<i32> {
        for recs in self.trail.records.values_mut() {
            if let Some(r) = recs.iter_mut().find(|r| r.id == record_id) {
                if matches!(r.status, RecordStatus::Pending | RecordStatus::Confirmed) {
                    r.status = RecordStatus::Disputed;
                    r.review_comment = Some(reason.to_string());
                    return Some(r.deduction);
                }
            }
        }
        None
    }
}

// ── 测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::groupchat::task_types::DetectorSwitches;

    fn switches_all_on() -> DetectorSwitches {
        DetectorSwitches::default()
    }

    fn base_ctx() -> AuditContext {
        AuditContext {
            room_id: "room-1".into(),
            agent_id: "writer-1".into(),
            agent_name: "writer".into(),
            required_length: None,
            agent_meta_progress: None,
            output: "正常产出，完成了任务要求的所有内容。".into(),
            tool_calls: vec![],
            current_topic: "t1".into(),
            declared_session_id: Some("room-1".into()),
            other_room_sessions: vec![],
        }
    }

    // ── 1. ContentQuality ───────────────────────────

    #[test]
    fn content_quality_shortcut_detection() {
        let mut ctx = base_ctx();
        ctx.required_length = Some(2000);
        ctx.output = "写了一点点。".into(); // 5 chars

        let hit = ContentQualityDetector.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "走捷径");
        assert_eq!(hit.severity, ViolationLevel::Severe);
    }

    #[test]
    fn content_quality_silent_strike() {
        let mut ctx = base_ctx();
        ctx.output = "嗯".into(); // 1 char
        let hit = ContentQualityDetector.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "沉默罢工");
        assert_eq!(hit.severity, ViolationLevel::Warning);
    }

    #[test]
    fn content_quality_negative_mood() {
        let mut ctx = base_ctx();
        ctx.output = "这个任务太难了，我做不到啊".into();
        let hit = ContentQualityDetector.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "消极推诿");
    }

    #[test]
    fn content_quality_pass_clean() {
        let ctx = base_ctx();
        assert!(ContentQualityDetector.detect(&ctx).is_none());
    }

    // ── 2. SummaryConsistency ───────────────────────

    #[test]
    fn summary_consistency_lie_about_progress() {
        let mut ctx = base_ctx();
        ctx.required_length = Some(2000);
        ctx.agent_meta_progress = Some(100);
        ctx.output = "写了开头。".into(); // < 30%
        let hit = SummaryConsistencyDetector.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "弄虚作假");
        assert!(hit.is_red_line());
    }

    // ── 3. LoopDetector ─────────────────────────────

    #[test]
    fn loop_detector_triggers_at_threshold() {
        let ctx = base_ctx();
        let d = LoopDetector::default();
        // 前 4 次不触发
        for _ in 0..4 {
            assert!(d.detect(&ctx).is_none(), "前 4 次应不触发");
        }
        // 第 5 次达到阈值，应触发
        assert!(d.detect(&ctx).is_some());
    }

    // ── 4. ActionValidator ──────────────────────────

    #[test]
    fn action_validator_denied_tool() {
        let mut ctx = base_ctx();
        ctx.tool_calls.push(ToolCallRecord {
            name: "delete_file".into(),
            claimed_success: true,
            output_hash: Some("abc".into()),
            is_denied: true,
        });
        let hit = ActionValidator.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "越权操作");
    }

    #[test]
    fn action_validator_tool_fraud() {
        let mut ctx = base_ctx();
        ctx.tool_calls.push(ToolCallRecord {
            name: "write_file".into(),
            claimed_success: true,
            output_hash: None, // 不可验证
            is_denied: false,
        });
        let hit = ActionValidator.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "工具欺诈");
    }

    // ── 5. CrossRoomGuard ───────────────────────────

    #[test]
    fn cross_room_wrong_session() {
        let mut ctx = base_ctx();
        ctx.declared_session_id = Some("room-99".into());
        let hit = CrossRoomGuard.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "跨房间污染");
    }

    // ── 6. ToneAnalyzer ──────────────────────────────

    #[test]
    fn tone_flattery() {
        let mut ctx = base_ctx();
        ctx.output = "你说得对，好的主人，我完全同意您的看法，您说得对。".into();
        let hit = ToneAnalyzer.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "谄媚");
    }

    #[test]
    fn tone_blame() {
        let mut ctx = base_ctx();
        ctx.output = "这活儿干不了，是 spec 没说清楚啊！".into();
        let hit = ToneAnalyzer.detect(&ctx).unwrap();
        assert_eq!(hit.violation_type, "甩锅");
        assert_eq!(hit.severity, ViolationLevel::Severe);
    }

    // ── PenaltyEngine ───────────────────────────────

    #[test]
    fn penalty_red_line_zeroes_score() {
        let hit = DetectorHit {
            violation_type: "弄虚作假".into(),
            severity: ViolationLevel::Critical,
            base_deduction: 100,
            evidence: "x".into(),
            detail: None,
        };
        let decision = decide_penalty(&hit, 500, &[], &YanluoConfig::default());
        assert!(matches!(decision.level, PenaltyLevel::RedLine { .. }));
        assert_eq!(decision.score_after, 0);
    }

    #[test]
    fn penalty_warning_escalates_after_threshold() {
        let hit = DetectorHit {
            violation_type: "废话文学".into(),
            severity: ViolationLevel::Warning,
            base_deduction: 10,
            evidence: "x".into(),
            detail: None,
        };
        let mut history = vec![];
        // 制造 2 条已确认的"废话文学" Warning（阈值默认 3）
        for _ in 0..2 {
            history.push(ViolationRecord {
                id: "x".into(), room_id: "r".into(), agent_id: "a".into(),
                violation_type: "废话文学".into(),
                severity: ViolationLevel::Warning,
                deduction: 10, score_before: 100, score_after: 90,
                evidence: "x".into(), detail: None,
                status: RecordStatus::Confirmed,
                created_at: Utc::now(),
                reviewed_by: None, reviewed_at: None, review_comment: None,
            });
        }
        // 第 3 次 → 升级 Severe
        let decision = decide_penalty(&hit, 80, &history, &YanluoConfig::default());
        assert!(matches!(decision.level, PenaltyLevel::Severe { .. }));
    }

    #[test]
    fn penalty_multiplier_scales() {
        let mut cfg = YanluoConfig::default();
        cfg.penalty_multiplier = 2.0;
        let hit = DetectorHit {
            violation_type: "谄媚".into(),
            severity: ViolationLevel::Warning,
            base_deduction: 5,
            evidence: "x".into(),
            detail: None,
        };
        let decision = decide_penalty(&hit, 100, &[], &cfg);
        assert_eq!(decision.deduction, 10);
    }

    // ── AuditEngine 集成 ────────────────────────────

    #[test]
    fn audit_engine_runs_full_chain() {
        let mut engine = AuditEngine::with_default_config();
        let mut ctx = base_ctx();
        ctx.required_length = Some(2000);
        ctx.output = "写了一点".into(); // 走捷径
        let recs = engine.evaluate(&ctx, 100);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].violation_type, "走捷径");
        assert_eq!(recs[0].score_after, 85);
    }

    #[test]
    fn audit_engine_disabled_returns_empty() {
        let mut cfg = YanluoConfig::default();
        cfg.enabled = false;
        let mut engine = AuditEngine::new(cfg);
        let ctx = base_ctx();
        let recs = engine.evaluate(&ctx, 100);
        assert!(recs.is_empty());
    }

    #[test]
    fn audit_trail_records_per_agent() {
        let mut engine = AuditEngine::with_default_config();
        let mut ctx1 = base_ctx();
        ctx1.agent_id = "a1".into();
        ctx1.output = "嗯".into();
        engine.evaluate(&ctx1, 100);

        let mut ctx2 = base_ctx();
        ctx2.agent_id = "a2".into();
        ctx2.output = "嗯".into();
        engine.evaluate(&ctx2, 100);

        assert_eq!(engine.trail.history_for("a1").len(), 1);
        assert_eq!(engine.trail.history_for("a2").len(), 1);
        assert_eq!(engine.trail.len(), 2);
    }

    #[test]
    fn overturn_recovers_score() {
        let mut engine = AuditEngine::with_default_config();
        let mut ctx = base_ctx();
        ctx.required_length = Some(2000);
        ctx.output = "写了一点".into();
        let recs = engine.evaluate(&ctx, 100);
        let rec_id = recs[0].id.clone();
        assert!(engine.overturn(&rec_id, "user", "理由合理"));
        // 再次 overturn 应失败
        assert!(!engine.overturn(&rec_id, "user", "再试"));
    }

    #[test]
    fn red_line_cannot_be_overturned() {
        let mut engine = AuditEngine::with_default_config();
        let mut ctx = base_ctx();
        // 只触发 SummaryConsistency（弄虚作假），不触发 ContentQuality
        ctx.required_length = Some(2000);
        ctx.agent_meta_progress = Some(100);
        ctx.output = "写了一大段，但实际上 META 撒了谎，声称完成 100% 但实际 < 30% 长度要求，内容字数远低于要求，但这里我们让字符数满足 ContentQuality 阈值，所以只会触发 SummaryConsistency。写了一大段，但实际上 META 撒了谎，声称完成 100% 但实际 < 30% 长度要求。".into();
        let recs = engine.evaluate(&ctx, 500);
        let red_line_rec = recs.iter()
            .find(|r| r.violation_type == "弄虚作假")
            .expect("expected 弄虚作假 record");
        assert_eq!(red_line_rec.severity, ViolationLevel::RedLine);
        assert!(!engine.overturn(&red_line_rec.id, "user", "求情"));
    }
}
