//! 盘内定向扫描：构建产物（kondo 式）+ 残留文件 + 大文件 + 空目录。
//!
//! 为什么需要它：`cleanup::RULES` 是**固定路径**规则（%TEMP%、缩略图、WER…），
//! 这些几乎都在 C:，选 D:/F: 时只剩回收站可清——对一块装代码/素材的数据盘
//! 等于没有清理能力。开源工具的做法分两派：
//!
//! - **kondo**（MIT）：遍历目录树，用 marker 文件识别项目类型
//!   （`Cargo.toml` → `target/`、`package.json` → `node_modules/`），
//!   再统计产物目录体积，让用户按项目逐条决定。本模块照这个模型实现。
//! - **WizTree / WinDirStat**：读 MFT 做全盘体积统计，快但只"看"不"删"。
//!   我们已有 MFT 扫描器，但它住在 filesearch 守护进程里，跨进程拿结果反而
//!   更慢；这里用**受预算约束的 walkdir**，配合一次目录列举复用（marker 判定
//!   直接用子项名集合查表，不再额外 stat）。
//!
//! 安全边界（比"删临时文件"更强，因为这里要删目录）：
//! 1. 只删**产物目录本身**，且目录名必须在白名单里、父目录必须仍有对应 marker；
//! 2. 不跟随 reparse point（junction/symlink），删除时也自己走一遍、跳过链接；
//! 3. 系统禁区（System32 / Program Files / WinSxS…）与卷根一律拒绝；
//! 4. 大文件/残留文件同样只删「文件」，且必须仍在所选盘内。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use crate::sysmon::drive_of;

/// 全盘遍历预算：数据盘动辄几十万目录，扫描必须能随时放弃。
const MAX_DIRS: usize = 400_000;
const SCAN_BUDGET: Duration = Duration::from_secs(45);
/// 大文件榜条目数（与 WizTree 的"最大文件"列表同量级，够定位问题）。
const BIG_FILE_LIMIT: usize = 50;
const BIG_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// 残留文件最多列这么多，超出只计数（避免一次列出几十万条把 UI 拖死）。
const STALE_LIMIT: usize = 400;
const STALE_MIN_AGE_DAYS: u64 = 7;
const EMPTY_DIR_LIMIT: usize = 300;

/// 目录名 → 所属项目类型。kondo 的核心表，只保留常见且**可安全重建**的产物。
const ARTIFACTS: &[(&str, &[&str])] = &[
    ("target", &["Cargo.toml"]),
    ("node_modules", &["package.json"]),
    ("dist", &["package.json"]),
    (".next", &["package.json"]),
    (".nuxt", &["package.json"]),
    (".turbo", &["package.json", "turbo.json"]),
    ("build", &["pom.xml", "build.gradle", "build.gradle.kts", "CMakeLists.txt"]),
    (".gradle", &["build.gradle", "build.gradle.kts"]),
    ("__pycache__", &["pyproject.toml", "requirements.txt", "setup.py", "setup.cfg"]),
    (".venv", &["pyproject.toml", "requirements.txt", "setup.py"]),
    ("venv", &["requirements.txt", "pyproject.toml"]),
    (".pytest_cache", &["pyproject.toml", "pytest.ini", "setup.cfg"]),
    ("bin", &["*.csproj", "*.sln"]),
    ("obj", &["*.csproj", "*.sln"]),
    ("go-build", &["go.mod"]),
];

/// 残留文件后缀（BleachBit 的"临时/备份/断点"类目里适合按扩展名抓的部分）。
const STALE_EXTS: &[&str] = &[
    "tmp", "bak", "old", "chk", "crdownload", "part", "dmp", "log.old", "swp", "swo",
];

/// 系统禁区：与 cleanup 模块同口径，另加整盘根与若干不可碰目录。
const FORBIDDEN: &[&str] = &[
    r"c:\windows",
    r"c:\program files",
    r"c:\program files (x86)",
    r"c:\programdata\microsoft",
    r"c:\$recycle.bin",
    r"c:\system volume information",
    r"c:\windows.old",
];

