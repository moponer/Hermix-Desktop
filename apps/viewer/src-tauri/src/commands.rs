//! Hermes Viewer 命令（command 必须放独立模块：lib.rs 根定义会与
//! generate_handler 宏冲突 E0255 —— 同 installer 教训）。
//!
//! 边界（决策笔记 2026-10-09-standalone-file-viewer）：
//!   md/text/code 直接渲染；图片 base64；PDF 走 pdf.js；
//!   xlsx/docx/pptx/csv 走前端引擎，bytes 透传；
//!   html 走前端沙箱 iframe（srcDoc + 桥脚本转系统浏览器）；
//!   老二进制 office 走「用系统程序打开」兜底。
//!
//! 文件读取上限（按类型给）：10MB 文本/图片/PDF/html，50MB xlsx/xls/docx/csv，
//! 100MB pptx（演示文稿常有大量图片）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// 单实例互斥体名 + 第二实例投递路径的 loopback 端口（与 BBS 3901 错开）。
pub const SINGLETON_MUTEX: &str = "HermixViewerSingleton";
pub const SINGLETON_PORT: u16 = 3907;

/// 文件读取上限：按类型给——10MB 给文本/图片/PDF/html（看一眼够用），
/// 50MB 给 xlsx/xls/docx/csv，100MB 给 pptx。超过一律建议走系统程序。
const TEXT_BYTE_LIMIT: u64 = 10 * 1024 * 1024;
const HTML_BYTE_LIMIT: u64 = 5 * 1024 * 1024;
const XLSX_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const DOCX_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const XLS_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const CSV_BYTE_LIMIT: u64 = 50 * 1024 * 1024;
const PPTX_BYTE_LIMIT: u64 = 100 * 1024 * 1024;

#[derive(Default)]
pub struct ViewerState {
    pub pending: Mutex<Vec<PathBuf>>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// RFC 3339 字符串（如 "2024-03-15T08:01:43Z"），前端直接用。
    pub mtime: Option<String>,
    pub extension: String,
    pub hidden: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub path: String,
    pub exists: bool,
    pub is_dir: bool,
    pub is_file: bool,
    pub size: u64,
    pub mtime: Option<String>,
    pub hidden: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriveRoot {
    pub letter: String,
    pub path: String,
    pub label: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileContent {
    /// Markdown / 纯文本 / 代码：前端按 variant 选渲染器。
    Text {
        name: String,
        /// md | text | code
        variant: String,
        content: String,
    },
    /// 图片（png/jpg/gif/webp/svg/bmp/ico）：data URL 前端直渲染。
    Image {
        name: String,
        mime: String,
        data_base64: String,
    },
    /// PDF：base64 交给前端 pdf.js（旧的 `<embed>` 弃用，WebView2 行为不一致）。
    Pdf { name: String, data_base64: String },
    /// 任意二进制（xlsx/xlsm/xls/docx/pptx/html/csv/tsv）：bytes 透传给前端引擎。
    /// 单一载荷种类，前端按 `extension` 分派。
    Bytes {
        name: String,
        extension: String,
        data_base64: String,
    },
    /// 二期不渲染的类型：走「用系统程序打开」兜底。
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
        "md" | "markdown" | "txt" | "log" | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "csv" | "tsv" => "text",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "h" | "hpp" | "cs" | "java"
        | "go" | "sh" | "bash" | "bat" | "ps1" | "sql" | "css" | "scss" | "less" | "vue" | "rb"
        | "php" | "lua" | "swift" | "kt" | "dart" | "scala" => "code",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif" => "image",
        "pdf" => "pdf",
        // 字节类：xlsx/xlsm/xls 由前端 ExcelJS/SheetJS 解析，
        // docx/docm/dotx/dotm 由 docx-preview 解析，pptx/pptm/ppsx/ppsm/potx/potm 由 pptx-browser 解析，
        // html/htm 走前端沙箱 iframe。
        "xlsx" | "xlsm" | "xls" | "docx" | "docm" | "dotx" | "dotm" | "pptx" | "pptm" | "ppsx"
        | "ppsm" | "potx" | "potm" | "html" | "htm" => "bytes",
        // 老二进制 office 仍走系统程序
        "doc" | "ppt" | "wps" | "et" | "dps" | "rtf" | "odt" | "ods" | "odp" => "legacy_office",
        _ => "unsupported",
    }
}

fn size_limit_for_ext(ext: &str) -> u64 {
    match ext {
        "html" | "htm" => HTML_BYTE_LIMIT,
        "xlsx" | "xlsm" => XLSX_BYTE_LIMIT,
        "xls" => XLS_BYTE_LIMIT,
        "docx" | "docm" | "dotx" | "dotm" => DOCX_BYTE_LIMIT,
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => PPTX_BYTE_LIMIT,
        "csv" | "tsv" => CSV_BYTE_LIMIT,
        _ => TEXT_BYTE_LIMIT,
    }
}

/// 零依赖 base64：避免引入 base64 crate（六步阶梯）。
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

/// RFC 3339 (秒精度)。前端用 `new Date(s)` 直接解析。
fn mtime_to_rfc3339(t: std::time::SystemTime) -> Option<String> {
    use std::time::UNIX_EPOCH;
    let dur = t.duration_since(UNIX_EPOCH).ok()?;
    let secs = dur.as_secs() as i64;
    chrono_like(secs)
}

/// 没有 chrono 依赖下的极简 RFC 3339：把 unix 秒换成本地年/月/日/时/分/秒。
/// 时区偏移用本地时间表达（前端用 new Date 解析时不需要时区，行为一致）。
fn chrono_like(secs: i64) -> Option<String> {
    if secs < 0 {
        return None;
    }
    let days = (secs / 86_400) as i64;
    let secs_in_day = secs.rem_euclid(86_400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    let (y, mo, d) = civil_from_days(days)?;
    Some(format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}",
    ))
}

