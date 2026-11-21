//! 工作区快照差分（2026-11-20）：把 **terminal 命令造成的文件变更** 变成
//! 与 write_file/patch 同权的 `file_change` 事件。
//!
//! 为什么存在：`extract_file_change` 只能看见**结构化写工具**的入参；
//! 模型经 terminal（`Set-Content` / `Out-File` / 重定向 / 脚本生成）改文件
//! 时，聊天尾部的「已编辑 N 个文件」卡片拿不到任何信号。本模块在命令
//! 前后各扫一次工作区，按 stat 差分出海量安全的变更清单。
//!
//! ## 有界性（大工作区不失控）
//!
//! - 跳过重目录（node_modules / target / .git / dist …）与隐藏目录；
//! - 文件数上限 MAX_FILES，超出即放弃本次差分（宁缺毋滥：半份差分比
//!   没有差分更误导）；
//! - 内容捕获只针对小于 MAX_CONTENT_BYTES 的文件，行数差分才有意义；
//!   二进制/超大文件只报「变了」，不编造行数。
//!
//! ## 诚实性
//!
//! mtime+size 都未变的文件视为未改——不读内容、不做哈希。这依赖
//! 「工具改文件必动 mtime」的文件系统常识，与 git status 同一假设。
use similar::TextDiff;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 扫描的文件数上限；超过即整份差分放弃。
const MAX_FILES: usize = 4_000;
/// 单文件内容捕获上限——超过只报变更不报行数。
const MAX_CONTENT_BYTES: u64 = 64 * 1_024;
/// 每次快照**内容总量**上限：4000 个 64KB 文件全读是 256MB，不可接受。
/// 超预算后只记 stat（仍能报告「变了」，只是不给行数）——把 IO 成本
/// 钉死在上限内，而不是随仓库大小线性漂移。
const MAX_TOTAL_CONTENT_BYTES: u64 = 8 * 1_024 * 1_024;
/// 重目录名（全小写比较）。这些目录里的一万个变更都不是模型的本意。
const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", ".git", "dist", "build", "out",
    ".next", ".turbo", "coverage", "__pycache__", ".venv", "venv",
];

#[derive(Debug, Clone)]
struct Stat {
    mtime: std::time::SystemTime,
    size: u64,
    /// 小文件在快照时留底——modified 的行数差分靠它。
    content: Option<String>,
}

/// 一次快照：路径 → stat（+ 小文件内容）。
#[derive(Debug, Default)]
pub struct WorkspaceSnapshot {
    files: BTreeMap<PathBuf, Stat>,
    truncated: bool,
    /// 已捕获内容字节数（受 MAX_TOTAL_CONTENT_BYTES 约束）。
    captured_bytes: u64,
}

fn is_skip_dir(dir_name: &str) -> bool {
    let d = dir_name.to_ascii_lowercase();
    SKIP_DIRS.iter().any(|s| *s == d)
}

/// 内容缓存（按工作区根）：mtime+size 未变的文件直接复用上次的内容，
/// 重复命令的快照从「全量重读」降为「纯 stat 差分」——这是 shell 批次
/// 里除进程启动外最大的单笔 IO。（2026-11-20 等待时间治理）
static CONTENT_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, BTreeMap<PathBuf, Stat>>>,
> = std::sync::OnceLock::new();

impl WorkspaceSnapshot {
    /// 扫描 root（递归）。root 不存在时得到空快照。
    pub fn take(root: &Path) -> Self {
        let mut snap = Self::default();
        if root.is_dir() {
            // 取出本根的缓存（走完再存回）；删除的文件条目自然消失。
            let cache = CONTENT_CACHE
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(root)
                .unwrap_or_default();
            snap.walk(root, root, &cache);
            CONTENT_CACHE
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(root.to_path_buf(), snap.files.clone());
        }
        snap
    }

    fn walk(&mut self, dir: &Path, root: &Path, cache: &BTreeMap<PathBuf, Stat>) {
        if self.truncated {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if self.files.len() >= MAX_FILES {
                self.truncated = true;
                return;
            }
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if is_skip_dir(&name) || name.starts_with('.') {
                    continue;
                }
                self.walk(&path, root, cache);
                continue;
            }
            if !ft.is_file() {
                continue; // 符号链接等跳过——差分只对真实文件负责
            }
            let Ok(meta) = entry.metadata() else { continue };
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            // 小文件留底（供 modified 的行数差分）；读失败等同无内容。
            // 受总预算约束：超预算的文件只记 stat。
            // 缓存命中（mtime+size 未变）零 IO 复用——不计入读预算。
            let cached_hit = cache
                .get(&rel)
                .filter(|c| {
                    c.mtime == meta.modified().unwrap_or(std::time::UNIX_EPOCH)
                        && c.size == meta.len()
                })
                .and_then(|c| c.content.clone());
            let content = match cached_hit {
                Some(c) => Some(c),
                None if meta.len() <= MAX_CONTENT_BYTES
                    && self.captured_bytes + meta.len() <= MAX_TOTAL_CONTENT_BYTES =>
                {
                    match std::fs::read_to_string(&path) {
                        Ok(c) => {
                            self.captured_bytes += meta.len();
                            Some(c)
                        }
                        Err(_) => None,
                    }
                }
                None => None,
            };
            self.files.insert(
                rel,
                Stat {
                    mtime: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    size: meta.len(),
                    content,
                },
            );
        }
    }
}

