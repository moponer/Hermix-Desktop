//! 垃圾清理引擎 —— 规则驱动（照抄 BleachBit / CCleaner 的模型，不自己发明）。
//!
//! CleanerML 的核心是：**清理项是数据，不是代码**。每条规则声明一组路径
//! （支持环境变量与通配）、一个动作（delete/truncate）、以及白名单；引擎
//! 只删"匹配到的东西"，不递归删目录、不碰白名单。这里同样：
//!
//! - 规则表 [`RULES`]：路径模板 + 最小年龄 + 是否需要管理员 + 是否危险；
//!   每条规则还带 **分类**（`group`）、**清理依据**（`hint`）与
//!   **规则出处**（`source`，开源项目名）——UI 必须明示"凭什么说它是垃圾"；
//! - 扫描：展开 `%VAR%` → **逐段展开通配** → 递归枚举（不跟随 symlink/junction）；
//! - 清理：**重新扫描**（绝不信任前端传来的路径），逐文件删除，失败的跳过
//!   并计数，最后尽力清掉空目录；
//! - 回收站不走文件删除，调 Shell 的 `SHEmptyRecycleBinW`（BleachBit 在
//!   Windows 上同样走 shell，手删 `$RECYCLE.BIN` 会破坏回收站元数据）。
//!
//! **逐段展开通配**是这一版修掉的真 bug：旧实现只在**最后一段**处理 `*`，
//! 于是 `%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache\*` 这种
//! "中间段带通配"的规则展开成了一个字面路径，`read_dir` 直接失败——
//! 浏览器缓存那条规则其实一直是 0 字节。现在中间段会真的去父目录里枚举。
//!
//! 安全红线：
//! 1. 只允许删除 [`RULES`] 展开出的目标（前缀校验，大小写不敏感）；
//! 2. [`FORBIDDEN_ROOTS`] 里的系统目录即使规则写错也拒绝；
//! 3. 符号链接/联接点一律跳过（`symlink_metadata`，不解引用）；
//! 4. 目录本身永不删除，只删文件 + 移除已空子目录。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use crate::sysmon::expand_env;

/// 规则作用域里的"绝对禁区"：即使某条规则的通配把这里纳入了，也拒绝删除。
const FORBIDDEN_ROOTS: &[&str] = &[
    r"c:\windows\system32",
    r"c:\windows\syswow64",
    r"c:\windows\winsxs",
    r"c:\windows\boot",
    r"c:\windows\servicing",
    r"c:\program files",
    r"c:\program files (x86)",
    r"c:\programdata\microsoft\windows\system32",
];

/// 单次扫描的预算：条目数上限 + 墙钟上限（**整份报告共用**）。
/// 垃圾目录动辄几十万文件，不设上限会让"打开面板"变成一次全盘遍历；
/// 也不能按规则各给一份预算，否则十几条规则叠加起来要等一分钟。
const MAX_ENTRIES: usize = 400_000;
const SCAN_BUDGET: Duration = Duration::from_secs(8);
/// 清理项在报告里最多展示多少个路径（浏览器缓存那种规则能展开几百个）。
const ROOTS_PREVIEW: usize = 8;

pub struct CleanRule {
    pub id: &'static str,
    pub title: &'static str,
    /// 清理依据：为什么这东西可以删（UI 直接展示给用户）。
    pub hint: &'static str,
    /// 分类：UI 按它分组展示。
    pub group: &'static str,
    /// 规则出处：开源清理工具里的同名规则（UI 展示"依据来源"）。
    pub source: &'static str,
    /// 绝对路径模板。路径中**任意一段**都可以含 `*`（逐段展开）。
    /// 以 `*` 结尾表示"清空该目录内容"；末段是 `xxx*` 形如按文件名通配。
    pub paths: &'static [&'static str],
    /// 只清理修改时间早于这个分钟数的文件（正在被写入的文件跳过）。
    pub min_age_minutes: u64,
    pub needs_admin: bool,
    /// 危险项：默认不勾选，UI 需二次确认。
    pub risky: bool,
}

