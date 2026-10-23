//! 垃圾清理引擎 —— 规则驱动（清理项是**数据**，不是代码）。
//!
//! 模型来自 BleachBit / CleanerML 一脉：每条规则声明一组路径（支持环境变量与
//! 通配）、一个动作（删除）、最小年龄、是否需要管理员、是否危险、以及**本地评分**
//! 与**恢复方式**；引擎只删"规则展开出来的东西"，不递归删目录、不碰禁区。
//!
//! - 规则表 [`RULES`]：路径模板 + 最小年龄 + 管理员 + 危险标记 + 评分 + 恢复方式；
//! - 扫描：展开 `%VAR%` → 通配匹配 → 递归枚举（不跟随 symlink/junction）；
//! - 清理：**重新扫描**（绝不信任前端传来的路径），逐文件删除，失败的跳过
//!   并计数，最后尽力清掉空目录；
//! - 回收站不走文件删除，调 Shell 的 `SHEmptyRecycleBinW`（BleachBit 在
//!   Windows 上同样走 shell，手删 `$RECYCLE.BIN` 会破坏回收站元数据）。
//!
//! 安全红线：
//! 1. 只允许删除 [`RULES`] 展开出的根之下的文件（前缀校验，大小写不敏感）；
//! 2. [`FORBIDDEN_ROOTS`] 里的系统目录即使规则写错也拒绝；
//! 3. 符号链接/联接点一律跳过（`symlink_metadata`，不解引用）；
//! 4. 目录本身永不删除，只删文件 + 移除已空子目录。
//!
//! AI 辅助（`cleanup_ai.rs`）只在这套规则之上**加建议**，不扩大删除范围：
//! 本地判定为高风险的项，模型即使说"可以清"也不采纳。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use crate::sysmon::{drive_of, expand_env};

/// 规则作用域里的"绝对禁区"：即使某条规则的通配把这里纳入了，也拒绝删除。
///
/// 注意 `c:\program files` / `c:\program files (x86)` 是**整棵树**禁区：
/// Steam 等游戏平台的缓存也装在 Program Files 下，但本引擎的红线是
/// "不碰 Program Files"（那里的东西装错版本号就装不回来了），
/// 所以那些清理项只能等禁区策略细化后再加。
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
/// 也不能按规则各给一份预算，否则 11 条规则叠加起来要等一分钟。
const MAX_ENTRIES: usize = 400_000;
const SCAN_BUDGET: Duration = Duration::from_secs(8);

pub struct CleanRule {
    pub id: &'static str,
    pub title: &'static str,
    pub hint: &'static str,
    pub group: &'static str,
    /// 绝对路径模板。以 `*` 结尾表示"清空该目录内容"；含 `*` 但不以 `*`
    /// 结尾表示按文件名通配（如 `thumbcache_*.db`）。
    pub paths: &'static [&'static str],
    /// 只清理修改时间早于这个分钟数的文件（正在被写入的文件跳过）。
    pub min_age_minutes: u64,
    pub needs_admin: bool,
    /// 危险项：默认不勾选，UI 需二次确认。
    pub risky: bool,
    /// **本地评分**（0-100）：这一项「有多适合直接清掉」。
    /// 纯缓存 = 高分；涉及用户数据或需要重新下载的 = 低分。
    /// 评分只决定**默认勾选与排序**，不代表执行权限（权限看 risky/禁区）。
    pub score: i32,
    /// 删掉之后怎么恢复（给用户看，别用"可恢复"这种含糊说法）。
    pub recoverable: &'static str,
}

/// 评分分档门槛：≥ 此值 → 默认勾选（安全档）。
const SCORE_SAFE: i32 = 80;

