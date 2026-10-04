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
//! 网络：直连。**2026-10-01 实测修正**：官方 registry 在部分国内网络下
//! 直连超时（HTTP 000），此前 09-30 的"可直连"结论不普适。reqwest 会自动
//! 走 `HTTPS_PROXY`/`HTTP_PROXY`；拉取失败时依序回退**上一次缓存 → 内置
//! 精选目录**（`catalog_fallback.json`，见 FALLBACK_JSON），绝不因联网
//! 失败让目录空白或 502。
//!
//! **自建子注册中心**（批次 M2 `self-hosted` 源）：官方 registry 开源且
//! API 形状专为子注册中心设计，Docker 一条命令即可运行同形镜像——
//!
//! ```text
//! # 需要 PostgreSQL（官方 docker-compose.yml 有完整编排）
//! docker run -p 8080:8080 ghcr.io/modelcontextprotocol/registry:latest
//! ```
//!
//! 然后在目录面板把数据源切到「自建子注册中心」并填入
//! `http://127.0.0.1:8080`（或设 env `HERMIX_MCP_REGISTRY_URL`，优先级
//! 更高）。切换只影响目录列表的来源；install 端点仍按 server 反域名
//! id 落本机配置，与源无关。源清单与切换端点见 `get_sources` /
//! `set_source` / `probe`。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::HttpServerState;
use crate::dashboard::mcp::{self, McpServerEntry, McpTransport};

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

// ── 目录源（批次 M2：多源手动切换）─────────────────────────
//
// 优先级：`HERMIX_MCP_REGISTRY_URL` env（显式覆盖，同 HERMIX_TLS_ADDR
// 模式）> 用户设置存的源 id（hermix_home/mcp_registry.json）。
// self-hosted 源额外读 registry_custom_url。切换只影响「列表从哪拉」，
// install 端点仍按 server 反域名 id 落到本机 MCP 配置，与源无关。

/// 源的机器 id（前端下拉的 value）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogSource {
    /// 官方 registry（默认）。境外线路，有代理时数据最全。
    Official,
    /// 内置精选目录（M1 的打包 JSON 当"伪源"，零网络依赖）。
    BuiltinOffline,
    /// 用户自建子注册中心（官方 ghcr 镜像，API 与官方同形）。
    SelfHosted,
}

impl CatalogSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::BuiltinOffline => "builtin-offline",
            Self::SelfHosted => "self-hosted",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "official" => Some(Self::Official),
            "builtin-offline" => Some(Self::BuiltinOffline),
            "self-hosted" => Some(Self::SelfHosted),
            _ => None,
        }
    }
}

/// 源持久化（hermix_home/mcp_registry.json）。文件缺失/损坏 = 默认 official。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryPrefs {
    /// 源 id（CatalogSource::as_str）。
    #[serde(default)]
    source: Option<String>,
    /// self-hosted 源的 base URL。
    #[serde(default)]
    registry_custom_url: Option<String>,
}

fn registry_prefs_path() -> PathBuf {
    hermix_config::paths::hermix_home().join("mcp_registry.json")
}

fn read_prefs() -> RegistryPrefs {
    std::fs::read_to_string(registry_prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_prefs(prefs: &RegistryPrefs) {
    let path = registry_prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(path, json);
    }
}

/// 解析出当前生效的 registry base URL（不含 path）。
/// env 优先；self-hosted 无自定义 URL 时回退官方并如实标注。
fn resolve_registry_base() -> (String, CatalogSource) {
    if let Some(url) = hermix_config::paths::env_compat("HERMIX_MCP_REGISTRY_URL", "HERMES_MCP_REGISTRY_URL") {
        if !url.trim().is_empty() {
            return (url.trim().trim_end_matches('/').to_string(), CatalogSource::SelfHosted);
        }
    }
    let prefs = read_prefs();
    let src = prefs
        .source
        .as_deref()
        .and_then(CatalogSource::parse)
        .unwrap_or(CatalogSource::Official);
    match src {
        CatalogSource::SelfHosted => {
            let url = prefs
                .registry_custom_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_end_matches('/').to_string())
                .unwrap_or_else(|| official_base().to_string());
            (url, CatalogSource::SelfHosted)
        }
        // builtin-offline 不走网络（get_catalog 提前返回），base 仅占位。
        _ => (official_base().to_string(), src),
    }
}