/// 一条差分结果。added/removed 只在能拿到前后内容时才有值。
///
/// `path` 是**相对 root、正斜杠分隔**的路径：事件跨平台同形，前端按 `/`
/// 归一的逻辑直接可用，不必再猜 Windows 反斜杠。
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub change_type: &'static str, // "created" | "modified" | "deleted"
    pub added: Option<u32>,
    pub removed: Option<u32>,
    pub summary: Option<String>,
}

/// 差分 before 与当前磁盘状态（root 与快照相同）。
pub fn diff_since(before: &WorkspaceSnapshot, root: &Path) -> Vec<FileDiff> {
    let after = WorkspaceSnapshot::take(root);
    let mut out = Vec::new();

    // created / modified
    for (rel, after_stat) in &after.files {
        match before.files.get(rel) {
            None => {
                let (added, summary) = content_and_count(after_stat);
                out.push(FileDiff {
                    path: rel.to_string_lossy().replace('\\', "/"),
                    change_type: "created",
                    added: Some(added),
                    removed: Some(0),
                    summary: Some(summary),
                });
            }
            Some(b) if b.mtime != after_stat.mtime || b.size != after_stat.size => {
                // 真 diff：前后内容都在就做行级比对（这是本模块存在的价值——
                // terminal 路径拿得到命令前后的真实内容）。任一缺失（二进制 /
                // 超限）就给 None，绝不用「总行数」冒充增删行数。
                let (added, removed) = match (&b.content, &after_stat.content) {
                    (Some(before_text), Some(after_text)) => {
                        let (a, r) = diff_line_counts(before_text, after_text);
                        (Some(a), Some(r))
                    }
                    _ => (None, None),
                };
                out.push(FileDiff {
                    path: rel.to_string_lossy().replace('\\', "/"),
                    change_type: "modified",
                    added,
                    removed,
                    summary: None,
                });
            }
            Some(_) => {} // stat 未变 = 未改
        }
    }
    // deleted
    for (rel, b) in &before.files {
        if !after.files.contains_key(rel) {
            out.push(FileDiff {
                path: rel.to_string_lossy().replace('\\', "/"),
                change_type: "deleted",
                added: Some(0),
                removed: Some(line_count(b).unwrap_or(0)),
                summary: None,
            });
        }
    }
    out
}

fn line_count(s: &Stat) -> Option<u32> {
    s.content.as_ref().map(|c| c.lines().count() as u32)
}

/// created 文件的单行摘要：内容能读就报行数，读不出（二进制）就报字节量。
fn content_and_count(s: &Stat) -> (u32, String) {
    match line_count(s) {
        Some(n) => (n, format!("新建文件，{n} 行")),
        None => (0, format!("新建文件，{} 字节", s.size)),
    }
}

/// 行级差分计数（added, removed）。给 modified 的前后都有内容时用，
/// 把「估算下界」升级成真 diff。
pub fn diff_line_counts(before: &str, after: &str) -> (u32, u32) {
    let d = TextDiff::from_lines(before, after);
    let mut added = 0u32;
    let mut removed = 0u32;
    for change in d.iter_all_changes() {
        let tag = change.tag();
        let n = change.to_string_lossy().lines().count() as u32;
        match tag {
            similar::ChangeTag::Insert => added += n,
            similar::ChangeTag::Delete => removed += n,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn created_modified_deleted_are_all_detected() {
        let dir = std::env::temp_dir().join(format!("wsdiff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "a.txt", b"one\n");
        write(&dir, "sub/b.txt", b"keep\n");
        std::thread::sleep(std::time::Duration::from_millis(20));

        let before = WorkspaceSnapshot::take(&dir);
        write(&dir, "new.txt", b"hello\n");
        write(&dir, "a.txt", b"one\ntwo\nthree\n");
        std::fs::remove_file(dir.join("sub/b.txt")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let diffs = diff_since(&before, &dir);
        let get = |p: &str| diffs.iter().find(|d| d.path == p);
        let created = get("new.txt").expect("created");
        assert_eq!(created.change_type, "created");
        assert_eq!(created.added, Some(1));
        let modified = get("a.txt").expect("modified");
        assert_eq!(modified.change_type, "modified");
        // 真 diff："one\n" → "one\ntwo\nthree\n" 只**新增** 2 行，删除 0 行。
        // （用前后总行数当增删数会虚报成 +3/−1——那不是差分，是两次 wc。）
        assert_eq!((modified.added, modified.removed), (Some(2), Some(0)));
        let deleted = get("sub/b.txt").expect("deleted");
        assert_eq!(deleted.change_type, "deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unchanged_workspace_yields_no_diffs() {
        let dir = std::env::temp_dir().join(format!("wsdiff-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "x.txt", b"stable\n");
        let before = WorkspaceSnapshot::take(&dir);
        assert!(diff_since(&before, &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_dirs_and_binary_are_bounded() {
        let dir = std::env::temp_dir().join(format!("wsdiff-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        write(&dir, "node_modules/pkg/index.js", b"var x\n");
        let before = WorkspaceSnapshot::take(&dir);
        assert!(before.files.is_empty(), "node_modules 必须被跳过");
        let bin = dir.join("blob.bin");
        std::fs::write(&bin, [0u8, 159, 146, 150]).unwrap();
        let s = WorkspaceSnapshot::take(&dir);
        assert!(line_count(&s.files[&PathBuf::from("blob.bin")]).is_none());
        assert!(bin.exists());
    }

    #[test]
    fn diff_line_counts_are_exact() {
        let (a, r) = diff_line_counts("one\n", "one\ntwo\nthree\n");
        assert_eq!((a, r), (2, 0));
        let (a, r) = diff_line_counts("x\ny\n", "y\n");
        assert_eq!((a, r), (0, 1));
    }
}
