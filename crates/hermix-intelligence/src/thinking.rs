//! L4: 双层思考 L1 / L2 — 快速响应 vs 深度处理
//!
//! 设计哲学：让 Agent 具备"何时该快、何时该深"的判断能力。
//!
//! - **L1 快速思考**：默认路径。短 prompt、零中间步骤、单次 LLM 调用。
//!   适用于：闲聊、简单查询、命令式指令、短事实问答。
//! - **L2 深度思考**：高成本路径。多步推理 + 工具调用 + 自评（self-critique）
//!   + 失败回退到 L1。适用于：复杂分析、跨工具编排、需要多视角权衡。
//!
//! ## 路由策略
//!
//! `select_mode(query, history, config)` 根据以下信号决定：
//! 1. **用户显式标记**：query 含 "深度" / "仔细分析" / "think carefully" 等
//! 2. **query 长度**：> 200 字符倾向 Deep
//! 3. **关键词命中**：含 "对比" / "权衡" / "分析原因" / "analyze" 等
//! 4. **历史 turn 数**：首轮 + 简单短问 → Quick；长对话累积复杂度 → Deep
//! 5. **任务类型**：含 "代码" / "实现" / "build" / "implement" → Deep
//!
//! ## 为什么不引入新模型？
//!
//! 用户哲学"做好自己"：不感知外部资源意味着：
//! - 不切模型（保持单模型，统一路由）
//! - 不下载多模型权重
//! - 不连新 API
//!
//! 双层思考只切换**prompt + 工具预算 + 自评步骤**，模型不变。
//!
//! ## 集成
//!
//! `agent_loop::AgentLoop::run()` 在 `prefetcher.run()` 之后调 `select_mode`，
//! 把 mode 注入 `ctx`，让后续 prompt 构建 / 工具执行 / 自评步骤按 mode 走分支。

use serde::{Deserialize, Serialize};

/// L4 双层思考的两种模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    /// L1 快速：短 prompt、零中间步骤、单次 LLM 调用
    Quick,
    /// L2 深度：多步推理 + 工具调用 + 自评 + 失败回退 L1
    Deep,
}

impl Default for ThinkingMode {
    fn default() -> Self {
        Self::Quick
    }
}

impl ThinkingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Deep => "deep",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "quick" | "l1" | "fast" => Some(Self::Quick),
            "deep" | "l2" | "slow" | "thorough" => Some(Self::Deep),
            _ => None,
        }
    }
}

/// 路由决策的理由（用于 acui directive + tracing）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingReason {
    /// 短闲聊 / 命令式 / 简单事实查询
    ShortOrImperative,
    /// 用户显式标记想深度
    UserExplicitDeep,
    /// query 长度超阈值
    LongQuery,
    /// 命中深度关键词
    DeepKeyword,
    /// 历史 turn 累积到 N 轮以上
    LongHistory,
    /// 任务类型：编码/构建/实现
    CodingTask,
    /// 含问号 + 多子问题
    MultiPartQuestion,
    /// 默认 L1
    Default,
}

impl RoutingReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShortOrImperative => "short_or_imperative",
            Self::UserExplicitDeep => "user_explicit_deep",
            Self::LongQuery => "long_query",
            Self::DeepKeyword => "deep_keyword",
            Self::LongHistory => "long_history",
            Self::CodingTask => "coding_task",
            Self::MultiPartQuestion => "multi_part_question",
            Self::Default => "default",
        }
    }
}

/// select_mode 的完整决策结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub mode: ThinkingMode,
    pub reason: RoutingReason,
    /// 0.0–1.0，对决策的置信度
    pub confidence: f32,
}

impl RoutingDecision {
    pub fn quick(reason: RoutingReason, confidence: f32) -> Self {
        Self {
            mode: ThinkingMode::Quick,
            reason,
            confidence: confidence.clamp(0.0, 1.0),
        }
    }

    pub fn deep(reason: RoutingReason, confidence: f32) -> Self {
        Self {
            mode: ThinkingMode::Deep,
            reason,
            confidence: confidence.clamp(0.0, 1.0),
        }
    }
}