fn official_base() -> &'static str {
    "https://registry.modelcontextprotocol.io"
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

/// 单页条数。官方 registry 校验 `limit <= 100`，再大一律 422 ——
/// 此前写死 limit=200，目录因此永远拉不到（2026-10-04 修）。
const PAGE_LIMIT: u32 = 100;
/// 一次拉取最多翻几页（游标分页）。registry 按 id 字母序返回，这里只取
/// 字母表头部的一段；精选高频条目由内置目录合并补齐（见 get_catalog）。
const CATALOG_PAGES: usize = 5;

async fn fetch_remote(base: &str, max_pages: usize) -> Result<Vec<CatalogItem>, String> {
    // 官方与自建子注册中心同 API 形状（官方文档明确子注册中心复用同一路径）。
    // `version=latest` 让服务端每个 id 只回最新版，否则同一 server 的多个
    // 版本占掉近半数名额（实测 100 条只剩 54 个唯一 id）。
    let client = http_client();
    let mut cursor: Option<String> = None;
    let mut envs: Vec<RawEnvelope> = Vec::new();
    for _ in 0..max_pages {
        let mut url = format!("{base}/v0.1/servers?limit={PAGE_LIMIT}&version=latest");
        if let Some(c) = &cursor {
            // cursor 形如 `name:version`（字母数字 + : /），query 里无需转义。
            url.push_str("&cursor=");
            url.push_str(c);
        }
        let text = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("请求 registry 失败：{e}"))?
            .error_for_status()
            .map_err(|e| format!("registry 返回错误：{e}"))?
            .text()
            .await
            .map_err(|e| format!("读取 registry 响应失败：{e}"))?;
        let page: RawList =
            serde_json::from_str(&text).map_err(|e| format!("解析 registry 响应失败：{e}"))?;
        let got = page.servers.len();
        envs.extend(page.servers);
        cursor = page.metadata.next_cursor.filter(|c| !c.is_empty() && got > 0);
        if cursor.is_none() {
            break;
        }
    }

    // 自建镜像可能不支持 version=latest，同一 id 仍可能有多个版本：
    // 优先 isLatest，其次后出现者。
    let mut picked: Vec<(bool, CatalogItem)> = Vec::new();
    for env in envs {
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
    #[serde(default)]
    metadata: RawPageMeta,
}

#[derive(Deserialize, Default)]
struct RawPageMeta {
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

// ── 内置精选目录（批次 M1）────────────────────────────────
//
// 编译期打包 catalog_fallback.json（30+ 条人工核验的常用 server，形状 =
// CatalogItem），承担两个角色：
// 1. 官方源联网成功时垫底合并（registry 按 id 字母序返回，只拉前几页
//    永远看不到 filesystem / git 这类高频条目）；
// 2. 兜底：新鲜缓存 → 网络拉取失败 → **内置目录**，目录页任何情况下
//    都有内容可看——502 分支删除。
// 数据源说明与条目准入标准见该 JSON 的 comment 字段。
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

    // 批次 M2：源由 env > 设置项决定。builtin-offline 源不走网络——
    // 直接返回内置目录（这正是它存在的意义：确定性数据源）。
    let (base, source) = resolve_registry_base();
    if source == CatalogSource::BuiltinOffline {
        let items = builtin_items();
        let n = items.len();
        return Json(CatalogResponse {
            items: items.into_iter().map(to_entry).collect(),
            fetched_at: now_secs(),
            from_cache: true,
            stale_reason: Some(format!(
                "数据源：内置精选目录（{n} 条，离线固定内容，不随后续社区更新）"
            )),
        })
        .into_response();
    }

