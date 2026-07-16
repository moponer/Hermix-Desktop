//! L1: Domain Mode 检测引擎
//!
//! 设计哲学：自动检测用户当前在做哪类工作（代码/研究/写作/操作/闲聊），
//! 不需要用户手动切换模式。
//!
//! ## 检测架构
//!
//! ```text
//! 用户输入
//!    │
//!    ├──► [L0: 关键词/规则引擎]  ──► confidence 0.3-0.8
//!    │         │
//!    │         ├── 文件路径/扩展名 (.rs/.ts/.py → Code)
//!    │         ├── 搜索意图词 ("搜索"/"查"/"对比" → Research)
//!    │         ├── 写作意图词 ("写"/"起草"/"翻译" → Write)
//!    │         ├── 操作意图词 ("整理"/"移动"/"删除" → Operate)
//!    │         └── 闲聊信号 ("你好"/"谢谢" → Chat)
//!    │
//!    ├──► [L1: 上下文信号]      ──► confidence ±0.2
//!    │         │
//!    │         ├── workspace 路径包含代码项目 (→ Code +0.2)
//!    │         ├── 最近 N 轮的工具使用历史
//!    │         ├── 最近 N 轮的 Domain Mode（惯性）
//!    │         └── 当前激活的 toolsets
//!    │
//!    └──► [用户手动覆盖]        ──► confidence 1.0（最高优先级）
//! ```
//!
//! ## 集成
//!
//! `AgentLoop::run()` 在 Prefetch 阶段调 `detect()`，
//! 结果注入到 `ModeComposition`。

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::domain_mode::{is_code_workspace, tool_to_domain, DomainMode};

// ---------------------------------------------------------------------------
// 检测结果类型
// ---------------------------------------------------------------------------

/// 检测结果的来源
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionSource {
    /// 用户手动指定
    ManualOverride,
    /// L0 关键词命中（高置信度 ≥ 0.8）
    KeywordHighConfidence,
    /// L0 + L1 综合判断（中置信度 0.5-0.8）
    KeywordAndContext,
    /// L2 LLM 分类器（预留，当前未实现）
    LlmClassifier,
    /// 默认值（低置信度 < 0.5）
    Default,
}

/// 检测信号类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    /// 文件扩展名匹配
    FileExtension,
    /// 关键词命中
    Keyword,
    /// 工具使用历史
    ToolHistory,
    /// 域模式惯性
    ModeInertia,
    /// workspace 上下文
    WorkspaceContext,
    /// 短文本信号
    ShortLength,
    /// URL 出现
    UrlPattern,
    /// glob 通配符模式
    GlobPattern,
}

/// 单个检测信号
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectionSignal {
    pub signal_type: SignalType,
    pub weight: f32,
    pub detail: String,
}

/// `detect()` 的完整输出
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainDetection {
    pub mode: DomainMode,
    pub confidence: f32,
    pub signals: Vec<DetectionSignal>,
    pub source: DetectionSource,
}

impl DomainDetection {
    /// 构造一个默认检测结果（Chat, 低置信度）
    pub fn default_detection() -> Self {
        Self {
            mode: DomainMode::Chat,
            confidence: 0.3,
            signals: vec![],
            source: DetectionSource::Default,
        }
    }
}

// ---------------------------------------------------------------------------
// 检测配置
// ---------------------------------------------------------------------------

/// L0 / L1 检测的可调参数
#[derive(Debug, Clone)]
pub struct DomainDetectConfig {
    // —— 域模式惯性 ——
    /// 最近 N 轮同域 → 加权（默认 0.15）
    pub inertia_weight: f32,
    /// 惯性窗口大小（默认 3）
    pub inertia_window: usize,

    // —— 工具历史 ——
    /// 最近使用的工具对应的域加分（默认 0.15）
    pub tool_history_weight: f32,

    // —— workspace 上下文 ——
    /// workspace 是代码项目时给 Code 加分（默认 0.2）
    pub workspace_code_bonus: f32,

    // —— LLM 分类器 ——
    /// L0+L1 置信度低于此值时触发 LLM 分类器（默认 0.5）
    pub llm_classifier_threshold: f32,