/// 扫描时整棵跳过的目录（既不是垃圾、也不该浪费时间下去）。
const SKIP_DIRS: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "windows",
    "windows.old",
    "program files",
    "program files (x86)",
    "appdata",
    ".git",
    "node_modules", // 产物目录：遇到就作为条目记录，不再向下找项目
    "target",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDir {
    pub path: String,
    /// 产物目录名（target / node_modules / …）。
    pub kind: String,
    /// 所属项目的 marker 文件（展示"凭什么认定这是项目"）。
    pub marker: String,
    pub bytes: u64,
    pub files: u64,
    /// 产物目录的最后修改时间（Unix 秒），UI 显示"多久没用"。
    pub modified: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundFile {
    pub path: String,
    pub bytes: u64,
    pub modified: u64,
    /// 命中的原因（扩展名 / 体积），UI 直接用。
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveScanReport {
    pub drive: String,
    pub artifacts: Vec<ArtifactDir>,
    /// 按体积降序的残留文件（*.tmp / *.bak / *.dmp …）。
    pub stale: Vec<FoundFile>,
    /// 按体积降序的大文件（> 200MB）。
    pub big: Vec<FoundFile>,
    pub empty_dirs: Vec<String>,
    pub empty_dir_count: u64,
    pub dirs_scanned: u64,
    pub elapsed_ms: u64,
    /// 命中预算被截断（结果不完整，UI 要明确告知）。
    pub truncated: bool,
}

struct Budget {
    deadline: Instant,
    dirs: usize,
}

impl Budget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + SCAN_BUDGET,
            dirs: 0,
        }
    }
    fn exhausted(&self) -> bool {
        self.dirs >= MAX_DIRS || Instant::now() >= self.deadline
    }
}

/// 扫一块盘。`progress` 每扫描若干目录推一次，用于前端进度条。
pub fn scan_drive(
    drive: &str,
    progress: Option<crate::cleanup::Progress>,
) -> DriveScanReport {
    let started = Instant::now();
    let root = PathBuf::from(format!("{drive}\\"));
    let mut budget = Budget::new();
    let mut report = DriveScanReport {
        drive: drive.to_string(),
        artifacts: Vec::new(),
        stale: Vec::new(),
        big: Vec::new(),
        empty_dirs: Vec::new(),
        empty_dir_count: 0,
        dirs_scanned: 0,
        elapsed_ms: 0,
        truncated: false,
    };

    if !root.is_dir() {
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return report;
    }

    // 大文件榜用"保留前 N 大"的朴素做法：N=50，插入排序比堆更好读。
    let mut big: Vec<FoundFile> = Vec::new();
    let mut stale: Vec<FoundFile> = Vec::new();
    let mut stale_total = 0u64;
    let mut last_progress = Instant::now();
    // 闭包里不能再借 `budget`（walk 已持有可变借用），自己数一份。
    let mut scanned = 0u64;

    walk(
        &root,
        drive,
        &mut budget,
        &mut |dir, entries: &[DirItem], this_bytes| {
            scanned += 1;
            // ① 空目录（没有子项，且不是卷根）
            if entries.is_empty() && dir != root {
                report.empty_dir_count += 1;
                if report.empty_dirs.len() < EMPTY_DIR_LIMIT {
                    report.empty_dirs.push(dir.display().to_string());
                }
            }

            // ② 项目产物：用「目录名 + 同级 marker」判定，marker 命中靠子项名集合查表
            if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                let lower = name.to_ascii_lowercase();
                for (artifact, markers) in ARTIFACTS {
                    if lower != *artifact {
                        continue;
                    }
                    // 产物目录自身的位置由父目录的 marker 决定，这里向上看一层。
                    let Some(parent) = dir.parent() else { continue };
                    let Some(marker) = markers.iter().find(|m| marker_present(parent, m)) else {
                        continue;
                    };
                    let (bytes, files) = tree_size(dir, &mut Budget::new());
                    report.artifacts.push(ArtifactDir {
                        path: dir.display().to_string(),
                        kind: (*artifact).to_string(),
                        marker: (*marker).to_string(),
                        bytes,
                        files,
                        modified: modified_secs(dir),
                    });
                    return;
                }
            }

            // ③ 残留文件 / 大文件：按单文件判定（this_bytes 只用于空目录判断）
            let _ = this_bytes;
            for item in entries {
                if item.is_dir {
                    continue;
                }
                let lower = item.name.to_ascii_lowercase();
                let matched_ext = STALE_EXTS
                    .iter()
                    .find(|ext| lower.ends_with(&format!(".{ext}")));
                let old_enough = item.modified > 0
                    && SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|now| now.as_secs().saturating_sub(item.modified))
                        .unwrap_or(0)
                        >= STALE_MIN_AGE_DAYS * 86_400;

                if let Some(ext) = matched_ext {
                    if old_enough {
                        stale_total += 1;
                        if stale.len() < STALE_LIMIT {
                            stale.push(FoundFile {
                                path: item.path.clone(),
                                bytes: item.bytes,
                                modified: item.modified,
                                reason: format!(".{ext} · 超过 {STALE_MIN_AGE_DAYS} 天未修改"),
                            });
                        }
                    }
                }
                if item.bytes >= BIG_FILE_BYTES {
                    big.push(FoundFile {
                        path: item.path.clone(),
                        bytes: item.bytes,
                        modified: item.modified,
                        reason: "大文件".to_string(),
                    });
                }
            }
            if big.len() > BIG_FILE_LIMIT * 4 {
                big.sort_by(|a, b| b.bytes.cmp(&a.bytes));
                big.truncate(BIG_FILE_LIMIT);
            }

            if let Some(progress) = &progress {
                if last_progress.elapsed() >= Duration::from_millis(400) {
                    last_progress = Instant::now();
                    progress(serde_json::json!({
                        "phase": "drive",
                        "done": scanned,
                        "total": MAX_DIRS,
                        "path": dir.display().to_string(),
                        "artifacts": report.artifacts.len(),
                        "stale": stale_total,
                        "big": big.len(),
                    }));
                }
            }
        },
    );

    big.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    big.truncate(BIG_FILE_LIMIT);
    stale.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    report.artifacts.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    report.stale = stale;
    report.big = big;
    report.dirs_scanned = budget.dirs as u64;
    report.truncated = budget.exhausted();
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

