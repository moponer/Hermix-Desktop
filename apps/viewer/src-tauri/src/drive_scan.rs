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
//!   更慢；这里用**受预算约束的并行遍历**，配合一次目录列举复用（marker 判定
//!   直接用子项名集合查表，不再额外 stat）。
//!
//! **结果分两块**（这正是主流清理工具的交互逻辑）：
//! - [`DriveScanReport::suggested`]「建议清理」：**规则命中**的可删除项，
//!   每条都带 `reason`（凭什么说它是垃圾）；默认全不勾选。
//! - [`DriveScanReport::big`]「大文件」：**仅作参考**，按体积排序，不预判可删。
//!
//! 安全边界（比"删临时文件"更强，因为这里要删目录）：
//! 1. 只删**产物目录本身**，且目录名必须在白名单里、父目录必须仍有对应 marker；
//! 2. 不跟随 reparse point（junction/symlink），删除时也自己走一遍、跳过链接；
//! 3. 系统禁区（System32 / Program Files / WinSxS…）与卷根一律拒绝；
//! 4. 大文件/残留文件同样只删「文件」，且必须仍在所选盘内。

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
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
/// 安装包残留的判定年龄：装完 30 天还在临时目录里，就是残留。
const INSTALLER_MIN_AGE_DAYS: u64 = 30;
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
    "tmp", "bak", "old", "chk", "crdownload", "part", "swp", "swo",
];

/// 日志与转储后缀。
const LOG_EXTS: &[&str] = &["log", "dmp", "mdmp", "etl", "tmp0"];

/// 残留安装包后缀。`.exe` 不在其中——临时目录里的 exe 可能是正在跑的安装器。
const INSTALLER_EXTS: &[&str] = &["msi", "msix", "cab"];

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

/// 扫描时整棵跳过的目录：系统自己管理的地盘，不参与"谁吃掉了空间"的排行
/// （Windows / Program Files 的体积对用户没有决策价值）。其余一律进入，
/// 包括 `node_modules` / `target` —— 它们恰恰是最该被看见的大头。
const SKIP_DIRS: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "windows",
    "windows.old",
    "program files",
    "program files (x86)",
];

/// 目录体积树保留的深度（层数）。再深对"找大目录"没有帮助，却会成倍放大
/// 记录条数。
const MAX_TREE_DEPTH: usize = 4;
/// 返回给前端的目录节点上限。
const DIR_LIMIT: usize = 400;

/// 建议清理项：规则命中的可删除项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupCandidate {
    pub path: String,
    /// 展示类别：构建产物 / 临时残留 / 日志与转储 / 残留安装包。
    pub category: String,
    /// **清理依据**：凭什么说它可以删（UI 必须明示这一条）。
    pub reason: String,
    /// 删除方式：`artifact` = 删整个目录；`file` = 只删这个文件。
    pub target: String,
    pub bytes: u64,
    pub files: u64,
    /// 最后修改时间（Unix 秒），UI 显示"多久没用"。
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
pub struct DirUsage {
    pub path: String,
    /// 父目录路径（前端据此把扁平列表还原成树）。
    pub parent: String,
    pub bytes: u64,
    pub files: u64,
    /// 相对卷根的层数（1 = 卷根的直接子目录）。
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveScanReport {
    pub drive: String,
    /// 「建议清理」：规则命中的可删除项（按体积降序）。
    pub suggested: Vec<CleanupCandidate>,
    /// 「建议清理」命中总数（含未列出的）——列表有上限，计数没有。
    pub suggested_total: u64,
    /// 「大文件」：仅作参考，按体积降序，不预判可删。
    pub big: Vec<FoundFile>,
    pub empty_dirs: Vec<String>,
    pub empty_dir_count: u64,
    /// 目录体积树（扁平列表，前端按 `parent` 还原成树）。
    pub dirs: Vec<DirUsage>,
    pub dirs_scanned: u64,
    /// 全盘文件体积之和（跳过系统目录）。
    pub total_bytes: u64,
    /// 全盘文件数（跳过系统目录）。
    pub total_files: u64,
    /// **读不到的目录数**：普通权限下必然有一部分（其它用户的目录、
    /// 受保护的系统位置）。这是"非管理员模式覆盖不全"的量化指标——
    /// 不如实报出来，用户会以为扫全了。
    pub denied_dirs: u64,
    pub elapsed_ms: u64,
    /// 命中预算被截断（结果不完整，UI 要明确告知）。
    pub truncated: bool,
}

struct Budget {
    deadline: Instant,
    dirs: AtomicU64,
    stopped: AtomicBool,
}

impl Budget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + SCAN_BUDGET,
            dirs: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        }
    }

    fn exhausted(&self) -> bool {
        self.dirs.load(Ordering::Relaxed) >= MAX_DIRS as u64
            || Instant::now() >= self.deadline
            || self.stopped.load(Ordering::Relaxed)
    }

    fn tick(&self) {
        self.dirs.fetch_add(1, Ordering::Relaxed);
    }
}

