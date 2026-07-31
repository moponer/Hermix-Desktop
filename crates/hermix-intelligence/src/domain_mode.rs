//! L1: Domain Mode — 工作域模式检测与预设
//!
//! 设计哲学：代码开发和通用工作处理是完全不同的任务模式。
//! 本模块定义 5 个工作域，每个域配置一组预设（工具优先级、
//! 系统提示词、安全约束、收敛策略、输出风格），在 per-turn
//! 级别自动检测并生效。
//!
//! 与 L2 (`TaskMode`: Craft/Plan/Ask) 和 L3 (`ThinkingMode`:
//! Quick/Deep) 正交组合使用。
//!
//! ## 三层正交架构
//!
//! ```text
//! Domain Mode (做什么)  ×  Execution Mode (怎么做)  ×  Thinking Mode (想多深)
//!    Code                  Craft                      Quick
//!    Research              Plan                       Deep
//!    Write                 Ask
//!    Operate
//!    Chat
//! ```
//!
//! ## 集成
//!
//! `AgentLoop::run()` 在 Prefetch 阶段调 `ModeComposition::compose()`，
//! 生成 `DomainDetection` + `DomainPreset`，注入到后续的：
//! - 系统提示词构建（`DirRoute::DomainMode`）
//! - 工具路由（`priority_tools` 加分）
//! - 安全门控（`safe_write_override`）
//! - 收敛检测（`max_stall_turns`）

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// DomainMode 枚举
// ---------------------------------------------------------------------------

/// L1 工作域模式 — 检测用户当前在做哪类工作。
/// 与 `TaskMode` (L2) 和 `ThinkingMode` (L3) 正交组合使用。
/// 每个 Domain Mode 配置一组"预设"：工具优先级、系统提示词
/// 附加块、安全约束微调、输出风格引导、收敛策略权重。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainMode {
    /// 软件开发 — 读写代码、运行测试、调试、重构
    Code,
    /// 信息研究 — 搜索网页、阅读文档、对比分析、总结归纳
    Research,
    /// 内容创作 — 写文档/报告/邮件/方案，非代码类写作
    Write,
    /// 系统操作 — 文件管理、进程控制、环境配置、自动化脚本
    Operate,
    /// 通用对话 — 闲聊、快速问答、简单指令
    Chat,
}

impl Default for DomainMode {
    fn default() -> Self {
        Self::Chat
    }
}

impl DomainMode {
    /// 所有域模式
    pub fn all() -> &'static [DomainMode] {
        &[
            Self::Code,
            Self::Research,
            Self::Write,
            Self::Operate,
            Self::Chat,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Research => "research",
            Self::Write => "write",
            Self::Operate => "operate",
            Self::Chat => "chat",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "code" | "coding" | "dev" | "development" => Some(Self::Code),
            "research" | "search" | "investigate" => Some(Self::Research),
            "write" | "writing" | "draft" | "compose" => Some(Self::Write),
            "operate" | "ops" | "system" | "file_ops" => Some(Self::Operate),
            "chat" | "general" | "default" => Some(Self::Chat),
            _ => None,
        }
    }

    /// 人类可读的中文名称
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Code => "代码开发",
            Self::Research => "信息研究",
            Self::Write => "内容创作",
            Self::Operate => "系统操作",
            Self::Chat => "通用对话",
        }
    }
}

impl std::fmt::Display for DomainMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// ConvergenceSignal — 收敛信号
// ---------------------------------------------------------------------------

/// 该模式下"任务完成"的判定信号。
/// 用于 `ConvergenceDetector` 的 advisory warning 逻辑：
/// 当检测器发现 stall 时，检查是否已满足收敛信号，
/// 如果满足则不触发 warning（任务确实完成了）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceSignal {
    /// 代码模式：编译通过 + 测试通过
    CompileAndTestPass,
    /// 研究模式：已收集 N 个独立来源
    SufficientSources(usize),
    /// 写作模式：用户确认满意
    UserSatisfied,
    /// 操作模式：目标文件/进程状态已达成
    TargetStateReached,
    /// 通用：LLM 自行判断完成
    LlmSelfDetermined,
}