/// L1 / L2 路由的可调参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// query 字符数超过该阈值才考虑 Deep（默认 200）
    pub long_query_threshold: usize,
    /// 历史 turn 数超过该阈值才考虑 Deep（默认 6）
    pub long_history_threshold: usize,
    /// 显式 Deep 触发词（中英文全覆盖）
    pub deep_markers: Vec<String>,
    /// Deep 关键词（命中其一就倾向 Deep）
    pub deep_keywords: Vec<String>,
    /// Coding 任务关键词
    pub coding_keywords: Vec<String>,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            long_query_threshold: 200,
            long_history_threshold: 6,
            deep_markers: vec![
                "深度".into(),
                "仔细分析".into(),
                "深入".into(),
                "认真".into(),
                "think carefully".into(),
                "think deeply".into(),
                "analyze thoroughly".into(),
                "in depth".into(),
            ],
            deep_keywords: vec![
                "对比".into(),
                "权衡".into(),
                "分析原因".into(),
                "比较".into(),
                "多角度".into(),
                "全面分析".into(),
                "complex".into(),
                "trade-off".into(),
                "tradeoff".into(),
                "compare".into(),
                "analyze".into(),
            ],
            coding_keywords: vec![
                "代码".into(),
                "实现".into(),
                "构建".into(),
                "重构".into(),
                "code".into(),
                "implement".into(),
                "build".into(),
                "refactor".into(),
            ],
        }
    }
}

/// 路由决策主函数
/// /// 按以下优先级检查（命中即返回，短路）：
/// 1. 用户显式 Deep 标记 → Deep (confidence 0.95)
/// 2. Coding 任务关键词 → Deep (confidence 0.7)
/// 3. Deep 关键词命中 → Deep (confidence 0.6)
/// 4. query 长度超阈值 → Deep (confidence 0.5)
/// 5. 历史 turn 超阈值 → Deep (confidence 0.5)
/// 6. Multi-part question（≥ 3 个问号或 ≥ 2 个 "并且"）→ Deep (confidence 0.55)
/// 7. 默认 → Quick (confidence 0.4)
pub fn select_mode(
    query: &str,
    history_len: usize,
    config: &ThinkingConfig,
) -> RoutingDecision {
    let query_trim = query.trim();
    let lower = query_trim.to_lowercase();

    // 1. 显式 Deep 标记
    for marker in &config.deep_markers {
        if lower.contains(&marker.to_lowercase()) {
            return RoutingDecision::deep(RoutingReason::UserExplicitDeep, 0.95);
        }
    }

    // 2. Coding 任务
    for kw in &config.coding_keywords {
        if lower.contains(&kw.to_lowercase()) {
            return RoutingDecision::deep(RoutingReason::CodingTask, 0.7);
        }
    }

    // 3. Deep 关键词
    for kw in &config.deep_keywords {
        if lower.contains(&kw.to_lowercase()) {
            return RoutingDecision::deep(RoutingReason::DeepKeyword, 0.6);
        }
    }

    // 4. Long query
    if query_trim.chars().count() > config.long_query_threshold {
        return RoutingDecision::deep(RoutingReason::LongQuery, 0.5);
    }

    // 5. Long history
    if history_len > config.long_history_threshold {
        return RoutingDecision::deep(RoutingReason::LongHistory, 0.5);
    }

    // 6. Multi-part question
    let question_marks = query_trim.matches('?').count() + query_trim.matches('？').count();
    let and_count = lower.matches("并且").count() + lower.matches(" and ").count();
    if question_marks >= 3 || and_count >= 2 {
        return RoutingDecision::deep(RoutingReason::MultiPartQuestion, 0.55);
    }

    // 7. 默认 Quick
    if query_trim.is_empty() || query_trim.chars().count() < 20 {
        RoutingDecision::quick(RoutingReason::ShortOrImperative, 0.6)
    } else {
        RoutingDecision::quick(RoutingReason::Default, 0.4)
    }
}

