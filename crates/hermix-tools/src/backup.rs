//! 自动备份：agent 覆盖文件前把旧版本复制到备份目录。
//!
//! 设计要点（first-principles）：
//! - 这是「兜底防数据丢失」能力，必须是 **best-effort** —— 任何错误只告警，
//!   绝不阻塞或失败原始写操作。一条备份失败不能把用户的写也拖挂。
//! - 唯一挂钩点：`write_file` 工具与 `patch` 工具（agent 两条破坏性写路径）。
//!   `Fs` 接缝、terminal backend、fuzzy patch 都汇到这两个工具，挂钩一次覆盖。
//! - 策略由服务端在配置加载/变更时通过 [`set_policy_from_config`] 注入；本模块
//!   不读 config.yaml，避免每次写都解析 YAML（铁律：信任边界外的重复 IO 要省）。

use std::path::{Component, Path, PathBuf};
use std::sync::{RwLock, OnceLock};

use hermix_config::SecurityConfig;

#[derive(Debug, Clone)]
struct BackupPolicy {
    enabled: bool,
    dir: PathBuf,
    max_bytes: u64,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: PathBuf::new(),
            max_bytes: 512 * 1024 * 1024,
        }
    }
}

fn policy_cell() -> &'static RwLock<BackupPolicy> {
    static CELL: OnceLock<RwLock<BackupPolicy>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(BackupPolicy::default()))
}

/// 由服务端在配置加载/变更时调用，刷新当前备份策略。
pub fn set_policy_from_config(sec: &SecurityConfig, hermix_home: &Path) {
    let dir = match &sec.backup_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => hermix_home.join("backups"),
    };
    let policy = BackupPolicy {
        enabled: sec.auto_backup,
        dir,
        max_bytes: sec.backup_max_mb.max(1) * 1024 * 1024,
    };
    if let Ok(mut g) = policy_cell().write() {
        *g = policy;
    }
}

/// 备份目录统计，供前端「安全中心」展示。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupStatus {
    pub enabled: bool,
    pub dir: Option<String>,
    pub total_bytes: u64,
    pub file_count: u64,
    pub last_backup: Option<String>,
}

/// 读取备份目录统计（best-effort）。
pub fn status() -> BackupStatus {
    let p = policy_cell().read().map(|g| g.clone()).unwrap_or_default();
    if !p.enabled || p.dir.as_os_str().is_empty() {
        return BackupStatus {
            enabled: p.enabled,
            dir: None,
            total_bytes: 0,
            file_count: 0,
            last_backup: None,
        };
    }
    let mut total: u64 = 0;
    let mut count: u64 = 0;
    let mut last: Option<String> = None;
    collect_dir(&p.dir, &mut total, &mut count, &mut last);
    BackupStatus {
        enabled: p.enabled,
        dir: Some(p.dir.to_string_lossy().into_owned()),
        total_bytes: total,
        file_count: count,
        last_backup: last,
    }
}

fn collect_dir(dir: &Path, total: &mut u64, count: &mut u64, last: &mut Option<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_dir(&p, total, count, last);
            } else if is_bak(&p) {
                if let Ok(meta) = p.metadata() {
                    *total += meta.len();
                    *count += 1;
                    update_last(last, &meta);
                }
            }
        }
    }
}

fn is_bak(p: &Path) -> bool {
    p.file_name()
        .map(|n| n.to_string_lossy().ends_with(".bak"))
        .unwrap_or(false)
}

fn update_last(last: &mut Option<String>, meta: &std::fs::Metadata) {
    if let Ok(mt) = meta.modified() {
        // UNIX 秒 + 纳秒，作为可直接比较的字符串。
        let secs = mt
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let s = secs.to_string();
        if last.as_ref().map(|l| s > *l).unwrap_or(true) {
            *last = Some(s);
        }
    }
}

/// 在覆盖 `path` 前调用：把 `original` 复制到备份目录。best-effort。
///
/// `original` 为空表示这是新文件（没有旧版本可备份），直接跳过。
pub fn try_backup(path: &Path, original: &[u8]) {
    let policy = match policy_cell().read() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };
    if !policy.enabled || policy.dir.as_os_str().is_empty() {
        return;
    }
    if original.is_empty() {
        return;
    }

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let date_dir = policy.dir.join(&date);
    let rel = backup_rel_path(path);
    let dest_dir = date_dir.join(rel.parent().unwrap_or_else(|| Path::new("")));
    let stem = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let ts = chrono::Local::now().format("%H%M%S%.3f");
    let dest = dest_dir.join(format!("{}.{}.bak", stem, ts));

    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        tracing::warn!(path = %path.display(), error = %e, "backup: mkdir failed");
        return;
    }
    if let Err(e) = std::fs::write(&dest, original) {
        tracing::warn!(path = %path.display(), error = %e, "backup: write failed");
        return;
    }
    enforce_cap(&policy);
}