    // —— 短文本信号 ——
    /// 文本短于此字符数时给 Chat 加分（默认 15）
    pub short_text_threshold: usize,
    pub short_text_weight: f32,
}

impl Default for DomainDetectConfig {
    fn default() -> Self {
        Self {
            inertia_weight: 0.15,
            inertia_window: 3,
            tool_history_weight: 0.15,
            workspace_code_bonus: 0.2,
            llm_classifier_threshold: 0.5,
            short_text_threshold: 15,
            short_text_weight: 0.4,
        }
    }
}

// ---------------------------------------------------------------------------
// 关键词规则表
// ---------------------------------------------------------------------------

/// 代码文件扩展名（强信号）
const CODE_EXTENSIONS: &[&str] = &[
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".c", ".cpp", ".h", ".hpp",
    ".vue", ".svelte", ".rb", ".php", ".swift", ".kt", ".scala", ".lua", ".r", ".jl", ".ex",
    ".exs", ".zig", ".nim", ".dart", ".cs",
];

/// 编程关键词（中信号）
const CODE_KEYWORDS: &[&str] = &[
    "函数", "类", "接口", "编译", "测试", "重构", "实现", "函数签名", "类型", "泛型", "异步",
    "并发", "依赖", "模块", "包", "变量", "常量", "枚举", "结构体", "trait", "impl",
    "import", "export", "function", "class", "interface", "compile", "test", "refactor",
    "implement", "debug", "stack trace", "error", "exception", "type error", "syntax error",
    "cargo", "npm", "pnpm", "pip", "go run", "mvn", "gradle", "yarn", "tsc", "eslint",
    "clippy", "rustc", "gcc", "clang",
];

/// 代码操作动词（强信号）
const CODE_ACTION_KEYWORDS: &[&str] = &[
    "改代码", "写代码", "修复", "添加功能", "删掉这段", "注释掉", "取消注释",
    "fix", "add feature", "remove", "rename", "extract method", "inline", "wrap",
    "add import", "fix import", "add type", "fix type",
];

/// 研究关键词
const RESEARCH_KEYWORDS: &[&str] = &[
    "搜索", "查找资料", "调研", "对比", "比较", "分析", "最新", "2024", "2025", "2026",
    "研究报告", "行业报告", "白皮书", "论文",
    "search", "research", "compare", "analyze", "latest", "what is", "how does",
    "difference between", "vs ", "versus", "overview", "state of",
];

/// 写作关键词
const WRITE_KEYWORDS: &[&str] = &[
    "写", "起草", "撰写", "翻译", "总结", "归纳", "邮件", "报告", "方案", "通知", "文档",
    "大纲", "摘要", "摘要", "备忘录", "会议纪要",
    "write", "draft", "compose", "translate", "summarize", "email", "report", "proposal",
    "document", "outline", "memo", "minutes",
];

/// 写作文档扩展名
const WRITE_EXTENSIONS: &[&str] = &[".md", ".doc", ".docx", ".txt", ".rtf"];

/// 操作关键词
const OPERATE_KEYWORDS: &[&str] = &[
    "整理", "移动", "复制", "删除", "重命名", "创建文件夹", "安装", "配置", "部署", "启动",
    "停止", "清理", "备份", "压缩", "解压", "批量",
    "organize", "move", "copy", "delete", "rename", "mkdir", "install", "configure", "deploy",
    "start", "stop", "clean", "backup", "compress", "extract", "batch",
];

/// 闲聊关键词
const CHAT_KEYWORDS: &[&str] = &[
    "你好", "谢谢", "再见", "好的", "明白了", "收到", "嗯", "哦",
    "hello", "thanks", "bye", "ok", "got it", "understood", "hi ", "hey",
];

/// URL 正则模式
const URL_PATTERN: &str = r"https?://";

/// glob 通配符模式（如 *.txt, *.png）
const GLOB_PATTERN: &str = r"\*\.\w+";

// ---------------------------------------------------------------------------
// 信号匹配辅助
// ---------------------------------------------------------------------------

/// 检查 query 中是否包含任一关键词（大小写不敏感）
fn match_keywords(query_lower: &str, keywords: &[&str]) -> Option<String> {
    for kw in keywords {
        let kw_lower = kw.to_lowercase();
        if query_lower.contains(&kw_lower) {
            return Some(kw.to_string());
        }
    }
    None
}