struct DirItem {
    name: String,
    path: String,
    is_dir: bool,
    bytes: u64,
    modified: u64,
}

/// 深度优先遍历。回调拿到「当前目录 + 子项摘要」，一次列举同时喂给
/// 空目录判定、marker 判定与单文件判定——避免为每个功能各 stat 一遍。
fn walk(
    dir: &Path,
    drive: &str,
    budget: &mut Budget,
    on_dir: &mut dyn FnMut(&Path, &[DirItem], u64),
) {
    if budget.exhausted() {
        return;
    }
    budget.dirs += 1;

    let Ok(reader) = fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<DirItem> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();

    for entry in reader.flatten() {
        if budget.exhausted() {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();

        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        // 不跟随链接/联接点：盘内的 junction 能把遍历与删除引到别处。
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() && SKIP_DIRS.iter().any(|skip| lower == *skip) {
            // 跳过整棵，但要把它本身作为条目交给上层（产物目录要能识别）。
            items.push(DirItem {
                name,
                path: path.display().to_string(),
                is_dir: true,
                bytes: 0,
                modified: modified_secs(&path),
            });
            continue;
        }
        if !is_allowed(&path, drive) {
            continue;
        }
        if meta.is_dir() {
            dirs.push(path.clone());
            items.push(DirItem {
                name,
                path: path.display().to_string(),
                is_dir: true,
                bytes: 0,
                modified: modified_secs(&path),
            });
        } else {
            items.push(DirItem {
                name,
                path: path.display().to_string(),
                is_dir: false,
                bytes: meta.len(),
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0),
            });
        }
    }

    on_dir(dir, &items, 0);

    for child in dirs {
        walk(&child, drive, budget, on_dir);
    }
}

/// marker 判定：`*.csproj` 这类通配要列目录，普通名直接 exists。
fn marker_present(dir: &Path, marker: &str) -> bool {
    if let Some(ext) = marker.strip_prefix("*.") {
        let Ok(reader) = fs::read_dir(dir) else {
            return false;
        };
        return reader.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(&format!(".{ext}"))
        });
    }
    dir.join(marker).is_file()
}

fn tree_size(dir: &Path, budget: &mut Budget) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        if budget.exhausted() {
            break;
        }
        budget.dirs += 1;
        let Ok(reader) = fs::read_dir(&current) else {
            continue;
        };
        for entry in reader.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