    match fetch_remote(&base, CATALOG_PAGES).await {
        Ok(fetched) => {
            let fetched_at = now_secs();
            // 官方源：精选目录垫底 + 在线去重合并（角色见 FALLBACK_JSON
            // 注释）。精选条目 id 命中时不被在线版覆盖——它们的安装命令
            // 是人工核验过的固定版本。自建源尊重镜像内容，不合并。
            let mut merged: Vec<CatalogItem> = if source == CatalogSource::Official {
                builtin_items()
            } else {
                Vec::new()
            };
            let curated: std::collections::HashSet<String> =
                merged.iter().map(|i| i.id.clone()).collect();
            merged.extend(fetched.into_iter().filter(|i| !curated.contains(&i.id)));
            write_cache(&state, &CatalogCache { fetched_at, items: merged.clone() });
            Json(CatalogResponse {
                items: merged.into_iter().map(to_entry).collect(),
                fetched_at,
                from_cache: false,
                stale_reason: None,
            })
            .into_response()
        }
        Err(e) => {
            // 第三层兜底（批次 M1）：拉不到网络 → 上一次缓存 → **内置精选
            // 目录**。502 分支删除：目录是体验增强，任何情况下都该有内容
            // 可看。来源对前端如实标注（from_cache + stale_reason）。
            if let Some(c) = cache {
                return Json(CatalogResponse {
                    items: c.items.into_iter().map(to_entry).collect(),
                    fetched_at: c.fetched_at,
                    from_cache: true,
                    stale_reason: Some(format!("联网拉取失败，正在显示上一次缓存：{e}")),
                })
                .into_response();
            }
            let items = builtin_items();
            let n = items.len();
            Json(CatalogResponse {
                items: items.into_iter().map(to_entry).collect(),
                fetched_at: now_secs(),
                from_cache: true,
                stale_reason: Some(format!(
                    "联网不可达（{e}），正在显示内置精选目录（{n} 条，不含最新社区条目）"
                )),
            })
            .into_response()
        }
    }
}

// ── 源设置与探测端点（批次 M2）────────────────────────────

/// 前端下拉的数据：全部内置源 + 当前生效项。
#[derive(Serialize)]
struct SourceInfo {
    id: &'static str,
    name: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct SourcesResponse {
    current: String,
    /// env 覆盖生效中（UI 显示"被环境变量锁定"提示，禁止切换）。
    env_override: bool,
    custom_url: Option<String>,
    sources: Vec<SourceInfo>,
}

/// `GET /api/mcp/catalog/sources` — 源清单 + 当前选择。
pub async fn get_sources(State(_state): State<HttpServerState>) -> Response {
    let prefs = read_prefs();
    let env_override = hermix_config::paths::env_compat(
        "HERMIX_MCP_REGISTRY_URL",
        "HERMES_MCP_REGISTRY_URL",
    )
    .is_some_and(|v| !v.trim().is_empty());
    Json(SourcesResponse {
        current: resolve_registry_base().1.as_str().to_string(),
        env_override,
        custom_url: prefs.registry_custom_url,
        sources: vec![
            SourceInfo {
                id: "official",
                name: "官方 Registry",
                description: "MCP 官方社区目录（境外线路，有代理时数据最全）",
            },
            SourceInfo {
                id: "builtin-offline",
                name: "内置精选目录",
                description: "离线固定内容，零网络依赖，断网开箱即用",
            },
            SourceInfo {
                id: "self-hosted",
                name: "自建子注册中心",
                description: "官方 registry 镜像（Docker 一条命令自建），填自定义 URL",
            },
        ],
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct SetSourceRequest {
    /// 源 id（sources 里列出的之一）。
    source: String,
    /// self-hosted 源必填的自定义 URL。
    #[serde(default)]
    custom_url: Option<String>,
}

/// `PUT /api/mcp/catalog/sources` — 切换数据源。
pub async fn set_source(
    State(_state): State<HttpServerState>,
    Json(body): Json<SetSourceRequest>,
) -> Response {
    if env_override_active() {
        return (
            StatusCode::CONFLICT,
            "HERMIX_MCP_REGISTRY_URL 环境变量已设置，配置文件的源选择被覆盖。请清除该环境变量后再切换。"
                .to_string(),
        )
            .into_response();
    }
    let Some(src) = CatalogSource::parse(&body.source) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("未知数据源：{}", body.source),
        )
            .into_response();
    };
    if src == CatalogSource::SelfHosted
        && body
            .custom_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        && read_prefs()
            .registry_custom_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            "self-hosted 源需要提供 custom_url（官方 registry 镜像地址）".to_string(),
        )
            .into_response();
    }
    let mut prefs = read_prefs();
    prefs.source = Some(src.as_str().to_string());
    if body.custom_url.is_some() {
        prefs.registry_custom_url = body.custom_url;
    }
    write_prefs(&prefs);
    Json(serde_json::json!({ "ok": true, "source": src.as_str() })).into_response()
}