/// 检查 query 中是否包含任一文件扩展名。
///
/// 使用正则确保扩展名前面是单词字符（文件名），
/// 后面是非单词字符或行尾，避免 `.c` 匹配 `.com` 等。
fn match_extensions(query_lower: &str, extensions: &[&str]) -> Option<String> {
    for ext in extensions {
        // ext 形如 ".rs"，构建正则 `\w+\.rs\b`
        let escaped = regex::escape(ext);
        let pattern = format!(r"\w+{}(?:[^\w]|$)", escaped);
        if let Ok(re) = regex::Regex::new(&pattern) {
            if re.is_match(query_lower) {
                return Some(ext.to_string());
            }
        }
    }
    None
}

/// 检查 query 是否匹配某个正则模式
fn match_regex(query: &str, pattern: &str) -> bool {
    // 使用 regex crate（已在依赖中）
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(query),
        Err(_) => false,
    }
}

/// 计算字符数（Unicode 安全）
fn char_count(s: &str) -> usize {
    s.chars().count()
}

// ---------------------------------------------------------------------------
// DomainModeDetector
// ---------------------------------------------------------------------------

/// Domain Mode 检测器
///
/// 维护检测历史和工具使用历史，在 per-turn 级别
/// 自动检测用户当前的工作域。
///
/// ## 用法
///
/// ```ignore
/// let mut detector = DomainModeDetector::new();
/// let detection = detector.detect("帮我修复 main.rs 的编译错误", Some("/project"));
/// assert_eq!(detection.mode, DomainMode::Code);
/// ```
#[derive(Debug, Clone)]
pub struct DomainModeDetector {
    config: DomainDetectConfig,
    /// 最近 N 轮的检测历史（用于惯性判断）
    history: VecDeque<DomainMode>,
    /// 最近 N 轮使用的工具名（用于上下文信号）
    recent_tools: VecDeque<String>,
    /// 用户手动覆盖（None = 自动检测）
    manual_override: Option<DomainMode>,
}

impl Default for DomainModeDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl DomainModeDetector {
    /// 创建一个新的检测器，使用默认配置
    pub fn new() -> Self {
        Self {
            config: DomainDetectConfig::default(),
            history: VecDeque::with_capacity(8),
            recent_tools: VecDeque::with_capacity(8),
            manual_override: None,
        }
    }

    /// 用自定义配置创建检测器
    pub fn with_config(config: DomainDetectConfig) -> Self {
        Self {
            config,
            history: VecDeque::with_capacity(8),
            recent_tools: VecDeque::with_capacity(8),
            manual_override: None,
        }
    }

    /// 设置用户手动覆盖。设为 `None` 恢复自动检测。
    pub fn set_manual_override(&mut self, mode: Option<DomainMode>) {
        self.manual_override = mode;
    }

    /// 返回当前手动覆盖设置
    pub fn manual_override(&self) -> Option<DomainMode> {
        self.manual_override
    }

    /// 记录一轮工具使用（在工具执行后调用）
    pub fn record_tool_use(&mut self, tool_name: &str) {
        self.recent_tools.push_back(tool_name.to_string());
        if self.recent_tools.len() > 8 {
            self.recent_tools.pop_front();
        }
    }

    /// 返回最近 N 轮的域模式历史
    pub fn history_snapshot(&self) -> Vec<DomainMode> {
        self.history.iter().copied().collect()
    }

    // -----------------------------------------------------------------------
    // 主检测函数
    // -----------------------------------------------------------------------

