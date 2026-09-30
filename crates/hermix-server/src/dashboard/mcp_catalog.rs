//! MCP 社区目录：拉取官方 registry → 归一化 → 缓存 → 浏览 / 一键添加。
//!
//! 数据源是 MCP 官方 registry（`registry.modelcontextprotocol.io`），即社区
//! 自己发布的清单。**信任边界**：目录里的东西是第三方写的，所以
//!
//! - 远端（HTTP）服务器：添加只写一条 URL 配置，**不执行任何代码** ——
//!   目录里绝大多数是这类，风险面仅是"连到谁的服务器"。
//! - 本地包（npm 等）：添加会变成 `npx <pkg>@<ver>`，即**在用户机器上执行
//!   第三方代码**。这类条目在前端明确标注，命令原样展示，不隐藏。
//! - `POST /api/mcp/catalog/install` 只接受目录 `id`，命令/URL 一律由服务端
//!   从目录解析 —— 客户端传什么 command/url 都不作数，否则这个端点就是个
//!   「任意命令执行」后门。
//!
//! 网络：直连。实测（2026-09-30）官方 registry 在国内可直连（~1s），
//! 所以默认路径不需要代理；被公司网 / 校园网策略拦截时，reqwest 会自动
//! 走 `HTTPS_PROXY`/`HTTP_PROXY`（开着系统代理的「环境变量」开关即生效）。
//! 拉不到就返回**上一次缓存**并标注 `stale`，绝不因为联网失败让目录空白。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::HttpServerState;
use crate::dashboard::mcp::{self, McpServerEntry, McpTransport};

const REGISTRY_URL: &str = "https://registry.modelcontextprotocol.io/v0.1/servers?limit=200";
/// 缓存新鲜期。目录变动很慢，一天一次足够；也让"没网也能开面板"。
const CACHE_TTL_SECS: u64 = 24 * 3600;
const FETCH_TIMEOUT_SECS: u64 = 20;

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
    pub fetched_at: u64,
    /// 这次返回的是磁盘缓存（没网 / 拉取失败）。
    pub from_cache: bool,
    /// 拉取失败时的原因（`from_cache` 为 true 时也有，好让 UI 说明数据可能旧了）。
    pub stale_reason: Option<String>,
}

// ── 缓存 ─────────────────────────────────────────────────

fn cache_path(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("mcp_catalog.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogCache {
    fetched_at: u64,
    items: Vec<CatalogItem>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache(state: &HttpServerState) -> Option<CatalogCache> {
    let raw = std::fs::read_to_string(cache_path(state)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(state: &HttpServerState, cache: &CatalogCache) {
    let path = cache_path(state);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(path, json);
    }
}

// ── 拉取 + 归一化 ────────────────────────────────────────

fn http_client() -> reqwest::Client {
    let b = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .user_agent("hermix-desktop/mcp-catalog");
    // 直连即可（实测可通）。reqwest 原生读 `HTTPS_PROXY` / `HTTP_PROXY`，
    // 被网络策略拦截时正好复用系统代理 —— 无需额外代码，也不改配置 schema。
    b.build().unwrap_or_default()
}

/// registry 原始条目（只取我们要的字段，其余忽略）。
#[derive(Deserialize)]
struct RawEnvelope {
    #[serde(default)]
    server: RawServer,
    #[serde(default)]
    _meta: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct RawServer {
    #[serde(default)]
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    remotes: Vec<RawRemote>,
    #[serde(default)]
    packages: Vec<RawPackage>,
}

#[derive(Deserialize)]
struct RawRemote {
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct RawPackage {
    #[serde(rename = "registryType", default)]
    registry_type: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "environmentVariables")]
    env_vars: Vec<RawEnvVar>,
}

#[derive(Deserialize)]
struct RawEnvVar {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "isRequired")]
    is_required: bool,
}

fn env_placeholders(raw: &str) -> Vec<String> {
    // registry 的 env 声明常写成 `${TOKEN}` / `${env:TOKEN}`，占位符名就是
    // 用户必须自己填的 key。这里两种写法都认。
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(i) = rest.find("${") {
        rest = &rest[i + 2..];
        let Some(end) = rest.find('}') else { break };
        let inner = &rest[..end];
        let key = inner.rsplit(':').next().unwrap_or(inner).trim();
        if !key.is_empty() && !out.iter().any(|k: &String| k == key) {
            out.push(key.to_string());
        }
        rest = &rest[end + 1..];
    }
    out
}

fn normalize(env: &RawEnvelope) -> Option<CatalogItem> {
    let s = &env.server;
    if s.name.trim().is_empty() {
        return None;
    }
    let description = s.description.clone().unwrap_or_default();
    if description.trim().is_empty() {
        return None;
    }
    // 远端优先：不执行代码，永远比本地包安全。
    let install = if let Some(r) = s.remotes.iter().find(|r| !r.url.trim().is_empty()) {
        CatalogInstall::Remote { url: r.url.clone() }
    } else if let Some(p) = s.packages.iter().find(|p| !p.identifier.trim().is_empty()) {
        match p.registry_type.as_str() {
            "npm" => CatalogInstall::Npm {
                package: p.identifier.clone(),
                version: p.version.clone(),
            },
            other => CatalogInstall::Manual {
                package: p.identifier.clone(),
                version: p.version.clone(),
                hint: format!(
                    "{other} 包：Hermix 不自动执行未知包管理器，请确认后在「手动添加」里填好命令。"
                ),
            },
        }
    } else {
        return None; // 既无 remote 也无 package：装了也用不了
    };

    let mut required_env: Vec<String> = Vec::new();
    for venv in s.packages.iter().flat_map(|p| p.env_vars.iter()) {
        if venv.is_required && !venv.name.is_empty() && !required_env.contains(&venv.name) {
            required_env.push(venv.name.clone());
        }
    }
    // 兜底：描述里出现 ${XXX} 也算要用户填。
    for key in env_placeholders(&description) {
        if !required_env.contains(&key) {
            required_env.push(key);
        }
    }

    let official = env
        ._meta
        .get("io.modelcontextprotocol.registry/official")
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        == Some("active");

    Some(CatalogItem {
        id: s.name.clone(),
        title: s
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| s.name.clone()),
        description,
        version: s.version.clone(),
        homepage: s.homepage.clone(),
        official,
        install,
        required_env,
    })
}

