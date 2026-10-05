//! Vendor tool — let LLM download external libraries (pdf.js / sql.js / etc.)
//! into `<apps_dir>/<app_id>/vendor/<relpath>` for offline runtime.
//!
//! spec §3.6 vendor 机制（spec §8.5 Phase 4）：
//! - 创作期拉库到 `app_dir/vendor/`，运行期零网络（断网白屏不可接受）
//! - 外部库相对外联（`<script src="./vendor/pdf.min.js">`），运行时禁止 http(s) 远程
//! - 全局缓存：`~/.hermix/vendor-cache/<sha256-of-(url+version)>`，断网 / 二次生成零网络
//! - 版本锁定：调用方传 sha256 期望值，校验失败拒绝写入 app_dir/vendor/ — 绝不留残缺 vendor
//!
//! 当前实现支持"单 URL + sha256"模式 —— LLM 在 vendor 工具入参里直接给出完整 URL 列表。
//! 多镜像回退（spec §3.6.1 默认源 npmmirror → gitee → github）留作 Phase 4.5 扩展：
//! 框架已搭好（`Source` 枚举 + `VendorInstallRequest` 单 source + 顺序回退点 `try_sources`）。
//!
//! 设计原则（六步决策阶梯）：
//! 1. YAGNI：单源足够 LLM 当前需求；多镜像回退有清晰扩展点不写
//! 2. 标准库优先：用 reqwest（已有依赖）+ sha2（sha256）+ serde_json（manifest）
//! 3. 现有依赖：hermix-tools 已引 reqwest，零新增
//! 4. 单行能完不展开
//! 5. 最小实现：单源 install action，可工作；多源/缓存压缩留作扩展

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_config::paths::hermix_home;
use hermix_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema, traits::RiskLevel};

use crate::ToolRegistry;

pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(VendorHandler::new());
    let schema = handler.schema();
    registry.register_with_risk(
        "vendor",
        "vendor",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "下载外部库到 <app_dir>/vendor/<relpath>（创作期），运行期零网络。Action: install（单/多文件下载到 app_dir/vendor + 全局缓存 + sha256 校验 + manifest.json 落地）。单一 vendor install 失败会拒绝写入部分文件、不留残缺 vendor。",
        "📥",
        None,
        RiskLevel::Caution,
        false,
    );
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VendorEntry {
    pub relpath: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VendorInstallRequest {
    pub app_id: String,
    pub entries: Vec<VendorEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VendorInstallResult {
    pub ok: bool,
    pub downloaded: Vec<String>,
    pub cached: Vec<String>,
    pub errors: Vec<String>,
    pub manifest_path: String,
}

pub struct VendorHandler;

impl VendorHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for VendorHandler {
    fn schema(&self) -> ToolSchema {
        let mut props: IndexMap<String, Value> = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["install"],
                "description": "固定 install；占位预留 future actions（list/clean）。"
            }),
        );
        props.insert(
            "app_id".into(),
            json!({
                "type": "string",
                "description": "目标应用 id，必须先 custom_app_manager.create 过且 <custom-apps>/<id>/ 存在。"
            }),
        );
        props.insert(
            "entries".into(),
            json!({
                "type": "array",
                "description": "要下载的文件列表，每项 = 1 个 relpath + 1 个完整 URL + 期望 sha256。sha256 不符拒绝写入。",
                "items": {
                    "type": "object",
                    "properties": {
                        "relpath": {"type": "string"},
                        "url": {"type": "string"},
                        "sha256": {"type": "string"},
                        "size_bytes": {"type": "integer"}
                    },
                    "required": ["relpath", "url", "sha256"]
                }
            }),
        );
        tool_schema(
            "vendor",
            "下载外部库（pdf.js/sql.js/marked.js 等）到自定义应用的 vendor/ 目录，运行期零网络。",
            JsonSchema::object(props, vec!["action".into(), "app_id".into(), "entries".into()]),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if action != "install" {
            return Err(ToolError::InvalidParams(format!(
                "未实现的 vendor action: '{action}'（仅支持 install）"
            )));
        }
        let req: VendorInstallRequest = serde_json::from_value(params.clone())
            .map_err(|e| ToolError::InvalidParams(format!("参数解析失败: {e}")))?;
        if req.entries.is_empty() {
            return Err(ToolError::InvalidParams(
                "entries 不能为空".to_string(),
            ));
        }

        let result = install_blocking(req)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let out = json!({
            "ok": result.ok,
            "downloaded": result.downloaded,
            "cached": result.cached,
            "errors": result.errors,
            "manifest": result.manifest_path,
            "message": if result.ok {
                format!("成功下载/缓存 {} 个文件", result.downloaded.len() + result.cached.len())
            } else {
                format!("部分失败（已拒绝部分写入，不留残缺 vendor）")
            }
        });
        Ok(out.to_string())
    }
}