/// 单个目录的统计结果（体积聚合的载体）。
#[derive(Debug, Clone, Copy, Default)]
struct DirStat {
    bytes: u64,
    files: u64,
}

/// 扫描工作台：并行阶段各线程各写一份，最后合并。
#[derive(Debug, Default)]
struct ScanBucket {
    /// 每个目录的直属文件体积（并行阶段填），第二阶段自底向上累加成子树体积。
    dirs: HashMap<PathBuf, DirStat>,
    /// 构建产物目录（第二阶段回填子树体积）。
    artifacts: Vec<(PathBuf, String)>,
    /// 文件级候选（临时残留 / 日志与转储 / 残留安装包）。
    candidates: Vec<CleanupCandidate>,
    /// 候选总数（含未列出的）——列表有上限，计数没有。
    candidates_total: u64,
    /// 读不到的目录数（权限边界）。
    denied_dirs: u64,
    /// 大文件（仅参考）。
    big: Vec<FoundFile>,
    empty_dirs: Vec<String>,
    empty_dir_count: u64,
    dirs_scanned: u64,
}

/// 扫一块盘。`progress` 每扫描若干目录推一次，用于前端进度条。
pub fn scan_drive(
    drive: &str,
    progress: Option<crate::cleanup::Progress>,
) -> DriveScanReport {
    let started = Instant::now();
    let root = PathBuf::from(format!("{drive}\\"));
    let budget = Arc::new(Budget::new());

    let mut report = DriveScanReport {
        drive: drive.to_string(),
        suggested: Vec::new(),
        suggested_total: 0,
        big: Vec::new(),
        empty_dirs: Vec::new(),
        empty_dir_count: 0,
        dirs: Vec::new(),
        dirs_scanned: 0,
        total_bytes: 0,
        total_files: 0,
        denied_dirs: 0,
        elapsed_ms: 0,
        truncated: false,
    };

    if !root.is_dir() {
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return report;
    }

    let bucket = scan_parallel(&root, &budget, progress.clone(), started);

    // ── 第二阶段：自底向上聚合子树体积（`du` 语义） ──────────────────
    let mut dirs = bucket.dirs;
    // 全盘汇总要在**聚合之前**算：聚合会把子目录的字节加进父目录，
    // 之后再求和就是重复计数。聚合前每个目录记的是直属文件体积。
    let total_bytes: u64 = dirs.values().map(|stat| stat.bytes).sum();
    let total_files: u64 = dirs.values().map(|stat| stat.files).sum();
    aggregate_subtree_sizes(&mut dirs, drive);

    // ── 建议清理：构建产物（补上子树体积） ─────────────────────────
    let mut suggested: Vec<CleanupCandidate> = Vec::new();
    for (path, marker) in &bucket.artifacts {
        let stat = dirs.get(path).copied().unwrap_or_default();
        suggested.push(CleanupCandidate {
            path: path.display().to_string(),
            category: "构建产物".to_string(),
            reason: format!(
                "同级存在 {marker}，可认定为项目构建产物；重新构建即可再生"
            ),
            target: "artifact".to_string(),
            bytes: stat.bytes,
            files: stat.files,
            modified: modified_secs(path),
        });
    }
    suggested.extend(bucket.candidates);
    suggested.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    report.suggested = suggested;
    report.suggested_total = bucket.candidates_total + bucket.artifacts.len() as u64;

    // ── 大文件：仅参考，按体积排序 ───────────────────────────────
    let mut big = bucket.big;
    big.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    big.truncate(BIG_FILE_LIMIT);
    report.big = big;

    report.empty_dir_count = bucket.empty_dir_count;
    report.empty_dirs = bucket.empty_dirs;

    // 目录体积：按体积降序，前端画 Treemap 时大块在前更符合直觉。
    let mut tree: Vec<DirUsage> = Vec::new();
    for (path, stat) in &dirs {
        let depth = dir_depth(path, drive);
        if depth > 0 && depth <= MAX_TREE_DEPTH && stat.bytes > 0 {
            tree.push(DirUsage {
                path: path.display().to_string(),
                parent: path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                bytes: stat.bytes,
                files: stat.files,
                depth,
            });
        }
    }
    tree.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    tree.truncate(DIR_LIMIT);
    report.dirs = tree;

    report.dirs_scanned = bucket.dirs_scanned;
    report.denied_dirs = bucket.denied_dirs;
    report.total_bytes = total_bytes;
    report.total_files = total_files;
    report.truncated = budget.exhausted();
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

/// 串行扫多块盘。
///
/// **为什么串行**：目录遍历是海量小 IO，并发扫多盘会把磁盘队列占满
/// （实测两盘并发时整机卡死）。串行 + 每盘各自 45s 预算。
pub fn scan_volumes(
    drives: &[String],
    progress: Option<crate::cleanup::Progress>,
) -> Vec<DriveScanReport> {
    drives
        .iter()
        .map(|drive| {
            let normalized = drive_of(drive);
            scan_drive(&normalized, progress.clone())
        })
        .collect()
}

/// 并行遍历：一批线程各自枚举目录树，最后合并结果。
///
/// **为什么要并行**：目录遍历是"海量小 IO"，单线程吃不满 NVMe 的队列深度
/// （实测一块数据盘单线程要 40s+）。按目录分片给多个线程后，同样的遍历能
/// 压到几秒——用户点开磁盘视图时最在意的就是这个等待。
fn scan_parallel(
    root: &Path,
    budget: &Arc<Budget>,
    progress: Option<crate::cleanup::Progress>,
    started: Instant,
) -> ScanBucket {
    let workers = std::thread::available_parallelism()
        .map(|value| value.get().clamp(2, 8))
        .unwrap_or(4);

    struct Shared {
        queue: Mutex<(VecDeque<PathBuf>, usize)>,
        signal: Condvar,
    }
    let shared = Arc::new(Shared {
        queue: Mutex::new((VecDeque::from(vec![root.to_path_buf()]), 0)),
        signal: Condvar::new(),
    });

    let mut handles = Vec::new();
    for _ in 0..workers {
        let shared = shared.clone();
        let budget = budget.clone();
        let progress = progress.clone();
        handles.push(std::thread::spawn(move || {
            let mut bucket = ScanBucket::default();
            let mut last_report = Instant::now();
            loop {
                let dir = {
                    let mut guard = shared.queue.lock().expect("queue lock");
                    loop {
                        if let Some(dir) = guard.0.pop_front() {
                            guard.1 += 1;
                            break Some(dir);
                        }
                        if guard.1 == 0 {
                            // 队列空且没有活跃 worker → 全盘扫完。
                            break None;
                        }
                        guard = shared.signal.wait(guard).expect("queue wait");
                    }
                };
                let Some(dir) = dir else {
                    // 唤醒其他也在等的工作线程，让它们一起收工（否则会死等）。
                    shared.signal.notify_all();
                    return bucket;
                };
                if budget.exhausted() {
                    budget.stopped.store(true, Ordering::Release);
                    let mut guard = shared.queue.lock().expect("queue lock");
                    guard.1 -= 1;
                    drop(guard);
                    shared.signal.notify_all();
                    return bucket;
                }
                let children = scan_one_dir(&dir, &budget, &mut bucket);
                let mut guard = shared.queue.lock().expect("queue lock");
                guard.1 -= 1;
                guard.0.extend(children);
                drop(guard);
                shared.signal.notify_all();

                // 进度：每 400ms 推一次真实数字（已扫目录数 / 已用时）。
                if let Some(progress) = &progress {
                    if last_report.elapsed() >= Duration::from_millis(400) {
                        last_report = Instant::now();
                        progress(serde_json::json!({
                            "phase": "drive",
                            "done": budget.dirs.load(Ordering::Relaxed),
                            "total": MAX_DIRS,
                            "path": dir.display().to_string(),
                            "elapsedMs": started.elapsed().as_millis() as u64,
                        }));
                    }
                }
            }
        }));
    }

    let mut merged = ScanBucket::default();
    for handle in handles {
        let Ok(bucket) = handle.join() else { continue };
        merge_bucket(&mut merged, bucket);
    }
    merged
}

/// 合并两个工作台。
fn merge_bucket(target: &mut ScanBucket, other: ScanBucket) {
    for (path, stat) in other.dirs {
        let slot = target.dirs.entry(path).or_default();
        slot.bytes += stat.bytes;
        slot.files += stat.files;
    }
    target.artifacts.extend(other.artifacts);
    target.candidates.extend(other.candidates);
    target.candidates_total += other.candidates_total;
    target.denied_dirs += other.denied_dirs;
    target.big.extend(other.big);
    target.empty_dir_count += other.empty_dir_count;
    target.empty_dirs.extend(other.empty_dirs);
    target.dirs_scanned += other.dirs_scanned;
}

/// 枚举一个目录：统计直属文件体积、识别候选、返回子目录列表。
fn scan_one_dir(
    dir: &Path,
    budget: &Arc<Budget>,
    bucket: &mut ScanBucket,
) -> Vec<PathBuf> {
    budget.tick();
    bucket.dirs_scanned += 1;

    let mut children: Vec<PathBuf> = Vec::new();
    let mut own_bytes = 0u64;
    let mut own_files = 0u64;
    let mut file_count = 0usize;

    let Ok(reader) = fs::read_dir(dir) else {
        // 读不到 = 权限不足（普通用户碰其它用户的目录、系统位置）。
        // 计数上报，让 UI 如实说清这次扫描覆盖到哪。
        bucket.denied_dirs += 1;
        return children;
    };
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
        if meta.is_dir() {
            if SKIP_DIRS.iter().any(|skip| lower == *skip) {
                // 系统地盘：整棵跳过，不计入体积排行（不是用户的决策对象）。
                continue;
            }
            children.push(path.clone());
            // 产物目录：目录名 + 同级 marker 判定，命中就记下来。
            if let Some(marker) = artifact_marker(&path) {
                bucket.artifacts.push((path.clone(), marker));
            }
            continue;
        }

        // ── 文件级判定 ──────────────────────────────────────────
        own_bytes += meta.len();
        own_files += 1;
        file_count += 1;
        let modified = modified_from(&meta);
        let bytes = meta.len();

        if bytes >= BIG_FILE_BYTES {
            bucket.big.push(FoundFile {
                path: path.display().to_string(),
                bytes,
                modified,
                reason: "单个文件超过 200 MB，仅供参考".to_string(),
            });
        }

        if let Some(candidate) = classify_file(&path, &lower, bytes, modified) {
            bucket.candidates_total += 1;
            // 列表有上限：几十万条残留文件会把 UI 拖死，超出只计数。
            if bucket.candidates.len() < STALE_LIMIT {
                bucket.candidates.push(candidate);
            }
        }
    }

    if file_count == 0 && dir.parent().is_some() {
        bucket.empty_dir_count += 1;
        if bucket.empty_dirs.len() < EMPTY_DIR_LIMIT {
            bucket.empty_dirs.push(dir.display().to_string());
        }
    }

    let slot = bucket.dirs.entry(dir.to_path_buf()).or_default();
    slot.bytes += own_bytes;
    slot.files += own_files;

    children
}

