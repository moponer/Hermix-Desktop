//! 二进制产物写盘（Word / Excel 编辑器的落盘通道）。
//!
//! 与 `pdf_ops::write_text` 同一套守卫：产物路径过 `safe_output_path`——
//! 与源文件相同（大小写不敏感、`..`、**硬链接**）一律拒绝。**原文件永不
//! 覆盖**：产物默认落成 `<原名>.edited.<扩展名>`。
//!
//! 载荷走 base64：Tauri v2 的命令参数默认 JSON 序列化，裸字节数组会展开
//! 成几百万个 JSON 数字（10MB 的 xlsx ≈ 40MB JSON）。base64 是本仓库传
//! 二进制的既有约定（`viewer_read_file` 的 `data_base64` 也走它）。

use std::path::PathBuf;

use crate::commands::base64_decode;
use crate::paths::safe_output_path;
use crate::pdf_ops::{save_bytes, SaveResult};

/// 写二进制产物。`src_path` 是源文件（守卫的比对基准），`out_path` 为空
/// 时落默认名（`<原名>.edited.<扩展名>`）。
#[tauri::command(async)]
pub fn write_bytes(
    src_path: String,
    out_path: Option<String>,
    data_base64: String,
) -> Result<SaveResult, String> {
    let bytes = base64_decode(&data_base64).ok_or("产物数据不是合法的 base64")?;
    let src = PathBuf::from(&src_path);
    let out = safe_output_path(&src, out_path.as_deref(), None)?;
    save_bytes(&out, &bytes, None)
}