fn modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 盘内路径准入：必须在所选盘内、深度 ≥ 2、不在禁区。
fn is_allowed(path: &Path, drive: &str) -> bool {
    let text = path.display().to_string().replace('/', "\\").to_ascii_lowercase();
    if !text.starts_with(&format!("{}\\", drive.to_ascii_lowercase())) {
        return false;
    }
    // 盘根本身与一级目录不删（`C:\Users` 这种误删是灾难性的）。
    let depth = text
        .trim_start_matches(&format!("{}\\", drive.to_ascii_lowercase()))
        .split('\\')
        .filter(|part| !part.is_empty())
        .count();
    if depth < 2 {
        return false;
    }
    !FORBIDDEN
        .iter()
        .any(|root| text == *root || text.starts_with(&format!("{root}\\")))
}

// ── 删除 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveResult {
    pub freed_bytes: u64,
    pub removed: u64,
    pub failed: u64,
    pub messages: Vec<String>,
}

/// 删除用户勾选的条目。**服务端逐条复核**，不信任前端传来的路径：
/// - 产物目录：目录名在 ARTIFACTS 里，父目录仍有 marker，盘内、非链接、非禁区；
/// - 大文件 / 残留文件：必须是文件、在盘内、非禁区。
pub fn remove_entries(kind: &str, paths: &[String], drive: &str) -> RemoveResult {
    let mut result = RemoveResult {
        freed_bytes: 0,
        removed: 0,
        failed: 0,
        messages: Vec::new(),
    };

    for raw in paths {
        let path = PathBuf::from(raw);
        if !is_allowed(&path, drive) {
            result.failed += 1;
            push_message(&mut result, format!("已拒绝（不在所选盘内或触碰系统目录）：{raw}"));
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            result.failed += 1;
            continue;
        };
        if meta.file_type().is_symlink() {
            result.failed += 1;
            push_message(&mut result, format!("已跳过符号链接：{raw}"));
            continue;
        }

        match kind {
            "artifact" => {
                if !meta.is_dir() || !artifact_allowed(&path) {
                    result.failed += 1;
                    push_message(&mut result, format!("已拒绝（不是可清理的构建产物）：{raw}"));
                    continue;
                }
                match remove_tree(&path) {
                    Ok((bytes, files)) => {
                        result.freed_bytes += bytes;
                        result.removed += files;
                    }
                    Err(error) => {
                        result.failed += 1;
                        push_message(&mut result, format!("{raw}：{error}"));
                    }
                }
            }
            // 空目录：只 `remove_dir`（非空必失败），绝不递归。
            "empty-dir" => {
                if !meta.is_dir() {
                    result.failed += 1;
                    continue;
                }
                match fs::remove_dir(&path) {
                    Ok(()) => result.removed += 1,
                    Err(_) => result.failed += 1,
                }
            }
            _ => {
                if meta.is_dir() {
                    result.failed += 1;
                    continue;
                }
                let bytes = meta.len();
                match fs::remove_file(&path) {
                    Ok(()) => {
                        result.freed_bytes += bytes;
                        result.removed += 1;
                    }
                    Err(error) => {
                        result.failed += 1;
                        push_message(&mut result, format!("{raw}：{error}"));
                    }
                }
            }
        }
    }

    result
}

/// 产物目录的二次校验：名字在白名单里，且父目录仍有对应 marker。
fn artifact_allowed(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let Some((_, markers)) = ARTIFACTS.iter().find(|(artifact, _)| lower == *artifact) else {
        return false;
    };
    let Some(parent) = dir.parent() else {
        return false;
    };
    markers.iter().any(|marker| marker_present(parent, marker))
}

