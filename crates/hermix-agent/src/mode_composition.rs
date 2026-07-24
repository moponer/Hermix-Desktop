//! ModeComposition — 三层正交模式组合器
//!
//! 在 `AgentLoop::run()` 的 Prefetch 阶段调用，将三层模式
//! 组合成一个统一的 `ModeComposition`，供后续阶段使用：
//!
//! - **L1 Domain Mode**（工作域）— 自动检测用户在做什么
//! - **L2 Execution Mode**（执行策略）— Craft / Plan / Ask
//! - **L3 Thinking Mode**（思考深度）— Quick / Deep
//!
//! ## 数据流
//!
//! ```text
//! 用户输入 + 上下文
//!      │
//!      ├──► DomainModeDetector::detect()  → DomainDetection (L1)
//!      │
//!      ├──► recommend_execution()          → TaskMode (L2)
//!      │
//!      ├──► select_thinking() with domain  → ThinkingMode (L3)
//!      │
//!      └──► ModeComposition { L1 + L2 + L3 + preset }
//!               │
//!               ├──► system prompt (DirRoute::DomainMode)
//!               ├──► tool router (priority_tools)
//!               ├──► mode gate (safe_write_override)
//!               └──► convergence detector (max_stall_turns)
//! ```

use hermix_core::TaskMode;
use hermix_intelligence::{
    select_mode, DomainMode, DomainModeDetector, DomainPreset, ThinkingConfig, ThinkingMode,
    domain_mode::preset_for,
};

// ---------------------------------------------------------------------------
// ModeComposition
// ---------------------------------------------------------------------------

/// 三层模式的组合结果 — 每个 turn 生成一次。
/// /// 在 `AgentLoop::run()` 的 Prefetch 阶段由 `compose()` 生成，
/// 注入到后续的系统提示词构建、工具路由、安全门控、收敛检测。
#[derive(Debug, Clone)]
pub struct ModeComposition {
    /// L1: 工作域检测结果
    pub domain_mode: DomainMode,
    pub domain_confidence: f32,
    /// L1: 对应的域预设
    pub preset: &'static DomainPreset,
    /// L2: 执行模式
    pub execution_mode: TaskMode,
    /// L3: 思考深度
    pub thinking_mode: ThinkingMode,
}

impl ModeComposition {
    /// 在 `AgentLoop::run()` 的 Prefetch 阶段生成三层模式组合。
    /// /// # 参数
    /// - `detector`: 域模式检测器（可变借用，会更新内部历史）
    /// - `query`: 用户最新输入文本
    /// - `history_len`: 对话历史轮数
    /// - `workspace_path`: 当前工作区路径（可选）
    /// - `task_mode`: Task 自带的执行模式（如果 Task 层已设置，优先用）
    /// - `thinking_config`: 思考模式路由配置
    pub fn compose(
        detector: &mut DomainModeDetector,
        query: &str,
        history_len: usize,
        workspace_path: Option<&str>,
        task_mode: TaskMode,
        thinking_config: &ThinkingConfig,
    ) -> Self {
        // L1: 检测工作域
        let detection = detector.detect(query, workspace_path);
        let preset = preset_for(detection.mode);

        // L2: 执行模式（优先用 Task 自带的，否则推荐）
        let execution_mode = if task_mode != TaskMode::default() {
            task_mode
        } else {
            Self::recommend_execution(detection.mode)
        };

        // L3: 思考深度（融合 domain 信号）
        let thinking_mode = Self::select_thinking(query, history_len, detection.mode, thinking_config);

        Self {
            domain_mode: detection.mode,
            domain_confidence: detection.confidence,
            preset,
            execution_mode,
            thinking_mode,
        }
    }

    /// 根据 Domain 推荐执行模式。
    /// /// 当 Task 层没有显式设置 mode 时，由 Domain 自动推荐：
    /// - Code → Craft（代码通常直接干）
    /// - Research → Ask（研究通常只读）
    /// - Write → Plan（写作先列大纲）
    /// - Operate → Plan（操作先预览）
    /// - Chat → Craft（闲聊直接回）
    pub fn recommend_execution(domain: DomainMode) -> TaskMode {
        match domain {
            DomainMode::Code => TaskMode::Craft,
            DomainMode::Research => TaskMode::Ask,
            DomainMode::Write => TaskMode::Plan,
            DomainMode::Operate => TaskMode::Plan,
            DomainMode::Chat => TaskMode::Craft,
        }
    }

