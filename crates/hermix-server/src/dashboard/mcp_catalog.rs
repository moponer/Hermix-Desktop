//! MCP 目录：内置精选清单 → 浏览 / 一键添加。**纯本地，零网络**。
//!
//! # 2026-10-04：取消远程拉取
//!
//! 此前有三个数据源：官方 registry（`registry.modelcontextprotocol.io`）、
//! 用户自建子注册中心（官方 ghcr 镜像，API 同形）、以及打包在二进制里的内置
//! 精选目录。三个源连同磁盘缓存（`mcp_catalog.json` + 24h TTL）、游标分页
//! （`limit=100` × 5 页）、`mcp_registry.json` 源偏好、probe 端点**一并删除**。
//!
//! 决策依据：
//! - 官方源自己的描述是「**境外线路**，有代理时数据最全」——对国内用户是硬伤，
//!   而它在失败路径上并不比内置目录更可靠（实测 2026-10-01 直连超时）。
//! - 内置目录本就是设计上的确定性数据源，只是被放在了"兜底层"。
//! - 代价（明确接受）：目录从 ~500 条收窄到 21 条；新增条目或升版本必须跟
//!   应用发版走。这是纯本地方案的固有成本，不是缺陷。
//!
//! # 信任边界
//!
//! 目录里的东西是第三方写的，所以：
//! - 远端（HTTP）服务器：添加只写一条 URL 配置，**不执行任何代码** ——
//!   风险面仅是"连到谁的服务器"。
//! - 本地包（npm 等）：添加会变成 `npx <pkg>@<ver>`，即**在用户机器上执行
//!   第三方代码**。这类条目在前端明确标注，命令原样展示，不隐藏。
//! - `POST /api/mcp/catalog/install` 只接受目录 `id`，命令/URL 一律由服务端
//!   从目录解析 —— 客户端传什么 command/url 都不作数，否则这个端点就是个
//!   「任意命令执行」后门。
//!
//! # 关于「内置」
//!
//! 内置的是**目录与安装定义**，不是「开箱可用」。MCP 是外部服务的接入层，
//! 24 条里 13 条需要用户填自己的凭据（token / API key / DATABASE_URI）——
//! 且这 13 条几乎全是境外服务，国内直连不可用。
//! 2026-10-05 补了 3 条国内零凭据条目（必应中文搜索 / 中国天气 / 国际化
//! 工作流），把「国内可用且开箱可用」的空位填上；**仍不做 auto-seed** ——
//! 是否默认装是另一件事，用户没要求过，凭空塞进默认上下文只会占 token。

// 2026-10-04 取消远程拉取后，这些 import 随取数层一并失效：
//   PathBuf/SystemTime/UNIX_EPOCH ← 磁盘缓存路径与 now_secs
//   Query/IntoResponse/Response    ← ?refresh=1 查询参数与 .into_response() 链
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::HttpServerState;
use crate::dashboard::mcp::{self, McpServerEntry, McpTransport};

// ── 对外形状（前端消费的契约）──────────────────────────────