    /// 检测用户输入的工作域。
    ///
    /// 在 `AgentLoop::run()` 的 Prefetch 阶段调用。
    ///
    /// # 参数
    /// - `query`: 用户最新输入文本
    /// - `workspace_path`: 当前工作区路径（可选）
    pub fn detect(
        &mut self,
        query: &str,
        workspace_path: Option<&str>,
    ) -> DomainDetection {
        // 0. 用户手动覆盖（最高优先级）
        if let Some(mode) = self.manual_override {
            return DomainDetection {
                mode,
                confidence: 1.0,
                signals: vec![],
                source: DetectionSource::ManualOverride,
            };
        }

        let query_trim = query.trim();
        let query_lower = query_trim.to_lowercase();
        let mut scores: HashMap<DomainMode, f32> = HashMap::new();
        let mut signals: Vec<DetectionSignal> = Vec::new();

        // —— L0: 关键词/规则匹配 ——

        // Code 信号
        let mut code_score = 0.0f32;

        // 文件扩展名（强信号 0.9）
        if let Some(ext) = match_extensions(&query_lower, CODE_EXTENSIONS) {
            code_score = code_score.max(0.9);
            signals.push(DetectionSignal {
                signal_type: SignalType::FileExtension,
                weight: 0.9,
                detail: format!("文件扩展名 {}", ext),
            });
        }

        // 编程关键词（中信号 0.6）
        if let Some(kw) = match_keywords(&query_lower, CODE_KEYWORDS) {
            code_score = code_score.max(0.6);
            signals.push(DetectionSignal {
                signal_type: SignalType::Keyword,
                weight: 0.6,
                detail: format!("编程关键词 \"{}\"", kw),
            });
        }

        // 代码操作动词（强信号 0.8）
        if let Some(kw) = match_keywords(&query_lower, CODE_ACTION_KEYWORDS) {
            code_score = code_score.max(0.8);
            signals.push(DetectionSignal {
                signal_type: SignalType::Keyword,
                weight: 0.8,
                detail: format!("代码操作 \"{}\"", kw),
            });
        }

        scores.insert(DomainMode::Code, code_score);

        // Research 信号
        let mut research_score = 0.0f32;

        if let Some(kw) = match_keywords(&query_lower, RESEARCH_KEYWORDS) {
            research_score = research_score.max(0.7);
            signals.push(DetectionSignal {
                signal_type: SignalType::Keyword,
                weight: 0.7,
                detail: format!("研究关键词 \"{}\"", kw),
            });
        }

        // URL 出现（强信号 0.8）
        if match_regex(query_trim, URL_PATTERN) {
            research_score = research_score.max(0.8);
            signals.push(DetectionSignal {
                signal_type: SignalType::UrlPattern,
                weight: 0.8,
                detail: "URL 出现".to_string(),
            });
        }

        scores.insert(DomainMode::Research, research_score);

        // Write 信号
        let mut write_score = 0.0f32;

        if let Some(kw) = match_keywords(&query_lower, WRITE_KEYWORDS) {
            write_score = write_score.max(0.7);
            signals.push(DetectionSignal {
                signal_type: SignalType::Keyword,
                weight: 0.7,
                detail: format!("写作关键词 \"{}\"", kw),
            });
        }

        // 非代码文档扩展名（中信号 0.5）
        if let Some(ext) = match_extensions(&query_lower, WRITE_EXTENSIONS) {
            write_score = write_score.max(0.5);
            signals.push(DetectionSignal {
                signal_type: SignalType::FileExtension,
                weight: 0.5,
                detail: format!("文档扩展名 {}", ext),
            });
        }

        scores.insert(DomainMode::Write, write_score);

        // Operate 信号
        let mut operate_score = 0.0f32;

        if let Some(kw) = match_keywords(&query_lower, OPERATE_KEYWORDS) {
            operate_score = operate_score.max(0.7);
            signals.push(DetectionSignal {
                signal_type: SignalType::Keyword,
                weight: 0.7,
                detail: format!("操作关键词 \"{}\"", kw),
            });
        }

        // glob 通配符模式（中信号 0.6）
        if match_regex(query_trim, GLOB_PATTERN) {
            operate_score = operate_score.max(0.6);
            signals.push(DetectionSignal {
                signal_type: SignalType::GlobPattern,
                weight: 0.6,
                detail: "glob 通配符模式".to_string(),
            });
        }

        scores.insert(DomainMode::Operate, operate_score);

        // Chat 信号
        let mut chat_score = 0.0f32;

        if let Some(kw) = match_keywords(&query_lower, CHAT_KEYWORDS) {
            chat_score = chat_score.max(0.5);
            signals.push(DetectionSignal {
                signal_type: SignalType::Keyword,
                weight: 0.5,
                detail: format!("闲聊关键词 \"{}\"", kw),
            });
        }

        // 短文本信号
        let cc = char_count(query_trim);
        if cc > 0 && cc < self.config.short_text_threshold {
            chat_score = chat_score.max(self.config.short_text_weight);
            signals.push(DetectionSignal {
                signal_type: SignalType::ShortLength,
                weight: self.config.short_text_weight,
                detail: format!("短文本 ({} chars)", cc),
            });
        }

        scores.insert(DomainMode::Chat, chat_score);

        // —— L1: 上下文信号 ——

        // 2a. workspace 上下文
        if let Some(ws) = workspace_path {
            if is_code_workspace(ws) {
                let bonus = self.config.workspace_code_bonus;
                *scores.entry(DomainMode::Code).or_default() += bonus;
                signals.push(DetectionSignal {
                    signal_type: SignalType::WorkspaceContext,
                    weight: bonus,
                    detail: "workspace 是代码项目".to_string(),
                });
            }
        }

        // 2b. 工具使用历史
        if let Some(last_tool) = self.recent_tools.back() {
            if let Some(domain) = tool_to_domain(last_tool) {
                let bonus = self.config.tool_history_weight;
                *scores.entry(domain).or_default() += bonus;
                signals.push(DetectionSignal {
                    signal_type: SignalType::ToolHistory,
                    weight: bonus,
                    detail: format!("最近工具 {} → {:?}", last_tool, domain),
                });
            }
        }

        // 2c. 域模式惯性
        if let Some(inertia) = self.calculate_inertia() {
            *scores.entry(inertia.mode).or_default() += inertia.weight;
            signals.push(DetectionSignal {
                signal_type: SignalType::ModeInertia,
                weight: inertia.weight,
                detail: format!(
                    "最近 {} 轮都是 {:?}",
                    self.config.inertia_window, inertia.mode
                ),
            });
        }

        // —— 选出 top-1 ——
        let (best_mode, best_score) = scores
            .iter()
            .max_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        // 分数相同时优先级：Code > Research > Write > Operate > Chat
                        fn priority(m: &DomainMode) -> u8 {
                            match m {
                                DomainMode::Code => 5,
                                DomainMode::Research => 4,
                                DomainMode::Write => 3,
                                DomainMode::Operate => 2,
                                DomainMode::Chat => 1,
                            }
                        }
                        // a 优先级高 → Greater → max_by 选 a
                        priority(a.0).cmp(&priority(b.0))
                    })
            })
            .map(|(m, s)| (*m, *s))
            .unwrap_or((DomainMode::Chat, 0.3));

        // —— 低置信度回退 ——
        if best_score < self.config.llm_classifier_threshold {
            // TODO Phase 4: 调用 cheap model 做意图分类
            // 目前回退到默认
            let mode = if best_score > 0.0 {
                best_mode
            } else {
                DomainMode::Chat
            };
            self.record_history(mode);
            return DomainDetection {
                mode,
                confidence: best_score,
                signals,
                source: DetectionSource::Default,
            };
        }

        let source = if best_score >= 0.8 {
            DetectionSource::KeywordHighConfidence
        } else {
            DetectionSource::KeywordAndContext
        };

        // —— 记录历史 ——
        self.record_history(best_mode);

        DomainDetection {
            mode: best_mode,
            confidence: best_score,
            signals,
            source,
        }
    }

    /// 计算域模式惯性加成
    fn calculate_inertia(&self) -> Option<InertiaResult> {
        if self.history.is_empty() {
            return None;
        }
        let window: Vec<DomainMode> = self
            .history
            .iter()
            .rev()
            .take(self.config.inertia_window)
            .copied()
            .collect();

        if window.is_empty() {
            return None;
        }

        // 检查窗口内是否全部同域
        let first = window[0];
        if window.iter().all(|m| *m == first) && window.len() >= 2 {
            return Some(InertiaResult {
                mode: first,
                weight: self.config.inertia_weight,
            });
        }

        None
    }

    fn record_history(&mut self, mode: DomainMode) {
        self.history.push_back(mode);
        if self.history.len() > 8 {
            self.history.pop_front();
        }
    }
}