/// 规则表。新增一类垃圾 = 加一条结构体，不改引擎。
///
/// 只收"删了不会丢用户数据"的目录：缓存、临时文件、转储、回收站。
/// 刻意**不**收下载目录、浏览器历史/书签、聊天记录——那些是数据不是垃圾。
pub const RULES: &[CleanRule] = &[
    // ── 系统与临时 ──────────────────────────────────────────────
    CleanRule {
        id: "user-temp",
        title: "用户临时文件",
        hint: "%TEMP% 下 10 分钟前的文件；正在被程序占用的会自动跳过。",
        group: "系统与临时",
        source: "BleachBit · Windows 临时文件",
        paths: &[r"%TEMP%\*"],
        min_age_minutes: 10,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "windows-temp",
        title: "Windows 临时文件",
        hint: "系统级 Temp；部分文件属于其他用户，需要管理员权限才能删除。",
        group: "系统与临时",
        source: "BleachBit · 系统临时目录",
        paths: &[r"%SystemRoot%\Temp\*"],
        min_age_minutes: 10,
        needs_admin: true,
        risky: false,
    },
    CleanRule {
        id: "windows-update-cache",
        title: "Windows 更新缓存",
        hint: "已下载的更新包；删除后如需更新会重新下载。需要管理员权限。",
        group: "系统与临时",
        source: "BleachBit · Windows 更新缓存",
        paths: &[r"%SystemRoot%\SoftwareDistribution\Download\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: true,
    },
    CleanRule {
        id: "delivery-optimization",
        title: "传递优化缓存",
        hint: "Windows 更新分发用的下载缓存；删后不影响已安装的更新。",
        group: "系统与临时",
        source: "BleachBit · 传递优化",
        paths: &[r"%ProgramData%\Microsoft\Windows\DeliveryOptimization\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
    },
    CleanRule {
        id: "thumbs",
        title: "缩略图缓存",
        hint: "资源管理器的 thumbcache 数据库，删后会自动重建（首次浏览稍慢）。",
        group: "系统与临时",
        source: "BleachBit · 缩略图",
        paths: &[r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "icon-cache",
        title: "图标缓存",
        hint: "iconcache 数据库，删后自动重建。",
        group: "系统与临时",
        source: "BleachBit · 图标缓存",
        paths: &[
            r"%LOCALAPPDATA%\IconCache.db",
            r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\iconcache_*.db",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "font-cache",
        title: "字体缓存",
        hint: "字体目录缓存，删后系统会按需重建（首次显示字体略慢）。",
        group: "系统与临时",
        source: "BleachBit · 字体缓存",
        paths: &[r"%LOCALAPPDATA%\FontCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "d3d-cache",
        title: "着色器缓存",
        hint: "D3D 着色器缓存，删后游戏/图形程序首次启动会重新编译。",
        group: "系统与临时",
        source: "BleachBit · DirectX 着色器缓存",
        paths: &[r"%LOCALAPPDATA%\D3DSCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "inet-cache",
        title: "系统网络缓存",
        hint: "Windows 网络临时文件（INetCache），删后浏览器首次访问会重新拉取。",
        group: "浏览器与网络",
        source: "BleachBit · 网络缓存",
        paths: &[r"%LOCALAPPDATA%\Microsoft\Windows\INetCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    // ── 浏览器与网络 ────────────────────────────────────────────
    CleanRule {
        id: "browser-cache",
        title: "浏览器缓存",
        hint: "Chrome / Edge 的 Cache、Code Cache、GPUCache；浏览器运行时部分文件会被占用而跳过。",
        group: "浏览器与网络",
        source: "BleachBit · Chromium 缓存",
        paths: &[
            r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache\*",
            r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Code Cache\*",
            r"%LOCALAPPDATA%\Google\Chrome\User Data\*\GPUCache\*",
            r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Cache\*",
            r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Code Cache\*",
            r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\GPUCache\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    // ── 日志与转储 ──────────────────────────────────────────────
    CleanRule {
        id: "crash-dumps",
        title: "崩溃转储",
        hint: "程序崩溃时写的 .dmp；不排查问题就不需要保留。",
        group: "日志与转储",
        source: "CCleaner · 崩溃转储",
        paths: &[r"%LOCALAPPDATA%\CrashDumps\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "windows-logs",
        title: "Windows 诊断日志",
        hint: "CBS 组件存储日志；只用于排查系统问题，删后不影响系统运行。",
        group: "日志与转储",
        source: "BleachBit · 系统日志",
        paths: &[r"%SystemRoot%\Logs\CBS\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
    },
    CleanRule {
        id: "error-reports",
        title: "错误报告归档",
        hint: "Windows 错误报告（WER）的历史归档。",
        group: "日志与转储",
        source: "CCleaner · 错误报告",
        paths: &[
            r"%ProgramData%\Microsoft\Windows\WER\ReportArchive\*",
            r"%ProgramData%\Microsoft\Windows\WER\ReportQueue\*",
        ],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
    },
    // ── 残留安装包 ──────────────────────────────────────────────
    CleanRule {
        id: "installer-temp",
        title: "残留安装包",
        hint: "临时目录里遗留的 .msi / .msix / .cab 安装包；安装完成后就没人再要它们。",
        group: "残留安装包",
        source: "BleachBit · 安装包残留",
        paths: &[
            r"%TEMP%\*.msi",
            r"%TEMP%\*.msix",
            r"%TEMP%\*.cab",
            r"%SystemRoot%\Temp\*.msi",
        ],
        min_age_minutes: 30,
        needs_admin: false,
        risky: false,
    },
    // ── 开发缓存 ────────────────────────────────────────────────
    CleanRule {
        id: "npm-cache",
        title: "npm 缓存",
        hint: "npm 下载的包缓存；删后下次安装会重新下载。",
        group: "开发缓存",
        source: "BleachBit · npm 缓存",
        paths: &[
            r"%LOCALAPPDATA%\npm-cache\*",
            r"%APPDATA%\npm-cache\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "yarn-cache",
        title: "Yarn 缓存",
        hint: "Yarn 下载的包缓存；删后下次安装会重新下载。",
        group: "开发缓存",
        source: "BleachBit · Yarn 缓存",
        paths: &[r"%LOCALAPPDATA%\Yarn\Cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "pnpm-store",
        title: "pnpm 存储",
        hint: "pnpm 的内容寻址存储；删后下次安装会重新下载。",
        group: "开发缓存",
        source: "BleachBit · pnpm 存储",
        paths: &[r"%LOCALAPPDATA%\pnpm\store\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "pip-cache",
        title: "pip 缓存",
        hint: "Python 包下载缓存；删后下次 pip install 会重新下载。",
        group: "开发缓存",
        source: "BleachBit · pip 缓存",
        paths: &[r"%LOCALAPPDATA%\pip\Cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "cargo-cache",
        title: "Cargo 下载缓存",
        hint: "crates.io 下载的 .crate 包缓存（不动已解压的源码）；删后下次构建会重新下载。",
        group: "开发缓存",
        source: "kondo · Cargo 缓存",
        paths: &[r"%USERPROFILE%\.cargo\registry\cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    CleanRule {
        id: "gradle-cache",
        title: "Gradle 缓存",
        hint: "Gradle 下载与构建缓存；删后下次构建会重新下载依赖。",
        group: "开发缓存",
        source: "BleachBit · Gradle 缓存",
        paths: &[r"%USERPROFILE%\.gradle\caches\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
    // ── 回收站 ──────────────────────────────────────────────────
    CleanRule {
        id: "recycle-bin",
        title: "回收站",
        hint: "通过系统 Shell 清空，等同于右键「清空回收站」。",
        group: "回收站",
        source: "Windows 磁盘清理 · 回收站",
        paths: &[],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanItem {
    pub id: String,
    pub title: String,
    /// 清理依据。
    pub hint: String,
    /// 分类（UI 分组标题）。
    pub group: String,
    /// 规则出处（开源清理工具里的同名规则）。
    pub source: String,
    pub bytes: u64,
    pub files: u64,
    pub risky: bool,
    pub needs_admin: bool,
    /// 规则展开出的实际目标（展示用；清理时服务端重新展开，不用这些字符串）。
    pub roots: Vec<String>,
    /// 展开出的目标总数。浏览器缓存那种规则能展开出几百个目录，
    /// 全塞进 payload 是浪费——`roots` 只留前几个，`roots_total` 给真实数量。
    pub roots_total: usize,
    /// 根目录不存在（或不适用于所选盘符）时为 false。
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub items: Vec<CleanItem>,
    pub total_bytes: u64,
    pub total_files: u64,
    pub elapsed_ms: u64,
    pub drive: Option<String>,
    /// 扫描预算耗尽，结果不完整（UI 必须告知用户）。
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanResult {
    pub freed_bytes: u64,
    pub deleted: u64,
    pub skipped: u64,
    pub failed: u64,
    /// 前若干条失败原因（去重后的前缀，够用户判断"是不是被占用了"）。
    pub messages: Vec<String>,
}

// ── Tauri 命令 ────────────────────────────────────────────────────────
// 扫描是大目录遍历，清理是 I/O 密集：一律 spawn_blocking，别堵 IPC 线程。

#[tauri::command]
pub async fn cleanup_scan(
    app: tauri::AppHandle,
    drive: Option<String>,
) -> Result<ScanReport, String> {
    use tauri::Emitter;
    // 进度走 Tauri 事件（`cleanup-progress`）：扫描是分规则串行的，
    // 每完成一条推一次，前端据此画确定进度条而不是干转圈。
    let progress: Progress = std::sync::Arc::new(move |payload| {
        let _ = app.emit("cleanup-progress", payload);
    });
    tauri::async_runtime::spawn_blocking(move || scan_with_progress(drive, Some(progress)))
        .await
        .map_err(|error| format!("扫描失败：{error}"))
}

#[tauri::command]
pub async fn cleanup_execute(
    app: tauri::AppHandle,
    ids: Vec<String>,
    drive: Option<String>,
) -> Result<CleanResult, String> {
    use tauri::Emitter;
    let progress: Progress = std::sync::Arc::new(move |payload| {
        let _ = app.emit("cleanup-progress", payload);
    });
    tauri::async_runtime::spawn_blocking(move || clean_with_progress(&ids, drive, Some(progress)))
        .await
        .map_err(|error| format!("清理失败：{error}"))
}

struct Budget {
    deadline: Instant,
    entries: usize,
}

impl Budget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + SCAN_BUDGET,
            entries: 0,
        }
    }

    /// 超预算就停：扫描必须"随时可以放弃"，否则打开面板等于卡死。
    fn exhausted(&self) -> bool {
        self.entries >= MAX_ENTRIES || Instant::now() >= self.deadline
    }

    fn tick(&mut self) {
        self.entries += 1;
    }
}

/// 进度回调：命令层把它接到 Tauri 事件上，前端据此画进度条。
/// 手工跑测试时传 `None`，逻辑完全一致。
pub type Progress = std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// 展开后的清理目标。
///
/// 区分三形态是因为**语义不同**：`%TEMP%\*` 是"清空目录内容"（递归），
/// `thumbcache_*.db` 是"只匹配直接子项里的这些文件"（不递归），
/// `IconCache.db` 是"就这一个文件"。
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// 目录内容（递归遍历其下所有文件）。
    Dir(PathBuf),
    /// 目录里按文件名通配匹配（只查直接子项）。
    Glob(PathBuf, String),
    /// 单个文件。
    File(PathBuf),
}

impl Target {
    fn describe(&self) -> String {
        match self {
            Target::Dir(path) => path.display().to_string(),
            Target::Glob(dir, pattern) => dir.join(pattern).display().to_string(),
            Target::File(path) => path.display().to_string(),
        }
    }
}

/// 扫描：按盘符过滤（`None` = 全部盘），返回每个清理项的体积与文件数。
pub fn scan(drive: Option<String>) -> ScanReport {
    scan_with_progress(drive, None)
}

pub fn scan_with_progress(drive: Option<String>, progress: Option<Progress>) -> ScanReport {
    let started = Instant::now();
    let mut items = Vec::with_capacity(RULES.len());
    let mut total_bytes = 0u64;
    let mut total_files = 0u64;
    // 整份报告共用一份预算：十几条规则各给 8s 的话，最坏要等一分钟。
    let mut budget = Budget::new();

    for rule in RULES {
        if rule.id == "recycle-bin" {
            let (files, bytes) = recycle_bin_usage(drive.as_deref(), &mut budget);
            total_bytes += bytes;
            total_files += files;
            items.push(CleanItem {
                id: rule.id.to_string(),
                title: rule.title.to_string(),
                hint: rule.hint.to_string(),
                group: rule.group.to_string(),
                source: rule.source.to_string(),
                bytes,
                files,
                risky: rule.risky,
                needs_admin: rule.needs_admin,
                roots: drive
                    .as_deref()
                    .map(|d| vec![format!("{d}\\$RECYCLE.BIN")])
                    .unwrap_or_else(|| {
                        crate::sysmon::disks()
                            .into_iter()
                            .map(|disk| format!("{}\\$RECYCLE.BIN", disk.drive))
                            .collect()
                    }),
                roots_total: 1,
                available: true,
            });
            if let Some(progress) = &progress {
                progress(serde_json::json!({
                    "phase": "scan",
                    "ruleId": rule.id,
                    "title": rule.title,
                    "bytes": total_bytes,
                    "files": total_files,
                    "done": items.len(),
                    "total": RULES.len(),
                }));
            }
            continue;
        }

        let targets = expand_rule_targets(rule, drive.as_deref());
        let mut files = 0u64;
        let mut bytes = 0u64;
        for target in &targets {
            let (count, size) = scan_target(rule, target, &mut budget);
            files += count;
            bytes += size;
        }
        // 预算耗尽后停止枚举：后面的规则如实报 0，不当成"没有垃圾"。
        total_bytes += bytes;
        total_files += files;
        if let Some(progress) = &progress {
            progress(serde_json::json!({
                "phase": "scan",
                "ruleId": rule.id,
                "title": rule.title,
                "bytes": total_bytes,
                "files": total_files,
                "done": items.len() + 1,
                "total": RULES.len(),
            }));
        }
        items.push(CleanItem {
            id: rule.id.to_string(),
            title: rule.title.to_string(),
            hint: rule.hint.to_string(),
            group: rule.group.to_string(),
            source: rule.source.to_string(),
            bytes,
            files,
            risky: rule.risky,
            needs_admin: rule.needs_admin,
            roots: targets.iter().take(ROOTS_PREVIEW).map(Target::describe).collect(),
            roots_total: targets.len(),
            available: !targets.is_empty(),
        });
    }

    ScanReport {
        items,
        total_bytes,
        total_files,
        elapsed_ms: started.elapsed().as_millis() as u64,
        drive,
        truncated: budget.exhausted(),
    }
}

/// 清理：**重新展开规则**再删——前端的路径一律不信。
pub fn clean(ids: &[String], drive: Option<String>) -> CleanResult {
    clean_with_progress(ids, drive, None)
}

pub fn clean_with_progress(
    ids: &[String],
    drive: Option<String>,
    progress: Option<Progress>,
) -> CleanResult {
    let mut result = CleanResult {
        freed_bytes: 0,
        deleted: 0,
        skipped: 0,
        failed: 0,
        messages: Vec::new(),
    };
    let mut budget = Budget::new();
    let selected: Vec<&CleanRule> = RULES
        .iter()
        .filter(|rule| ids.iter().any(|id| id == rule.id))
        .collect();
    let total = selected.len().max(1);

    for (index, rule) in selected.into_iter().enumerate() {
        if rule.id == "recycle-bin" {
            match empty_recycle_bin(drive.as_deref()) {
                Ok(()) => result.messages.push("回收站已清空。".to_string()),
                Err(error) => {
                    result.failed += 1;
                    result.messages.push(error);
                }
            }
            if let Some(progress) = &progress {
                progress(serde_json::json!({
                    "phase": "clean",
                    "ruleId": rule.id,
                    "title": rule.title,
                    "done": index + 1,
                    "total": total,
                    "deleted": result.deleted,
                    "freedBytes": result.freed_bytes,
                }));
            }
            continue;
        }

        let targets = expand_rule_targets(rule, drive.as_deref());
        for target in &targets {
            clean_target(rule, target, &mut budget, &mut result);
            // 目录清空后把空目录也收掉（非空时 remove_dir 自然失败）。
            if let Target::Dir(dir) = target {
                remove_empty_dirs(dir, &mut budget);
            }
        }
        if let Some(progress) = &progress {
            progress(serde_json::json!({
                "phase": "clean",
                "ruleId": rule.id,
                "title": rule.title,
                "done": index + 1,
                "total": total,
                "deleted": result.deleted,
                "freedBytes": result.freed_bytes,
                "skipped": result.skipped,
            }));
        }
    }

    result.messages = dedupe_messages(result.messages);
    result
}

/// 展开一条规则的路径模板，并按盘符过滤。
///
/// **逐段展开**：中间段带 `*` 时真的去父目录里枚举（这正是旧实现漏掉的）。
fn expand_rule_targets(rule: &CleanRule, drive: Option<&str>) -> Vec<Target> {
    let mut targets = Vec::new();
    for template in rule.paths {
        for target in expand_template(template) {
            if let Some(drive) = drive {
                // 注意：这里必须用**盘符提取器**，不能用 `sysmon::drive_of`——
                // 后者是把挂载点（"C:\"）转成键（"C:"）的，给完整路径调用会
                // 原样返回整条路径，于是"选中某块盘"时每条规则都被判成
                // "不在该盘"而丢掉（旧实现就是这个 bug：选 D: 时只剩回收站可清）。
                if !drive_letter(&target.describe()).eq_ignore_ascii_case(drive) {
                    continue;
                }
            }
            targets.push(target);
        }
    }
    targets
}

/// 从路径里取盘符：`C:\Users\...` → `C:`；取不到返回空串。
fn drive_letter(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        path[..2].to_ascii_uppercase()
    } else {
        String::new()
    }
}

/// 把一条路径模板展开成具体目标。
///
/// 语义（与旧 `split_pattern` 一致，但支持中间段通配）：
/// - 末段是 `*` → [`Target::Dir`]：清空该目录**内容**（递归）；
/// - 末段是 `xxx*` 之类 → [`Target::Glob`]：在父目录里按文件名匹配（不递归）；
/// - 末段无通配 → [`Target::File`]：就这一个文件。
fn expand_template(template: &str) -> Vec<Target> {
    let Some(expanded) = expand_env(template) else {
        return Vec::new();
    };
    let text = expanded.display().to_string().replace('/', "\\");
    let mut segments: Vec<String> = text
        .split('\\')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
        .collect();
    if segments.is_empty() {
        return Vec::new();
    }
    let last = segments.pop();
    // 第一段是盘符（"C:"）。路径只有一段时（比如环境变量展开成 "C:\"）
    // 没有盘符段可用——直接放弃这条模板，绝不 panic。
    let Some((drive, rest)) = segments.split_first() else {
        return Vec::new();
    };
    let drive = drive.clone();
    let segments = rest.to_vec();

    // 逐段展开：遇到带 `*` 的段就去当前目录里枚举匹配。
    //
    // 根段必须带尾分隔符（"C:\" 而不是 "C:"）：`PathBuf::from("C:")` 是
    // **驱动器相对路径**（"当前目录所在的 C 盘"），join 出来是 `C:Users`，
    // `read_dir` 直接失败——这不是理论问题，是第一版就踩的坑。
    let mut current: Vec<PathBuf> = vec![PathBuf::from(format!("{drive}\\"))];
    for segment in segments {
        let mut next: Vec<PathBuf> = Vec::new();
        for base in &current {
            if segment.contains('*') {
                let Ok(entries) = fs::read_dir(base) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if glob_match(&segment, &name) {
                        next.push(entry.path());
                    }
                }
            } else {
                next.push(base.join(&segment));
            }
        }
        current = next;
        if current.is_empty() {
            return Vec::new();
        }
    }

    match last {
        Some(segment) if segment == "*" => {
            let mut out: Vec<Target> = Vec::with_capacity(current.len());
            for base in current {
                out.push(Target::Dir(base));
            }
            out
        }
        Some(segment) if segment.contains('*') => {
            let mut out: Vec<Target> = Vec::with_capacity(current.len());
            for base in current {
                out.push(Target::Glob(base, segment.clone()));
            }
            out
        }
        Some(segment) => {
            let mut out: Vec<Target> = Vec::with_capacity(current.len());
            for base in current {
                out.push(Target::File(base.join(segment.clone())));
            }
            out
        }
        None => {
            let mut out: Vec<Target> = Vec::with_capacity(current.len());
            for base in current {
                out.push(Target::Dir(base));
            }
            out
        }
    }
}

/// 统计一个目标里的文件数与字节数。
fn scan_target(rule: &CleanRule, target: &Target, budget: &mut Budget) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;

    match target {
        Target::Dir(dir) => {
            walk(dir, budget, &mut |path| {
                if is_deletable(rule, path) {
                    files += 1;
                    bytes += file_size(path);
                }
            });
        }
        Target::Glob(dir, pattern) => {
            let Ok(entries) = fs::read_dir(dir) else {
                return (0, 0);
            };
            for entry in entries.flatten() {
                budget.tick();
                if budget.exhausted() {
                    break;
                }
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if !glob_match(pattern, &name) || !is_deletable(rule, &path) {
                    continue;
                }
                files += 1;
                bytes += file_size(&path);
            }
        }
        Target::File(path) => {
            if is_deletable(rule, path) {
                files += 1;
                bytes += file_size(path);
            }
        }
    }

    (files, bytes)
}

fn clean_target(
    rule: &CleanRule,
    target: &Target,
    budget: &mut Budget,
    result: &mut CleanResult,
) {
    let mut targets: Vec<PathBuf> = Vec::new();
    match target {
        Target::Dir(dir) => {
            walk(dir, budget, &mut |path| {
                if is_deletable(rule, path) {
                    targets.push(path.to_path_buf());
                }
            });
        }
        Target::Glob(dir, pattern) => {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    budget.tick();
                    if budget.exhausted() {
                        break;
                    }
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if glob_match(pattern, &name) && is_deletable(rule, &path) {
                        targets.push(path);
                    }
                }
            }
        }
        Target::File(path) => {
            if is_deletable(rule, path) {
                targets.push(path.clone());
            }
        }
    }

    for path in targets {
        let size = file_size(&path);
        match fs::remove_file(&path) {
            Ok(()) => {
                result.deleted += 1;
                result.freed_bytes += size;
            }
            Err(error) => {
                // 被占用（ERROR_SHARING_VIOLATION）/ 无权限是常态，不算异常。
                result.skipped += 1;
                if result.failed < 20 {
                    result
                        .messages
                        .push(format!("{}：{}", short_path(&path), error));
                }
            }
        }
    }
}

/// 递归遍历目录，只对**文件**回调；符号链接与联接点整体跳过（不解引用，
/// 否则 %TEMP% 里的一个 junction 能把删除引导到别处）。
fn walk(dir: &Path, budget: &mut Budget, on_file: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        budget.tick();
        if budget.exhausted() {
            return;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(&path, budget, on_file);
        } else if is_allowed_path(&path) {
            on_file(&path);
        }
    }
}

/// 规则 + 引擎级安全校验：年龄、禁区、符号链接。
fn is_deletable(rule: &CleanRule, path: &Path) -> bool {
    if !is_allowed_path(path) {
        return false;
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.is_dir() || meta.file_type().is_symlink() {
        return false;
    }
    if rule.min_age_minutes == 0 {
        return true;
    }
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    age >= Duration::from_secs(rule.min_age_minutes * 60)
}

/// 禁区校验：路径（小写、正斜杠归一化）不得落在任何系统关键目录下。
fn is_allowed_path(path: &Path) -> bool {
    let text = path.display().to_string().replace('/', "\\").to_lowercase();
    !FORBIDDEN_ROOTS
        .iter()
        .any(|root| text == *root || text.starts_with(&format!("{root}\\")))
}

fn remove_empty_dirs(dir: &Path, budget: &mut Budget) {
    if budget.exhausted() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        remove_empty_dirs(&path, budget);
        // 只删空目录；有内容时 remove_dir 会失败，忽略即可。
        let _ = fs::remove_dir(&path);
    }
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn short_path(path: &Path) -> String {
    let text = path.display().to_string();
    if text.len() <= 80 {
        return text;
    }
    let tail: String = text.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

fn dedupe_messages(messages: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    messages
        .into_iter()
        .filter(|m| seen.insert(m.clone()))
        .take(20)
        .collect()
}

/// 极简通配：只支持 `*`（任意长）与 `?`（单字符）。缓存文件名用不到更复杂的。
fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);

    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p].eq_ignore_ascii_case(&name[n])) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = n;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

// ── 回收站 ────────────────────────────────────────────────────────────

/// 回收站占用：Windows 没有公开的"按盘统计" API，这里按 `$RECYCLE.BIN`
/// 的实际体积估算（只读，不删）。
fn recycle_bin_usage(drive: Option<&str>, budget: &mut Budget) -> (u64, u64) {
    let candidates: Vec<PathBuf> = match drive {
        Some(d) => vec![PathBuf::from(format!(r"{d}\$RECYCLE.BIN"))],
        None => crate::sysmon::disks()
            .into_iter()
            .map(|disk| PathBuf::from(format!(r"{}\$RECYCLE.BIN", disk.drive)))
            .collect(),
    };

    let mut files = 0u64;
    let mut bytes = 0u64;
    for root in candidates {
        if !root.exists() {
            continue;
        }
        walk(&root, budget, &mut |path| {
            files += 1;
            bytes += file_size(path);
        });
    }
    (files, bytes)
}

#[cfg(windows)]
fn empty_recycle_bin(drive: Option<&str>) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::SHEmptyRecycleBinW;
    // SHERB_NOCONFIRMATION(1) | SHERB_NOPROGRESSUI(2) | SHERB_NOSOUND(4)
    const FLAGS: u32 = 0x7;

    let root: Vec<u16> = match drive {
        Some(d) => format!("{d}\\").encode_utf16().chain(Some(0)).collect(),
        None => vec![0],
    };
    let hr = unsafe { SHEmptyRecycleBinW(0, root.as_ptr(), FLAGS) };
    if hr < 0 {
        return Err(format!("清空回收站失败（HRESULT 0x{hr:08X}）。"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn empty_recycle_bin(_drive: Option<&str>) -> Result<(), String> {
    Err("仅支持 Windows。".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_cache_file_names() {
        assert!(glob_match("thumbcache_*.db", "thumbcache_96.db"));
        assert!(!glob_match("thumbcache_*.db", "iconcache_96.db"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a?c", "abc"));
    }

    #[test]
    fn forbidden_roots_are_rejected() {
        assert!(!is_allowed_path(Path::new(r"C:\Windows\System32\drivers\etc\hosts")));
        assert!(!is_allowed_path(Path::new(r"c:\program files\app\a.dll")));
        assert!(is_allowed_path(Path::new(r"C:\Users\me\AppData\Local\Temp\a.tmp")));
    }

    /// 中间段带 `*` 的模板必须真的展开（旧实现只处理末段，浏览器缓存规则
    /// 因此一直是 0 字节）。
    #[test]
    fn middle_segment_wildcards_expand_to_real_directories() {
        let root = std::env::temp_dir().join(format!("hermix-cleanup-wild-{}", std::process::id()));
        // 形态照抄真实规则：<根>\*\<子目录>\*（Chrome 是 User Data\<配置>\Cache）
        fs::create_dir_all(root.join("Profile 1").join("Cache")).unwrap();
        fs::create_dir_all(root.join("Default").join("Cache")).unwrap();

        let template = format!("{}\\*\\Cache\\*", root.display());
        let targets = expand_template(&template);
        assert_eq!(targets.len(), 2, "两个用户配置目录都要命中");
        assert!(targets.iter().all(|target| matches!(target, Target::Dir(_))));
        assert!(targets
            .iter()
            .any(|target| target.describe().contains("Profile 1")));

        let _ = fs::remove_dir_all(&root);
    }

    /// 末段是纯 `*` → 目录内容（递归）；末段是 `xxx*` → 只匹配直接子项。
    #[test]
    fn trailing_star_means_directory_contents() {
        let root = std::env::temp_dir().join(format!("hermix-cleanup-shape-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let dir_targets = expand_template(&format!("{}\\*", root.display()));
        assert!(matches!(dir_targets[0], Target::Dir(_)));

        let glob_targets = expand_template(&format!("{}\\*.tmp", root.display()));
        assert!(matches!(glob_targets[0], Target::Glob(_, _)));

        let file_targets = expand_template(&format!("{}\\a.db", root.display()));
        assert!(matches!(file_targets[0], Target::File(_)));

        let _ = fs::remove_dir_all(&root);
    }

    /// 规则表自身的安全性：任何规则都不得落在禁区里。
    #[test]
    fn no_rule_targets_forbidden_roots() {
        for rule in RULES {
            for template in rule.paths {
                let expanded = expand_env(template).expect("env expanded");
                assert!(
                    is_allowed_path(&expanded),
                    "规则 {} 指向了禁区：{}",
                    rule.id,
                    expanded.display()
                );
            }
        }
    }

    /// 盘符过滤必须按**盘符**判定，不是按整条路径。
    ///
    /// 旧实现拿 `sysmon::drive_of`（那是把挂载点转键用的）去处理完整路径，
    /// 结果永远不等于 "C:" —— 选中某块盘时所有规则都被丢掉，"只剩回收站可清"。
    #[test]
    fn drive_filter_keeps_only_targets_on_that_drive() {
        let rule = CleanRule {
            id: "test",
            title: "测试",
            hint: "",
            group: "测试",
            source: "测试",
            paths: &[r"%TEMP%\*"],
            min_age_minutes: 0,
            needs_admin: false,
            risky: false,
        };
        let temp = std::env::var("TEMP").expect("TEMP");
        let drive = temp.get(..2).expect("drive letter").to_string();

        let all = expand_rule_targets(&rule, None);
        assert!(!all.is_empty(), "不筛盘时应有目标");

        let mine = expand_rule_targets(&rule, Some(&drive));
        assert_eq!(mine.len(), all.len(), "本盘目标不该被过滤掉");

        let other = if drive.eq_ignore_ascii_case("C:") { "D:" } else { "C:" };
        assert!(
            expand_rule_targets(&rule, Some(other)).is_empty(),
            "别的盘不该命中"
        );
    }

    /// 路径模板只有一段时不能 panic（环境变量展开成 "C:\\" 的极端情况）。
    #[test]
    fn single_segment_template_does_not_panic() {
        let targets = expand_template("C:\\");
        assert!(targets.is_empty());
    }

    /// 临时探针：跑一次真实扫描并打印报告（人工核对规则展开是否命中）。
    /// 依赖本机环境，默认不跑。
    #[test]
    #[ignore = "依赖本机环境，人工核对用"]
    fn probe_real_scan() {
        let report = scan(None);
        for item in &report.items {
            println!(
                "{:>28}  {:>10}  {:>8} 个  可用={}  根={:?}",
                item.title,
                item.bytes,
                item.files,
                item.available,
                item.roots
            );
        }
        println!("合计 {} bytes / {} files", report.total_bytes, report.total_files);
    }

    /// 每条规则都要有分类与依据出处——UI 靠这两项分组并明示依据。
    #[test]
    fn every_rule_declares_group_and_source() {
        for rule in RULES {
            assert!(!rule.group.is_empty(), "规则 {} 缺分类", rule.id);
            assert!(!rule.source.is_empty(), "规则 {} 缺依据出处", rule.id);
            assert!(!rule.hint.is_empty(), "规则 {} 缺清理依据", rule.id);
        }
    }

    fn fake_rule() -> CleanRule {
        CleanRule {
            id: "test",
            title: "测试",
            hint: "",
            group: "测试",
            source: "测试",
            paths: &[],
            min_age_minutes: 0,
            needs_admin: false,
            risky: false,
        }
    }

    /// 通配规则只匹配**直接子项**（缓存文件不递归），清理时非匹配文件必须留下。
    #[test]
    fn glob_scan_and_clean_touch_only_matching_files() {
        let dir = std::env::temp_dir().join(format!("hermix-cleanup-glob-{}", std::process::id()));
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("a.tmp"), vec![0u8; 1024]).unwrap();
        fs::write(dir.join("keep.txt"), b"keep").unwrap();
        fs::write(sub.join("b.tmp"), vec![0u8; 2048]).unwrap();

        let rule = fake_rule();
        let target = Target::Glob(dir.clone(), "*.tmp".to_string());
        let mut budget = Budget::new();
        let (files, bytes) = scan_target(&rule, &target, &mut budget);
        assert_eq!((files, bytes), (1, 1024), "只算直接子项里的 *.tmp");

        let mut result = CleanResult {
            freed_bytes: 0,
            deleted: 0,
            skipped: 0,
            failed: 0,
            messages: Vec::new(),
        };
        clean_target(&rule, &target, &mut budget, &mut result);
        assert_eq!(result.deleted, 1);
        assert_eq!(result.freed_bytes, 1024);
        assert!(dir.join("keep.txt").exists(), "不匹配的文件必须保留");
        assert!(sub.join("b.tmp").exists(), "通配规则不递归子目录");

        let _ = fs::remove_dir_all(&dir);
    }

    /// `...\*` 形态会递归清空目录内容（临时/缓存目录的常见形态）。
    #[test]
    fn recursive_rule_walks_subdirectories() {
        let dir = std::env::temp_dir().join(format!("hermix-cleanup-rec-{}", std::process::id()));
        let sub = dir.join("deep").join("deeper");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("a.tmp"), vec![0u8; 100]).unwrap();
        fs::write(sub.join("b.tmp"), vec![0u8; 200]).unwrap();

        let rule = fake_rule();
        let mut budget = Budget::new();
        let (files, bytes) = scan_target(&rule, &Target::Dir(dir.clone()), &mut budget);
        assert_eq!((files, bytes), (2, 300));

        let _ = fs::remove_dir_all(&dir);
    }
}