/// 一条目录项的安装方式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CatalogInstall {
    /// 远端 HTTP：只需 URL，不执行代码。
    Remote { url: String },
    /// npm 包：会执行 `npx <package>@<version>`。
    Npm { package: String, version: Option<String> },
    /// 其他包管理器（pypi 等）：**不自动装**，前端给出手填命令的预填值。
    Manual {
        package: String,
        version: Option<String>,
        hint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogItem {
    /// registry 里的反域名 id，install 端点只认它。
    pub id: String,
    pub title: String,
    pub description: String,
    pub version: Option<String>,
    pub homepage: Option<String>,
    /// registry 官方标记（`_meta...official.status == active`）。
    pub official: bool,
    pub install: CatalogInstall,
    /// 包声明里必填的环境变量名（`${API_KEY}` 之类），前端引导用户填。
    pub required_env: Vec<String>,
}

/// 目录项 + 本地化 + 扫描结论（前端一次拿全，不用二次请求）。
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub item: CatalogItem,
    /// 中文名（内置对照表命中时非空）
    pub title_zh: Option<String>,
    /// 中文描述（内置对照表命中时非空）
    pub description_zh: Option<String>,
    /// 分类（中文键）：办公 / 开发 / 检索 / 通讯 / 数据 / 通用
    pub category: &'static str,
    pub scan: super::mcp_scan::ScanResult,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogResponse {
    pub items: Vec<CatalogEntry>,
}

// ── 内置精选目录（批次 M1，2026-10-04 起为唯一数据源）──────────
//
// 编译期打包 catalog_fallback.json（24 条人工核验的常用 server，形状 =
// CatalogItem）。此前它是三层取数链的**兜底层**（磁盘缓存 → 联网拉取 →
// 内置），取消远程拉取后它就是目录本身：任何网络状况下都返回同一份内容，
// 目录因此从一个「会随网络变化的列表」变成确定性函数。
//
// 数据源说明与条目准入标准见该 JSON 的 comment 字段——其中「不得与内建工具
// 重复」和「同一能力只留一个」两条硬约束是 2026-10-04 收窄 32→21 时新增的。
const FALLBACK_JSON: &str = include_str!("catalog_fallback.json");

/// 解析内置目录。形状损坏（发布事故）时返回空表并记日志——
/// 不 panic：目录是体验增强，不是正确性依赖。
fn builtin_items() -> Vec<CatalogItem> {
    match serde_json::from_str::<serde_json::Value>(FALLBACK_JSON) {
        Ok(v) => serde_json::from_value::<Vec<CatalogItem>>(v["items"].clone()).unwrap_or_else(|e| {
            tracing::warn!("内置 MCP 目录解析失败（发布产物损坏？）：{e}");
            Vec::new()
        }),
        Err(e) => {
            tracing::warn!("内置 MCP 目录 JSON 不可读：{e}");
            Vec::new()
        }
    }
}


// ── 本地化（内置中文对照表）────────────────────────────────
//
// registry 的 description 是英文，用户要的是中文。策略：**内置一份人工
// 校对过的常用条目中文表**，命中就用中文；未收录的退回英文原文（不做
// 运行时翻译 —— 那要额外延迟和 token 成本，且目录会一直缺中文）。
// 收录标准：知名度高 + 我们确实建议用户装的。新增条目直接往表里加一行。
#[derive(Clone, Copy)]
struct ZhEntry {
    /// registry id 的关键字（小写包含匹配）
    match_key: &'static str,
    title: &'static str,
    desc: &'static str,
    category: &'static str,
}

const ZH: &[ZhEntry] = &[
    ZhEntry { match_key: "filesystem", title: "文件系统", desc: "读写本地文件与目录，AI 常用的基础能力。", category: "办公" },
    ZhEntry { match_key: "git", title: "Git", desc: "查看提交记录、分支与差异。", category: "开发" },
    ZhEntry { match_key: "github", title: "GitHub", desc: "管理 Issue、PR 与仓库内容。", category: "开发" },
    ZhEntry { match_key: "gitlab", title: "GitLab", desc: "管理 GitLab 的 Issue 与合并请求。", category: "开发" },
    ZhEntry { match_key: "fetch", title: "网页抓取", desc: "抓取网页正文并转成适合阅读的文本。", category: "检索" },
    ZhEntry { match_key: "brave", title: "Brave 搜索", desc: "联网搜索并返回带来源的结果。", category: "检索" },
    ZhEntry { match_key: "tavily", title: "Tavily 搜索", desc: "面向 AI 的搜索接口，返回精简结果。", category: "检索" },
    ZhEntry { match_key: "perplexity", title: "Perplexity 搜索", desc: "带引用的问答式检索。", category: "检索" },
    ZhEntry { match_key: "exa", title: "Exa 搜索", desc: "语义化网页搜索与内容检索。", category: "检索" },
    // 2026-10-05 国内零凭据批次。match_key 取 slug 的首段分词：bing /
    // weather / i18n 均 ≥4 字符走前缀匹配，已核对不会与既有键抢命中
    // （三个 slug 分词后无更长的既有键能前缀命中）。
    ZhEntry { match_key: "bing", title: "必应中文搜索", desc: "抓取必应中文结果并提取网页正文，免 API key，国内直连可用。", category: "检索" },
    ZhEntry { match_key: "weather", title: "中国天气网", desc: "查询任意中文城市的实时天气与 5 天预报，含湿度与 AQI，免 API key。", category: "检索" },
    ZhEntry { match_key: "i18n", title: "国际化工作流", desc: "提取硬编码文案、生成 i18n key、检查翻译文件缺失与死 key，纯本地免 key。", category: "开发" },
    ZhEntry { match_key: "context7", title: "Context7 文档", desc: "按需拉取第三方库的最新版文档。", category: "开发" },
    ZhEntry { match_key: "memory", title: "记忆", desc: "跨会话保存与召回事实与偏好。", category: "通用" },
    ZhEntry { match_key: "sequential", title: "结构化思考", desc: "把复杂任务拆成可检查的推理步骤。", category: "通用" },
    ZhEntry { match_key: "sqlite", title: "SQLite", desc: "查询本地 SQLite 数据库。", category: "数据" },
    ZhEntry { match_key: "postgres", title: "PostgreSQL", desc: "查询 PostgreSQL 数据库。", category: "数据" },
    ZhEntry { match_key: "mysql", title: "MySQL", desc: "查询 MySQL 数据库。", category: "数据" },
    ZhEntry { match_key: "mongo", title: "MongoDB", desc: "查询 MongoDB。", category: "数据" },
    ZhEntry { match_key: "playwright", title: "浏览器自动化", desc: "驱动真实浏览器点击、填表、截图。", category: "办公" },
    ZhEntry { match_key: "puppeteer", title: "浏览器自动化", desc: "驱动 Chromium 完成网页操作。", category: "办公" },
    ZhEntry { match_key: "slack", title: "Slack", desc: "读取频道消息、发送通知。", category: "通讯" },
    ZhEntry { match_key: "notion", title: "Notion", desc: "读写 Notion 页面与数据库。", category: "办公" },
    ZhEntry { match_key: "feishu", title: "飞书", desc: "读写多维表格、发消息。", category: "办公" },
    ZhEntry { match_key: "lark", title: "飞书（Lark）", desc: "飞书海外版的多维表格与消息。", category: "办公" },
    ZhEntry { match_key: "figma", title: "Figma", desc: "读取设计稿的节点与样式。", category: "办公" },
    ZhEntry { match_key: "sentry", title: "Sentry", desc: "查询线上错误与性能数据。", category: "开发" },
    ZhEntry { match_key: "github-mcp", title: "GitHub 代码", desc: "搜索与读取仓库中的代码。", category: "开发" },
    ZhEntry { match_key: "docker", title: "Docker", desc: "列出与管理本机容器。", category: "开发" },
    ZhEntry { match_key: "kubernetes", title: "Kubernetes", desc: "查询与管理集群资源。", category: "开发" },
    ZhEntry { match_key: "grafana", title: "Grafana", desc: "查询监控面板与告警。", category: "开发" },
    ZhEntry { match_key: "linear", title: "Linear", desc: "管理需求与任务。", category: "办公" },
    ZhEntry { match_key: "jira", title: "Jira", desc: "查询与创建工单。", category: "办公" },
    ZhEntry { match_key: "stripe", title: "Stripe", desc: "查询订单与订阅数据。", category: "数据" },
    ZhEntry { match_key: "supabase", title: "Supabase", desc: "管理数据库与鉴权配置。", category: "数据" },
    ZhEntry { match_key: "time", title: "时间", desc: "时区换算与当前时间查询。", category: "通用" },
];

fn zh_for(item: &CatalogItem) -> Option<&'static ZhEntry> {
    // 按「词」匹配：id 取最后一段 slug，与标题一起按非字母数字切词；长
    // 关键词优先；命中 = 词相等，或（关键词 ≥4 字符时）词以其开头。
    // 此前是「整条 id 包含关键词」——"io.github." 天然包含 "git"，所有
    // GitHub 托管的 server 全被翻成了 Git 的名字与描述；"digital" 里的
    // "git" 同理误伤。
    let slug = item.id.rsplit('/').next().unwrap_or(&item.id);
    let slug_l = slug.to_lowercase();
    let title = item.title.to_lowercase();
    let tokens: Vec<&str> = slug_l
        .split(|c: char| !c.is_ascii_alphanumeric())
        .chain(title.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|t| !t.is_empty())
        .collect();
    let mut keys: Vec<&ZhEntry> = ZH.iter().collect();
    keys.sort_by_key(|z| std::cmp::Reverse(z.match_key.len()));
    keys.into_iter()
        .find(|z| {
            tokens.iter().any(|t| {
                *t == z.match_key || (z.match_key.len() >= 4 && t.starts_with(z.match_key))
            })
        })
}