fn is_latest(env: &RawEnvelope) -> bool {
    env._meta
        .get("io.modelcontextprotocol.registry/official")
        .and_then(|v| v.get("isLatest"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

async fn fetch_remote() -> Result<Vec<CatalogItem>, String> {
    let text = http_client()
        .get(REGISTRY_URL)
        .send()
        .await
        .map_err(|e| format!("请求 registry 失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("registry 返回错误：{e}"))?
        .text()
        .await
        .map_err(|e| format!("读取 registry 响应失败：{e}"))?;
    let raw: RawList = serde_json::from_str(&text).map_err(|e| format!("解析 registry 响应失败：{e}"))?;

    // 同一 id 有多个版本：优先 isLatest，其次后出现者。
    let mut picked: Vec<(bool, CatalogItem)> = Vec::new();
    for env in raw.servers {
        let Some(item) = normalize(&env) else { continue };
        let latest = is_latest(&env);
        match picked.iter_mut().find(|(_, i)| i.id == item.id) {
            Some(slot) => {
                if latest && !slot.0 {
                    *slot = (true, item);
                }
            }
            None => picked.push((latest, item)),
        }
    }
    Ok(picked.into_iter().map(|(_, i)| i).collect())
}

#[derive(Deserialize)]
struct RawList {
    #[serde(default)]
    servers: Vec<RawEnvelope>,
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
    let id = item.id.to_lowercase();
    let title = item.title.to_lowercase();
    ZH.iter()
        .find(|z| id.contains(z.match_key) || title.contains(z.match_key))
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

#[derive(Deserialize, Default)]
pub struct CatalogQuery {
    /// `?refresh=1` 跳过缓存强制重拉。
    #[serde(default)]
    refresh: bool,
}

/// `GET /api/mcp/catalog` — 社区目录（缓存优先，24h 过期）。
pub async fn get_catalog(
    State(state): State<HttpServerState>,
    Query(q): Query<CatalogQuery>,
) -> Response {
    let cache = read_cache(&state);
    let fresh = cache
        .as_ref()
        .is_some_and(|c| now_secs().saturating_sub(c.fetched_at) < CACHE_TTL_SECS);
    if !q.refresh && fresh {
        let c = cache.unwrap();
        return Json(CatalogResponse {
            items: c.items.into_iter().map(to_entry).collect(),
            fetched_at: c.fetched_at,
            from_cache: true,
            stale_reason: None,
        })
        .into_response();
    }

    match fetch_remote().await {
        Ok(items) => {
            let fetched_at = now_secs();
            write_cache(&state, &CatalogCache { fetched_at, items: items.clone() });
            Json(CatalogResponse {
                items: items.into_iter().map(to_entry).collect(),
                fetched_at,
                from_cache: false,
                stale_reason: None,
            })
            .into_response()
        }
        Err(e) => {
            // 拉不到就退回上一次缓存（可能旧，但比空白强）。
            match cache {
                Some(c) => Json(CatalogResponse {
                    items: c.items.into_iter().map(to_entry).collect(),
                    fetched_at: c.fetched_at,
                    from_cache: true,
                    stale_reason: Some(format!("联网拉取失败，正在显示上一次缓存：{e}")),
                })
                .into_response(),
                None => (
                    StatusCode::BAD_GATEWAY,
                    // 实测（2026-09-30）：官方 registry 在国内可直连（~1s），
                    // 所以"开代理"不是常规解法。真正会被拦的是公司网/校园网/
                    // 严格出口策略，那才有设 HTTPS_PROXY 的必要。
                    Json(serde_json::json!({
                        "error": e,
                        "hint": "请检查网络连接。官方 registry 在国内通常可直连；若你的网络策略拦截了它，可开启系统代理的「环境变量」后重试。"
                    })),
                )
                    .into_response(),
            }
        }
    }
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
    let cache = read_cache(&state)
        .ok_or_else(|| (StatusCode::PRECONDITION_FAILED, "目录尚未加载，请先打开一次目录".to_string()))?;
    let item = cache
        .items
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

    fn env_with(desc: &str) -> RawEnvelope {
        RawEnvelope {
            server: RawServer {
                name: "acme/demo".into(),
                title: Some("Demo".into()),
                description: Some(desc.into()),
                version: Some("1.2.3".into()),
                homepage: None,
                remotes: vec![],
                packages: vec![],
            },
            _meta: Default::default(),
        }
    }

    #[test]
    fn remote_entry_normalizes_to_remote_install() {
        let mut e = env_with("demo server");
        e.server.remotes.push(RawRemote { url: "https://x.example/mcp".into() });
        let it = normalize(&e).expect("should normalize");
        assert!(it.official == false);
        match it.install {
            CatalogInstall::Remote { url } => assert_eq!(url, "https://x.example/mcp"),
            other => panic!("expected remote, got {other:?}"),
        }
    }

    #[test]
    fn npm_entry_normalizes_to_npm_install_with_version() {
        let mut e = env_with("demo server");
        e.server.packages.push(RawPackage {
            registry_type: "npm".into(),
            identifier: "demo-mcp".into(),
            version: Some("1.2.3".into()),
            env_vars: vec![],
        });
        let it = normalize(&e).expect("should normalize");
        assert!(matches!(it.install, CatalogInstall::Npm { .. }));
        assert_eq!(it.version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn unknown_package_manager_becomes_manual_not_auto_exec() {
        let mut e = env_with("demo server");
        e.server.packages.push(RawPackage {
            registry_type: "pypi".into(),
            identifier: "demo".into(),
            version: None,
            env_vars: vec![],
        });
        let it = normalize(&e).expect("should normalize");
        assert!(
            matches!(it.install, CatalogInstall::Manual { .. }),
            "pypi must not auto-exec"
        );
    }

    #[test]
    fn entry_without_transport_and_package_is_dropped() {
        let e = env_with("x");
        assert!(normalize(&e).is_none(), "既无 remote 也无 package 的条目不可用");
    }

    #[test]
    fn missing_description_is_dropped() {
        let mut e = env_with("demo server");
        e.server.description = Some("   ".into());
        e.server.remotes.push(RawRemote { url: "https://x".into() });
        assert!(normalize(&e).is_none());
    }

    #[test]
    fn official_meta_marks_item() {
        let mut e = env_with("demo server");
        e.server.remotes.push(RawRemote { url: "https://x".into() });
        e._meta.insert(
            "io.modelcontextprotocol.registry/official".into(),
            serde_json::json!({ "status": "active" }),
        );
        assert!(normalize(&e).expect("normalize").official);
    }

    #[test]
    fn required_env_placeholder_is_extracted() {
        let mut e = env_with("需要 ${API_KEY} 和 ${env:TOKEN}");
        e.server.remotes.push(RawRemote { url: "https://x".into() });
        let it = normalize(&e).expect("normalize");
        assert!(it.required_env.contains(&"API_KEY".to_string()));
        assert!(it.required_env.contains(&"TOKEN".to_string()));
    }
}