/// 把任意源路径映射成备份目录下的相对路径：保留目录结构（便于回滚定位），
/// 去掉 Windows 盘符冒号与根分隔符，避免在不同驱动器间撞名。
fn backup_rel_path(path: &Path) -> PathBuf {
    let mut rel = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                let s = p.as_os_str().to_string_lossy().replace(':', "");
                if !s.is_empty() {
                    rel.push(s);
                }
            }
            Component::RootDir | Component::CurDir | Component::ParentDir => {}
            Component::Normal(s) => rel.push(s),
        }
    }
    if rel.as_os_str().is_empty() {
        rel.push("unknown");
    }
    rel
}

/// 超出上限时按修改时间从最旧开始删，直到回到上限内。
fn enforce_cap(policy: &BackupPolicy) {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    collect_with_mtime(&policy.dir, &mut files, &mut total);
    if total <= policy.max_bytes {
        return;
    }
    files.sort_by_key(|(_, _, mt)| *mt);
    let mut idx = 0;
    while total > policy.max_bytes && idx < files.len() {
        let (p, size, _) = &files[idx];
        if std::fs::remove_file(p).is_ok() {
            total -= *size;
        }
        idx += 1;
    }
}

fn collect_with_mtime(
    dir: &Path,
    out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>,
    total: &mut u64,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_with_mtime(&p, out, total);
            } else if is_bak(&p) {
                if let Ok(meta) = p.metadata() {
                    *total += meta.len();
                    let mt = meta
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    out.push((p, meta.len(), mt));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_config::SecurityConfig;

    #[test]
    fn rel_path_preserves_structure_without_drive_colon() {
        // Windows `D:\foo\bar.txt` → `D/foo/bar.txt`（去冒号，保留层级）。
        let p = backup_rel_path(Path::new("D:\\foo\\bar.txt"));
        assert_eq!(p, PathBuf::from("D/foo/bar.txt"));
        // POSIX 绝对路径同样保留层级。
        let q = backup_rel_path(Path::new("/home/user/a.txt"));
        assert_eq!(q, PathBuf::from("home/user/a.txt"));
    }

    #[test]
    fn try_backup_writes_bak_and_status_reflects_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let backup_dir = tmp.path().join("backups");
        std::fs::create_dir_all(&home).unwrap();

        set_policy_from_config(
            &SecurityConfig {
                auto_backup: true,
                backup_dir: Some(backup_dir.to_string_lossy().into_owned()),
                backup_max_mb: 512,
            },
            &home,
        );

        let target = tmp.path().join("work.txt");
        std::fs::write(&target, b"v1").unwrap();
        // 覆盖前，把 v1 备份。
        try_backup(&target, b"v1");

        let st = status();
        assert!(st.enabled);
        assert_eq!(st.file_count, 1);
        assert!(st.total_bytes >= 2);

        // 新文件（original 为空）不备份。
        let fresh = tmp.path().join("fresh.txt");
        try_backup(&fresh, &[]);
        assert_eq!(status().file_count, 1);
    }

    #[test]
    fn enforce_cap_deletes_oldest_over_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        // 直接验证 enforce_cap 删除逻辑：手写三个备份（创建顺序即 mtime 顺序，
        // f1 最旧），上限 2 字节 → 删最旧 1 个，剩 2 个。
        let d = tmp.path().join("d");
        std::fs::create_dir_all(&d).unwrap();
        let f1 = d.join("a.1.bak");
        let f2 = d.join("b.2.bak");
        let f3 = d.join("c.3.bak");
        std::fs::write(&f1, b"x").unwrap();
        std::fs::write(&f2, b"x").unwrap();
        std::fs::write(&f3, b"x").unwrap();
        let policy = BackupPolicy {
            enabled: true,
            dir: d.clone(),
            max_bytes: 2,
        };
        enforce_cap(&policy);
        assert_eq!(status_dir_count(&d), 2);
    }

    fn status_dir_count(dir: &Path) -> usize {
        let mut n = 0;
        if let Ok(es) = std::fs::read_dir(dir) {
            for e in es.flatten() {
                if e.path().is_file() && is_bak(&e.path()) {
                    n += 1;
                }
            }
        }
        n
    }
}