    /// 融合 domain 信号的思考深度选择。
    /// /// 先用现有 `select_mode()` 获取 base 决策，
    /// 再根据 Domain Mode 调整：
    /// - Code → 总是 Deep（多文件关联需要深度推理）
    /// - Research → 总是 Deep（多源交叉需要深度分析）
    /// - Write → 看 base（简单邮件 Quick，复杂报告 Deep）
    /// - Operate → 总是 Quick（操作类任务通常直接执行）
    /// - Chat → 总是 Quick（闲聊不需要深度）
    pub fn select_thinking(
        query: &str,
        history_len: usize,
        domain: DomainMode,
        config: &ThinkingConfig,
    ) -> ThinkingMode {
        // 先用现有 select_mode 获取 base 决策
        let base = select_mode(query, history_len, config);

        // Domain 信号叠加
        match domain {
            DomainMode::Code => ThinkingMode::Deep,
            DomainMode::Research => ThinkingMode::Deep,
            DomainMode::Write => base.mode,
            DomainMode::Operate => ThinkingMode::Quick,
            DomainMode::Chat => ThinkingMode::Quick,
        }
    }

    /// 返回当前组合的人类可读摘要（用于 tracing / 调试）
    pub fn summary(&self) -> String {
        format!(
            "{:?}×{:?}×{:?} (conf={:.2})",
            self.domain_mode, self.execution_mode, self.thinking_mode, self.domain_confidence
        )
    }
}