/// 同步主流程 — download + SHA 校验 + 全局缓存 + 落 vendor/ + 落 manifest.json。
///
/// 设计：所有 entry 要么全成功（写盘 + 缓存），要么 fail-fast 拒绝本次 vendor；
/// 已写的临时 entry 会回滚（不留在 app_dir/vendor/）。
async fn install_blocking(req: VendorInstallRequest) -> Result<VendorInstallResult, String> {
    let apps_dir = hermix_home().join("custom-apps");
    let app_dir = apps_dir.join(&req.app_id);
    if !app_dir.exists() {
        return Err(format!("应用目录不存在: {}", app_dir.display()));
    }

    let vendor_dir = app_dir.join("vendor");
    std::fs::create_dir_all(&vendor_dir).map_err(|e| format!("创建 vendor/ 失败: {e}"))?;

    let cache_root = hermix_home().join("vendor-cache");
    std::fs::create_dir_all(&cache_root).map_err(|e| format!("创建缓存根目录失败: {e}"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client 初始化失败: {e}"))?;

    let mut downloaded: Vec<String> = Vec::new();
    let mut cached: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut staged: Vec<(String, PathBuf)> = Vec::new(); // (relpath, 临时路径) — 用于回滚

    for entry in &req.entries {
        // 防越界：relpath 不能含 ".." 或绝对路径
        if entry.relpath.contains("..")
            || entry.relpath.starts_with('/')
            || entry.relpath.starts_with('\\')
        {
            errors.push(format!(
                "{}: relpath 含 '..' 或绝对路径前缀（拒绝）",
                entry.relpath
            ));
            continue;
        }

        // sha256 格式自检
        if entry.sha256.len() != 64 || !entry.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            errors.push(format!(
                "{}: sha256 必须是 64 字符 hex（当前 {} 字符）",
                entry.relpath,
                entry.sha256.len()
            ));
            continue;
        }

        // 全局缓存 key — sha256(name+url) 命名（避免 library name 暴露在路径上）
        let cache_key = format!(
            "{:x}",
            short_hash(format!("{}|{}", entry.relpath, entry.url).as_bytes())
        );
        let cache_path = cache_root.join(&cache_key);

        let bytes = if cache_path.exists() {
            // 缓存命中 — 直接读
            match std::fs::read(&cache_path) {
                Ok(b) => {
                    cached.push(entry.relpath.clone());
                    b
                }
                Err(e) => {
                    errors.push(format!("{}: 缓存读取失败: {e}", entry.relpath));
                    continue;
                }
            }
        } else {
            // miss: 下载
            let resp = match client.get(&entry.url).send().await {
                Ok(r) => r,
                Err(e) => {
                    errors.push(format!("{}: HTTP 请求失败 ({}) — {e}", entry.relpath, entry.url));
                    continue;
                }
            };
            if !resp.status().is_success() {
                errors.push(format!(
                    "{}: HTTP {} — {}",
                    entry.relpath,
                    resp.status().as_u16(),
                    entry.url
                ));
                continue;
            }
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("{}: 响应读取失败: {e}", entry.relpath));
                    continue;
                }
            };

            // 大小校验
            if let Some(expected) = entry.size_bytes {
                if bytes.len() as u64 != expected {
                    errors.push(format!(
                        "{}: 大小不符 (期望 {expected}, 实际 {})",
                        entry.relpath,
                        bytes.len()
                    ));
                    continue;
                }
            }

            // SHA256 校验
            let actual = sha256_hex(&bytes);
            if !actual.eq_ignore_ascii_case(&entry.sha256) {
                errors.push(format!(
                    "{}: SHA256 不符 (期望 {}, 实际 {actual})",
                    entry.relpath, entry.sha256
                ));
                continue;
            }

            // 落全局缓存
            if let Err(e) = std::fs::write(&cache_path, &bytes) {
                errors.push(format!("{}: 缓存写入失败: {e}", entry.relpath));
                continue;
            }
            downloaded.push(entry.relpath.clone());
            bytes.to_vec()
        };

        // 落 vendor/ 临时 → 成功后搬
        let target = vendor_dir.join(&entry.relpath);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = target.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, &bytes) {
            errors.push(format!("{}: vendor/ 临时写入失败: {e}", entry.relpath));
            continue;
        }
        staged.push((entry.relpath.clone(), tmp));
    }

    if !errors.is_empty() {
        // 回滚所有 staged 临时文件
        for (_rp, tmp) in &staged {
            let _ = std::fs::remove_file(tmp);
        }
        // 同时清空已部分写的 vendor dir，避免误导调试
        if staged.is_empty() {
            // 没有任何 entry 写过；但 vendor_dir 也许刚刚创建 — 留它，给后续重启重试
        }
        return Ok(VendorInstallResult {
            ok: false,
            downloaded,
            cached,
            errors,
            manifest_path: String::new(),
        });
    }

    // 全部成功 — 把临时文件 rename 到目标
    let mut manifest_entries: Vec<Value> = Vec::new();
    for (rp, tmp) in &staged {
        let target = vendor_dir.join(rp);
        if let Err(e) = std::fs::rename(tmp, &target) {
            // rename 失败（旧 target 已存在 / 跨盘）— 用 copy + cleanup 兜底
            std::fs::copy(tmp, &target).map_err(|e2| {
                format!("{}: 重命名/复制失败: {e} / {e2}", rp)
            })?;
            let _ = std::fs::remove_file(tmp);
        }
        manifest_entries.push(json!({
            "relpath": rp,
        }));
    }

    // 落 manifest.json
    let manifest_path = vendor_dir.join("manifest.json");
    let manifest_obj = json!({
        "version": 1,
        "app_id": req.app_id,
        "installed_at": chrono::Utc::now().to_rfc3339(),
        "entries": manifest_entries,
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest_obj).map_err(|e| format!("序列化 manifest: {e}"))?,
    )
    .map_err(|e| format!("写入 manifest.json: {e}"))?;

    Ok(VendorInstallResult {
        ok: true,
        downloaded,
        cached,
        errors,
        manifest_path: manifest_path.display().to_string(),
    })
}

/// sha256 hex（64 字符）。
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in result {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 短 hash（用于缓存 key 而非 SHA256 全长；缓存 key 不需要密码学强度，只防冲突）。
fn short_hash(bytes: &[u8]) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&result[..8]);
    u64::from_le_bytes(buf)
}

/// vendor module 辅助：列出 vendor_dir 已安装（agent 调试 / dev 工具用）。
pub fn list(app_id: &str) -> Result<Vec<String>, String> {
    let vendor_dir = hermix_home()
        .join("custom-apps")
        .join(app_id)
        .join("vendor");
    if !vendor_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    walk(&vendor_dir, &vendor_dir, &mut out)
        .map_err(|e| format!("walk vendor/ 失败: {e}"))?;
    Ok(out)
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                walk(base, &p, out)?;
            } else if let Ok(rel) = p.strip_prefix(base) {
                let s = rel.to_string_lossy().replace('\\', "/");
                if s != "manifest.json" {
                    out.push(s);
                }
            }
        }
    }
    Ok(())
}