/// 组装成前端消费的条目：中文 + 扫描结论。
fn to_entry(item: CatalogItem) -> CatalogEntry {
    let zh = zh_for(&item).copied();
    let scan = super::mcp_scan::scan(&item);
    CatalogEntry {
        item,
        title_zh: zh.map(|z| z.title.to_string()),
        description_zh: zh.map(|z| z.desc.to_string()),
        category: zh.map(|z| z.category).unwrap_or("其它"),
        scan,
    }
}

// ── 端点 ─────────────────────────────────────────────────

/// `GET /api/mcp/catalog` — 内置精选目录。
///
/// 无参数、无缓存、无网络：目录是编译期打包的常量，返回的是一个确定性结果。
/// 此前带 `?refresh=1` 与 24h TTL 缓存，是三层取数链的遗留；缓存唯一的作用
/// 是掩盖「联网时看到 A、断网时看到 B」——取消远程拉取后该问题不存在。
pub async fn get_catalog(State(_state): State<HttpServerState>) -> Json<CatalogResponse> {
    Json(CatalogResponse {
        items: builtin_items().into_iter().map(to_entry).collect(),
    })
}

#[derive(Deserialize)]
pub struct InstallRequest {
    /// 目录 id。**只有这个字段来自客户端**。
    pub id: String,
    /// 覆盖版本（可选，默认用目录里的最新）。
    #[serde(default)]
    pub version: Option<String>,
    /// 必填环境变量的值（key = 目录声明的 required_env）。
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// 二次确认：扫描结论为「需确认」时，前端把命令与风险逐条展示给用户后
    /// 再带这个标记重发。**不带就拒** —— 提示可以糊弄，标记不能白送。
    #[serde(default)]
    pub acknowledge: bool,
}