fn env_override_active() -> bool {
    hermix_config::paths::env_compat("HERMIX_MCP_REGISTRY_URL", "HERMES_MCP_REGISTRY_URL")
        .is_some_and(|v| !v.trim().is_empty())
}

#[derive(Deserialize)]
pub struct ProbeQuery {
    /// 不传 = 探测当前生效源。
    #[serde(default)]
    source: Option<String>,
    /// self-hosted 试探测时可以带临时 URL（未保存先测试）。
    #[serde(default)]
    url: Option<String>,
}

#[derive(Serialize)]
struct ProbeResponse {
    ok: bool,
    source: String,
    url: String,
    /// 端到端毫秒；失败时为 0。
    latency_ms: u64,
    /// 形状校验：响应是否是可解析的 registry servers 列表。
    shape_ok: bool,
    error: Option<String>,
}

/// `GET /api/mcp/catalog/probe` — 源连通性测试（「测试连接」按钮）。
pub async fn probe(Query(q): Query<ProbeQuery>) -> Response {
    let src = q
        .source
        .as_deref()
        .and_then(CatalogSource::parse)
        .or_else(|| Some(resolve_registry_base().1))
        .unwrap_or(CatalogSource::Official);
    // URL 解析：self-hosted 优先用请求里带的临时 URL（保存前试测），
    // 其余源用各自固定端点。
    let url = match (src, q.url.as_deref().map(str::trim).filter(|s| !s.is_empty())) {
        (CatalogSource::SelfHosted, Some(u)) => u.trim_end_matches('/').to_string(),
        (CatalogSource::SelfHosted, None) => resolve_registry_base().0,
        (CatalogSource::BuiltinOffline, _) => {
            // 离线源没有网络语义，探测 = 内置目录非空。
            let n = builtin_items().len();
            return Json(ProbeResponse {
                ok: n > 0,
                source: src.as_str().to_string(),
                url: "builtin://catalog_fallback.json".to_string(),
                latency_ms: 0,
                shape_ok: n > 0,
                error: if n > 0 { None } else { Some("内置目录为空（发布产物异常）".into()) },
            })
            .into_response();
        }
        _ => official_base().to_string(),
    };
    let started = std::time::Instant::now();
    // 探测只拉一页：连通性测试不必付翻页的时间。
    let result = fetch_remote(&url, 1).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(items) => Json(ProbeResponse {
            ok: true,
            source: src.as_str().to_string(),
            url,
            latency_ms,
            shape_ok: !items.is_empty(),
            error: None,
        })
        .into_response(),
        Err(e) => Json(ProbeResponse {
            ok: false,
            source: src.as_str().to_string(),
            url,
            latency_ms,
            shape_ok: false,
            error: Some(e),
        })
        .into_response(),
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

    #[test]
    fn builtin_fallback_parses_nonempty() {
        // 条目 kind 漂移会让整表反序列化失败（曾发生：8 条 kind:"pypi"，
        // 枚举只有 remote/npm/manual）→ 断网兜底目录变 0 条。
        assert!(builtin_items().len() >= 30, "内置精选目录解析失败或为空");
    }
}