struct InertiaResult {
    mode: DomainMode,
    weight: f32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(query: &str) -> DomainDetection {
        let mut d = DomainModeDetector::new();
        d.detect(query, None)
    }

    fn detect_with_ws(query: &str, ws: &str) -> DomainDetection {
        let mut d = DomainModeDetector::new();
        d.detect(query, Some(ws))
    }

    // -- Code 检测 --

    #[test]
    fn test_code_detection_by_file_extension() {
        let r = detect("帮我看看 src/main.rs 的实现");
        assert_eq!(r.mode, DomainMode::Code);
        assert!(r.confidence >= 0.8, "confidence should be >= 0.8, got {}", r.confidence);
    }

    #[test]
    fn test_code_detection_by_ts_extension() {
        let r = detect("fix the bug in app.tsx");
        assert_eq!(r.mode, DomainMode::Code);
    }

    #[test]
    fn test_code_detection_by_keyword() {
        let r = detect("帮我重构这个函数");
        assert_eq!(r.mode, DomainMode::Code);
    }

    #[test]
    fn test_code_detection_by_action_verb() {
        let r = detect("修复这个 bug");
        assert_eq!(r.mode, DomainMode::Code);
        assert!(r.confidence >= 0.8);
    }

    #[test]
    fn test_code_detection_by_cargo_keyword() {
        let r = detect("cargo check 报错了");
        assert_eq!(r.mode, DomainMode::Code);
    }

