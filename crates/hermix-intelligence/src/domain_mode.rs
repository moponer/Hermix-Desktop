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
//! `prompt_assembly::resolve_domain_mode` 每 turn 跑一次检测（用户 pin
//! 优先，否则 `DomainModeDetector::detect`），把 `DomainMode` 注入
//! `RouteContext.domain_mode`，由 `DirRoute::DomainMode` 门控本文件的
//! 域指令块。这是该模块当前唯一的生产消费路径。

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
// DomainPreset — 域预设配置
// ---------------------------------------------------------------------------

/// 一个 Domain Mode 的系统提示词预设 —— 当前只剩「注入哪段域指令」一件事。
///
/// 2026-09-29 瘦身：原字段 `priority_tools` / `safe_write_override` /
/// `convergence_signals` / `max_stall_turns` / `output_style` 的唯一写入方是
/// `hermix_agent::mode_composition`，而那个模块从没有非测试调用方（整簇死代码已
/// 删除）；其消费方（工具路由加权、`ModeGate`、收敛阈值）同样从未接线。将来真要
/// 复活某项能力时，按需加字段 + 补消费方，不提前预留。
#[derive(Debug, Clone)]
pub struct DomainPreset {
    /// 对应的域模式
    pub mode: DomainMode,
    /// 附加到系统提示词的 domain 指令块（`Chat` 为空串 —— 不注入）
    pub system_directive: &'static str,
    /// L1 域的工具路由偏置：路由打分后对这些工具 +5.0
    /// （经 `PriorityBoost` 消费，`TurnContext::priority_tools`）。
    /// 加权不是开关——未列出的工具仍可用。`Chat` 为空表。
    pub priority_tools: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// 域指令常量
// ---------------------------------------------------------------------------

pub const CODE_DOMAIN_DIRECTIVE: &str = "\
# 代码开发模式
1. 改前先读：任何编辑前用 read_file 看当前内容（> 1500 行用 offset/limit 分段）；禁止凭记忆改代码
2. 改后即验：Rust 跑 cargo check --workspace --all-targets；TS 跑 node_modules/.bin/tsc --noEmit；有对应测试就跑最小集。验证失败不得报告完成
3. 展示用 diff：给用户看修改时用 diff 片段，禁止重贴整个文件
4. 不自动 git commit——除非用户明确要求；git 安全规则以系统提示为准
5. 临时脚本不进源码树：走 code_execution 或系统临时目录（file_lifecycle 规则为准）";

pub const RESEARCH_DOMAIN_DIRECTIVE: &str = "\
# 信息研究模式
1. 多源交叉：重要结论至少 2 个独立来源验证；用 web_search 检索、web_extract 抓正文
2. 长文档/多网页走 delegate_task（Explore 角色）隔离阅读，返回结论不贴原文
3. 引用来源：每个事实性结论标注来源与日期；链接经 url_safety 校验
4. 不确定即说明——信息不足时明确说\"目前无法确认\"，不要编造
5. 总结先行：回答开头给一句话结论，再展开细节";

pub const WRITE_DOMAIN_DIRECTIVE: &str = "\
# 内容创作模式
1. 先明确四要素：文档类型、读者、渠道、期望长度——缺了先问一句
2. 结构先行：先给大纲，确认后成文（用户已给明确结构除外）
3. 文档类产物走 document-authoring 画布管线（<hermix-canvas> 流式 + write_file 落盘）；素材用 markitdown 读、长文 delegate_task 隔离读
4. 完整交付：产出完整可用文档，不给\"你可以这样写…\"的片段
5. 格式与分节遵循系统提示 REPORT_SECTIONING 规则";

pub const OPERATE_DOMAIN_DIRECTIVE: &str = "\
# 系统操作模式
1. 批量操作先列清单：将要执行的逐条操作先展示再执行（terminal / storage_clean）
2. 不可逆操作（删除/覆盖/移动）前明确确认；删除用 storage_clean（回收站可恢复），不用裸 Remove-Item
3. 路径用绝对路径（反斜杠原生形态），不依赖当前目录
4. 结构化工具优先于 shell：move_file 代替 mv/Copy-Item，storage_analyzer 代替手工扫描——审批引擎对它们有准确的风险分级
5. 操作后验证目标状态（list_files / Get-Item），验证失败不得报完成";

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

/// Code 域路由偏置：读写代码与执行验证的工具加权
pub const CODE_PRIORITY_TOOLS: &[&str] = &[
    "read_file",
    "search_files",
    "terminal",
    "code_execution",
    "write_file",
];

/// Research 域路由偏置：检索、抓取、隔离阅读
pub const RESEARCH_PRIORITY_TOOLS: &[&str] = &[
    "web_search",
    "web_extract",
    "delegate_task",
    "markitdown",
    "session_search",
];

/// Write 域路由偏置：素材读取与产物落盘
pub const WRITE_PRIORITY_TOOLS: &[&str] = &["write_file", "markitdown", "delegate_task"];

/// Operate 域路由偏置：结构化文件/磁盘操作
pub const OPERATE_PRIORITY_TOOLS: &[&str] = &[
    "terminal",
    "move_file",
    "storage_analyzer",
    "storage_clean",
    "code_execution",
];

pub static CODE_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Code,
    system_directive: CODE_DOMAIN_DIRECTIVE,
    priority_tools: CODE_PRIORITY_TOOLS,
};

pub static RESEARCH_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Research,
    system_directive: RESEARCH_DOMAIN_DIRECTIVE,
    priority_tools: RESEARCH_PRIORITY_TOOLS,
};

pub static WRITE_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Write,
    system_directive: WRITE_DOMAIN_DIRECTIVE,
    priority_tools: WRITE_PRIORITY_TOOLS,
};

pub static OPERATE_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Operate,
    system_directive: OPERATE_DOMAIN_DIRECTIVE,
    priority_tools: OPERATE_PRIORITY_TOOLS,
};

pub static CHAT_PRESET: DomainPreset = DomainPreset {
    mode: DomainMode::Chat,
    system_directive: "",
    priority_tools: &[],
};


// ---------------------------------------------------------------------------
// 工具到域的映射
// ---------------------------------------------------------------------------

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
    fn test_priority_tools_non_empty_except_chat() {
        for mode in DomainMode::all() {
            let p = preset_for(*mode);
            if *mode == DomainMode::Chat {
                assert!(p.priority_tools.is_empty());
            } else {
                assert!(!p.priority_tools.is_empty(), "priority_tools empty for {mode:?}");
            }
        }
    }

    #[test]
    fn test_priority_tools_no_duplicates_within_preset() {
        for mode in DomainMode::all() {
            let p = preset_for(*mode);
            let mut seen = std::collections::HashSet::new();
            for name in p.priority_tools {
                assert!(seen.insert(*name), "duplicate tool {name} in {mode:?}");
            }
        }
    }

    #[test]
    fn test_code_priority_tools_expected_entries() {
        let p = preset_for(DomainMode::Code);
        assert!(p.priority_tools.contains(&"read_file"));
        assert!(p.priority_tools.contains(&"terminal"));
        assert!(p.priority_tools.contains(&"code_execution"));
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