// ---------------------------------------------------------------------------
// DomainPreset — 域预设配置
// ---------------------------------------------------------------------------

/// 一个 Domain Mode 的完整预设。
/// 在 per-turn 级别由 `ModeComposition::compose()` 生成，
/// 注入到工具路由、系统提示词、安全门控、收敛检测。
#[derive(Debug, Clone)]
pub struct DomainPreset {
    /// 对应的域模式
    pub mode: DomainMode,

    // —— 工具优先级 ——
    /// 在 `ToolSchemaRouter` 中给这些工具加分（+5.0）
    pub priority_tools: &'static [&'static str],
    /// 在 Ask 执行模式下也允许的"安全写入工具"
    /// （例如 Code+Ask 允许 write_file，但 Research+Ask 不允许）
    pub safe_write_override: &'static [&'static str],

    // —— 系统提示词 ——
    /// 附加到系统提示词的 domain 指令块
    pub system_directive: &'static str,

    // —— 收敛策略 ——
    /// 该模式下"任务完成"的判定信号
    pub convergence_signals: &'static [ConvergenceSignal],
    /// 最大连续无进展轮数（超过后 advisory warning）
    pub max_stall_turns: u32,

    // —— 输出风格 ——
    /// 输出风格引导文本
    pub output_style: &'static str,
}

// ---------------------------------------------------------------------------
// 域指令常量
// ---------------------------------------------------------------------------

pub const CODE_DOMAIN_DIRECTIVE: &str = "\
# 代码开发模式
你当前在代码开发模式。以下规则优先于通用规则：
1. 修改前先读文件 — 不要凭记忆改代码，先 read_file 确认当前内容
2. 改完即验证 — 有测试就跑测试，有编译就 cargo check / tsc
3. diff 优先 — 展示修改时用 diff 格式，不要重贴整个文件
4. 类型安全 — Rust/TS 改动后确保类型检查通过
5. 提交纪律 — 不要自动 git commit，除非用户明确要求";

pub const RESEARCH_DOMAIN_DIRECTIVE: &str = "\
# 信息研究模式
你当前在信息研究模式。以下规则优先于通用规则：
1. 多源交叉 — 重要结论至少需要 2 个独立来源验证
2. 引用来源 — 每个事实性陈述后附 [来源] 标记
3. 时效性 — 优先引用近期信息，标注信息日期
4. 不确定即说明 — 信息不足时明确说\"目前无法确认\"，不要编造
5. 总结先行 — 回答开头给一句话结论，再展开细节";

pub const WRITE_DOMAIN_DIRECTIVE: &str = "\
# 内容创作模式
你当前在内容创作模式。以下规则优先于通用规则：
1. 先明确格式 — 开始前确认文档类型（邮件/报告/方案/通知）和目标读者
2. 结构先行 — 先给大纲，再填充内容（除非用户已给明确结构）
3. 语气适配 — 根据场景调整：正式报告 vs 内部通知 vs 私人邮件
4. 完整交付 — 产出完整可用文档，不要给\"你可以这样写...\"的片段
5. 格式规范 — Markdown 格式，标题层级清晰，列表对齐";

pub const OPERATE_DOMAIN_DIRECTIVE: &str = "\
# 系统操作模式
你当前在系统操作模式。以下规则优先于通用规则：
1. 操作前预览 — 批量操作前先列出将要执行的操作清单
2. 不可逆操作确认 — 删除/移动/覆盖前明确提示风险
3. 路径绝对化 — 使用绝对路径，不要依赖当前目录
4. 原子操作 — 尽量原子化操作，失败可回滚
5. 结果验证 — 操作后验证目标状态（ls/check/exist）";