impl Default for ModeComposition {
    fn default() -> Self {
        let preset = preset_for(DomainMode::Chat);
        Self {
            domain_mode: DomainMode::Chat,
            domain_confidence: 0.3,
            preset,
            execution_mode: TaskMode::Craft,
            thinking_mode: ThinkingMode::Quick,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn compose(query: &str) -> ModeComposition {
        let mut detector = DomainModeDetector::new();
        ModeComposition::compose(
            &mut detector,
            query,
            0,
            None,
            TaskMode::default(),
            &ThinkingConfig::default(),
        )
    }

    fn compose_with_ws(query: &str, ws: &str) -> ModeComposition {
        let mut detector = DomainModeDetector::new();
        ModeComposition::compose(
            &mut detector,
            query,
            0,
            Some(ws),
            TaskMode::default(),
            &ThinkingConfig::default(),
        )
    }

    // -- 基本组合测试 --

    #[test]
    fn test_code_craft_deep() {
        let comp = compose("帮我重构 auth 模块");
        assert_eq!(comp.domain_mode, DomainMode::Code);
        assert_eq!(comp.execution_mode, TaskMode::Craft);
        assert_eq!(comp.thinking_mode, ThinkingMode::Deep);
    }

    #[test]
    fn test_research_ask_deep() {
        let comp = compose("搜索一下 Rust async 生态最新进展");
        assert_eq!(comp.domain_mode, DomainMode::Research);
        assert_eq!(comp.execution_mode, TaskMode::Ask);
        assert_eq!(comp.thinking_mode, ThinkingMode::Deep);
    }

    #[test]
    fn test_write_plan_deep_or_quick() {
        // Short write query → Quick (base select_mode returns Quick)
        let comp = compose("写邮件");
        assert_eq!(comp.domain_mode, DomainMode::Write);
        assert_eq!(comp.execution_mode, TaskMode::Plan);
        assert_eq!(comp.thinking_mode, ThinkingMode::Quick);

        // Long query with write keyword → Write domain + Deep thinking
        let long_query = format!("起草{}", "a".repeat(300));
        let comp = compose(&long_query);
        assert_eq!(comp.domain_mode, DomainMode::Write);
        assert_eq!(comp.thinking_mode, ThinkingMode::Deep);
    }

    #[test]
    fn test_operate_plan_quick() {
        let comp = compose("把下载文件夹里的截图整理到 D:/Photos");
        assert_eq!(comp.domain_mode, DomainMode::Operate);
        assert_eq!(comp.execution_mode, TaskMode::Plan);
        assert_eq!(comp.thinking_mode, ThinkingMode::Quick);
    }

    #[test]
    fn test_chat_craft_quick() {
        let comp = compose("你好");
        assert_eq!(comp.domain_mode, DomainMode::Chat);
        assert_eq!(comp.execution_mode, TaskMode::Craft);
        assert_eq!(comp.thinking_mode, ThinkingMode::Quick);
    }

    // -- Task Mode 优先级 --

    #[test]
    fn test_task_mode_overrides_recommendation() {
        let mut detector = DomainModeDetector::new();
        let comp = ModeComposition::compose(
            &mut detector,
            "帮我修复 bug",
            0,
            None,
            TaskMode::Ask, // 用户显式设置 Ask
            &ThinkingConfig::default(),
        );
        // Domain 检测到 Code → 推荐 Craft，但 Task 设了 Ask → 用 Ask
        assert_eq!(comp.execution_mode, TaskMode::Ask);
    }

    #[test]
    fn test_default_task_mode_uses_recommendation() {
        let comp = compose("帮我修复 main.rs 的编译错误");
        assert_eq!(comp.execution_mode, TaskMode::Craft);
    }

    // -- Thinking Mode 融合 --

    #[test]
    fn test_code_always_deep() {
        let comp = compose("read main.rs");
        assert_eq!(comp.domain_mode, DomainMode::Code);
        assert_eq!(comp.thinking_mode, ThinkingMode::Deep);
    }

    #[test]
    fn test_research_always_deep() {
        let comp = compose("搜索一下");
        assert_eq!(comp.domain_mode, DomainMode::Research);
        assert_eq!(comp.thinking_mode, ThinkingMode::Deep);
    }

    #[test]
    fn test_chat_always_quick() {
        let comp = compose("谢谢");
        assert_eq!(comp.domain_mode, DomainMode::Chat);
        assert_eq!(comp.thinking_mode, ThinkingMode::Quick);
    }

    #[test]
    fn test_operate_always_quick() {
        let comp = compose("删除临时文件夹");
        assert_eq!(comp.domain_mode, DomainMode::Operate);
        assert_eq!(comp.thinking_mode, ThinkingMode::Quick);
    }

    // -- Preset 正确性 --

    #[test]
    fn test_preset_matches_domain() {
        let comp = compose("帮我修复 main.rs");
        assert_eq!(comp.preset.mode, DomainMode::Code);
        assert!(comp.preset.priority_tools.contains(&"read_file"));
    }

    #[test]
    fn test_research_preset_has_search_tools() {
        let comp = compose("搜索一下");
        assert!(comp.preset.priority_tools.contains(&"web_search"));
    }

    // -- workspace 影响 --

    #[test]
    fn test_workspace_boosts_code() {
        let comp = compose_with_ws("帮我看看这个函数", ".");
        assert_eq!(comp.domain_mode, DomainMode::Code);
    }

    // -- summary --

    #[test]
    fn test_summary_format() {
        let comp = compose("帮我修复 main.rs");
        let s = comp.summary();
        assert!(s.contains("Code"));
        assert!(s.contains("Craft"));
        assert!(s.contains("Deep"));
    }

    // -- Default --

    #[test]
    fn test_default_composition() {
        let comp = ModeComposition::default();
        assert_eq!(comp.domain_mode, DomainMode::Chat);
        assert_eq!(comp.execution_mode, TaskMode::Craft);
        assert_eq!(comp.thinking_mode, ThinkingMode::Quick);
    }

    // -- recommend_execution 独立测试 --

    #[test]
    fn test_recommend_execution_all_domains() {
        assert_eq!(ModeComposition::recommend_execution(DomainMode::Code), TaskMode::Craft);
        assert_eq!(ModeComposition::recommend_execution(DomainMode::Research), TaskMode::Ask);
        assert_eq!(ModeComposition::recommend_execution(DomainMode::Write), TaskMode::Plan);
        assert_eq!(ModeComposition::recommend_execution(DomainMode::Operate), TaskMode::Plan);
        assert_eq!(ModeComposition::recommend_execution(DomainMode::Chat), TaskMode::Craft);
    }

    // -- select_thinking 独立测试 --

    #[test]
    fn test_select_thinking_code_override() {
        // 即使 base select_mode 返回 Quick，Code 也强制 Deep
        let mode = ModeComposition::select_thinking("hi", 0, DomainMode::Code, &ThinkingConfig::default());
        assert_eq!(mode, ThinkingMode::Deep);
    }

    #[test]
    fn test_select_thinking_chat_override() {
        // 即使 base select_mode 返回 Deep，Chat 也强制 Quick
        let long_query = &"a".repeat(300);
        let mode = ModeComposition::select_thinking(long_query, 0, DomainMode::Chat, &ThinkingConfig::default());
        assert_eq!(mode, ThinkingMode::Quick);
    }

    #[test]
    fn test_select_thinking_write_follows_base() {
        // Write 看 base：短 query → Quick
        let mode = ModeComposition::select_thinking("写邮件", 0, DomainMode::Write, &ThinkingConfig::default());
        assert_eq!(mode, ThinkingMode::Quick);

        // 长 query → Deep
        let long_query = &"a".repeat(300);
        let mode = ModeComposition::select_thinking(long_query, 0, DomainMode::Write, &ThinkingConfig::default());
        assert_eq!(mode, ThinkingMode::Deep);
    }
}
