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

/// Marker present in a message that already carries a spill pointer.
///
/// The agent layer spills at two points (`budget::enforce_budget_with_spill`
/// and `push_tool_result`). Without this marker the second one rewrote
/// the file the first one had just written — with the *preview* in place
/// of the full result — so the pointer the model was given led back to
/// the preview. The retrieval path was a loop.
pub const SPILL_POINTER_MARKER: &str = "已落盘";

/// Does this content already carry a spill pointer?
pub fn is_spilled(content: &str) -> bool {
    content.contains(SPILL_POINTER_MARKER)
}

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
        // ★ The preview is cut on a **line** boundary, and the cut point
        // is reported in lines.
        //
        // The retrieval tool is line-addressed (`read_file`'s `offset` is
        // a 1-based line number), but the old message reported a *char*
        // count ("以上为前 20000 字符"). The model cannot convert chars
        // to lines — it has no way to know how many lines 20,000
        // characters occupy — so every read-back started at the wrong
        // place. Reporting the exact line where the omitted part begins
        // makes the promised `read_file(path, offset=N)` call actually
        // correct.
        let (head, head_lines, total_lines) = head_by_lines(content, inline_chars);
        match self.write(tool_call_id, content) {
            Ok(path) => {
                self.evict_if_needed();
                let omitted = total - head.chars().count();
                if total_lines > 1 && head_lines > 0 {
                    // Line-addressable: hand back the exact offset.
                    let next_line = head_lines + 1;
                    format!(
                        "{head}\n\n[... 完整结果 {total} 字符 / {total_lines} 行已落盘：{} ...]\n\
                         以上是前 {head_lines} 行（共 {total_lines} 行），第 {next_line} 行起未展示。\n\
                         取回缺失部分：read_file(path=\"{}\", offset={next_line}, limit=200) —— \
                         按行号续读，逐段取回，不要一次性全读进上下文。",
                        path.display(),
                        path.display()
                    )
                } else {
                    // A single line (or one line longer than the whole
                    // budget): line addressing cannot help here, so fall
                    // back to reporting characters.
                    format!(
                        "{head}\n\n[... 完整结果 {total} 字符已落盘：{} ...]\n\
                         以上为前 {} 字符，其余 {omitted} 字符未展示。\n\
                         需要完整内容时用 read_file 读取该路径（配合 offset/limit 分段读），\
                         不要一次性全读进上下文。",
                        path.display(),
                        head.chars().count()
                    )
                }
            }
            Err(e) => {
                // 写不下就退化成截断：结果已经拿到，不能整个丢掉。
                tracing::warn!("tool result spill failed ({e}), falling back to truncation");
                format!(
                    "{head}\n\n[... truncated {omitted} characters ...]",
                    omitted = total - head.chars().count()
                )
            }
        }
    }

    /// Spill content that has **no tool-call id** — a tool's own output
    /// cap (`terminal`) runs before any call id exists.
    ///
    /// The file is keyed by a hash of the content, so spilling the same
    /// output twice is idempotent (one file, rewritten in place) and
    /// two different outputs never collide.
    pub fn maybe_spill_by_content(&self, content: &str, inline_chars: usize) -> String {
        self.maybe_spill(&content_key(content), content, inline_chars)
    }

    /// Like [`Self::maybe_spill_by_content`], but reports the failure
    /// instead of degrading to a truncation.
    ///
    /// `None` means "the content could not be written" — the caller
    /// decides what to do (the tool layer falls back to its old hard
    /// cut rather than dropping the result).
    pub fn try_spill_by_content(
        &self,
        content: &str,
        inline_chars: usize,
    ) -> Option<String> {
        if content.chars().count() <= inline_chars {
            return None;
        }
        let key = content_key(content);
        let (head, head_lines, total_lines) = head_by_lines(content, inline_chars);
        let path = self.write(&key, content).ok()?;
        self.evict_if_needed();
        let total = content.chars().count();
        if total_lines > 1 && head_lines > 0 {
            let next_line = head_lines + 1;
            Some(format!(
                "{head}\n\n[... 完整结果 {total} 字符 / {total_lines} 行已落盘：{} ...]\n\
                 以上是前 {head_lines} 行（共 {total_lines} 行），第 {next_line} 行起未展示。\n\
                 取回缺失部分：read_file(path=\"{}\", offset={next_line}, limit=200) —— \
                 按行号续读，逐段取回，不要一次性全读进上下文。",
                path.display(),
                path.display()
            ))
        } else {
            Some(format!(
                "{head}\n\n[... 完整结果 {total} 字符已落盘：{} ...]\n\
                 以上为前 {} 字符，其余 {} 字符未展示。\n\
                 需要完整内容时用 read_file 读取该路径（配合 offset/limit 分段读），\
                 不要一次性全读进上下文。",
                path.display(),
                head.chars().count(),
                total - head.chars().count()
            ))
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

/// A stable key for content that has no call id (FNV-1a over the
/// bytes). Stable across restarts, so a spilled file that is still on
/// disk keeps the same name.
fn content_key(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in content.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("out-{hash:016x}")
}

/// 文件名只保留安全字符，避免 tool_call_id 里的分隔符逃逸出目录。
fn sanitize_id(id: &str) -> String {    let cleaned: String = id
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
/// Cut the content to at most `budget` chars, ending on a line
/// boundary, and report where the omitted part begins.
///
/// Returns `(head, lines_shown, total_lines)`.
///
/// Why a line boundary: the only way back to the omitted bytes is
/// `read_file(path, offset=N)`, and `offset` counts **lines**. A char
/// cut would leave the model unable to name a correct `N`.
///
/// `lines_shown` is 0 when the first line alone exceeds the budget —
/// then no line boundary exists inside the budget and the caller
/// falls back to char-based reporting.
fn head_by_lines(content: &str, budget: usize) -> (String, usize, usize) {
    let total_lines = content.lines().count();
    let cut = content
        .char_indices()
        .nth(budget)
        .map(|(byte, _)| byte)
        .unwrap_or(content.len());
    let prefix = &content[..cut];
    match prefix.rfind('\n') {
        Some(nl) => {
            let head = &prefix[..nl];
            let lines = head.lines().count();
            (head.to_string(), lines, total_lines)
        }
        None => {
            // The first line is longer than the whole budget → there is
            // no line boundary to cut on.
            (prefix.to_string(), 0, total_lines)
        }
    }
}

/// char 安全的前缀切片（CJK / emoji 不会切在多字节中间）。
///
/// Kept for the callers that need a plain char cut; the spill path
/// itself uses [`head_by_lines`] so the cut point is line-addressable.
#[allow(dead_code)]
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

    /// The cut point must be **line**-addressable, because the only way
    /// back to the omitted bytes is `read_file(path, offset=N)` and
    /// `offset` counts lines. A char-based cut point left the model
    /// unable to name a correct `N`.
    #[test]
    fn spill_reports_the_line_the_omitted_part_starts_at() {
        let s = sink("lines");
        let body = "行\n".repeat(30_000); // 30,000 lines
        let out = s.maybe_spill("call_lines", &body, 1_000);

        // The preview is whole lines only: it is a prefix of the
        // original AND the original's next character is a newline —
        // i.e. the cut fell on a line boundary, not inside a line.
        let preview = out.split("\n\n[... 完整结果").next().unwrap();
        assert!(body.starts_with(preview), "预览必须是原文的前缀");
        let next = &body[preview.len()..];
        assert!(
            next.starts_with('\n'),
            "预览必须停在行边界上（原文的下一个字符必须是换行）"
        );

        // The message names the line the omitted part starts at, and
        // the offset to hand to `read_file`.
        assert!(out.contains("第 501 行起未展示"), "必须报告未展示部分的起始行");
        assert!(
            out.contains("offset=501"),
            "必须给出可直接使用的 read_file offset，实际输出：{out}"
        );
    }

    /// The reported offset must actually point at the first omitted
    /// line — a wrong number here is worse than no number at all.
    #[test]
    fn the_reported_offset_points_at_the_first_omitted_line() {
        let s = sink("offset");
        let mut body = String::new();
        for i in 0..1000 {
            body.push_str(&format!("line-{i}\n"));
        }
        let out = s.maybe_spill("call_offset", &body, 200);

        // Parse the offset out of the message and use it the way the
        // model would.
        let at = out.find("offset=").expect("必须给出 offset");
        let offset: usize = out[at + 7..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .expect("offset 必须是数字");

        // The line at that offset must be **exactly** the line after
        // the preview's last line — that is the whole contract.
        let preview = out.split("\n\n[... 完整结果").next().unwrap();
        let preview_last = preview.lines().last().expect("预览至少一行");
        let path = s.dir().join("call_offset.txt");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        let at_offset = on_disk
            .lines()
            .nth(offset - 1)
            .expect("offset 指向的行必须存在");
        let n: usize = preview_last
            .rsplit('-')
            .next()
            .and_then(|s| s.parse().ok())
            .expect("预览末行必须是 line-N 形式");
        assert_eq!(
            at_offset,
            format!("line-{}", n + 1),
            "offset={offset} 必须正好指向预览末行之后的那一行"
        );
    }

    /// A single-line result has no line boundary to cut on — the
    /// message must fall back to char-based reporting rather than
    /// claiming a line offset that does not exist.
    #[test]
    fn single_line_result_falls_back_to_char_reporting() {
        let s = sink("oneline");
        let body = "x".repeat(5_000);
        let out = s.maybe_spill("call_one", &body, 100);
        assert!(!out.contains("offset="), "单行结果不能报行 offset：{out}");
        assert!(out.contains("字符"), "单行结果必须回落到字符口径：{out}");
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