/// Howard Hinnant `civil_from_days`：从 1970-01-01 起算的天数反推年月日。
/// 不引 chrono：viewer 是 sidecar，再多一个 crate 不划算。
fn civil_from_days(z: i64) -> Option<(i32, u32, u32)> {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    if y < 1 || y > 9999 {
        return None;
    }
    Some((y as i32, m, d))
}

/// Windows 隐藏属性（FILE_ATTRIBUTE_HIDDEN = 0x2）。非 Windows 平台用
/// 名字首字符 `.` 兜底（项目是 Windows-only，但留好跨平台路径便于单测）。
#[cfg(windows)]
fn is_hidden(meta: &std::fs::Metadata, name: &str) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    if meta.file_attributes() & HIDDEN != 0 {
        return true;
    }
    name.starts_with('.')
}
#[cfg(not(windows))]
fn is_hidden(_meta: &std::fs::Metadata, name: &str) -> bool {
    name.starts_with('.')
}

fn file_name_lossy(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn validate_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("路径为空".into());
    }
    if path.contains('\0') {
        return Err("路径包含非法字符".into());
    }
    Ok(PathBuf::from(path))
}

fn build_dir_entry(p: &Path) -> Option<DirEntry> {
    let meta = std::fs::metadata(p).ok()?;
    let name = file_name_lossy(p);
    if name.is_empty() {
        return None;
    }
    let is_dir = meta.is_dir();
    let size = if is_dir { 0 } else { meta.len() };
    let mtime = meta.modified().ok().and_then(mtime_to_rfc3339);
    let hidden = is_hidden(&meta, &name);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Some(DirEntry {
        name,
        path: p.to_string_lossy().to_string(),
        is_dir,
        size,
        mtime,
        extension: ext,
        hidden,
    })
}

fn sort_entries(entries: &mut [DirEntry]) {
    // 目录在前；同类内按名称排序（locale-naive：依赖 Windows 排序规则，
    // Rust std 的 `sort` 走字节序；用户在 Windows 资源管理器里看到的也
    // 是 Unicode 字典序，相差不大）。
    use std::cmp::Ordering;
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
}