// ---------------------------------------------------------------------------
// 预设表
// ---------------------------------------------------------------------------

/// 获取指定域模式的预设配置。
/// 返回 `&'static` 引用，因为所有预设都是编译期常量。
pub fn preset_for(mode: DomainMode) -> &'static DomainPreset {
    match mode {
        DomainMode::Code => &CODE_PRESET,
        DomainMode::Research => &RESEARCH_PRESET,
        DomainMode::Write => &WRITE_PRESET,
        DomainMode::Operate => &OPERATE_PRESET,
        DomainMode::Chat => &CHAT_PRESET,
    }
}

// 静态收敛信号常量
static CODE_CONVERGENCE: &[ConvergenceSignal] = &[
    ConvergenceSignal::CompileAndTestPass,
    ConvergenceSignal::LlmSelfDetermined,
];
static RESEARCH_CONVERGENCE: &[ConvergenceSignal] = &[
    ConvergenceSignal::SufficientSources(3),
    ConvergenceSignal::LlmSelfDetermined,
];
static WRITE_CONVERGENCE: &[ConvergenceSignal] = &[
    ConvergenceSignal::UserSatisfied,
    ConvergenceSignal::LlmSelfDetermined,
];
static OPERATE_CONVERGENCE: &[ConvergenceSignal] = &[
    ConvergenceSignal::TargetStateReached,
    ConvergenceSignal::LlmSelfDetermined,
];
static CHAT_CONVERGENCE: &[ConvergenceSignal] = &[ConvergenceSignal::LlmSelfDetermined];

pub static CODE_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Code,
    priority_tools: &[
        "read_file",
        "write_file",
        "patch",
        "search_files",
        "terminal",
        "grep",
        "list_files",
    ],
    // 代码模式下即使是 Ask 也允许 write_file（用户可能只是
    // 想看代码后做小修改），但不允许 terminal
    safe_write_override: &["write_file", "patch"],
    system_directive: CODE_DOMAIN_DIRECTIVE,
    convergence_signals: CODE_CONVERGENCE,
    max_stall_turns: 5,
    output_style: "代码优先。用代码块展示修改，用 diff 展示变更。说明控制在 3 行以内。",
};

pub static RESEARCH_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Research,
    priority_tools: &[
        "web_search",
        "web_fetch",
        "browser_navigate",
        "browser_snapshot",
        "read_pdf",
        "memory",
        "session_search",
    ],
    safe_write_override: &[],
    system_directive: RESEARCH_DOMAIN_DIRECTIVE,
    convergence_signals: RESEARCH_CONVERGENCE,
    max_stall_turns: 7,
    output_style: "结构化报告。用标题分区，引用来源链接，给出明确结论。",
};

pub static WRITE_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Write,
    priority_tools: &[
        "write_file",
        "read_file",
        "read_pdf",
        "web_search",
    ],
    safe_write_override: &["write_file"],
    system_directive: WRITE_DOMAIN_DIRECTIVE,
    convergence_signals: WRITE_CONVERGENCE,
    max_stall_turns: 4,
    output_style: "完整文档输出。用 Markdown 结构化，注意段落层次和逻辑连贯性。",
};

pub static OPERATE_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Operate,
    priority_tools: &[
        "list_files",
        "move_file",
        "copy_file",
        "delete_file",
        "create_directory",
        "terminal",
        "process_registry",
    ],
    safe_write_override: &[],
    system_directive: OPERATE_DOMAIN_DIRECTIVE,
    convergence_signals: OPERATE_CONVERGENCE,
    max_stall_turns: 3,
    output_style: "操作日志风格。每步操作前说明意图，操作后确认结果。",
};

pub static CHAT_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Chat,
    priority_tools: &["memory"],
    safe_write_override: &[],
    system_directive: "",
    convergence_signals: CHAT_CONVERGENCE,
    max_stall_turns: 2,
    output_style: "自然对话。像真人一样完整、有温度地交流:该展开时展开,该收住时收住;不堆砌格式,也不敷衍成只言片语。",
};