/// 递归删除，但**不跟随** reparse point：遇到链接只删链接本身。
/// 不用 `remove_dir_all`——它对链接的处理随版本而变，这里自己控制。
fn remove_tree(dir: &Path) -> Result<(u64, u64), String> {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    let mut dirs: Vec<PathBuf> = Vec::new();

    while let Some(current) = stack.pop() {
        let reader = fs::read_dir(&current).map_err(|error| format!("读取失败：{error}"))?;
        dirs.push(current.clone());
        for entry in reader.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                // 链接本身可以删，但绝不进去。
                let _ = fs::remove_file(&path);
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else {
                let size = meta.len();
                match fs::remove_file(&path) {
                    Ok(()) => {
                        bytes += size;
                        files += 1;
                    }
                    Err(_) => { /* 被占用：留给调用方统计 */ }
                }
            }
        }
    }

    // 自底向上删空目录（顺序反转 = 深的先删）。
    for dir in dirs.iter().rev() {
        let _ = fs::remove_dir(dir);
    }

    if dir.exists() {
        return Err("目录仍有文件被占用，已删除其余部分".to_string());
    }
    Ok((bytes, files))
}

fn push_message(result: &mut RemoveResult, message: String) {
    if result.messages.len() < 20 {
        result.messages.push(message);
    }
}

// ── Tauri 命令 ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn cleanup_scan_drive(
    app: tauri::AppHandle,
    drive: String,
) -> Result<DriveScanReport, String> {
    use tauri::Emitter;
    let normalized = drive_of(&drive);
    if normalized.len() != 2 || !normalized.ends_with(':') {
        return Err(format!("盘符无效：{drive}"));
    }
    let progress: crate::cleanup::Progress = std::sync::Arc::new(move |payload| {
        let _ = app.emit("cleanup-progress", payload);
    });
    tauri::async_runtime::spawn_blocking(move || scan_drive(&normalized, Some(progress)))
        .await
        .map_err(|error| format!("盘内扫描失败：{error}"))
}

#[tauri::command]
pub async fn cleanup_remove_entries(
    kind: String,
    paths: Vec<String>,
    drive: String,
) -> Result<RemoveResult, String> {
    let normalized = drive_of(&drive);
    tauri::async_runtime::spawn_blocking(move || remove_entries(&kind, &paths, &normalized))
        .await
        .map_err(|error| format!("清理失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_drive_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hermix-drive-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// marker 判定 + 产物识别：`package.json` 旁边出现 node_modules 才算项目产物。
    #[test]
    fn detects_artifacts_by_marker() {
        let root = temp_drive_dir("artifact");
        let project = root.join("web");
        fs::create_dir_all(project.join("node_modules")).unwrap();
        fs::write(project.join("package.json"), b"{}").unwrap();
        fs::write(project.join("node_modules").join("a.js"), vec![0u8; 100]).unwrap();

        // 没有 marker 的同名目录不应被认成产物。
        let loose = root.join("loose");
        fs::create_dir_all(loose.join("node_modules")).unwrap();

        assert!(artifact_allowed(&project.join("node_modules")));
        assert!(!artifact_allowed(&loose.join("node_modules")));

        let _ = fs::remove_dir_all(&root);
    }

    /// 删除产物目录：递归清空并按文件名统计释放量；链接不跟随。
    #[test]
    fn removes_artifact_tree_and_reports_bytes() {
        let root = temp_drive_dir("remove");
        let target = root.join("target");
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(target.join("debug").join("app.exe"), vec![0u8; 2048]).unwrap();

        let (bytes, files) = remove_tree(&target).expect("remove");
        assert_eq!(files, 1);
        assert_eq!(bytes, 2048);
        assert!(!target.exists());
        assert!(root.exists(), "只删产物目录，不动项目根");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_paths_outside_drive_and_shallow_dirs() {
        assert!(!is_allowed(Path::new(r"C:\Users"), "C:"));
        assert!(!is_allowed(Path::new(r"C:\Windows\Temp\x.tmp"), "C:"));
        assert!(!is_allowed(Path::new(r"D:\a\b"), "C:"));
        assert!(is_allowed(Path::new(r"D:\code\app\target"), "D:"));
    }

    #[test]
    fn rejects_artifact_without_marker_on_remove() {
        let root = temp_drive_dir("guard");
        let artifacts = root.join("target");
        fs::create_dir_all(&artifacts).unwrap();
        let result = remove_entries(
            "artifact",
            &[artifacts.display().to_string()],
            &drive_of(&root.display().to_string()),
        );
        assert_eq!(result.removed, 0);
        assert!(artifacts.exists(), "没有 marker 的 target 不允许删");
        let _ = fs::remove_dir_all(&root);
    }
}