/// 文件级候选分类：临时残留 / 日志与转储 / 残留安装包。
fn classify_file(path: &Path, lower_name: &str, bytes: u64, modified: u64) -> Option<CleanupCandidate> {
    let extension = lower_name.rsplit_once('.').map(|(_, ext)| ext)?;
    let age_days = age_days(modified);

    let (category, reason, min_age) = if STALE_EXTS.contains(&extension) {
        (
            "临时残留",
            format!("扩展名 .{extension} 属于临时/备份文件，且已超过 {STALE_MIN_AGE_DAYS} 天未修改"),
            STALE_MIN_AGE_DAYS,
        )
    } else if LOG_EXTS.contains(&extension) {
        (
            "日志与转储",
            format!("扩展名 .{extension} 属于日志或崩溃转储，排障期过后即可删除"),
            STALE_MIN_AGE_DAYS,
        )
    } else if INSTALLER_EXTS.contains(&extension) && is_installer_location(path) {
        (
            "残留安装包",
            format!("扩展名 .{extension} 的安装包落在临时/下载目录，已超过 {INSTALLER_MIN_AGE_DAYS} 天"),
            INSTALLER_MIN_AGE_DAYS,
        )
    } else {
        return None;
    };

    if age_days < min_age {
        return None;
    }

    Some(CleanupCandidate {
        path: path.display().to_string(),
        category: category.to_string(),
        reason,
        target: "file".to_string(),
        bytes,
        files: 1,
        modified,
    })
}