// ---------------------------------------------------------------------------
// 工具到域的映射
// ---------------------------------------------------------------------------

/// 根据工具名推断它属于哪个工作域。
/// 用于 L1 上下文信号：如果最近使用的工具属于某个域，
/// 则当前输入可能也在同一个域。
pub fn tool_to_domain(tool_name: &str) -> Option<DomainMode> {
    match tool_name {
        // 代码工具
        "read_file" | "write_file" | "patch" | "apply_patch" | "edit_file" | "grep"
        | "search_files" | "terminal" | "list_files" => Some(DomainMode::Code),
        // 研究工具
        "web_search" | "web_fetch" | "browser_navigate" | "browser_snapshot"
        | "browser_click" | "browser_type" | "browser_scroll" | "browser_vision"
        | "browser_wait_for" | "read_pdf" | "session_search" => Some(DomainMode::Research),
        // 操作工具
        "move_file" | "copy_file" | "delete_file" | "create_directory"
        | "process_registry" | "code_execution" => Some(DomainMode::Operate),
        // 通用工具 — 不映射到特定域
        "memory" | "skill_manage" | "todo" | "task_create" | "task_update" | "task_list"
        | "task_get" | "compact_tool_chain" => None,
        _ => None,
    }
}

/// 判断 workspace 路径是否是代码项目。
/// 通过检查常见代码项目标识文件来推断。
pub fn is_code_workspace(path: &str) -> bool {
    let indicators = [
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pom.xml",
        "build.gradle",
        ".git",
        "tsconfig.json",
        "requirements.txt",
        "pyproject.toml",
        "CMakeLists.txt",
        "Makefile",
    ];
    let base = std::path::Path::new(path);
    indicators.iter().any(|f| base.join(f).exists())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- DomainMode 基本属性 --

    #[test]
    fn test_as_str_round_trip() {
        for mode in DomainMode::all() {
            let s = mode.as_str();
            let back = DomainMode::from_str(s).unwrap();
            assert_eq!(*mode, back, "round-trip failed for {s}");
        }
    }

    #[test]
    fn test_from_str_aliases() {
        assert_eq!(DomainMode::from_str("coding"), Some(DomainMode::Code));
        assert_eq!(DomainMode::from_str("dev"), Some(DomainMode::Code));
        assert_eq!(DomainMode::from_str("search"), Some(DomainMode::Research));
        assert_eq!(DomainMode::from_str("draft"), Some(DomainMode::Write));
        assert_eq!(DomainMode::from_str("ops"), Some(DomainMode::Operate));
        assert_eq!(DomainMode::from_str("general"), Some(DomainMode::Chat));
        assert_eq!(DomainMode::from_str("xyz"), None);
    }

    #[test]
    fn test_serde_snake_case() {
        let j = serde_json::to_string(&DomainMode::Code).unwrap();
        assert_eq!(j, "\"code\"");
        let j = serde_json::to_string(&DomainMode::Research).unwrap();
        assert_eq!(j, "\"research\"");
        let j = serde_json::to_string(&DomainMode::Write).unwrap();
        assert_eq!(j, "\"write\"");
        let j = serde_json::to_string(&DomainMode::Operate).unwrap();
        assert_eq!(j, "\"operate\"");
        let j = serde_json::to_string(&DomainMode::Chat).unwrap();
        assert_eq!(j, "\"chat\"");
    }

    #[test]
    fn test_default_is_chat() {
        assert_eq!(DomainMode::default(), DomainMode::Chat);
    }

    #[test]
    fn test_display_name_not_empty() {
        for mode in DomainMode::all() {
            assert!(!mode.display_name().is_empty());
        }
    }

    #[test]
    fn test_display_impl() {
        assert_eq!(format!("{}", DomainMode::Code), "code");
    }

    // -- 预设表 --

    #[test]
    fn test_preset_for_all_modes() {
        for mode in DomainMode::all() {
            let preset = preset_for(*mode);
            assert_eq!(preset.mode, *mode);
        }
    }

    #[test]
    fn test_code_preset_has_code_tools() {
        let p = preset_for(DomainMode::Code);
        assert!(p.priority_tools.contains(&"read_file"));
        assert!(p.priority_tools.contains(&"write_file"));
        assert!(p.priority_tools.contains(&"terminal"));
    }

    #[test]
    fn test_code_preset_safe_write_override() {
        let p = preset_for(DomainMode::Code);
        assert!(p.safe_write_override.contains(&"write_file"));
        // terminal 不在 safe_write_override 中
        assert!(!p.safe_write_override.contains(&"terminal"));
    }

    #[test]
    fn test_research_preset_no_write_override() {
        let p = preset_for(DomainMode::Research);
        assert!(p.safe_write_override.is_empty());
    }

    #[test]
    fn test_write_preset_has_write_tools() {
        let p = preset_for(DomainMode::Write);
        assert!(p.priority_tools.contains(&"write_file"));
    }

    #[test]
    fn test_operate_preset_has_file_ops() {
        let p = preset_for(DomainMode::Operate);
        assert!(p.priority_tools.contains(&"move_file"));
        assert!(p.priority_tools.contains(&"delete_file"));
    }

    #[test]
    fn test_chat_preset_minimal() {
        let p = preset_for(DomainMode::Chat);
        assert!(p.system_directive.is_empty());
        assert_eq!(p.max_stall_turns, 2);
    }

    #[test]
    fn test_directive_non_empty_except_chat() {
        for mode in DomainMode::all() {
            let p = preset_for(*mode);
            if *mode == DomainMode::Chat {
                assert!(p.system_directive.is_empty());
            } else {
                assert!(!p.system_directive.is_empty(), "directive empty for {mode:?}");
            }
        }
    }

    #[test]
    fn test_convergence_signals_non_empty() {
        for mode in DomainMode::all() {
            let p = preset_for(*mode);
            assert!(!p.convergence_signals.is_empty(), "no convergence signals for {mode:?}");
        }
    }

    // -- tool_to_domain --

    #[test]
    fn test_tool_to_domain_code_tools() {
        assert_eq!(tool_to_domain("read_file"), Some(DomainMode::Code));
        assert_eq!(tool_to_domain("write_file"), Some(DomainMode::Code));
        assert_eq!(tool_to_domain("terminal"), Some(DomainMode::Code));
        assert_eq!(tool_to_domain("grep"), Some(DomainMode::Code));
    }

    #[test]
    fn test_tool_to_domain_research_tools() {
        assert_eq!(tool_to_domain("web_search"), Some(DomainMode::Research));
        assert_eq!(tool_to_domain("browser_navigate"), Some(DomainMode::Research));
        assert_eq!(tool_to_domain("read_pdf"), Some(DomainMode::Research));
    }

    #[test]
    fn test_tool_to_domain_operate_tools() {
        assert_eq!(tool_to_domain("move_file"), Some(DomainMode::Operate));
        assert_eq!(tool_to_domain("delete_file"), Some(DomainMode::Operate));
    }

    #[test]
    fn test_tool_to_domain_generic_returns_none() {
        assert_eq!(tool_to_domain("memory"), None);
        assert_eq!(tool_to_domain("skill_manage"), None);
        assert_eq!(tool_to_domain("unknown_tool"), None);
    }

    // -- is_code_workspace --

    #[test]
    fn test_is_code_workspace_with_cargo_toml() {
        // workspace root should have Cargo.toml
        assert!(is_code_workspace("."));
    }

    #[test]
    fn test_is_code_workspace_nonexistent_path() {
        assert!(!is_code_workspace("/nonexistent/path/xyz"));
    }
}
