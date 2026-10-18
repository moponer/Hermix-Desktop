//! Session-level read-state tracking for file edits — ZCode
//! read-file-state 模式的 hermix 移植（机制级对标结论，2026-10-18）。
//!
//! 目的：堵住"patch 零状态"的洞——没读过就能改、读取后文件被外部修改
//! 就盲写覆盖、截断视图也能当编辑依据。规则：
//!
//! - `read_file` / `write_file` 成功后为 (session, absolute path) 记录
//!   `{mtime_ms, size, partial}`（partial = 输出被截断的部分视图）。
//! - `patch` 前校验（仅对携带 `_session_id` 的 agent 派发生效；dashboard
//!   /API 直调不注入 session，跳过校验保持兼容）：
//!   - 未读过 → `NotRead`（ZCode FILE_NOT_READ）；
//!   - 读的是截断视图 → `Partial`（ZCode isPartialView 拒编辑）；
//!   - 当前 mtime/size 与记录不一致 → `Stale`（读取后文件被外部改动）。
//! - patch/write 成功后刷新记录（mtime/size 已随写入变化）。
//!
//! 全局状态按 session 键控、内存态：重启即清（与 task_manager 的教训
//! 不同——这里丢失的代价只是"重新 read_file 一次"，无数据损失）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy)]
pub(crate) struct FileReadState {
    pub mtime_ms: u128,
    pub size: u64,
    pub partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditCheck {
    /// 已读且未变化——放行。
    Ok,
    /// 会话内未读过该文件。
    NotRead,
    /// 读到的是截断视图，不能作为编辑依据。
    Partial,
    /// 读取后文件被外部修改（mtime 或 size 变化）。
    Stale,
}

static STATES: OnceLock<Mutex<HashMap<String, HashMap<PathBuf, FileReadState>>>> =
    OnceLock::new();

fn states() -> &'static Mutex<HashMap<String, HashMap<PathBuf, FileReadState>>> {
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 当前 (mtime_ms, size) 指纹；文件不存在时 None。
fn fingerprint(path: &Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some((mtime_ms, meta.len()))
}

/// 记录一次成功读取/写入。`partial` = 输出被截断（部分视图）。
pub(crate) fn record(session: &str, abs_path: &Path, partial: bool) {
    let Some((mtime_ms, size)) = fingerprint(abs_path) else {
        return;
    };
    // Poison-tolerant: a panic while holding the table must not turn every
    // later read_file/patch into a panic of its own (the data is a cache of
    // mtime/size — usable even if a previous holder died mid-update).
    let mut all = states()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    all.entry(session.to_string())
        .or_default()
        .insert(
            abs_path.to_path_buf(),
            FileReadState {
                mtime_ms,
                size,
                partial,
            },
        );
}

/// patch 前校验。`session` 为 None（非 agent 调用方）时放行。
pub(crate) fn check_edit(session: Option<&str>, abs_path: &Path) -> EditCheck {
    let Some(session) = session else {
        return EditCheck::Ok;
    };
    let all = states()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = all.get(session).and_then(|m| m.get(abs_path)) else {
        return EditCheck::NotRead;
    };
    if state.partial {
        return EditCheck::Partial;
    }
    match fingerprint(abs_path) {
        // 文件被删了也视为 stale（让模型重读后再决定）。
        None => EditCheck::Stale,
        Some((mtime_ms, size)) => {
            if mtime_ms != state.mtime_ms || size != state.size {
                EditCheck::Stale
            } else {
                EditCheck::Ok
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn not_read_then_stale_then_fresh() {
        let dir = std::env::temp_dir().join(format!("frs-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = temp_file(&dir, "a.txt", b"v1");
        let session = "sess-frs-1";

        assert_eq!(check_edit(Some(session), &file), EditCheck::NotRead);

        record(session, &file, false);
        assert_eq!(check_edit(Some(session), &file), EditCheck::Ok);

        // 外部修改 → stale。
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, b"v2-much-longer-content").unwrap();
        assert_eq!(check_edit(Some(session), &file), EditCheck::Stale);

        // 重新读取 → ok；partial 视图拒编辑。
        record(session, &file, false);
        assert_eq!(check_edit(Some(session), &file), EditCheck::Ok);
        record(session, &file, true);
        assert_eq!(check_edit(Some(session), &file), EditCheck::Partial);

        // 无 session（dashboard/API 直调）→ 放行。
        assert_eq!(check_edit(None, &file), EditCheck::Ok);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