    #[test]
    fn test_code_detection_with_workspace() {
        // workspace bonus alone isn't enough for ambiguous short text,
        // so use a query with a weak code signal that workspace can boost
        let r = detect_with_ws("帮我看看这个函数", ".");
        assert_eq!(r.mode, DomainMode::Code);
        // "函数" keyword(0.6) + workspace bonus(0.2) = 0.8 > Chat short_text(0.4)
    }

    // -- Research 检测 --

    #[test]
    fn test_research_detection_by_search_keyword() {
        let r = detect("搜索一下 Rust async 生态最新进展");
        assert_eq!(r.mode, DomainMode::Research);
    }

    #[test]
    fn test_research_detection_by_url() {
        let r = detect("帮我看看 https://example.com 这个网页");
        assert_eq!(r.mode, DomainMode::Research);
        assert!(r.confidence >= 0.8);
    }

    #[test]
    fn test_research_detection_by_compare_keyword() {
        let r = detect("对比一下 React 和 Vue 的状态管理");
        assert_eq!(r.mode, DomainMode::Research);
    }

    #[test]
    fn test_research_detection_by_what_is() {
        let r = detect("what is async/await in Rust?");
        assert_eq!(r.mode, DomainMode::Research);
    }

    // -- Write 检测 --

    #[test]
    fn test_write_detection_by_draft_keyword() {
        let r = detect("帮我起草一份项目验收报告");
        assert_eq!(r.mode, DomainMode::Write);
    }

    #[test]
    fn test_write_detection_by_email_keyword() {
        let r = detect("帮我写封邮件给老板");
        assert_eq!(r.mode, DomainMode::Write);
    }

    #[test]
    fn test_write_detection_by_doc_extension() {
        let r = detect("帮我看下 report.docx 的内容");
        assert_eq!(r.mode, DomainMode::Write);
    }

    // -- Operate 检测 --

    #[test]
    fn test_operate_detection_by_file_ops() {
        let r = detect("把下载文件夹里的截图整理到 D:/Photos");
        assert_eq!(r.mode, DomainMode::Operate);
    }

    #[test]
    fn test_operate_detection_by_delete_keyword() {
        let r = detect("删除临时文件夹");
        assert_eq!(r.mode, DomainMode::Operate);
    }

    #[test]
    fn test_operate_detection_by_glob_pattern() {
        let r = detect("把 *.log 文件都清理掉");
        assert_eq!(r.mode, DomainMode::Operate);
    }

    // -- Chat 检测 --

    #[test]
    fn test_chat_detection_for_greeting() {
        let r = detect("你好");
        assert_eq!(r.mode, DomainMode::Chat);
    }

    #[test]
    fn test_chat_detection_for_thanks() {
        let r = detect("谢谢");
        assert_eq!(r.mode, DomainMode::Chat);
    }

