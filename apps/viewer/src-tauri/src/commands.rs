//! Hermes Viewer 命令（command 必须放独立模块：lib.rs 根定义会与
//! generate_handler 宏冲突 E0255 —— 同 installer 教训）。
//!
//! 一期渲染边界（决策笔记 2026-10-09-standalone-file-viewer）：
//! md/text/code 直接渲染、图片 base64、PDF 交 WebView2 内建查看器；
//! office/unknown 走「用系统程序打开」兜底（预览级定位）。

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;

/// 单实例互斥体名 + 第二实例投递路径的 loopback 端口（与 BBS 3901 错开）。
pub const SINGLETON_MUTEX: &str = "HermixViewerSingleton";
pub const SINGLETON_PORT: u16 = 3907;

/// 单文件读取上限：viewer 是「看」不是「装」。超过提示用系统程序打开。
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Default)]
pub struct ViewerState {
    /// 启动参数带入、单实例投递进来的待开文件（前端 ready 后消费）。
    pub pending: Mutex<Vec<PathBuf>>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileContent {
    /// Markdown / 纯文本 / 代码：前端按 kind 选渲染器。
    Text {
        name: String,
        /// md | text | code
        variant: String,
        content: String,
    },
    /// 图片（png/jpg/gif/webp/svg/bmp）：data URL 前端直渲染。
    Image {
        name: String,
        mime: String,
        data_base64: String,
    },
    /// PDF：base64 交 WebView2 内建查看器（<embed>）。
    Pdf {
        name: String,
        data_base64: String,
    },
    /// 一期不渲染的类型：给出系统程序打开的兜底。
    Unsupported {
        name: String,
        extension: String,
        reason: String,
    },
}

fn ext_of(p: &str) -> String {
    PathBuf::from(p)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn classify(ext: &str) -> &'static str {
    match ext {
        "md" | "markdown" => "md",
        "txt" | "log" | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "csv" => "text",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "h" | "hpp" | "cs" | "java"
        | "go" | "sh" | "bat" | "ps1" | "sql" | "css" | "html" | "vue" | "rb" | "php" => "code",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => "image",
        "pdf" => "pdf",
        _ => "unsupported",
    }
}

/// 零依赖 base64（标准字母表 + padding）：避免为此引入 base64 crate
/// （六步阶梯）。fmt::Write 才能写 String（io::Write 不行）。
fn base64_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let _ = write!(out, "{}", TBL[(n >> 18) as usize & 63] as char);
        let _ = write!(out, "{}", TBL[(n >> 12) as usize & 63] as char);
        let _ = if chunk.len() > 1 {
            write!(out, "{}", TBL[(n >> 6) as usize & 63] as char)
        } else {
            write!(out, "=")
        };
        let _ = if chunk.len() > 2 {
            write!(out, "{}", TBL[n as usize & 63] as char)
        } else {
            write!(out, "=")
        };
    }
    out
}

/// 读文件并按扩展名分发。路径是绝对路径（资源管理器/主程序/命令行传入）。
#[tauri::command]
pub fn viewer_read_file(path: String) -> Result<FileContent, String> {
    let p = PathBuf::from(&path);
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法读取文件信息: {e}"))?;
    if !meta.is_file() {
        return Err(format!("不是文件: {path}"));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(format!(
            "文件超过 10MB（{} MB），请在系统程序中打开",
            meta.len() / 1024 / 1024
        ));
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let ext = ext_of(&path);
    let bytes = std::fs::read(&p).map_err(|e| format!("读取失败: {e}"))?;
    match classify(&ext) {
        "image" => {
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "svg" => "image/svg+xml",
                "bmp" => "image/bmp",
                "ico" => "image/x-icon",
                _ => "application/octet-stream",
            };
            Ok(FileContent::Image {
                name,
                mime: mime.into(),
                data_base64: base64_encode(&bytes),
            })
        }
        "pdf" => Ok(FileContent::Pdf {
            name,
            data_base64: base64_encode(&bytes),
        }),
        "md" | "text" | "code" => {
            let content = String::from_utf8_lossy(&bytes).to_string();
            Ok(FileContent::Text {
                name,
                variant: match classify(&ext) {
                    "md" => "md",
                    "text" => "text",
                    _ => "code",
                }
                .into(),
                content,
            })
        }
        _ => Ok(FileContent::Unsupported {
            name,
            extension: ext,
            reason: "一期预览不支持该类型".into(),
        }),
    }
}

/// 用系统默认程序打开（Unsupported 兜底 / 用户点「用系统程序打开」）。
/// explorer.exe <file> 走默认关联，零额外 feature。
#[tauri::command]
pub fn viewer_open_with_system(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 前端就绪后取走启动参数/单实例投递的待开文件。
#[tauri::command]
pub fn viewer_take_pending(state: tauri::State<ViewerState>) -> Vec<String> {
    let mut pending = state.pending.lock().unwrap();
    pending.drain(..).map(|p| p.to_string_lossy().to_string()).collect()
}

/// 拖入/选择的新路径登记（前端从 drop 事件拿路径后再经这里统一读，
/// 保持读入口单一——全部走 viewer_read_file）。
#[tauri::command]
pub fn viewer_exists(path: String) -> bool {
    std::fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// AI 右栏 — 直连本地 server 的 /v1/ REST（非流式，一期预览级）。
// viewer 是独立进程，不能用主程序前端的 stdio 桥（那根管道归主 app）；
// 本地 HTTP 面无鉴权且仅 127.0.0.1 可达（server 默认 bind 127.0.0.1:3000）。
// ---------------------------------------------------------------------------

/// API 基址：默认内置端口，env `HERMIX_VIEWER_API_BASE` 可覆写（与
/// server 的 bind env 解耦——这里要的是完整 base，不是端口）。
fn api_base() -> String {
    std::env::var("HERMIX_VIEWER_API_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:3000".into())
}

/// 非流式问一轮：建会话（标题带文件名）→ send_message → 返回回复全文。
/// 文件上下文拼进 prompt（服务器零改动；selection 为空只带路径）。
#[tauri::command]
pub async fn ai_send(
    text: String,
    file_path: Option<String>,
    selection: Option<String>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("HTTP client 构建失败: {e}"))?;
    let base = api_base().trim_end_matches('/').to_string();

    let title = file_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|n| format!("查看器: {}", n.to_string_lossy()))
        .unwrap_or_else(|| "查看器对话".into());
    let created: serde_json::Value = client
        .post(format!("{base}/v1/sessions"))
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await
        .map_err(|e| format!("创建会话失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("解析会话响应失败: {e}"))?;
    let sid = created
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("会话响应缺 id: {created}"))?
        .to_string();

    let mut prompt = String::new();
    if let Some(p) = &file_path {
        prompt.push_str(&format!("[正在查看的文件] {p}\n\n"));
    }
    if let Some(sel) = &selection {
        let sel = sel.trim();
        if !sel.is_empty() {
            prompt.push_str("[选中的片段]\n");
            // 选区可能是大段文本：截断保护，避免一次拖爆请求体。
            prompt.push_str(&sel.chars().take(4000).collect::<String>());
            prompt.push_str("\n\n");
        }
    }
    prompt.push_str(&text);

    let resp = client
        .post(format!("{base}/v1/sessions/{sid}/messages"))
        .json(&serde_json::json!({ "text": prompt }))
        .send()
        .await
        .map_err(|e| format!("发送失败: {e}"))?;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if status != 200 {
        return Err(format!(
            "HTTP {status}: {}",
            body.get("error").and_then(|v| v.as_str()).unwrap_or("(无详情)")
        ));
    }
    Ok(body)
}
