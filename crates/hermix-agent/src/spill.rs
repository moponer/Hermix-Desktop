//! 超大工具结果落盘（spill）。
//!
//! 根因：**截断是有损的**。模型拿到 `[... truncated 80000 characters ...]`
//! 之后只剩两条路 —— 重跑工具（重复付费，且第二次结果未必相同）或者瞎编。
//! 落盘把完整结果变成可按需取回的资产：上下文只留一段预览 + 一个
//! `read_file` 能打开的绝对路径，模型需要细节时自己分段去取。
//!
//! # 命名与幂等
//!
//! 用 `tool_call_id` 而不是内容哈希：同一轮里同一个调用重复落盘会覆盖自身，
//! 不产生垃圾堆积；同时文件名可反查是哪次调用产生的。
//!
//! # 失败语义
//!
//! 落盘失败**不阻断**。磁盘满、权限不足、目录被占用时退化为纯截断 ——
//! 工具结果本身已经拿到了，绝不能因为写不下就把整条结果丢掉。
//!
//! # 同步 IO
//!
//! 落盘走 `std::fs`。调用点在 agent loop 的工具写回阶段，一轮最多几次，
//! 单次耗时远小于同轮的 LLM 请求，不值得为此把预算链路整体异步化。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 超过这个字符数的工具结果改为「预览 + 路径」进上下文。
/// 约 5–8K token，够模型判断下一步，又不至于把上下文灌满。
pub const SPILL_INLINE_CHARS: usize = 20_000;

/// 落盘目录里保留的文件上限。超出时按修改时间删最老的 —— spill 是
/// 临时取回通道，不是归档，没有长期保存价值。
const SPILL_MAX_FILES: usize = 200;

/// 工具结果的落盘目标。
#[derive(Debug, Clone)]
pub struct SpillSink {
    dir: PathBuf,
}

impl SpillSink {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// 默认落盘目录（`$hermix_home/data/spill/`）。
    pub fn default_dir() -> Self {
        Self::new(hermix_config::paths::spill_dir())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 超阈值则落盘并返回「预览 + 取回指引」，否则原样返回。
    ///
    /// `inline_chars` 传 [`SPILL_INLINE_CHARS`]；调用方也可以按预算收紧。
    pub fn maybe_spill(&self, tool_call_id: &str, content: &str, inline_chars: usize) -> String {
        let total = content.chars().count();
        if total <= inline_chars {
            return content.to_string();
        }
        let head = head_chars(content, inline_chars);
        match self.write(tool_call_id, content) {
            Ok(path) => {
                self.evict_if_needed();
                let omitted = total - inline_chars;
                format!(
                    "{head}\n\n[... 完整结果 {total} 字符已落盘：{} ...]\n\
                     以上为前 {inline_chars} 字符，其余 {omitted} 字符未展示。\
                     需要完整内容时用 read_file 读取该路径，配合 offset/limit 分段读，\
                     不要一次性全读进上下文。",
                    path.display()
                )
            }
            Err(e) => {
                // 写不下就退化成截断：结果已经拿到，不能整个丢掉。
                tracing::warn!("tool result spill failed ({e}), falling back to truncation");
                format!(
                    "{head}\n\n[... truncated {omitted} characters ...]",
                    omitted = total - inline_chars
                )
            }
        }
    }

    fn write(&self, tool_call_id: &str, content: &str) -> std::io::Result<PathBuf> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{}.txt", sanitize_id(tool_call_id)));
        fs::write(&path, content)?;
        Ok(path)
    }

    /// 文件数超上限时删最老的，避免长期会话把磁盘写满。
    fn evict_if_needed(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("txt"))
            .filter_map(|e| {
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((mtime, e.path()))
            })
            .collect();
        if files.len() <= SPILL_MAX_FILES {
            return;
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let excess = files.len() - SPILL_MAX_FILES;
        for (_, path) in files.into_iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }
}

/// 文件名只保留安全字符，避免 tool_call_id 里的分隔符逃逸出目录。
fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "result".to_string()
    } else {
        cleaned
    }
}

/// char 安全的前缀切片（CJK / emoji 不会切在多字节中间）。
fn head_chars(content: &str, n: usize) -> &str {
    match content.char_indices().nth(n) {
        Some((byte, _)) => &content[..byte],
        None => content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(name: &str) -> SpillSink {
        let dir = std::env::temp_dir().join(format!("hermix-spill-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        SpillSink::new(dir)
    }

    #[test]
    fn short_result_passes_through_unchanged() {
        let s = sink("passthrough");
        let out = s.maybe_spill("call_1", "hello", SPILL_INLINE_CHARS);
        assert_eq!(out, "hello");
        assert!(!s.dir().exists(), "短结果不应创建落盘目录");
    }

    #[test]
    fn oversized_result_is_spilled_with_retrievable_path() {
        let s = sink("spill");
        let body = "行\n".repeat(30_000);
        let out = s.maybe_spill("call_abc", &body, 1_000);
        assert!(out.len() < body.len());
        assert!(out.contains("已落盘"));
        let path = s.dir().join("call_abc.txt");
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
        assert!(out.contains(&path.display().to_string()), "指引里必须给出可读取的路径");
    }

    #[test]
    fn spill_is_idempotent_per_tool_call() {
        let s = sink("idem");
        let body = "x".repeat(5_000);
        s.maybe_spill("call_1", &body, 100);
        s.maybe_spill("call_1", &body, 100);
        let n = fs::read_dir(s.dir()).unwrap().count();
        assert_eq!(n, 1);
    }

    #[test]
    fn spill_failure_degrades_to_truncation_not_data_loss() {
        // 目录位置被一个普通文件占住 → create_dir_all 必然失败。
        let blocker = std::env::temp_dir().join("hermix-spill-blocker");
        fs::write(&blocker, b"not a dir").unwrap();
        let s = SpillSink::new(blocker.clone());
        let body = "y".repeat(500);
        let out = s.maybe_spill("call_1", &body, 100);
        assert!(out.starts_with(&"y".repeat(100)));
        assert!(out.contains("truncated"), "写盘失败必须退化成截断而不是丢结果");
    }

    #[test]
    fn head_slice_is_char_safe_for_cjk() {
        let body = "中文内容".repeat(10);
        let head = head_chars(&body, 7);
        assert_eq!(head.chars().count(), 7);
        assert_eq!(head, "中文内容中文内");
    }

    #[test]
    fn hostile_tool_call_id_cannot_escape_dir() {
        let s = sink("escape");
        let body = "z".repeat(3_000);
        s.maybe_spill("../../evil", &body, 10);
        let names: Vec<String> = fs::read_dir(s.dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![".._.._evil.txt".to_string()]);
    }
}