/// `POST /api/mcp/catalog/install` — 从目录添加一条 MCP 配置。
///
/// 命令 / URL 全部由服务端从目录解析（见模块头「信任边界」），客户端
/// 只能给 id、可选版本和环境变量值。已存在同名 id → 更新而不是重复添加。
pub async fn install(
    State(state): State<HttpServerState>,
    Json(req): Json<InstallRequest>,
) -> Result<Json<McpServerEntry>, (StatusCode, String)> {
    // 直读内置目录。此前读的是磁盘缓存（`mcp_catalog.json`），于是"必须先
    // 打开过一次目录页"才装得上——而 `builtin-offline` 源恰恰不写缓存，
    // 在该源下点安装必然 412。取消远程拉取后没有缓存可读，这个死结自动解开。
    let items = builtin_items();
    let item = items
        .iter()
        .find(|i| i.id == req.id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("目录里没有 '{}'", req.id)))?;

    // ── 安全关卡 ──
    let scan = super::mcp_scan::scan(item);
    if scan.verdict == super::mcp_scan::Verdict::Block {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "该条目未通过安全扫描，已拒绝安装：{}",
                scan.hits
                    .iter()
                    .map(|h| h.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ));
    }
    if scan.verdict == super::mcp_scan::Verdict::Warn && !req.acknowledge {
        // 428：客户端应带 acknowledge=true 重发（此时 UI 已经把命令与风险
        // 逐条展示给用户看过并要求确认）。
        return Err((
            StatusCode::PRECONDITION_REQUIRED,
            format!(
                "该条目需要确认后安装。将要执行：{}。风险：{}",
                scan.command_preview.clone().unwrap_or_default(),
                scan.hits
                    .iter()
                    .map(|h| h.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ));
    }
    let version = req.version.clone().or_else(|| item.version.clone());
    let (transport, command, args, url) = match &item.install {
        CatalogInstall::Remote { url } => (McpTransport::Http, None, None, Some(url.clone())),
        CatalogInstall::Npm { package, .. } => {
            let pkg = match &version {
                Some(v) => format!("{package}@{v}"),
                None => package.clone(),
            };
            // `npx -y`：非交互 + 自动装。没有全局安装，不污染用户环境。
            (
                McpTransport::Stdio,
                Some("npx".to_string()),
                Some(vec!["-y".to_string(), pkg]),
                None,
            )
        }
        CatalogInstall::Manual { package, version: _, hint } => {
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                format!("该条目需要手动配置（{hint}）包：{package}"),
            ))
        }
    };

    let missing: Vec<&String> = item
        .required_env
        .iter()
        .filter(|k| !req.env.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "缺少必填环境变量：{}",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、")
            ),
        ));
    }

    let entry = McpServerEntry {
        id: item.id.clone(),
        name: item.title.clone(),
        transport,
        command,
        args,
        url,
        token: None, // 目录安装走 npm/官方源，无凭据；远程 token 走手动添加
        env: if req.env.is_empty() { None } else { Some(req.env) },
        enabled: true,
        description: Some(item.description.clone()),
        created_at: String::new(), // create_server 路径会补
    };
    mcp::create_server(State(state), Json(entry))
        .await
        .map_err(|(s, e)| (s, e))
}