// ════════════════════════════════════════════════════════════════════
//  Tauri 命令
// ════════════════════════════════════════════════════════════════════

/// 读文件并按扩展名分发。路径是绝对路径（资源管理器/主程序/命令行传入）。
#[tauri::command]
pub fn viewer_read_file(path: String) -> Result<FileContent, String> {
    let p = validate_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法读取文件信息: {e}"))?;
    if !meta.is_file() {
        return Err(format!("不是文件: {path}"));
    }
    let name = file_name_lossy(&p);
    let ext = ext_of(&path);
    let class = classify(&ext);
    let limit = size_limit_for_ext(&ext);
    if meta.len() > limit {
        return Err(format!(
            "文件超过 {}MB（{} MB），请在系统程序中打开",
            limit / 1024 / 1024,
            meta.len() / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("读取失败: {e}"))?;
    Ok(match class {
        "image" => {
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "svg" => "image/svg+xml",
                "bmp" => "image/bmp",
                "ico" => "image/x-icon",
                "avif" => "image/avif",
                _ => "application/octet-stream",
            };
            FileContent::Image {
                name,
                mime: mime.into(),
                data_base64: base64_encode(&bytes),
            }
        }
        "pdf" => FileContent::Pdf {
            name,
            data_base64: base64_encode(&bytes),
        },
        "text" | "code" => {
            let content = String::from_utf8_lossy(&bytes).to_string();
            let variant = if class == "text" { "text" } else { "code" };
            FileContent::Text {
                name,
                variant: variant.into(),
                content,
            }
        }
        "bytes" => FileContent::Bytes {
            name,
            extension: ext,
            data_base64: base64_encode(&bytes),
        },
        "legacy_office" => FileContent::Unsupported {
            name,
            extension: ext,
            reason: "老版二进制格式需要 Office，请用系统程序打开".into(),
        },
        _ => FileContent::Unsupported {
            name,
            extension: ext,
            reason: "暂不支持该类型，请用系统程序打开".into(),
        },
    })
}

/// 列目录。可选 `show_hidden`：默认 false（与 Windows 资源管理器一致）。
/// 不递归：树形 UI 由前端按需逐级请求，避免一次性把大目录全塞回来。
#[tauri::command]
pub fn viewer_list_dir(path: String, show_hidden: Option<bool>) -> Result<Vec<DirEntry>, String> {
    let p = validate_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法访问: {e}"))?;
    if !meta.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let show_hidden = show_hidden.unwrap_or(false);
    let read = std::fs::read_dir(&p).map_err(|e| format!("读取目录失败: {e}"))?;
    let mut out: Vec<DirEntry> = Vec::new();
    for entry in read.flatten() {
        if let Some(item) = build_dir_entry(&entry.path()) {
            if !show_hidden && item.hidden {
                continue;
            }
            out.push(item);
        }
    }
    sort_entries(&mut out);
    Ok(out)
}

/// 单个文件 / 目录的元信息。前端用它判断能否进入 / 能否预览。
#[tauri::command]
pub fn viewer_stat(path: String) -> Result<FileStat, String> {
    let p = validate_path(&path)?;
    match std::fs::metadata(&p) {
        Ok(meta) => {
            let name = file_name_lossy(&p);
            let mtime = meta.modified().ok().and_then(mtime_to_rfc3339);
            let hidden = is_hidden(&meta, &name);
            Ok(FileStat {
                path: p.to_string_lossy().to_string(),
                exists: true,
                is_dir: meta.is_dir(),
                is_file: meta.is_file(),
                size: meta.len(),
                mtime,
                hidden,
            })
        }
        Err(_) => Ok(FileStat {
            path: p.to_string_lossy().to_string(),
            exists: false,
            is_dir: false,
            is_file: false,
            size: 0,
            mtime: None,
            hidden: false,
        }),
    }
}

/// 父目录（用于「上一级」按钮）。已是盘根时返回 None。
#[tauri::command]
pub fn viewer_parent(path: String) -> Option<String> {
    let p = PathBuf::from(&path);
    let parent = p.parent()?.to_string_lossy().to_string();
    if parent.is_empty() {
        return None;
    }
    Some(parent)
}

/// 列出本机盘符（Windows）。Linux/macOS 留空数组。
#[tauri::command]
pub fn viewer_drives() -> Vec<DriveRoot> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
        let mask = unsafe { GetLogicalDrives() };
        (0..26)
            .filter_map(|i| {
                if mask & (1 << i) == 0 {
                    return None;
                }
                let letter = ((b'A' + i) as char).to_string();
                let path = format!("{letter}:\\");
                let label = match std::fs::metadata(&path) {
                    Ok(_) => format!("本地磁盘 ({letter}:)"),
                    Err(_) => format!("磁盘 ({letter}:)"),
                };
                Some(DriveRoot { letter, path, label })
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// 在 Windows 资源管理器里高亮选中一个文件（`explorer /select,"path"`）。
/// 没有原生 API 走这条路径——全靠 Shell 启动 explorer.exe。
#[tauri::command]
pub fn viewer_reveal_in_explorer(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        let p = validate_path(&path)?;
        if !p.exists() {
            return Err(format!("文件不存在: {path}"));
        }
        let arg = format!("/select,{}", p.display());
        std::process::Command::new("explorer.exe")
            .arg(&arg)
            .spawn()
            .map_err(|e| format!("启动资源管理器失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    Err("仅支持 Windows（项目边界）".into())
}

/// 在目录下做文件名子串搜索（深度 1，不递归到子目录——避免大目录爆栈）。
/// `max` 限制返回条数（默认 200），避免一次性传太多。
#[tauri::command]
pub fn viewer_search_dir(
    path: String,
    query: String,
    show_hidden: Option<bool>,
    max: Option<usize>,
) -> Result<Vec<DirEntry>, String> {
    let p = validate_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法访问: {e}"))?;
    if !meta.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let show_hidden = show_hidden.unwrap_or(false);
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new()); // 空查询：不返回任何结果
    }
    let max = max.unwrap_or(200);
    let read = std::fs::read_dir(&p).map_err(|e| format!("读取目录失败: {e}"))?;
    let mut out: Vec<DirEntry> = Vec::new();
    for entry in read.flatten() {
        let Some(item) = build_dir_entry(&entry.path()) else {
            continue;
        };
        if !show_hidden && item.hidden {
            continue;
        }
        if !item.name.to_lowercase().contains(&q) {
            continue;
        }
        out.push(item);
        if out.len() >= max {
            break;
        }
    }
    sort_entries(&mut out);
    Ok(out)
}

/// 用系统默认程序打开。
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

/// ai_send 的归一化返回：reply 从 server 响应中取出，sessionId 回传给
/// 前端，后续消息带上即可复用同一会话（多轮上下文由 server 侧保存）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiReply {
    pub reply: String,
    pub session_id: String,
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

/// 非流式问一轮：无 session_id 时建会话（标题带文件名），有则复用；
/// 然后往同一会话 send_message。文件上下文拼进 prompt（服务器零改动；
/// selection 为空只带路径）。
#[tauri::command]
pub async fn ai_send(
    text: String,
    file_path: Option<String>,
    selection: Option<String>,
    session_id: Option<String>,
) -> Result<AiReply, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("HTTP client 构建失败: {e}"))?;
    let base = api_base().trim_end_matches('/').to_string();

    let sid = match session_id.filter(|s| !s.trim().is_empty()) {
        Some(sid) => sid,
        None => {
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
            created
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("会话响应缺 id: {created}"))?
                .to_string()
        }
    };

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
    let reply = body
        .get("reply")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("消息响应缺 reply: {body}"))?
        .to_string();
    Ok(AiReply { reply, session_id: sid })
}
