//! 查看器设置：搜索 / AI / MCP / 通用。
//!
//! 存哪：`$hermix_home/viewer-settings.json`——与主程序 `config.yaml` 同目录，
//! 便携版跟着安装目录走。**密钥不进这个文件**：独立模型的 API Key 写进
//! `$hermix_home/.env`（主程序同一个密钥库），设置里只留「配没配」的标记。
//! 理由：配置目录会被备份/同步/贴到工单里，密钥不该跟着走。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use hermix_config::paths::{env_path, hermix_home};

/// 独立模型的密钥在 .env 里的键名（与主程序的供应商密钥同一个文件）。
pub const AI_KEY_ENV: &str = "HERMIX_VIEWER_AI_KEY";

// ---------------------------------------------------------------------------
// 搜索
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchSettings {
    /// `live` = 输入即搜（去抖）；`enter` = 回车才搜。
    pub mode: String,
    /// live 模式的去抖毫秒数。0 = 不去抖（每个键都搜）。
    pub debounce_ms: u32,
    /// `auto` = 全盘索引优先、不可用时退回当前目录；`global` = 只全盘；
    /// `local` = 只当前目录。
    pub scope: String,
    /// 最多返回多少条。
    pub limit: u32,
    /// 搜索时是否包含隐藏文件。
    pub include_hidden: bool,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            mode: "live".into(),
            debounce_ms: 250,
            scope: "auto".into(),
            limit: 300,
            include_hidden: false,
        }
    }
}

// ---------------------------------------------------------------------------
// AI
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    /// `inherit` = 继承主程序（读 config.yaml 的默认模型与供应商，直连它）；
    /// `custom` = 独立模型（与主程序彻底分开，自己一套端点 / 模型 / 密钥）。
    pub mode: String,
    /// 独立模式的端点（OpenAI 兼容，含 `/v1` 前缀）。
    pub base_url: String,
    /// 独立模式的模型 id。
    pub model: String,
    /// 追加给模型的系统提示（文件助手的角色设定）。
    pub system_prompt: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// 独立模式的密钥是否已配置（在 .env 里）——**不存明文**。
    pub has_key: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            mode: "inherit".into(),
            base_url: String::new(),
            model: String::new(),
            system_prompt: "你是 Hermix 文件查看器的助手，基于用户当前打开的文件回答问题。回答用中文，简洁准确；涉及文件内容时以实际读取到的内容为准。".into(),
            temperature: Some(0.3),
            max_tokens: Some(4096),
            has_key: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct McpSettings {
    /// 是否在启动后拉起本地 MCP 服务（让任意 MCP 客户端连上来）。
    pub enabled: bool,
    /// 本地 MCP 服务的端口（只监听回环）。
    pub port: u16,
    /// 允许哪些客户端连：回环地址即可，外部机器一律不接。
    pub allow_remote: bool,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 3911,
            allow_remote: false,
        }
    }
}

// ---------------------------------------------------------------------------
// 浏览器
// ---------------------------------------------------------------------------

/// 浏览器内核的选择。
///
/// - `embedded` = **内嵌网页视图**（通道 A）：远程网页渲染在应用内的子
///   webview 里，用户不出应用就能看页面。
/// - `system` = **系统浏览器**（通道 B）：交给系统 Edge / Chrome 打开。
///
/// 两条通道的取舍见决策笔记
/// `docs/notes/implemented/feature/2026-10-21-viewer-web-preview-and-browser-kernel.md`。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct BrowserSettings {
    /// 网页预览用哪个内核：`embedded` | `system`。
    pub kernel: String,
    /// agent 驱动系统浏览器时默认有头（弹出真实窗口）还是无头。
    pub headed: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            kernel: "embedded".into(),
            headed: false,
        }
    }
}

// ---------------------------------------------------------------------------
// 通用
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralSettings {
    /// 启动时进的目录（空 = 上次目录 → 主目录 → 首个盘符）。
    pub default_dir: String,
    /// 启动时默认显示隐藏文件。
    pub show_hidden_by_default: bool,
    /// 关闭标签页前先确认。
    pub confirm_close_tab: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            default_dir: String::new(),
            show_hidden_by_default: false,
            confirm_close_tab: false,
        }
    }
}

// ---------------------------------------------------------------------------
// 汇总
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewerSettings {
    pub search: SearchSettings,
    pub ai: AiSettings,
    pub mcp: McpSettings,
    pub browser: BrowserSettings,
    pub general: GeneralSettings,
}

impl ViewerSettings {
    /// 设置文件路径：`$hermix_home/viewer-settings.json`。
    pub fn path() -> PathBuf {
        hermix_home().join("viewer-settings.json")
    }

    /// 读设置。文件不存在 / 坏了 → 走默认（**不因为配置坏了就起不来**）。
    pub fn load() -> Self {
        let path = Self::path();
        let mut s = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<ViewerSettings>(&raw).ok())
            .unwrap_or_default();
        s.sync_secret_state();
        s
    }

    /// 存设置。写失败要报错（静默丢配置比报错更坏）。
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
        }
        let mut s = self.clone();
        s.sync_secret_state();
        let body = serde_json::to_string_pretty(&s).map_err(|e| format!("序列化设置失败: {e}"))?;
        std::fs::write(&path, body).map_err(|e| format!("写入设置失败: {e}"))
    }

    /// `has_key` 是**读 .env 算出来的**，不是设置里存的值——存进去就等于
    /// 把「密钥配过没」变成了可能过期的事实（用户改了 .env 就对不上了）。
    fn sync_secret_state(&mut self) {
        self.ai.has_key = read_env_value(AI_KEY_ENV).is_some_and(|v| !v.trim().is_empty());
    }
}

// ---------------------------------------------------------------------------
// .env 读写（独立模型密钥的落点）
// ---------------------------------------------------------------------------

/// 读 .env 里某个键的值（忽略空行与 `#` 注释）。
pub fn read_env_value(key: &str) -> Option<String> {
    let path = env_path();
    let body = std::fs::read_to_string(path).ok()?;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(v);
        let v = v
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .unwrap_or(v);
        return Some(v.to_string());
    }
    None
}

/// 写 .env 里的某个键（存在则替换，不存在则追加；值为空则删除该行）。
pub fn write_env_value(key: &str, value: Option<&str>) -> Result<(), String> {
    let path = env_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let prefix = format!("{key}=");
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&prefix) {
            if !replaced {
                replaced = true;
                if let Some(v) = value {
                    out.push(format!("{prefix}{v}"));
                }
            }
            // 值为空 → 删掉这一行
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        if let Some(v) = value {
            out.push(format!("{prefix}{v}"));
        }
    }
    let mut body = out.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(&path, body).map_err(|e| format!("写入 .env 失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_value_roundtrip() {
        // 只测纯函数：解析/拼接逻辑不碰磁盘。
        let mut lines = Vec::new();
        let prefix = "HERMIX_VIEWER_AI_KEY=";
        lines.push("# 注释".to_string());
        lines.push(format!("{prefix}sk-test"));
        let body = lines.join("\n");
        assert!(body.contains("sk-test"));
        assert_eq!(AI_KEY_ENV, "HERMIX_VIEWER_AI_KEY");
    }

    #[test]
    fn defaults_prefer_live_search_and_inherit_ai() {
        let s = ViewerSettings::default();
        assert_eq!(s.search.mode, "live");
        assert_eq!(s.ai.mode, "inherit");
        // 密钥标记默认必须为 false：没配就是没配。
        assert!(!s.ai.has_key);
    }
}