// ── 测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 目录形状一旦漂移，整表反序列化就会失败（曾发生：8 条 `kind:"pypi"`，
    /// 而枚举只有 remote/npm/manual）→ 目录静默变 0 条，用户看到空面板而
    /// 没有任何错误提示。取消远程拉取后这份 JSON 是**唯一**数据源，这条更关键。
    #[test]
    fn builtin_catalog_parses_nonempty() {
        assert!(
            builtin_items().len() >= 15,
            "内置目录解析失败或过少：{}",
            builtin_items().len()
        );
    }

    /// id 重复会让 install 端点永远命中第一条，用户再也装不上第二个。
    #[test]
    fn builtin_catalog_has_no_duplicate_ids() {
        let mut seen = HashSet::new();
        for it in builtin_items() {
            assert!(seen.insert(it.id.clone()), "目录内 id 重复: {}", it.id);
        }
    }

    /// **本次收窄的核心不变式**：目录不得收进与内建工具重复的条目。
    ///
    /// 内建已有 file(9 工具) / memory / web_extract / markitdown / read_pdf /
    /// terminal / browser(8)+cua(16) 等 34 个 toolset。重复的 MCP server 只会
    /// 给 LLM 增加同义工具、拉高 token 成本、并在选工具时制造歧义——2026-10-04
    /// 据此从 32 条砍到 21 条。这条测试把该决策钉死，防止以后"看着眼熟又加回来"。
    #[test]
    fn builtin_catalog_does_not_duplicate_builtin_tools() {
        // slug 关键字 → 已被内建能力覆盖的 server 片段
        const BUILTIN_COVERED: &[&str] = &[
            "filesystem", // ↔ file toolset
            "memory",     // ↔ memory + daily_memory
            "sequential", // ↔ 内建思考/压缩
            "fetch",      // ↔ web_extract
            "markitdown", // ↔ 内建 markitdown
            "commands",   // ↔ terminal
            "puppeteer",  // ↔ browser + cua
            "browserbase",// ↔ browser + cua
            "everything", // reference test server，不该进正式目录
        ];
        for it in builtin_items() {
            let slug = it.id.rsplit('/').next().unwrap_or("").to_lowercase();
            for covered in BUILTIN_COVERED {
                assert!(
                    !slug.contains(covered),
                    "目录收进了与内建工具重复的条目: {}（命中 \"{covered}\"）",
                    it.id
                );
            }
        }
    }

    /// 同一能力只留一个。旧版有两个 notion、两个 postgres（且权限模型相反：
    /// 一个 read-only、一个 full access + DATABASE_URI）。
    #[test]
    fn builtin_catalog_has_no_same_capability_duplicates() {
        let items = builtin_items();
        let count = |needle: &str| {
            items
                .iter()
                .filter(|i| i.id.to_lowercase().contains(needle))
                .count()
        };
        for cap in ["notion", "postgres", "sqlite", "redis", "qdrant"] {
            assert_eq!(count(cap), 1, "「{cap}」类应只保留 1 条，实际 {}", count(cap));
        }
    }

    /// 声明了 env 的条目必须给出非空变量名——空串会让前端渲染出一个没有
    /// label 的输入框，用户无从填写。
    #[test]
    fn required_env_names_are_nonempty() {
        for it in builtin_items() {
            for k in &it.required_env {
                assert!(!k.trim().is_empty(), "{} 声明了空的 env 名", it.id);
            }
        }
    }

    /// 每条都要有中文描述路径可走：内置目录不再有"在线英文兜底"，
    /// 前端展示直接依赖 title_zh / description_zh。
    #[test]
    fn builtin_items_have_description() {
        for it in builtin_items() {
            assert!(!it.description.trim().is_empty(), "{} 缺描述", it.id);
            assert!(!it.title.trim().is_empty(), "{} 缺标题", it.id);
        }
    }

    fn zh_item(id: &str, title: &str) -> CatalogItem {
        CatalogItem {
            id: id.into(),
            title: title.into(),
            description: "d".into(),
            version: None,
            homepage: None,
            official: false,
            install: CatalogInstall::Remote { url: "https://x".into() },
            required_env: vec![],
        }
    }

    #[test]
    fn github_scope_不会把一切翻成git() {
        // "io.github." 天然包含 "git"——整条 id 子串匹配曾把所有 GitHub
        // 托管的 server 全翻成 Git 的名字与描述。
        let it = zh_item("io.github.modelcontextprotocol/server-github", "GitHub");
        assert_eq!(zh_for(&it).map(|z| z.title), Some("GitHub"));
    }

    #[test]
    fn slug_git_仍然命中() {
        let it = zh_item("io.github.modelcontextprotocol/server-git", "Git");
        assert_eq!(zh_for(&it).map(|z| z.title), Some("Git"));
    }

    #[test]
    fn github_键优先于git前缀() {
        let it = zh_item("io.github.github-mcp-server", "GitHub MCP");
        assert_eq!(zh_for(&it).map(|z| z.title), Some("GitHub"));
    }

    #[test]
    fn digital里的git不再误伤() {
        let it = zh_item("io.github.digitalocean/mcp", "DigitalOcean");
        // 用 is_none() 而非 assert_eq!(.., None)：ZhEntry 只 derive 了
        // Clone/Copy，没有 Debug/PartialEq，assert_eq! 编译不过。
        assert!(zh_for(&it).is_none());
    }
}