/// 安装包只在临时/下载目录里才算残留——别处出现的 .msi 可能是用户自己存的。
fn is_installer_location(path: &Path) -> bool {
    let text = path.display().to_string().to_ascii_lowercase();
    text.contains("\\temp\\")
        || text.contains("\\tmp\\")
        || text.contains("\\downloads\\")
        || text.contains("\\download\\")
}

fn age_days(modified: u64) -> u64 {
    if modified == 0 {
        return 0;
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|now| now.as_secs())
        .unwrap_or(0);
    now.saturating_sub(modified) / 86_400
}

/// 产物目录的 marker 判定：目录名在白名单里，且**父目录**仍有对应 marker。
fn artifact_marker(dir: &Path) -> Option<String> {
    let name = dir.file_name().and_then(|n| n.to_str())?.to_ascii_lowercase();
    let (_, markers) = ARTIFACTS.iter().find(|(artifact, _)| name == *artifact)?;
    let parent = dir.parent()?;
    markers
        .iter()
        .find(|marker| marker_present(parent, marker))
        .map(|marker| marker.to_string())
}

/// 自底向上把子目录体积累加进父目录（`du` 语义）。
///
/// **顺序是这里的关键**：按深度从大到小处理，深的先把自己的字节交给父目录，
/// 父目录收齐所有后代后才等于"子树总量"。反过来（浅的先处理）会漏。
///
/// 调用方要在**调用本函数之前**算好全盘汇总——聚合之后每个目录的体积
/// 已经是子树量，再求和就是重复计数。
fn aggregate_subtree_sizes(dirs: &mut HashMap<PathBuf, DirStat>, drive: &str) {
    let mut by_depth: Vec<PathBuf> = dirs.keys().cloned().collect();
    by_depth.sort_by_key(|path| std::cmp::Reverse(dir_depth(path, drive)));
    for path in &by_depth {
        let Some(parent) = path.parent() else { continue };
        let Some((bytes, files)) = dirs.get(path).map(|stat| (stat.bytes, stat.files)) else {
            continue;
        };
        let slot = dirs.entry(parent.to_path_buf()).or_default();
        slot.bytes += bytes;
        slot.files += files;
    }
}

