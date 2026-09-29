use std::path::{Path, PathBuf};

const MARKITDOWN_BINARY_NAME: &str = "markitdown-x86_64-pc-windows-msvc.exe";

/// Resolve the path to the bundled `markitdown` sidecar.
///
/// 解析顺序与 `officecli` 保持一致：显式环境变量 → 开发态 manifest 目录 →
/// 装版目录下的 `binaries/`。两条依赖走同一套规则，排查时不用记两份。
pub fn resolve_markitdown_path() -> PathBuf {
    if let Ok(p) = std::env::var("MARKITDOWN_BIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(MARKITDOWN_BINARY_NAME);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    exe_dir.join("binaries").join(MARKITDOWN_BINARY_NAME)
}

pub fn ensure_markitdown() -> Result<PathBuf, String> {
    let path = resolve_markitdown_path();
    path.canonicalize().map_err(|_| {
        format!(
            "markitdown binary not found at {}. Please ensure it is bundled with the application.",
            path.display()
        )
    })
}

/// 把任意文档转成 Markdown 文本。
///
/// 二进制从扩展名 / 魔数自动判格式，不需要 `--ext`。`max_chars` 直接透传给
/// sidecar，由它自己截断 —— 前端不必先读完整文件再砍，省一次大字符串拷贝。
pub async fn convert_to_markdown(
    path: &Path,
    max_chars: Option<u32>,
) -> Result<String, String> {
    let bin = ensure_markitdown()?;
    let mut args = vec![path.to_string_lossy().to_string()];
    if let Some(n) = max_chars {
        args.push("--max-chars".to_string());
        args.push(n.to_string());
    }
    let output = tokio::process::Command::new(&bin)
        .args(&args)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW: 防黑窗闪烁
        .output()
        .await
        .map_err(|e| format!("failed to run markitdown: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("markitdown failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