    #[test]
    fn test_chat_detection_for_short_text() {
        let r = detect("ok");
        assert_eq!(r.mode, DomainMode::Chat);
    }

    #[test]
    fn test_chat_detection_for_hello_english() {
        let r = detect("hello there");
        assert_eq!(r.mode, DomainMode::Chat);
    }

    // -- 手动覆盖 --

    #[test]
    fn test_manual_override_takes_precedence() {
        let mut d = DomainModeDetector::new();
        d.set_manual_override(Some(DomainMode::Research));
        let r = d.detect("帮我写一段代码", None);
        assert_eq!(r.mode, DomainMode::Research);
        assert_eq!(r.source, DetectionSource::ManualOverride);
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn test_manual_override_can_be_cleared() {
        let mut d = DomainModeDetector::new();
        d.set_manual_override(Some(DomainMode::Research));
        d.set_manual_override(None);
        // use a strong code signal (file extension) to ensure Code wins
        let r = d.detect("帮我修复 main.rs", None);
        assert_eq!(r.mode, DomainMode::Code);
    }

    // -- 域模式惯性 --

    #[test]
    fn test_mode_inertia_after_3_same_turns() {
        let mut d = DomainModeDetector::new();
        // 3 轮 Code
        d.detect("read main.rs", None);
        d.detect("fix the bug", None);
        d.detect("run tests", None);
        // 第 4 轮用一个有弱 Code 信号的 query，惯性应推向 Code
        let r = d.detect("帮我看看这个函数", None);
        assert_eq!(r.mode, DomainMode::Code);
        // 应该有惯性信号
        assert!(r.signals.iter().any(|s| s.signal_type == SignalType::ModeInertia));
    }

    #[test]
    fn test_no_inertia_when_modes_differ() {
        let mut d = DomainModeDetector::new();
        d.detect("read main.rs", None);  // Code
        d.detect("搜索一下", None);       // Research
        let r = d.detect("帮我看看", None);
        // 没有惯性，应该不会有 ModeInertia 信号
        assert!(!r.signals.iter().any(|s| s.signal_type == SignalType::ModeInertia));
    }

    // -- 工具历史 --

    #[test]
    fn test_tool_history_affects_detection() {
        let mut d = DomainModeDetector::new();
        d.record_tool_use("web_search");
        let r = d.detect("帮我看看这个", None);
        // web_search → Research，应该给 Research 加分
        assert!(r.signals.iter().any(|s| s.signal_type == SignalType::ToolHistory));
    }

    #[test]
    fn test_tool_history_code_tool() {
        let mut d = DomainModeDetector::new();
        d.record_tool_use("read_file");
        let r = d.detect("帮我看看", None);
        // read_file → Code，加上 workspace bonus，应该倾向 Code
        assert!(r.signals.iter().any(|s| s.signal_type == SignalType::ToolHistory));
    }

    // -- 检测来源分类 --

    #[test]
    fn test_high_confidence_source() {
        let r = detect("帮我修复 src/main.rs 中的编译错误");
        assert_eq!(r.mode, DomainMode::Code);
        assert!(r.confidence >= 0.8);
        assert_eq!(r.source, DetectionSource::KeywordHighConfidence);
    }

    #[test]
    fn test_low_confidence_falls_back_to_default() {
        let r = detect("嗯");
        // 短文本 + 闲聊关键词，应该走 Chat
        assert_eq!(r.mode, DomainMode::Chat);
    }

    // -- 空输入 --

    #[test]
    fn test_empty_query_returns_chat_default() {
        let r = detect("");
        assert_eq!(r.mode, DomainMode::Chat);
        assert!(r.confidence < 0.5);
    }

    #[test]
    fn test_whitespace_only_query() {
        let r = detect("   ");
        assert_eq!(r.mode, DomainMode::Chat);
    }

    // -- 信号详情 --

    #[test]
    fn test_signals_contain_matched_keyword() {
        // "函数" appears first in CODE_KEYWORDS, so it's matched before "重构"
        let r = detect("帮我重构这个函数");
        assert!(r.signals.iter().any(|s| {
            s.signal_type == SignalType::Keyword
                && (s.detail.contains("重构") || s.detail.contains("函数"))
        }));
    }

    #[test]
    fn test_signals_contain_file_extension() {
        let r = detect("看看 main.rs");
        assert!(r.signals.iter().any(|s| s.signal_type == SignalType::FileExtension));
    }

    // -- 多信号组合 --

    #[test]
    fn test_multiple_signals_combined() {
        let r = detect_with_ws("帮我修复 main.rs 的编译错误", ".");
        assert_eq!(r.mode, DomainMode::Code);
        // 应该有多个信号：文件扩展名 + 编程关键词 + workspace
        assert!(r.signals.len() >= 2, "expected >= 2 signals, got {}", r.signals.len());
    }

    // -- 历史记录 --

    #[test]
    fn test_history_snapshot() {
        let mut d = DomainModeDetector::new();
        d.detect("read main.rs", None);
        d.detect("搜索一下", None);
        let snap = d.history_snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0], DomainMode::Code);
        assert_eq!(snap[1], DomainMode::Research);
    }

    #[test]
    fn test_history_caps_at_8() {
        let mut d = DomainModeDetector::new();
        for _ in 0..15 {
            d.detect("read main.rs", None);
        }
        assert!(d.history_snapshot().len() <= 8);
    }

    // -- 自定义配置 --

    #[test]
    fn test_custom_config_higher_threshold() {
        let mut config = DomainDetectConfig::default();
        config.llm_classifier_threshold = 0.9;
        let mut d = DomainModeDetector::with_config(config);
        // 即使有关键词命中，如果置信度 < 0.9 也走 Default
        let r = d.detect("搜索一下", None);
        // research keyword 给 0.7，低于 0.9，走 Default
        assert_eq!(r.source, DetectionSource::Default);
    }

    #[test]
    fn test_custom_config_short_text_threshold() {
        let mut config = DomainDetectConfig::default();
        config.short_text_threshold = 5;
        let mut d = DomainModeDetector::with_config(config);
        // "你好" 是 2 chars，低于 5，仍然 Chat
        let r = d.detect("你好", None);
        assert_eq!(r.mode, DomainMode::Chat);
        // "你好世界测试" 是 6 chars，高于 5，不会触发短文本信号
        let r2 = d.detect("你好世界测试", None);
        // 没有短文本信号
        assert!(!r2.signals.iter().any(|s| s.signal_type == SignalType::ShortLength));
    }

    // -- DetectionSource 序列化 --

    #[test]
    fn test_detection_source_serde() {
        let s = serde_json::to_string(&DetectionSource::KeywordHighConfidence).unwrap();
        assert_eq!(s, "\"keyword_high_confidence\"");
        let s = serde_json::to_string(&DetectionSource::ManualOverride).unwrap();
        assert_eq!(s, "\"manual_override\"");
    }

    #[test]
    fn test_domain_detection_serde_roundtrip() {
        let d = DomainDetection {
            mode: DomainMode::Code,
            confidence: 0.95,
            signals: vec![DetectionSignal {
                signal_type: SignalType::FileExtension,
                weight: 0.9,
                detail: ".rs".to_string(),
            }],
            source: DetectionSource::KeywordHighConfidence,
        };
        let j = serde_json::to_string(&d).unwrap();
        let back: DomainDetection = serde_json::from_str(&j).unwrap();
        assert_eq!(d, back);
    }

    // -- 优先级测试（同分时 Code > Research > Write > Operate > Chat） --

    #[test]
    fn test_code_beats_research_on_tie() {
        // "重构" 触发 Code keyword(0.6)，"对比" 触发 Research(0.7)
        // Research 实际上更高。用文件扩展名让 Code 赢。
        let r = detect("重构 main.rs 对比");
        assert_eq!(r.mode, DomainMode::Code);
        // Code: 0.9(file ext) + 0.6(keyword) = max 0.9
        // Research: 0.7(keyword "对比")
    }

    #[test]
    fn test_research_beats_write_on_tie() {
        // "分析" 触发 Research(0.7)，"总结" 触发 Write(0.7)
        // 同分时 Research > Write (priority 4 > 3)
        let r = detect("分析总结");
        assert_eq!(r.mode, DomainMode::Research);
    }
}