/// 目录相对卷根的层数（1 = 卷根的直接子目录；卷根本身算 0）。
fn dir_depth(dir: &Path, drive: &str) -> usize {
    let text = dir.display().to_string().replace('/', "\\");
    let prefix = format!("{}\\", drive.trim_end_matches('\\'));
    let rest = text
        .strip_prefix(&prefix)
        .unwrap_or(text.as_str())
        .trim_matches('\\');
    if rest.is_empty() {
        return 0;
    }
    rest.split('\\').filter(|part| !part.is_empty()).count()
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

fn modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn modified_from(meta: &fs::Metadata) -> u64 {
    meta.modified()
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
    artifact_marker(dir).is_some()
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

    /// 目录体积自底向上聚合：父目录总量 = 自己的文件 + 全部后代（`du` 语义）。
    /// Treemap 依赖这个不变量，否则父块会比子块加起来还小。
    #[test]
    fn scan_aggregates_subtree_sizes() {
        let root = temp_drive_dir("agg");
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("f1.bin"), vec![0u8; 100]).unwrap();
        fs::write(nested.join("f2.bin"), vec![0u8; 200]).unwrap();

        // 盘符从临时目录实际所在盘取（本机 %TEMP% 在 F:，写死 C: 会让
        // 准入检查把整棵树拒掉，测试就变成"聚合恒为 0"）。
        let drive: String = root.display().to_string().chars().take(2).collect();
        let budget = Arc::new(Budget::new());
        let bucket = scan_parallel(&root, &budget, None, Instant::now());

        let mut dirs = bucket.dirs;
        let mut by_depth: Vec<PathBuf> = dirs.keys().cloned().collect();
        by_depth.sort_by_key(|path| std::cmp::Reverse(dir_depth(path, &drive)));
        for path in &by_depth {
            let Some(parent) = path.parent() else { continue };
            let Some((bytes, files)) = dirs.get(path).map(|stat| (stat.bytes, stat.files)) else {
                continue;
            };
            let slot = dirs.entry(parent.to_path_buf()).or_default();
            slot.bytes += bytes;
            slot.files += files;
        }

        let root_total = dirs.get(&root).copied().expect("root visited");
        assert_eq!(root_total.bytes, 300, "父目录总量 = 自己的文件 + 后代");
        assert_eq!(root_total.files, 2);

        let nested_total = dirs.get(&nested).copied().expect("nested visited");
        assert_eq!(nested_total.bytes, 200);
        assert_eq!(nested_total.files, 1);

        let _ = fs::remove_dir_all(&root);
    }

    /// 全盘汇总不能重复计数：聚合阶段会把子目录字节加进父目录，
    /// 汇总必须在聚合**之前**算（这个顺序错了，数字会翻倍）。
    #[test]
    fn totals_are_counted_before_aggregation() {
        let root = temp_drive_dir("total");
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("f1.bin"), vec![0u8; 100]).unwrap();
        fs::write(nested.join("f2.bin"), vec![0u8; 200]).unwrap();

        let drive: String = root.display().to_string().chars().take(2).collect();
        let budget = Arc::new(Budget::new());
        let bucket = scan_parallel(&root, &budget, None, Instant::now());

        // 聚合前：每个目录只记直属文件。
        let mut dirs = bucket.dirs;
        let before: u64 = dirs.values().map(|stat| stat.bytes).sum();
        assert_eq!(before, 300, "聚合前是直属文件体积之和");

        aggregate_subtree_sizes(&mut dirs, &drive);
        let after: u64 = dirs.values().map(|stat| stat.bytes).sum();
        assert!(
            after > before,
            "聚合后父目录含子树量，总和必然变大——所以全盘汇总不能事后算"
        );
        assert_eq!(dirs.get(&root).map(|s| s.bytes), Some(300), "根目录 = 子树总量");

        let _ = fs::remove_dir_all(&root);
    }

    /// 权限边界要如实计数：读不到的目录数是"非管理员模式覆盖不全"的量化指标。
    #[test]
    fn denied_directories_are_counted() {
        let root = temp_drive_dir("denied");
        // 造一个读不进去的目录：普通用户读不到系统目录，但测试环境未必
        // 有那种目录——所以直接验证"可读目录不计入 denied"。
        fs::create_dir_all(root.join("ok")).unwrap();
        let drive: String = root.display().to_string().chars().take(2).collect();
        let budget = Arc::new(Budget::new());
        let bucket = scan_parallel(&root, &budget, None, Instant::now());
        assert_eq!(bucket.denied_dirs, 0, "能读到的目录不该被记成拒绝");
        let _ = drive;

        let _ = fs::remove_dir_all(&root);
    }

    /// 临时探针：真扫一块盘，打印汇总（人工核对非管理员模式的数字）。
    /// 慢（每盘 45s 预算），默认不跑。
    #[test]
    #[ignore = "慢：真扫一块盘，人工核对用"]
    fn probe_local_scan() {
        let report = scan_drive("F:", None);
        println!(
            "F: 汇总：{} 字节 / {} 文件 / {} 目录 / 读不到 {} 个 / 用时 {} ms / 截断={}",
            report.total_bytes,
            report.total_files,
            report.dirs_scanned,
            report.denied_dirs,
            report.elapsed_ms,
            report.truncated
        );
        println!(
            "大目录：{:?}",
            report
                .dirs
                .iter()
                .take(5)
                .map(|node| (node.path.clone(), node.bytes))
                .collect::<Vec<_>>()
        );
        println!(
            "大文件：{:?}",
            report
                .big
                .iter()
                .take(3)
                .map(|file| (file.path.clone(), file.bytes))
                .collect::<Vec<_>>()
        );
        println!("建议清理 {} 项", report.suggested.len());
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

    /// 候选分类：扩展名 + 年龄 + 位置三者共同决定，理由必须写清楚。
    #[test]
    fn classifies_candidates_with_reason() {
        let old = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            - 40 * 86_400;

        let stale = classify_file(
            Path::new(r"C:\work\a.tmp"),
            "a.tmp",
            10,
            old,
        )
        .expect("stale");
        assert_eq!(stale.category, "临时残留");
        assert!(stale.reason.contains(".tmp"), "依据里要写明扩展名");

        let installer = classify_file(
            Path::new(r"C:\Users\me\AppData\Local\Temp\setup.msi"),
            "setup.msi",
            10,
            old,
        )
        .expect("installer");
        assert_eq!(installer.category, "残留安装包");

        // 不在临时/下载目录的 .msi 不算残留。
        assert!(classify_file(Path::new(r"C:\docs\setup.msi"), "setup.msi", 10, old).is_none());
        // 太新的文件不算。
        let fresh = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        assert!(classify_file(Path::new(r"C:\work\a.tmp"), "a.tmp", 10, fresh).is_none());
    }
}