/// 规则表。新增一类垃圾 = 加一条结构体，不改引擎。
///
/// 只收"删了不会丢用户数据"的目录：缓存、临时文件、转储、回收站。
/// 刻意**不**收下载目录、浏览器历史/书签、聊天记录——那些是数据不是垃圾。
///
/// 分类沿用 BleachBit 的两层结构（应用 → 选项），这里压成一维 `group`：
/// 系统 / 浏览器 / 显卡 / 开发工具 / 应用 / 游戏。
pub const RULES: &[CleanRule] = &[
    CleanRule {
        id: "user-temp",
        title: "用户临时文件",
        hint: "%TEMP% 下 10 分钟前的文件；正在被程序占用的会自动跳过。",
        group: "系统",
        paths: &[r"%TEMP%\*"],
        min_age_minutes: 10,
        needs_admin: false,
        risky: false,
        score: 90,
        recoverable: "不可恢复（临时文件本就该丢）",
    },
    CleanRule {
        id: "windows-temp",
        title: "Windows 临时文件",
        hint: "系统级 Temp；部分文件属于其他用户，需要管理员权限才能删除。",
        group: "系统",
        paths: &[r"%SystemRoot%\Temp\*"],
        min_age_minutes: 10,
        needs_admin: true,
        risky: false,
        score: 85,
        recoverable: "不可恢复",
    },
    CleanRule {
        id: "recycle-bin",
        title: "回收站",
        hint: "通过系统 Shell 清空，等同于右键「清空回收站」。",
        group: "系统",
        paths: &[],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 70,
        recoverable: "不可恢复（等同于清空回收站）",
    },
    CleanRule {
        id: "thumbnails",
        title: "缩略图缓存",
        hint: "资源管理器的 thumbcache 数据库，删后会自动重建（首次浏览稍慢）。",
        group: "系统",
        paths: &[r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建（首次浏览稍慢）",
    },
    CleanRule {
        id: "icon-cache",
        title: "图标缓存",
        hint: "iconcache 数据库，删后自动重建。",
        group: "系统",
        paths: &[
            r"%LOCALAPPDATA%\IconCache.db",
            r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\iconcache_*.db",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "crash-dumps",
        title: "崩溃转储",
        hint: "程序崩溃时写的 .dmp；不排查问题就不需要保留。",
        group: "系统",
        paths: &[r"%LOCALAPPDATA%\CrashDumps\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 80,
        recoverable: "不可恢复（失去崩溃现场）",
    },
    CleanRule {
        id: "minidump",
        title: "系统小型转储",
        hint: "蓝屏时 Windows 写的 Minidump，排查蓝屏才需要。",
        group: "系统",
        paths: &[r"%SystemRoot%\Minidump\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 75,
        recoverable: "不可恢复（失去蓝屏现场）",
    },
    CleanRule {
        id: "d3d-cache",
        title: "着色器缓存",
        hint: "D3D 着色器缓存，删后游戏/图形程序首次启动会重新编译。",
        group: "系统",
        paths: &[r"%LOCALAPPDATA%\D3DSCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建（首次启动会重新编译着色器）",
    },
    CleanRule {
        id: "delivery-optimization",
        title: "传递优化缓存",
        hint: "Windows 更新/商店下载的共享缓存；删除后需要时会重新下载。",
        group: "系统",
        paths: &[r"%ProgramData%\Microsoft\Windows\DeliveryOptimization\Cache\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 85,
        recoverable: "自动重下",
    },
    CleanRule {
        id: "inet-cache",
        title: "IE / INet 缓存",
        hint: "Windows 传统联网组件的缓存与历史文件。",
        group: "系统",
        paths: &[r"%LOCALAPPDATA%\Microsoft\Windows\INetCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "font-cache",
        title: "字体缓存",
        hint: "系统字体缓存；清掉后新字体要重新扫描（首次启动变慢）。",
        group: "系统",
        paths: &[r"%LOCALAPPDATA%\FontCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 70,
        recoverable: "自动重建（首次启动变慢）",
    },
    CleanRule {
        id: "cbs-logs",
        title: "组件服务日志",
        hint: "Windows 组件服务（CBS）的排障日志，体积通常很大。",
        group: "系统",
        paths: &[r"%SystemRoot%\Logs\CBS\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 75,
        recoverable: "不可恢复（排障日志）",
    },
    CleanRule {
        id: "panther-logs",
        title: "系统安装日志",
        hint: "Windows 安装/升级过程的 Panther 日志。",
        group: "系统",
        paths: &[r"%SystemRoot%\Panther\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 75,
        recoverable: "不可恢复（安装日志）",
    },
    CleanRule {
        id: "prefetch",
        title: "预读文件",
        hint: "Windows 用来加速启动的预读数据；清掉后前几次启动会变慢。",
        group: "系统",
        paths: &[r"%SystemRoot%\Prefetch\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 55,
        recoverable: "自动重建（前几次启动变慢）",
    },
    CleanRule {
        id: "error-reports",
        title: "错误报告归档",
        hint: "Windows 错误报告（WER）的历史归档。",
        group: "系统",
        paths: &[
            r"%ProgramData%\Microsoft\Windows\WER\ReportArchive\*",
            r"%ProgramData%\Microsoft\Windows\WER\ReportQueue\*",
        ],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 80,
        recoverable: "不可恢复（排障日志）",
    },
    CleanRule {
        id: "browser-cache",
        title: "浏览器缓存",
        hint: "Chrome / Edge 的 Cache、Code Cache、GPUCache；浏览器运行时部分文件会被占用而跳过。",
        group: "浏览器",
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
        score: 85,
        recoverable: "自动重建（登录态保留）",
    },
    CleanRule {
        id: "browser-sw-cache",
        title: "浏览器离线站点缓存",
        hint: "Service Worker 的 CacheStorage：网页的离线副本，删后这些站点要重新联网加载。",
        group: "浏览器",
        paths: &[
            r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Service Worker\CacheStorage\*",
            r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Service Worker\CacheStorage\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 60,
        recoverable: "自动重建（离线站点要重新联网）",
    },
    CleanRule {
        id: "nvidia-cache",
        title: "NVIDIA 着色器缓存",
        hint: "NVIDIA 的 DXCache / GLCache，删后游戏首次运行会重新编译着色器。",
        group: "显卡",
        paths: &[
            r"%LOCALAPPDATA%\NVIDIA\DXCache\*",
            r"%LOCALAPPDATA%\NVIDIA\GLCache\*",
            r"%ProgramData%\NVIDIA Corporation\NV_Cache\*",
        ],
        min_age_minutes: 0,
        needs_admin: true,
        risky: false,
        score: 85,
        recoverable: "自动重建（首次运行重新编译着色器）",
    },
    CleanRule {
        id: "amd-cache",
        title: "AMD 着色器缓存",
        hint: "AMD 的 DxCache / VkCache。",
        group: "显卡",
        paths: &[
            r"%LOCALAPPDATA%\AMD\DxCache\*",
            r"%LOCALAPPDATA%\AMD\VkCache\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "intel-cache",
        title: "Intel 着色器缓存",
        hint: "Intel 的 ShaderCache。",
        group: "显卡",
        paths: &[r"%LOCALAPPDATA%\Intel\ShaderCache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "vscode-cache",
        title: "VS Code 缓存",
        hint: "编辑器的工作区缓存与扩展安装包缓存（不含你的代码与设置）。",
        group: "开发工具",
        paths: &[
            r"%APPDATA%\Code\Cache\*",
            r"%APPDATA%\Code\CachedData\*",
            r"%APPDATA%\Code\CachedExtensionVSIXs\*",
            r"%APPDATA%\Code\Code Cache\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "npm-cache",
        title: "npm 缓存",
        hint: "Node 包管理器下载的包缓存。",
        group: "开发工具",
        paths: &[r"%LOCALAPPDATA%\npm-cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重下",
    },
    CleanRule {
        id: "pip-cache",
        title: "pip 缓存",
        hint: "Python 包下载缓存。",
        group: "开发工具",
        paths: &[r"%LOCALAPPDATA%\pip\Cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重下",
    },
    CleanRule {
        id: "nuget-cache",
        title: "NuGet 缓存",
        hint: ".NET 包缓存；清掉后离线还原会失败（需联网重下）。",
        group: "开发工具",
        paths: &[r"%LOCALAPPDATA%\NuGet\v3-cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 80,
        recoverable: "自动重下",
    },
    CleanRule {
        id: "cargo-cache",
        title: "Cargo 下载缓存",
        hint: "Rust 依赖的下载缓存（只清 cache，不动已解压的源码）。",
        group: "开发工具",
        paths: &[r"%USERPROFILE%\.cargo\registry\cache\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重下",
    },
    CleanRule {
        id: "gradle-cache",
        title: "Gradle 缓存",
        hint: "Gradle 构建缓存与依赖缓存。",
        group: "开发工具",
        paths: &[r"%USERPROFILE%\.gradle\caches\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 65,
        recoverable: "自动重下（首次构建变慢）",
    },
    CleanRule {
        id: "onedrive-logs",
        title: "OneDrive 日志",
        hint: "同步客户端的本地日志，不含云端文件。",
        group: "应用",
        paths: &[r"%LOCALAPPDATA%\Microsoft\OneDrive\logs\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 85,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "teams-cache",
        title: "Teams 缓存",
        hint: "Teams 客户端的界面与媒体缓存。",
        group: "应用",
        paths: &[
            r"%APPDATA%\Microsoft\Teams\Cache\*",
            r"%APPDATA%\Microsoft\Teams\Code Cache\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 75,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "discord-cache",
        title: "Discord 缓存",
        hint: "Discord 客户端的图片与代码缓存。",
        group: "应用",
        paths: &[
            r"%APPDATA%\discord\Cache\*",
            r"%APPDATA%\discord\Code Cache\*",
        ],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 75,
        recoverable: "自动重建",
    },
    CleanRule {
        id: "spotify-cache",
        title: "Spotify 缓存",
        hint: "音乐缓存与离线副本（离线歌会失效）。",
        group: "应用",
        paths: &[r"%LOCALAPPDATA%\Spotify\Storage\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 60,
        recoverable: "自动重建（离线歌曲需重新下载）",
    },
    CleanRule {
        id: "zoom-logs",
        title: "Zoom 日志",
        hint: "会议客户端日志与录制临时文件。",
        group: "应用",
        paths: &[r"%APPDATA%\Zoom\logs\*"],
        min_age_minutes: 0,
        needs_admin: false,
        risky: false,
        score: 75,
        recoverable: "不可恢复（日志）",
    },
    CleanRule {
        id: "windows-update-cache",
        title: "Windows 更新缓存",
        hint: "已下载的更新包；删除后如需更新会重新下载。需要管理员权限。",
        group: "系统",
        paths: &[r"%SystemRoot%\SoftwareDistribution\Download\*"],
        min_age_minutes: 0,
        needs_admin: true,
        risky: true,
        score: 40,
        recoverable: "需重新下载",
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanItem {
    pub id: String,
    pub title: String,
    pub hint: String,
    pub group: String,
    pub bytes: u64,
    pub files: u64,
    pub risky: bool,
    pub needs_admin: bool,
    /// 本地评分（0-100）：这一项有多适合直接清掉（纯缓存高、涉及用户数据低）。
    pub score: i32,
    /// 分档：`safe`（默认勾选）/ `review`（看一眼再决定）/ `risky`（高风险，默认不勾）。
    pub bucket: String,
    /// 删掉之后怎么恢复（给用户看，别写"可恢复"这种含糊说法）。
    pub recoverable: String,
    /// 规则展开出的根（展示用；清理时服务端重新展开，不用这些字符串）。
    pub roots: Vec<String>,
    /// 根目录不存在（或不适用于所选盘符）时为 false。
    pub available: bool,
}

/// 规则 → 分档。高风险一票否决（不管评分多高）。
fn bucket_of(rule: &CleanRule) -> &'static str {
    if rule.risky {
        "risky"
    } else if rule.score >= SCORE_SAFE {
        "safe"
    } else {
        "review"
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub items: Vec<CleanItem>,
    pub total_bytes: u64,
    pub total_files: u64,
    pub elapsed_ms: u64,
    pub drive: Option<String>,
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

/// 扫描：按盘符过滤（`None` = 全部盘），返回每个清理项的体积与文件数。
pub fn scan(drive: Option<String>) -> ScanReport {
    scan_with_progress(drive, None)
}

pub fn scan_with_progress(drive: Option<String>, progress: Option<Progress>) -> ScanReport {
    let started = Instant::now();
    let mut items = Vec::with_capacity(RULES.len());
    let mut total_bytes = 0u64;
    let mut total_files = 0u64;
    // 整份报告共用一份预算：11 条规则各给 8s 的话，最坏要等一分半。
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
                bytes,
                files,
                risky: rule.risky,
                needs_admin: rule.needs_admin,
                score: rule.score,
                bucket: bucket_of(rule).to_string(),
                recoverable: rule.recoverable.to_string(),
                roots: drive
                    .as_deref()
                    .map(|d| vec![format!("{d}\\$RECYCLE.BIN")])
                    .unwrap_or_else(|| vec![r"$RECYCLE.BIN".to_string()]),
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

        let roots = expand_rule_roots(rule, drive.as_deref());
        let mut files = 0u64;
        let mut bytes = 0u64;
        for root in &roots {
            let (count, size) = scan_root(rule, root, &mut budget);
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
            bytes,
            files,
            risky: rule.risky,
            needs_admin: rule.needs_admin,
            score: rule.score,
            bucket: bucket_of(rule).to_string(),
            recoverable: rule.recoverable.to_string(),
            roots: roots.iter().map(|p| p.display().to_string()).collect(),
            available: !roots.is_empty(),
        });
    }

    ScanReport {
        items,
        total_bytes,
        total_files,
        elapsed_ms: started.elapsed().as_millis() as u64,
        drive,
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

        let roots = expand_rule_roots(rule, drive.as_deref());
        for root in &roots {
            clean_root(rule, root, &mut budget, &mut result);
            // 规则多以 `\*` 结尾，直接拿它 read_dir 会失败；去掉通配段再清理
            // 空目录（只删空的，非空时 remove_dir 自然失败）。
            let (base, _) = split_pattern(root);
            remove_empty_dirs(&base, &mut budget);
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
fn expand_rule_roots(rule: &CleanRule, drive: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for template in rule.paths {
        let Some(expanded) = expand_env(template) else {
            continue;
        };
        if let Some(drive) = drive {
            let root_drive = drive_of(&expanded.display().to_string());
            if !root_drive.eq_ignore_ascii_case(drive) {
                continue;
            }
        }
        roots.push(expanded);
    }
    roots
}

/// 根的形态决定枚举方式：
/// - 以 `*` 结尾 → 清空目录内容（递归）；
/// - 末段含 `*` → 在父目录里按文件名通配匹配；
/// - 其它 → 单个文件或目录（目录按"内容"处理）。
fn split_pattern(path: &Path) -> (PathBuf, Option<String>) {
    let text = path.display().to_string();
    let Some((parent, last)) = text.rsplit_once('\\') else {
        return (path.to_path_buf(), None);
    };
    if !last.contains('*') {
        return (path.to_path_buf(), None);
    }
    (PathBuf::from(parent), Some(last.to_string()))
}

fn scan_root(rule: &CleanRule, root: &Path, budget: &mut Budget) -> (u64, u64) {
    let (base, pattern) = split_pattern(root);
    let mut files = 0u64;
    let mut bytes = 0u64;

    match pattern.as_deref() {
        // `%TEMP%\*` / `...\Cache\*`：递归遍历目录内容。
        Some("*") | None => {
            if base.is_file() && base.exists() {
                if is_deletable(rule, &base) {
                    files += 1;
                    bytes += file_size(&base);
                }
            } else {
                walk(base.as_path(), budget, &mut |path| {
                    if is_deletable(rule, path) {
                        files += 1;
                        bytes += file_size(path);
                    }
                });
            }
        }
        // `thumbcache_*.db`：只匹配直接子项（缓存文件不递归）。
        Some(glob) => {
            let Ok(entries) = fs::read_dir(&base) else {
                return (0, 0);
            };
            for entry in entries.flatten() {
                budget.tick();
                if budget.exhausted() {
                    break;
                }
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if !glob_match(glob, &name) || !is_deletable(rule, &path) {
                    continue;
                }
                files += 1;
                bytes += file_size(&path);
            }
        }
    }

    (files, bytes)
}

fn clean_root(rule: &CleanRule, root: &Path, budget: &mut Budget, result: &mut CleanResult) {
    let (base, pattern) = split_pattern(root);

    let mut targets: Vec<PathBuf> = Vec::new();
    match pattern.as_deref() {
        Some("*") | None => {
            if base.is_file() && base.exists() {
                if is_deletable(rule, &base) {
                    targets.push(base.clone());
                }
            } else {
                walk(base.as_path(), budget, &mut |path| {
                    if is_deletable(rule, path) {
                        targets.push(path.to_path_buf());
                    }
                });
            }
        }
        Some(glob) => {
            if let Ok(entries) = fs::read_dir(&base) {
                for entry in entries.flatten() {
                    budget.tick();
                    if budget.exhausted() {
                        break;
                    }
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if glob_match(glob, &name) && is_deletable(rule, &path) {
                        targets.push(path);
                    }
                }
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

    #[test]
    fn pattern_split_keeps_base_and_glob() {
        let (base, pattern) = split_pattern(Path::new(r"C:\Temp\*"));
        assert_eq!(pattern.as_deref(), Some("*"));
        assert!(base.ends_with("Temp"));

        let (base, pattern) = split_pattern(Path::new(r"C:\Cache\thumbcache_*.db"));
        assert_eq!(pattern.as_deref(), Some("thumbcache_*.db"));
        assert!(base.ends_with("Cache"));
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

    fn fake_rule() -> CleanRule {
        CleanRule {
            id: "test",
            title: "测试",
            hint: "",
            group: "测试",
            paths: &[],
            min_age_minutes: 0,
            needs_admin: false,
            risky: false,
            score: 90,
            recoverable: "不可恢复",
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
        let root = dir.join("*.tmp");
        let mut budget = Budget::new();
        let (files, bytes) = scan_root(&rule, &root, &mut budget);
        assert_eq!((files, bytes), (1, 1024), "只算直接子项里的 *.tmp");

        let mut result = CleanResult {
            freed_bytes: 0,
            deleted: 0,
            skipped: 0,
            failed: 0,
            messages: Vec::new(),
        };
        clean_root(&rule, &root, &mut budget, &mut result);
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
        let (files, bytes) = scan_root(&rule, &dir.join("*"), &mut budget);
        assert_eq!((files, bytes), (2, 300));

        let _ = fs::remove_dir_all(&dir);
    }
}
