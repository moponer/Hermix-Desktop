//! 文档 → Markdown：调用随桌面端一起分发的 `markitdown` sidecar。
//!
//! ## 为什么需要它
//!
//! 查看器原本**没有文本抽取能力**（`readMarkdown` 退化成"读文本"），于是：
//!   - PDF 的「文本」视图点了直接报错（二进制文件读不出文字）；
//!   - epub / msg 这类格式完全打不开；
//!   - agent 要读 Office 文档只能拿原始字节 → 上下文直接炸。
//!
//! ## 为什么几乎零成本
//!
//! markitdown 就在**同一个 `binaries/` 目录**里 —— 桌面端的 tauri.conf 把
//! `hermix` / `markitdown` / `hermix-viewer` / `hermix-filesearch` 四个 sidecar
//! 装在一起，查看器是其中之一。装版后两者是兄弟文件，**不需要任何打包改动**。
//!
//! ## 支持哪些格式
//!
//! 按 sidecar 自己 `--list-plugins` 的输出写死（不是"希望它支持"）：
//! docx / pptx / xlsx / xls / pdf / html / epub / msg / csv / tsv / json /
//! xml / zip / 图片 / 音视频 ……
//!
//! 桌面端那份白名单里含 rtf / odt / doc / ppt，但 sidecar 并不支持它们 ——
//! 写进去只会让用户等一个必然失败的报错。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

const MARKITDOWN_BINARY_NAME: &str = "markitdown-x86_64-pc-windows-msvc.exe";

/// 前端一次渲染得动的 Markdown 上限（与桌面端一致）。
const MAX_CHARS: u32 = 200_000;

/// sidecar 实际支持的扩展名（`markitdown --list-plugins` 的输出）。
const SUPPORTED: &[&str] = &[
    "docx", "xlsx", "xlsm", "xls", "pptx", "pdf", "html", "htm", "xhtml", "epub", "msg",
    "csv", "tsv", "json", "xml", "xsd", "xsl", "zip", "md", "markdown", "txt", "text", "log",
    "ini", "cfg", "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "mp3", "wav", "flac",
    "ogg", "m4a", "aac", "wma",
];