/// L1 Quick prompt 模板前缀（极简，要求短答）
pub const L1_PROMPT_PREFIX: &str = "\
You are a fast-response assistant. Answer the user's query in 1–3 sentences. \
If the query needs tools or multi-step reasoning, say so briefly and stop. \
Do not elaborate unless asked.";

/// L2 Deep prompt 模板前缀（多步 + 自评）
pub const L2_PROMPT_PREFIX: &str = "\
You are a deep-reasoning assistant. Approach the user's query in steps: \
(1) Restate the problem; (2) Identify constraints; (3) Plan a path; \
(4) Execute via tools if needed; (5) Self-critique your answer. \
If step (5) reveals errors, retry from step (3). Final answer must include \
which steps you took and your confidence level.";

/// 根据 mode 选 prompt 前缀
pub fn prompt_prefix_for(mode: ThinkingMode) -> &'static str {
    match mode {
        ThinkingMode::Quick => L1_PROMPT_PREFIX,
        ThinkingMode::Deep => L2_PROMPT_PREFIX,
    }
}

/// L2 自评 prompt 片段（Deep 模式下，模型首轮回答后追加）
pub const L2_SELF_CRITIQUE: &str = "\
Self-critique: For each claim in your answer above, rate confidence 0.0–1.0. \
Identify any step where you are below 0.7 confidence and either re-research \
or explicitly note the uncertainty to the user.";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ThinkingConfig {
        ThinkingConfig::default()
    }

    // -- ThinkingMode serde + as_str -----------------------------------------

    #[test]
    fn mode_as_str_round_trip() {
        for s in ["quick", "deep"] {
            let m = ThinkingMode::from_str(s).unwrap();
            assert_eq!(m.as_str(), s);
        }
    }

    #[test]
    fn mode_aliases() {
        assert_eq!(ThinkingMode::from_str("l1"), Some(ThinkingMode::Quick));
        assert_eq!(ThinkingMode::from_str("l2"), Some(ThinkingMode::Deep));
        assert_eq!(ThinkingMode::from_str("fast"), Some(ThinkingMode::Quick));
        assert_eq!(ThinkingMode::from_str("thorough"), Some(ThinkingMode::Deep));
        assert_eq!(ThinkingMode::from_str("xxx"), None);
    }

    #[test]
    fn mode_default_is_quick() {
        assert_eq!(ThinkingMode::default(), ThinkingMode::Quick);
    }

    #[test]
    fn mode_serde_snake_case() {
        let j = serde_json::to_string(&ThinkingMode::Quick).unwrap();
        assert_eq!(j, "\"quick\"");
        let j = serde_json::to_string(&ThinkingMode::Deep).unwrap();
        assert_eq!(j, "\"deep\"");
    }

    // -- 1. 显式 Deep 标记 ----------------------------------------------------

    #[test]
    fn user_explicit_chinese_deep() {
        let d = select_mode("请深度分析这个项目", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::UserExplicitDeep);
        assert!(d.confidence > 0.9);
    }

    #[test]
    fn user_explicit_english_deep() {
        let d = select_mode("think carefully about this", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::UserExplicitDeep);
    }

    #[test]
    fn user_explicit_in_depth() {
        let d = select_mode("explain it in depth", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
    }

    // -- 2. Coding 任务 -------------------------------------------------------

    #[test]
    fn coding_keyword_triggers_deep() {
        for q in [
            "帮我写一段代码",
            "implement a function",
            "refactor the parser",
            "build a REST API",
        ] {
            let d = select_mode(q, 0, &cfg());
            assert_eq!(d.mode, ThinkingMode::Deep, "q={q}");
            assert_eq!(d.reason, RoutingReason::CodingTask, "q={q}");
        }
    }

    // -- 3. Deep 关键词 -------------------------------------------------------

    #[test]
    fn deep_keyword_compare_triggers_deep() {
        let d = select_mode("对比一下 React 和 Vue 的状态管理", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::DeepKeyword);
    }

    #[test]
    fn deep_keyword_tradeoff_triggers_deep() {
        let d = select_mode("what's the trade-off here?", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
    }

    // -- 4. Long query --------------------------------------------------------

    #[test]
    fn long_query_triggers_deep() {
        let long = "a".repeat(300);
        let d = select_mode(&long, 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::LongQuery);
    }

    #[test]
    fn exactly_at_threshold_is_quick() {
        // 200 chars threshold — at 200, should still be Quick (strict >).
        let at = "a".repeat(200);
        let d = select_mode(&at, 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Quick, "at 200 should be Quick, got {:?}", d);
    }

    // -- 5. Long history ------------------------------------------------------

    #[test]
    fn long_history_triggers_deep() {
        let d = select_mode("ok 好的", 7, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::LongHistory);
    }

    // -- 6. Multi-part question -----------------------------------------------

    #[test]
    fn three_question_marks_triggers_deep() {
        let d = select_mode("why?? what's the catch? how does it scale?", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::MultiPartQuestion);
    }

    #[test]
    fn two_and_connects_triggers_deep() {
        let d = select_mode("how does X work and why does Y fail and where to start?", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::MultiPartQuestion);
    }

    // -- 7. 默认 Quick --------------------------------------------------------

    #[test]
    fn empty_query_is_quick() {
        let d = select_mode("", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Quick);
    }

    #[test]
    fn short_greeting_is_quick() {
        let d = select_mode("hi 你好", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Quick);
    }

    #[test]
    fn short_factual_is_quick() {
        let d = select_mode("Rust 是什么？", 0, &cfg());
        assert_eq!(d.mode, ThinkingMode::Quick);
    }

    // -- Confidence clamp -----------------------------------------------------

    #[test]
    fn confidence_clamps_to_unit_range() {
        let d = RoutingDecision::quick(RoutingReason::Default, 5.0);
        assert!(d.confidence <= 1.0);
        let d = RoutingDecision::deep(RoutingReason::Default, -1.0);
        assert!(d.confidence >= 0.0);
    }

    // -- prompt 前缀 ----------------------------------------------------------

    #[test]
    fn quick_prompt_is_shorter() {
        assert!(L1_PROMPT_PREFIX.len() < L2_PROMPT_PREFIX.len());
    }

    #[test]
    fn prompt_prefix_for_returns_right_text() {
        assert!(prompt_prefix_for(ThinkingMode::Quick).contains("fast-response"));
        assert!(prompt_prefix_for(ThinkingMode::Deep).contains("deep-reasoning"));
    }

    // -- 自评 prompt 不为空 ---------------------------------------------------

    #[test]
    fn self_critique_prompt_non_empty() {
        assert!(L2_SELF_CRITIQUE.contains("confidence"));
        assert!(L2_SELF_CRITIQUE.contains("0.7"));
    }

    // -- 路由决策 serde roundtrip --------------------------------------------

    #[test]
    fn routing_decision_serde_roundtrip() {
        let d = RoutingDecision::deep(RoutingReason::UserExplicitDeep, 0.95);
        let j = serde_json::to_string(&d).unwrap();
        let back: RoutingDecision = serde_json::from_str(&j).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn routing_reason_serde_snake_case() {
        let j = serde_json::to_string(&RoutingReason::LongHistory).unwrap();
        assert_eq!(j, "\"long_history\"");
    }

    // -- 自定义 config -------------------------------------------------------

    #[test]
    fn custom_config_short_threshold() {
        let mut c = cfg();
        c.long_query_threshold = 10;
        let d = select_mode("a somewhat medium query here", 0, &c);
        assert_eq!(d.mode, ThinkingMode::Deep);
        assert_eq!(d.reason, RoutingReason::LongQuery);
    }

    #[test]
    fn custom_config_no_markers() {
        let mut c = cfg();
        c.deep_markers.clear();
        c.coding_keywords.clear();
        c.deep_keywords.clear();
        let d = select_mode("请深度分析", 0, &c);
        // 没有显式 marker 了 → 走默认 → Quick
        assert_eq!(d.mode, ThinkingMode::Quick);
    }
}