/// 找 markitdown 可执行文件。
///
/// 解析顺序（与桌面端同一套规则，只是多一条"兄弟目录"）：
///   1. 环境变量 `MARKITDOWN_BIN`
///   2. **当前 exe 旁边的 `binaries/`** —— 装版时查看器与 markitdown 同目录
///   3. 开发态：仓库里桌面端的 `binaries/`
fn resolve_markitdown_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MARKITDOWN_BIN") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(MARKITDOWN_BINARY_NAME);
            if sibling.is_file() {
                return Some(sibling);
            }
            // 有些布局把 sidecar 放在上一层的 binaries/。
            if let Some(root) = dir.parent() {
                let nested = root.join("binaries").join(MARKITDOWN_BINARY_NAME);
                if nested.is_file() {
                    return Some(nested);
                }
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../desktop-tauri/src-tauri/binaries")
        .join(MARKITDOWN_BINARY_NAME);
    if dev.is_file() {
        return Some(dev);
    }
    None
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownResult {
    pub markdown: String,
    pub file_name: String,
    /// `true` 表示输出被 sidecar 截断，前端应提示"仅显示前 N 字符"。
    pub truncated: bool,
}

/// 文档 → Markdown（PDF / Office / epub / msg 的文本视图都走这条）。
#[tauri::command]
pub async fn document_to_markdown(file_path: String) -> Result<MarkdownResult, String> {
    let raw = PathBuf::from(&file_path);
    if raw.as_os_str().is_empty() {
        return Err("路径为空".into());
    }
    let canonical = raw
        .canonicalize()
        .map_err(|e| format!("无法定位文件：{e}"))?;
    if !canonical.is_file() {
        return Err("不是文件".into());
    }
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !SUPPORTED.contains(&ext.as_str()) {
        // 旧版二进制 Office（.doc/.ppt 及 WPS 系）没有任何开源解析器
        // （markitdown 只认 OOXML）。报错要给出路：系统程序能开、另存为
        // 新格式后可预览——而不是一句「暂不支持」把人晾在原地。
        const LEGACY_BINARY: &[&str] = &[
            "doc", "ppt", "dot", "pot", "wps", "et", "dps", "rtf",
        ];
        if LEGACY_BINARY.contains(&ext.as_str()) {
            // 老版 .doc（Word 97-2003）走本侧的 OLE 尽力抽取（cfb 解析
            // WordDocument 流，见 legacy_doc.rs）；其余遗留二进制仍给出路。
            if matches!(ext.as_str(), "doc" | "dot") {
                match crate::legacy_doc::extract_doc_text(&canonical) {
                    Ok(text) => {
                        return Ok(MarkdownResult {
                            markdown: text,
                            file_name: canonical
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("document.doc")
                                .to_string(),
                            truncated: false,
                        });
                    }
                    Err(e) => {
                        return Err(format!(
                            "旧版 .doc 预览失败：{e}。可点「用系统程序打开」\
                             （Word / WPS 均可），另存为 .docx 后即可完整预览。"
                        ));
                    }
                }
            }
            return Err(format!(
                "旧版二进制格式 .{ext} 没有浏览器内的解析器。点下方「用系统程序打开」\
                 （Word / WPS 均可）；在 Office 里另存为 .docx / .pptx 后就能直接预览与编辑。"
            ));
        }
        return Err(format!("暂不支持 .{ext} 的文本抽取"));
    }

    let bin = resolve_markitdown_path().ok_or_else(|| {
        "找不到 markitdown（它随桌面端一起分发；开发态请先构建桌面端的 binaries/）".to_string()
    })?;
    let file_name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // 进程调用要离开主线程：Tauri 的同步命令跑在主线程上，sidecar 转换一
    // 个几百页的 PDF 要几秒，卡在主线程上整个窗口就没响应了。
    let out = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = Command::new(&bin);
        cmd.arg(&canonical);
        cmd.arg("--max-chars").arg(MAX_CHARS.to_string());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：防黑窗闪烁。
            cmd.creation_flags(0x08000000);
        }
        cmd.output()
    })
    .await
    .map_err(|e| format!("markitdown 任务失败：{e}"))?
    .map_err(|e| format!("无法运行 markitdown：{e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("markitdown 失败：{}", stderr.trim()));
    }
    let markdown = String::from_utf8_lossy(&out.stdout).to_string();
    let truncated = markdown.chars().count() >= MAX_CHARS as usize;
    Ok(MarkdownResult {
        markdown,
        file_name,
        truncated,
    })
}

/// 供测试与诊断用：这个扩展名能不能做文本抽取。
pub fn supports_text_extraction(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单必须与 sidecar 的真实能力一致。
    ///
    /// 桌面端那份白名单里写着 doc / rtf / odt / odp，但 sidecar 的
    /// `--list-plugins` 里根本没有它们 —— 写进去只是让用户等一个必然失败的
    /// 报错。这条测试锁住"白名单 = sidecar 能力"这个事实。
    #[test]
    fn whitelist_matches_sidecar_capability() {
        assert!(supports_text_extraction(Path::new("a.pdf")));
        assert!(supports_text_extraction(Path::new("a.DOCX")));
        assert!(supports_text_extraction(Path::new("a.epub")));
        assert!(supports_text_extraction(Path::new("a.msg")));
        assert!(!supports_text_extraction(Path::new("a.doc")));
        assert!(!supports_text_extraction(Path::new("a.rtf")));
        assert!(!supports_text_extraction(Path::new("a.odt")));
        assert!(!supports_text_extraction(Path::new("a.ods")));
        assert!(!supports_text_extraction(Path::new("a.ppt")));
    }
}
