//! 本地快速查找（Everything 风格）。
//!
//! # 存储（arena 化，内存是第一约束）
//!
//! 百万级文件的索引绝不能每条一份 `BTreeMap<String, Struct>`——key 路径 +
//! value 路径 + 文件名三份字符串，300 万文件直接吃掉 1GB+（实测教训）。
//! 现在的结构：
//! - `names: String` 全部文件名拼接进一个 arena（NTFS 文件名 ≤255 字符，
//!   (offset: u32, len: u16) 定位）；同目录文件名不重复存储路径。
//! - `dirs: Vec<String>` 目录路径去重共享（interning），一个目录下的
//!   1 万个文件只存一次目录串。
//! - `entries: Vec<Entry>` 定长 32B/条。峰值内存 ≈ 文件数 × ~90B，
//!   300 万文件约 250-300MB（此前 1GB+ 降到约 1/4）。
//!
//! # 并发（速度但不抢 UI）
//!
//! MFT 原始读取需要管理员权限，桌面应用不采用。遍历用
//! `Parallelism::Serial`：**不引 rayon**——rayon 工作线程是池自建的，
//! Windows 新线程不继承创建者优先级，之前设的 BELOW_NORMAL 落不到
//! 干活的线程上（实测教训）。Serial 模式下遍历就发生在当前线程，
//! 优先级设置真正生效；并行度来自「每盘一根线程」。
//!
//! # 匹配
//!
//! `nucleo-matcher`（Helix 模糊算法）。大小写折叠用可复用 scratch
//! 缓冲，逐条零分配。查询在 `spawn_blocking` 上执行，锁内只 clone
//! 最终 ≤200 条。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use nucleo_matcher::{Config, Matcher, Utf32Str};
use serde::Serialize;
use tauri::command;

/// 索引有效期：10 分钟。过期后下次查询触发后台重建，当前查询先用
/// 旧索引回答（宁旧不卡）。
const INDEX_TTL: Duration = Duration::from_secs(600);

/// updates.jsonl 增量流的截断阈值（64 MB）。tail 重放消费后超过即截断
/// 归零——helper 按 append 语义写，截断后从新 EOF 追加。此前只在 helper
/// 重启时清一次，无人消费时实测膨胀到 189 MB。
const UPDATES_TRUNCATE_THRESHOLD: u64 = 64 * 1024 * 1024;

/// 提权 helper 的状态，暴露给前端做"需要授权/失败重试"提示。
///
/// 分类依据是 `usn_index_via_helper` 的错误文案（ShellExecuteW 的 UAC
/// 拒绝/超时不可编程探测，只能按文案归类），文案变更需同步这里。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperState {
    /// helper 可用或从未尝试过（无提示）。
    Ok,
    /// UAC 被拒绝或超时——用户主动授权后即可秒级建索引。
    Unauthorized,
    /// helper 缺失/非 NTFS/其他失败。
    Failed,
}

impl Default for HelperState {
    fn default() -> Self {
        Self::Ok
    }
}

/// 单次返回上限。
const MAX_RESULTS: usize = 200;

/// 子串匹配的最大收集数（".dll" 能命中几十万条，超过这个量级对用户
/// 已无意义，提前收工）。
const MAX_SUBSTRING_COLLECT: usize = 5000;

#[derive(Debug, Clone, Serialize)]
pub struct LocalSearchHit {
    /// 完整路径（Windows 反斜杠形式）。
    pub path: String,
    /// 文件名（含扩展名）。
    pub name: String,
    pub is_dir: bool,
    /// 字节大小；目录为 null。
    pub size: Option<u64>,
    /// 修改时间（Unix 秒）；取不到为 null。
    pub modified_secs: Option<i64>,
}

/// 搜索响应：带索引状态，UI 据此显示"后台索引中，结果可能不全"。
#[derive(Debug, Serialize)]
pub struct LocalSearchResponse {
    pub hits: Vec<LocalSearchHit>,
    /// 当前可见的索引条目数（重建中 = 已发现的文件数）。
    pub indexed: usize,
    pub building: bool,
}

/// 定长条目：32B/文件（含 padding）。
#[derive(Debug, Clone)]
struct Entry {
    /// 在 [`Index::names`] 里的字节偏移。
    name_off: u32,
    /// 文件名字节数（UTF-8）。
    name_len: u16,
    /// 在 [`Index::dirs`] 里的下标。
    dir: u32,
    size: u64,
    mtime: i64,
    is_dir: bool,
    /// NTFS FileReferenceNumber（增量重放的定位键）。jwalk 路径 = 0。
    frn: u64,
}

/// 索引来源。决定 TTL 语义：jwalk 全量没有增量保鲜，受 `INDEX_TTL` 约束；
/// USN 路径有 tail 重放持续保鲜，不做周期性全量重建——每次重建都意味着
/// 再弹一次 UAC，正是把用户推向「拒绝授权」的元凶（见笔记
/// 2026-10-05-local-index-uac-chain）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum IndexSource {
    #[default]
    Jwalk,
    Usn,
}

#[derive(Debug, Default)]
struct Index {
    /// 所有文件名拼接的 arena。
    names: String,
    /// 目录路径（原始大小写，去重）。下标 0 保留给"无父目录"。
    dirs: Vec<String>,
    entries: Vec<Entry>,
    /// frn → entries 下标（USN 增量重放用；jwalk 路径为空）。
    frn_map: std::collections::HashMap<u64, u32>,
    /// 目录 frn → 自身完整路径的 dirs 下标（重放 add 时定位父目录）。
    frn_dirs: std::collections::HashMap<u64, u32>,
    /// 索引来源（决定 TTL 语义，见 [`IndexSource`]）。
    source: IndexSource,
}

impl Index {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn name(&self, e: &Entry) -> &str {
        &self.names[e.name_off as usize..e.name_off as usize + e.name_len as usize]
    }

    /// 完整显示路径 = 目录 + 反斜杠 + 文件名。
    fn path(&self, e: &Entry) -> String {
        let dir = &self.dirs[e.dir as usize];
        let name = self.name(e);
        if dir.ends_with('\\') {
            format!("{dir}{name}")
        } else {
            format!("{dir}\\{name}")
        }
    }
}

struct FileIndex {
    index: Index,
    /// 建成时间。None = 从未建成。
    built_at: Option<Instant>,
    building: bool,
    /// 最近一次 helper 尝试的结果（前端"需要授权/重试"提示的数据源）。
    helper_state: HelperState,
}

impl FileIndex {
    fn empty() -> Self {
        Self {
            index: Index::default(),
            built_at: None,
            building: false,
            helper_state: HelperState::Ok,
        }
    }
}

static INDEX: OnceLock<Mutex<FileIndex>> = OnceLock::new();
/// 后台重建代数：防止旧线程的完成结果覆盖新一轮请求触发的重建。
static INDEX_GEN: AtomicU64 = AtomicU64::new(0);
/// 重建期间的进度计数（已发现的文件数）。
static PROGRESS: AtomicU64 = AtomicU64::new(0);
/// 提权 helper 本进程内是否已判定不可用（UAC 被拒 / exe 缺失 / 非 NTFS）。
/// 失败一次后不再重试：重试 = 再弹一次 UAC，对用户是骚扰、对磁盘是空转。
static HELPER_FAILED: AtomicBool = AtomicBool::new(false);
/// 启动自动扫描是否已发起过（只发一次，失败也不再自动重试）。
static AUTO_SCANNED: AtomicBool = AtomicBool::new(false);

fn index_cell() -> &'static Mutex<FileIndex> {
    INDEX.get_or_init(|| Mutex::new(FileIndex::empty()))
}

/// 要索引的根目录：本机所有固定盘的根（从 'C' 起，跳过 A:/B: 软盘
/// 遗留）。只收 `DRIVE_FIXED`——可移动盘/网络盘慢且无意义，失效映射盘
/// （subst 残留、断连的网络映射）会让 helper 枚举报 0x80070002 噪音。
fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for letter in 'C'..='Z' {
        let root = PathBuf::from(format!("{letter}:\\"));
        if root.is_dir() && is_fixed_drive(&root) {
            roots.push(root);
        }
    }
    roots
}

#[cfg(windows)]
fn is_fixed_drive(root: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    // windows 0.58 的常量分布：GetDriveTypeW 在 Storage::FileSystem，
    // DRIVE_* 常量在 System::WindowsProgramming（Cargo.toml 已开对应 feature）。
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: wide 以 NUL 结尾；GetDriveTypeW 无副作用。
    unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) == DRIVE_FIXED }
}

#[cfg(not(windows))]
fn is_fixed_drive(_root: &Path) -> bool {
    true
}

/// 大小写折叠进可复用缓冲，返回折叠后的 &str。逐条零分配的关键。
fn fold_into<'a>(buf: &'a mut String, s: &str) -> &'a str {
    buf.clear();
    buf.extend(s.chars().flat_map(|c| c.to_lowercase()));
    buf.as_str()
}

/// 当前线程降为 BELOW_NORMAL 优先级。在盘根遍历线程开头调用：
/// jwalk 的 rayon 工作线程由该线程派生，Windows 上子线程继承创建者的
/// 优先级，于是整个扫描都让位于 UI。扫描仍然快——只是 CPU 调度上
/// 排在交互线程之后（Everything 同款策略）。
#[cfg(windows)]
fn lower_current_thread_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn lower_current_thread_priority() {}

/// 单盘遍历：产出该盘的 arena 分片。Serial 遍历发生在当前线程
/// （不引 rayon——优先级才能落到干活线程上，见模块注释）；
/// 并行度 = 每盘一根线程；线程优先级 BELOW_NORMAL。
fn walk_root(root: &Path) -> Index {
    lower_current_thread_priority();
    let mut index = Index::default();
    index.dirs.push(String::new()); // 0 号占位，下标从 1 起
    let mut dir_ids: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        // Serial：遍历在当前（低优先级）线程上执行，不引 rayon。
        .parallelism(jwalk::Parallelism::Serial)
        .process_read_dir(|_, _, _, dir_entries| {
            dir_entries.retain(|e| e.as_ref().is_ok());
        });

    let mut local_progress = 0u64;
    for entry in walker.into_iter().flatten() {
        if entry.depth() >= 64 {
            continue;
        }
        let parent = entry.parent_path();
        let raw_name = entry.file_name();
        let name = raw_name.to_string_lossy();
        if name.is_empty() {
            continue;
        }

        // 目录 interning：同一目录只存一次路径串。
        let parent_key = parent.to_string_lossy().into_owned();
        let dir_id = match dir_ids.get(&parent_key) {
            Some(&id) => id,
            None => {
                let id = index.dirs.len() as u32;
                index.dirs.push(parent_key.clone());
                dir_ids.insert(parent_key, id);
                id
            }
        };

        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|md| md.is_dir()).unwrap_or(false);
        let (size, mtime) = match &meta {
            Some(md) => (
                if md.is_dir() { 0 } else { md.len() },
                md.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            ),
            None => (0, 0),
        };

        let name_off = index.names.len() as u32;
        index.names.push_str(&name);
        let name_len = (index.names.len() as u64 - name_off as u64).min(u16::MAX as u64) as u16;
        index.entries.push(Entry {
            name_off,
            name_len,
            dir: dir_id,
            size,
            mtime,
            is_dir,
            frn: 0,
        });

        local_progress += 1;
        if local_progress % 4096 == 0 {
            PROGRESS.fetch_add(4096, Ordering::Relaxed);
        }
    }
    PROGRESS.fetch_add(local_progress % 4096, Ordering::Relaxed);
    index
}

/// 全量重建：优先走提权 helper 的 MFT/USN 枚举（秒级，Everything 路线），
/// helper 不可用（未授权/非管理员拒绝/非 NTFS）时按 `allow_jwalk` 决定是否
/// 回退 jwalk 目录遍历（分钟级）。
fn rebuild_index(gen: u64, allow_jwalk_fallback: bool) {
    let mut merged = match usn_index_via_helper() {
        Ok(mut idx) => {
            // helper 路线可用：启动增量重放线程（幂等，仅首访实际创建）。
            #[cfg(windows)]
            usn_helper::spawn_tail_replay();
            idx.source = IndexSource::Usn;
            idx
        }
        Err(e) => {
            eprintln!("[local_search] USN helper unavailable: {e}");
            if !allow_jwalk_fallback {
                // 自动路径：就此打住，索引留空。启动时用户没发起过任何请求，
                // 一次全盘 jwalk 要霸占磁盘几分钟 —— 自动工作只做「便宜或不做」。
                // 真正需要结果时（打开查找面板）走 local_search，仍有 jwalk 兜底。
                if let Ok(mut cell) = index_cell().lock() {
                    cell.building = false;
                }
                return;
            }
            fallback_jwalk_index()
        }
    };

    // 过代检查：期间又有新重建请求（gen 已前进）则放弃旧结果。
    if INDEX_GEN.load(Ordering::Acquire) != gen {
        if let Ok(mut cell) = index_cell().lock() {
            cell.building = false;
        }
        return;
    }
    if let Ok(mut cell) = index_cell().lock() {
        std::mem::swap(&mut cell.index, &mut merged);
        cell.built_at = Some(Instant::now());
        cell.building = false;
    }
}

// ── 提权 helper 客户端（USN/MFT 路线） ────────────────────────────────
//
// 交接协议（无管道：提权边界上 std 管道 API 需要 DACL/overlapped 全套，
// 临时文件交接语义简单且 SSD 写 300MB 仅数秒）：
//   helper（提权）写 snapshot-<ts>.jsonl（原始记录：frn/parent/name/
//   is_dir/ts）+ done.json（{"ok":..,"indexed":..,"snapshot":..}）。
//   helper 常驻：之后继续写 updates.jsonl 增量流（见 spawn_tail_replay）。
//   主进程轮询 done.json 出现 → 流式读 snapshot → 删 snapshot。
// 拉起方式：ShellExecuteW("runas") 触发 UAC；用户拒绝/超时 → 回退 jwalk。
// 主进程重启时若 helper.alive 新鲜（<60s）则不重复拉起（免二次 UAC），
// 直接读现有 snapshot/updates。

#[cfg(windows)]
mod usn_helper {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};

    /// helper 的交换目录（固定路径，与 helper 约定一致）。
    pub fn exchange_dir() -> PathBuf {
        std::env::temp_dir().join("hermix-usn-index")
    }

    /// 定位 usn-indexer.exe。多级 fallback：
    /// 1. current_exe 同目录（打包布局：主程序与 helper 同目录，Cargo 同
    ///    crate 多 bin 本就产到一起）
    /// 2. src-tauri/target/debug —— dev 布局：tauri dev / 部分构建流把主
    ///    程序产到别处（如 CARGO_TARGET_DIR=release/target），而 src-tauri
    ///    自己的 target 里躺着 `cargo build -p hermix-desktop` 的产物
    /// 3. 仓库根 target/debug —— 纯 cargo 默认布局
    /// 生产安装目录永远是情形 1；2/3 只为开发期省一次「去别的 target 目录
    /// 手动拷 exe」。
    pub fn locate_helper() -> Option<PathBuf> {
        let exe_name = "usn-indexer.exe";
        let mut candidates: Vec<PathBuf> = Vec::with_capacity(3);
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join(exe_name));
            }
        }
        // CARGO_MANIFEST_DIR = apps/desktop-tauri/src-tauri（编译期常量）。
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        candidates.push(manifest.join("target/debug").join(exe_name));
        // 仓库根 = manifest 的上两级。
        if let Some(repo) = manifest.parent().and_then(|p| p.parent()) {
            candidates.push(repo.join("target/debug").join(exe_name));
        }
        candidates.into_iter().find(|p| p.exists())
    }

    /// done.json 内容。
    #[derive(serde::Deserialize)]
    pub struct Done {
        pub ok: bool,
        pub indexed: usize,
        pub snapshot: Option<String>,
        pub error: Option<String>,
    }

    /// snapshot 行的原始记录。
    ///
    /// `op` 字段已删除：更新流走的是独立的 `UpdLine`（见下），本结构只
    /// 解析 snapshot，而 helper 写 snapshot 时本就不带 op。留着它只会让
    /// "snapshot/updates 共用" 这句注释继续骗人。
    #[derive(serde::Deserialize)]
    pub struct RawLine {
        #[serde(default)]
        pub frn: u64,
        #[serde(default)]
        pub parent: u64,
        pub name: String,
        #[serde(default)]
        pub is_dir: bool,
        #[serde(default)]
        pub ts: i64,
    }

    /// ShellExecuteW("runas") 拉起提权 helper。失败 = UAC 被拒/启动失败。
    fn spawn_elevated(helper: &Path, out_dir: &Path, volumes: &[String]) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let to_wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let op = to_wide("runas");
        let file = to_wide(&helper.to_string_lossy());
        let params = to_wide(&format!(
            "\"{}\" {}",
            out_dir.to_string_lossy(),
            volumes.join(" ")
        ));
        let h = unsafe {
            ShellExecuteW(
                HWND::default(),
                PCWSTR(op.as_ptr()),
                PCWSTR(file.as_ptr()),
                PCWSTR(params.as_ptr()),
                PCWSTR::null(),
                SW_HIDE,
            )
        };
        if h.is_invalid() {
            return Err("UAC 被拒绝或 helper 启动失败".into());
        }
        Ok(())
    }

    /// helper 是否已驻留（alive 文件 60s 内新鲜）→ 主进程重启免二次 UAC。
    fn helper_resident(dir: &Path) -> bool {
        dir.join("helper.alive")
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(60))
            .unwrap_or(false)
    }

    /// 主进程心跳线程：每 5s touch main.alive（helper 见不到新鲜心跳就自杀）。
    pub fn spawn_heartbeat() {
        let dir = exchange_dir();
        std::thread::spawn(move || {
            let alive = dir.join("main.alive");
            loop {
                let _ = std::fs::write(&alive, b"1");
                std::thread::sleep(Duration::from_secs(5));
            }
        });
    }

    /// 主进程**正常退出**时调用：让 helper 立刻退出，不等 60s 心跳超时。
    ///
    /// 为什么必须有这条：helper 是提权进程，退出前一直锁着自己的镜像文件。
    /// 用户「关掉应用就重装 / 升级」时 NSIS 要覆写 usn-indexer.exe，会直接报
    /// 「无法打开要写入的文件」。主进程非提权，**不能**用 TerminateProcess
    /// 去杀它（OpenProcess 会 ACCESS_DENIED），所以只能走文件信号。
    ///
    /// 崩溃 / 强杀走不到这里，由 helper 侧 60s 心跳陈旧规则兜底。
    pub fn request_helper_shutdown() {
        let _ = std::fs::create_dir_all(exchange_dir());
        let _ = std::fs::write(exchange_dir().join("shutdown.request"), b"1");
    }

    /// 一个 arena 索引 + frn 映射。snapshot 灌入与 updates 重放共用。
    pub struct Rebuilt {
        pub index: Index,
    }

    /// updates.jsonl 增量行（helper 侧协议见 usn_indexer.rs 头注释）。
    #[derive(serde::Deserialize)]
    pub struct UpdLine {
        pub op: String,
        #[serde(default)]
        pub frn: u64,
        #[serde(default)]
        pub parent: u64,
        #[serde(default)]
        pub name: String,
        #[serde(default)]
        pub is_dir: bool,
        #[serde(default)]
        pub ts: i64,
    }

    /// tail updates.jsonl 重放线程：把 helper 的增删事件物理应用到内存
    /// 索引（frn 定位条目）。文件从尾部读，读完后轮询新行；从偏移 0 起
    /// 只在首次（复用驻留 helper 时补齐错过的增量）。
    ///
    /// OnceLock 守卫：真正"仅首访创建"。此前每次 helper 成功重建都会
    /// spawn 一个新线程——TTL 曾每 10 分钟触发一次重建，线程数随之增长，
    /// 且每个新线程从 offset 0 重读整个增量文件。
    pub fn spawn_tail_replay() {
        static TAIL_SPAWNED: OnceLock<()> = OnceLock::new();
        if TAIL_SPAWNED.set(()).is_err() {
            return;
        }
        let dir = exchange_dir();
        std::thread::spawn(move || {
            let updates = dir.join("updates.jsonl");
            let mut offset: u64 = 0;
            loop {
                std::thread::sleep(Duration::from_millis(2000));
                let Ok(meta) = std::fs::metadata(&updates) else { continue };
                let len = meta.len();
                if len <= offset {
                    continue; // 无新数据（或日志被 helper 清空重开）
                }
                if offset == 0 {
                    // 首轮只追尾：跳过历史（全量 snapshot 已含），除非
                    // helper 是复用的驻留进程——历史即错过的增量，须重放。
                    // 简化：首轮从 0 读，全量重建晚于历史事件时只是重复
                    // add（frn 覆盖，幂等）/ del（frn 不存在，跳过）。
                }
                let Ok(file) = std::fs::File::open(&updates) else { continue };
                use std::io::{Seek, SeekFrom};
                let mut reader = BufReader::new(file);
                if reader.seek(SeekFrom::Start(offset)).is_err() {
                    continue;
                }
                let mut consumed = offset;
                let mut lines = String::new();
                // 只读上次见过的范围（snapshot 重建时 helper 会 truncate，
                // len < offset 的情况上面已 continue 掉）。
                loop {
                    lines.clear();
                    match reader.read_line(&mut lines) {
                        Ok(0) => break,
                        Ok(n) => {
                            consumed += n as u64;
                            if let Ok(u) = serde_json::from_str::<UpdLine>(&lines) {
                                replay_update(&u);
                            }
                        }
                        Err(_) => break,
                    }
                }
                offset = consumed;
                // 增量流治理：消费后文件超阈值即截断归零。helper 的 append
                // 句柄（FILE_APPEND_DATA）每次写都落在当时 EOF，截断后从
                // 新 EOF 追加，offset 同步归零即可。极端时序下可能丢一批
                // 刚写入的增量——resnap 全量校正兜底（见笔记 Consequences）。
                // 此前无治理：文件只在 helper 重启时清一次，实测膨胀到 189MB。
                if consumed > UPDATES_TRUNCATE_THRESHOLD {
                    if std::fs::File::options()
                        .write(true)
                        .truncate(true)
                        .open(&updates)
                        .is_ok()
                    {
                        offset = 0;
                    }
                }
            }
        });
    }

    /// 应用一条增量到全局索引（锁内，O(1) 定位）。
    fn replay_update(u: &UpdLine) {
        let Ok(mut cell) = index_cell().lock() else { return };
        let idx = &mut cell.index;
        match u.op.as_str() {
            "del" => {
                if let Some(eid) = idx.frn_map.remove(&u.frn) {
                    // swap_remove：O(1)。arena 里的 name/dir 留成死数据
                    // （小碎片，量可忽略），不整体重排。
                    let last = idx.entries.len() - 1;
                    if eid as usize != last {
                        idx.entries.swap(eid as usize, last);
                        let moved = idx.entries[eid as usize].frn;
                        if moved != 0 {
                            idx.frn_map.insert(moved, eid);
                        }
                    }
                    idx.entries.pop();
                }
            }
            "add" => {
                if u.frn == 0 || u.name.is_empty() {
                    return;
                }
                // frn 已存在 → 同名覆盖（rename new-name 场景）。
                let parent = u.parent;
                let name = u.name.clone();
                let is_dir = u.is_dir;
                let ts = u.ts;
                if let Some(&eid) = idx.frn_map.get(&u.frn) {
                    // 原地改名/移动：更新 name arena 追加 + 指向新副本。
                    let dir_id = resolve_parent_dir(idx, parent, &name);
                    let name_off = idx.names.len() as u32;
                    idx.names.push_str(&name);
                    let name_len =
                        (idx.names.len() as u64 - name_off as u64).min(u16::MAX as u64) as u16;
                    let e = &mut idx.entries[eid as usize];
                    e.name_off = name_off;
                    e.name_len = name_len;
                    e.dir = dir_id;
                    e.mtime = ts;
                    e.is_dir = is_dir;
                } else {
                    let dir_id = resolve_parent_dir(idx, parent, &name);
                    let name_off = idx.names.len() as u32;
                    idx.names.push_str(&name);
                    let name_len =
                        (idx.names.len() as u64 - name_off as u64).min(u16::MAX as u64) as u16;
                    idx.entries.push(Entry {
                        name_off,
                        name_len,
                        dir: dir_id,
                        size: 0,
                        mtime: ts,
                        is_dir,
                        frn: u.frn,
                    });
                    idx.frn_map.insert(u.frn, (idx.entries.len() - 1) as u32);
                    if is_dir {
                        idx.frn_dirs.insert(u.frn, dir_id);
                    }
                }
            }
            _ => {} // journal_broken 等：等 resnap 全量校正
        }
    }

    /// 由 parent frn 找父目录的 dirs 下标；找不到归入未知桶 ""（0 号）。
    fn resolve_parent_dir(idx: &mut Index, parent: u64, _name: &str) -> u32 {
        if parent == 0 {
            return 0;
        }
        if let Some(&id) = idx.frn_dirs.get(&parent) {
            return id;
        }
        0
    }

    /// 完成 USN 索引：复用/拉起 helper → 轮询 done → 读流灌 arena。
    /// 任何一步失败都返回 Err，由调用方回退 jwalk。
    pub fn run(volumes: &[String]) -> Result<Rebuilt, String> {
        let helper = locate_helper().ok_or(
            "helper 不存在：current_exe 同目录、src-tauri/target/debug、\
             仓库根 target/debug 均未找到 usn-indexer.exe",
        )?;
        let dir = exchange_dir();
        let resident = helper.exists() && helper_resident(&dir);
        if !resident {
            if !helper.exists() {
                return Err(format!("helper 不存在: {}", helper.display()));
            }
            std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
            // 清理上一代交接文件（helper 自己也会清 done，双保险）。
            let _ = std::fs::remove_file(dir.join("done.json"));
            let _ = std::fs::remove_file(dir.join("resnap.request"));
            // 上一轮主进程退出时写的退出信号必须先清掉，否则刚拉起的 helper
            // 会在第一个轮询 tick（1s 内）读到它并立刻自杀。
            let _ = std::fs::remove_file(dir.join("shutdown.request"));
            spawn_elevated(&helper, &dir, volumes)?;
        }
        spawn_heartbeat();

        // 轮询 done.json：上限 300s（覆盖 UAC 等待 + 全盘枚举）。
        // 只接受"新于本次轮询起点"的 done.json（驻留复用时旧 done 的
        // snapshot 已被上次主进程读走删除，陈旧 done 会让这里干等 300s
        // 且进度恒 0）——此时发 resnap.request 让 helper 重新全量枚举。
        // 等待期间按 snapshot 文件增长量估算进度（每条记录约 90B）。
        let done_obj: Done = {
            let done = dir.join("done.json");
            let deadline = Instant::now() + Duration::from_secs(300);
            let poll_start = std::time::SystemTime::now();
            let done_fresh = |dir: &Path| -> bool {
                std::fs::metadata(dir.join("done.json"))
                    .and_then(|m| m.modified())
                    .map(|t| t >= poll_start)
                    .unwrap_or(false)
            };
            if resident && !done_fresh(&dir) {
                let _ = std::fs::write(dir.join("resnap.request"), b"1");
            }
            let snap_probe = |dir: &Path| -> Option<PathBuf> {
                // 取最新的 snapshot-<ts>.jsonl（文件名含时间戳，字典序即时间序）。
                std::fs::read_dir(dir)
                    .ok()?
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("snapshot-"))
                    .max_by_key(|e| e.file_name())
                    .map(|e| e.path())
            };
            let mut last_est: u64 = 0;
            let mut resnap_deadline: Option<Instant> = None;
            loop {
                if Instant::now() > deadline {
                    return Err("helper 超时（300s）".into());
                }
                // 驻留 helper 25s 内未响应 resnap（旧版本 helper 不认识
                // 该协议）→ 放弃，回退 jwalk。
                if let Some(dl) = resnap_deadline {
                    if Instant::now() > dl {
                        return Err("驻留 helper 未响应重枚举请求".into());
                    }
                }
                if done_fresh(&dir) {
                    if let Ok(text) = std::fs::read_to_string(&done) {
                        if let Ok(d) = serde_json::from_str::<Done>(&text) {
                            if !d.ok {
                                break d; // helper 明确失败
                            }
                            match &d.snapshot {
                                Some(s) if dir.join(s).exists() => break d,
                                _ => {
                                    // done 新鲜但 snapshot 缺失：等枚举写盘
                                    if resnap_deadline.is_none() && resident {
                                        resnap_deadline =
                                            Some(Instant::now() + Duration::from_secs(25));
                                    }
                                }
                            }
                        }
                    }
                } else if resident && resnap_deadline.is_none() {
                    // done 一直不新鲜：可能旧 helper 不支持 resnap。
                    resnap_deadline = Some(Instant::now() + Duration::from_secs(25));
                    let _ = std::fs::write(dir.join("resnap.request"), b"1");
                }
                if let Some(snap) = snap_probe(&dir) {
                    if let Ok(meta) = std::fs::metadata(&snap) {
                        let est = meta.len() / 90;
                        if est > last_est {
                            // 绝对值覆盖（估算含读取误差，不能累加）。
                            PROGRESS.fetch_add(est - last_est, Ordering::Relaxed);
                            last_est = est;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        };
        if !done_obj.ok {
            return Err(format!(
                "helper 失败: {}",
                done_obj.error.unwrap_or_else(|| "unknown".into())
            ));
        }
        let snapshot = dir.join(done_obj.snapshot.as_deref().unwrap_or(""));

        // 流式读 snapshot：先收原始记录，再两遍完成路径重建。
        // 第一遍：frn → (parent, name, is_dir, ts)；第二遍自顶向下拼路径。
        struct Raw {
            parent: u64,
            name: String,
            is_dir: bool,
            ts: i64,
        }
        let mut raws: HashMap<u64, Raw> = HashMap::with_capacity(done_obj.indexed + 1024);
        let file = std::fs::File::open(&snapshot).map_err(|e| format!("open snapshot: {e}"))?;
        let mut progress: u64 = 0;
        for line in BufReader::with_capacity(1 << 20, file).lines() {
            let Ok(line) = line else { break };
            if line.is_empty() {
                continue;
            }
            let Ok(r) = serde_json::from_str::<RawLine>(&line) else { continue };
            raws.insert(
                r.frn,
                Raw { parent: r.parent, name: r.name, is_dir: r.is_dir, ts: r.ts },
            );
            progress += 1;
            if progress % 4096 == 0 {
                PROGRESS.fetch_add(4096, Ordering::Relaxed);
            }
        }
        PROGRESS.fetch_add((progress % 4096) as u64, Ordering::Relaxed);
        let _ = std::fs::remove_file(&snapshot); // ~300MB 临时数据不滞留

        // 第二遍：迭代重建每个 FRN 的完整路径（memoized，全量摊销 O(n)）。
        let mut paths: HashMap<u64, String> = HashMap::with_capacity(raws.len());
        fn path_of(frn: u64, raws: &HashMap<u64, Raw>, paths: &mut HashMap<u64, String>) -> String {
            if let Some(p) = paths.get(&frn) {
                return p.clone();
            }
            let Some(r) = raws.get(&frn) else { return String::new() };
            let parent_path = if r.parent == 0 || r.parent == frn {
                String::new()
            } else {
                path_of(r.parent, raws, paths)
            };
            let full = if parent_path.is_empty() {
                r.name.clone()
            } else {
                format!("{parent_path}\\{}", r.name)
            };
            paths.insert(frn, full.clone());
            full
        }
        for &frn in raws.keys() {
            path_of(frn, &raws, &mut paths);
        }

        // 第三遍：灌 arena。目录先登记 frn_dirs（add 重放依赖）。
        let mut index = Index::default();
        index.dirs.push(String::new()); // 0 号占位
        let mut dir_ids: HashMap<String, u32> = HashMap::new();
        // 卷根（C:\ 等）作为目录路径直接进 dirs。
        let mut sorted: Vec<(&u64, &Raw)> = raws.iter().map(|(k, v)| (k, v)).collect();
        sorted.sort_by_key(|(_, r)| r.name.len()); // 稳定顺序（无所谓，纯确定性）
        for (&frn, r) in sorted.iter() {
            let full = paths[&frn].clone();
            if r.is_dir {
                let id = match dir_ids.get(&full) {
                    Some(&id) => id,
                    None => {
                        let id = index.dirs.len() as u32;
                        index.dirs.push(full.clone());
                        dir_ids.insert(full, id);
                        id
                    }
                };
                let name_off = index.names.len() as u32;
                index.names.push_str(&r.name);
                let name_len =
                    (index.names.len() as u64 - name_off as u64).min(u16::MAX as u64) as u16;
                index.entries.push(Entry {
                    name_off,
                    name_len,
                    dir: id,
                    size: 0,
                    mtime: r.ts,
                    is_dir: true,
                    frn,
                });
                let eid = (index.entries.len() - 1) as u32;
                index.frn_map.insert(frn, eid);
                index.frn_dirs.insert(frn, id); // frn → 自己路径的 dirs 下标
            }
        }
        // 文件条目（父目录 frn_dirs 已就绪）。
        for (&frn, r) in sorted.iter() {
            if r.is_dir {
                continue;
            }
            let full = &paths[&frn];
            let (dir_key, name) = match full.rsplit_once('\\') {
                Some((p, n)) => (p.to_string(), n.to_string()),
                None => (String::new(), full.clone()),
            };
            let dir_id = match dir_ids.get(&dir_key) {
                Some(&id) => id,
                None => {
                    let id = index.dirs.len() as u32;
                    index.dirs.push(dir_key.clone());
                    dir_ids.insert(dir_key, id);
                    id
                }
            };
            let name_off = index.names.len() as u32;
            index.names.push_str(&name);
            let name_len = (index.names.len() as u64 - name_off as u64).min(u16::MAX as u64) as u16;
            index.entries.push(Entry {
                name_off,
                name_len,
                dir: dir_id,
                size: 0,
                mtime: r.ts,
                is_dir: false,
                frn,
            });
            index.frn_map.insert(frn, (index.entries.len() - 1) as u32);
        }
        Ok(Rebuilt { index })
    }
}

/// UAC 拒绝/超时的错误文案片段。ShellExecuteW("runas") 的用户拒绝在
/// Win32 层不可编程区分，只能按我们自己的错误文案归类；文案变更时
/// 同步这里（见 `spawn_elevated` 的 "UAC 被拒绝或 helper 启动失败"）。
const UAC_DENIED_MARKERS: &[&str] = &["UAC"];

/// 把 helper 失败归类为前端可呈现的状态。默认 Failed；错误文案含
/// UAC 标记的视为用户未授权。
fn classify_helper_error(e: &str) -> HelperState {
    let upper = e.to_ascii_uppercase();
    if UAC_DENIED_MARKERS.iter().any(|m| upper.contains(m)) {
        HelperState::Unauthorized
    } else {
        HelperState::Failed
    }
}

/// 记录一次 helper 失败：置标志 + 落状态。自动路径据此当轮放弃重试
/// （重试 = 再弹一次 UAC，对用户是骚扰、对磁盘是空转）。
fn record_helper_failure(e: String) -> String {
    HELPER_FAILED.store(true, Ordering::Release);
    if let Ok(mut cell) = index_cell().lock() {
        cell.helper_state = classify_helper_error(&e);
    }
    e
}

/// 用户显式点"重新授权/重试"：清失败标记 + 状态复位，允许再弹一次 UAC。
fn reset_helper_failure() {
    HELPER_FAILED.store(false, Ordering::Release);
    if let Ok(mut cell) = index_cell().lock() {
        cell.helper_state = HelperState::Ok;
    }
}

/// 走提权 helper 建索引。非 Windows / helper 失败时返回 Err，并把失败
/// 记进 `HELPER_FAILED` —— 之后自动路径不再重试；用户可在查找面板点
/// 「重新授权」显式重试（见 `reset_helper_failure`）。
#[cfg(windows)]
fn usn_index_via_helper() -> Result<Index, String> {
    if HELPER_FAILED.load(Ordering::Acquire) {
        return Err("helper 本进程内已失败，不再重试".into());
    }
    let volumes: Vec<String> = search_roots()
        .into_iter()
        .map(|p| {
            p.to_string_lossy()
                .trim_end_matches('\\')
                .trim_end_matches(':')
                .to_string()
        })
        .collect();
    if volumes.is_empty() {
        return Err(record_helper_failure("未发现 NTFS 卷".into()));
    }
    usn_helper::run(&volumes)
        .map(|r| r.index)
        .map_err(record_helper_failure)
}

#[cfg(not(windows))]
fn usn_index_via_helper() -> Result<Index, String> {
    Err("USN helper 仅支持 Windows".into())
}

/// jwalk 回退路径：每盘一个系统线程串行遍历（BELOW_NORMAL 优先级）。
fn fallback_jwalk_index() -> Index {
    let roots = search_roots();
    let shards: Vec<Index> = std::thread::scope(|s| {
        let handles: Vec<_> = roots.iter().map(|r| s.spawn(move || walk_root(r))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    let mut merged = Index::default();
    merged.dirs.push(String::new());
    // 合并：重排 dir 下标（各分片的 dirs 独立编号）。
    for shard in shards {
        let dir_base = merged.dirs.len() as u32;
        merged.dirs.extend(shard.dirs.into_iter().skip(1));
        let name_base = merged.names.len() as u32;
        merged.names.push_str(&shard.names);
        for mut e in shard.entries {
            e.dir += dir_base;
            e.name_off += name_base;
            merged.entries.push(e);
        }
    }
    merged
}

/// 确保索引可用：未建或过期时在后台触发重建，立即返回。
/// `allow_jwalk_fallback` = false 时只用提权 helper（见 [`rebuild_index`]）。
/// `force` = true 时无视 TTL 与既有索引（"重新授权"按钮用：不管当前
/// 有没有 jwalk 旧索引，都按用户意愿再走一次提权路线）。
fn ensure_index(allow_jwalk_fallback: bool, force: bool) {
    let mut cell = match index_cell().lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    if cell.building
        || (!force
            // TTL 只约束 jwalk 全量索引：USN 路径有 tail 重放持续保鲜，
            // 周期性全量重建 = 周期性再弹 UAC（用户拒绝授权的元凶）。
            && cell.index.source == IndexSource::Jwalk
            && cell.built_at.map_or(false, |t| t.elapsed() < INDEX_TTL))
    {
        return;
    }
    cell.building = true;
    drop(cell);
    PROGRESS.store(0, Ordering::Relaxed);
    let gen = INDEX_GEN.fetch_add(1, Ordering::AcqRel);
    // catch_unwind：walker panic 也必须复位 building，否则索引永远
    // 卡在"重建中"，之后所有查询返回空。
    std::thread::spawn(move || {
        if std::panic::catch_unwind(|| rebuild_index(gen, allow_jwalk_fallback)).is_err() {
            if let Ok(mut cell) = index_cell().lock() {
                cell.building = false;
            }
        }
    });
}

/// 应用启动时调用：**自动**在后台把本机文件索引建好，用户第一次打开
/// 查找面板就已经能搜（此前要等面板里敲一下字才触发，白等好几分钟）。
///
/// 只走提权 helper（MFT/USN，秒级）。helper 不可用时索引留空而不回退
/// jwalk —— 自动的全盘遍历会在用户毫无请求时霸占磁盘；那次代价留给
/// 用户显式打开面板的路径去付（见 [`local_search`]）。全程不阻塞启动。
pub fn start_background_index() {
    if AUTO_SCANNED.swap(true, Ordering::AcqRel) {
        return;
    }
    eprintln!("[local_search] background index scan starting");
    ensure_index(false, false);
}

/// 主进程退出时调用：让常驻的提权 helper 立刻退出（见
/// [`usn_helper::request_helper_shutdown`] 的「为什么必须有这条」）。
///
/// 非 Windows / helper 从未拉起时是空操作。必须在窗口 Destroyed 时调 —— 那是
/// 主进程生命周期里最后一个还能执行代码的点。
pub fn shutdown_background_index() {
    #[cfg(windows)]
    usn_helper::request_helper_shutdown();
}

/// 纯同步搜索执行体（spawn_blocking 上跑）。锁内完成匹配与打分，
/// 只 clone 最终入选的 ≤200 条。
fn search_blocking(query: String, ext: Option<String>, dirs_only: bool) -> LocalSearchResponse {
    let ext = ext.map(|e| e.trim_start_matches('.').to_lowercase());
    let ql = query.trim().to_lowercase();

    let cell = match index_cell().lock() {
        Ok(c) => c,
        Err(_) => {
            return LocalSearchResponse { hits: Vec::new(), indexed: 0, building: false }
        }
    };
    let building = cell.building;
    let indexed = if building {
        PROGRESS.load(Ordering::Relaxed) as usize
    } else {
        cell.index.len()
    };
    let idx = &cell.index;

    let mut hits: Vec<LocalSearchHit> = Vec::new();
    // 大小写折叠 scratch：整个搜索过程复用一份缓冲。dir_scratch 单独一份，
    // 因为路径查询的目录片段要在整轮循环里保持折叠态（见 substring 分支）。
    let mut scratch = String::with_capacity(1024);
    let mut dir_scratch = String::with_capacity(256);

    if ql.is_empty() {
        // 空查询：最近修改优先，select_nth 部分排序。
        let mut scored: Vec<(i64, u32)> = idx
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !dirs_only || e.is_dir)
            .filter(|(_, e)| match &ext {
                Some(x) => {
                    let name = idx.name(e);
                    name.len() > x.len() + 1
                        && name[name.len() - x.len() - 1..].starts_with('.')
                        && name[name.len() - x.len()..].eq_ignore_ascii_case(x)
                }
                None => true,
            })
            .map(|(i, e)| (e.mtime, i as u32))
            .collect();
        if scored.len() > MAX_RESULTS {
            scored.select_nth_unstable_by(MAX_RESULTS - 1, |a, b| b.0.cmp(&a.0));
            scored.truncate(MAX_RESULTS);
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, i) in scored {
            let e = &idx.entries[i as usize];
            hits.push(LocalSearchHit {
                path: idx.path(e),
                name: idx.name(e).to_owned(),
                is_dir: e.is_dir,
                size: if e.is_dir { None } else { Some(e.size) },
                modified_secs: Some(e.mtime),
            });
        }
    } else if ql.contains('\\') || ql.contains('.') {
        // 路径/扩展名习惯：大小写不敏感子串（零分配折叠）。
        //
        // 带反斜杠的查询按**路径**语义拆：最后一个 `\` 之前是目录片段，
        // 之后是文件名片段。此前这里只分别匹配 name 和 dir，跨分隔符的
        // 查询（`sub\beta`）两边都不含反斜杠，永远匹配不上 —— 搜索框
        // placeholder 承诺的「支持文件名/路径」实际只支持到单段。
        // 拆成两段各自折叠，不拼接完整路径，逐条仍然零分配。
        let (dir_q, name_q) = match ql.rsplit_once('\\') {
            Some((d, n)) if !n.is_empty() => (Some(fold_into(&mut dir_scratch, d)), n),
            _ => (None, ql.as_str()),
        };
        for (i, e) in idx.entries.iter().enumerate() {
            if dirs_only && !e.is_dir {
                continue;
            }
            let name = idx.name(e);
            if let Some(x) = &ext {
                let m = name.len() > x.len() + 1
                    && name[name.len() - x.len() - 1..].starts_with('.')
                    && name[name.len() - x.len()..].eq_ignore_ascii_case(x);
                if !m {
                    continue;
                }
            }
            let name_l = fold_into(&mut scratch, name);
            let matched = match dir_q {
                // 目录片段必须落在目录末尾（`sub` 只匹配 ...\sub，不匹配
                // ...\subdir），文件名片段落在文件名里。
                Some(dq) => {
                    name_l.contains(name_q)
                        && fold_into(&mut scratch, &idx.dirs[e.dir as usize]).ends_with(dq)
                }
                None => {
                    // 先看文件名（短、命中率高），未中再看目录。
                    name_l.contains(name_q)
                        || fold_into(&mut scratch, &idx.dirs[e.dir as usize]).contains(name_q)
                }
            };
            if matched {
                let e = &idx.entries[i];
                hits.push(LocalSearchHit {
                    path: idx.path(e),
                    name: idx.name(e).to_owned(),
                    is_dir: e.is_dir,
                    size: if e.is_dir { None } else { Some(e.size) },
                    modified_secs: Some(e.mtime),
                });
                if hits.len() >= MAX_SUBSTRING_COLLECT {
                    break;
                }
            }
        }
    } else {
        // 模糊匹配：nucleo（Helix 算法），对文件名打分。
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut scored: Vec<(u16, u32)> = Vec::new();
        let mut name_buf = Vec::new();
        let mut needle_buf = Vec::new();
        let needle = Utf32Str::new(&ql, &mut needle_buf);
        for (i, e) in idx.entries.iter().enumerate() {
            if dirs_only && !e.is_dir {
                continue;
            }
            if let Some(x) = &ext {
                let name = idx.name(e);
                let m = name.len() > x.len() + 1
                    && name[name.len() - x.len() - 1..].starts_with('.')
                    && name[name.len() - x.len()..].eq_ignore_ascii_case(x);
                if !m {
                    continue;
                }
            }
            let name_l = fold_into(&mut scratch, idx.name(e));
            let hay = Utf32Str::new(name_l, &mut name_buf);
            if let Some(score) = matcher.fuzzy_match(hay, needle) {
                scored.push((score, i as u32));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, i) in scored.into_iter().take(MAX_RESULTS) {
            let e = &idx.entries[i as usize];
            hits.push(LocalSearchHit {
                path: idx.path(e),
                name: idx.name(e).to_owned(),
                is_dir: e.is_dir,
                size: if e.is_dir { None } else { Some(e.size) },
                modified_secs: Some(e.mtime),
            });
        }
    }
    drop(cell);

    LocalSearchResponse { hits, indexed, building }
}

/// 索引状态（前端轮询用）：只读两个计数器，**不做任何匹配**。
///
/// 索引动辄百万级条目，重跑一次模糊匹配是几百毫秒的 CPU，而扫描进度条
/// 每 500ms 就要刷一次 —— 进度刷新必须走这条廉价通道，否则「数字不变」
/// 会被换成「界面卡顿」。
#[derive(Debug, Serialize)]
pub struct IndexStatus {
    /// 重建中 = 已发现的文件数；已建成 = 索引条目数。
    pub indexed: usize,
    pub building: bool,
    /// 最近一次提权 helper 尝试的结果（unauthorized/failed 时前端出提示条）。
    pub helper_state: HelperState,
}

#[command]
pub fn local_search_status() -> IndexStatus {
    let Ok(cell) = index_cell().lock() else {
        return IndexStatus { indexed: 0, building: false, helper_state: HelperState::Ok };
    };
    let building = cell.building;
    IndexStatus {
        indexed: if building {
            PROGRESS.load(Ordering::Relaxed) as usize
        } else {
            cell.index.len()
        },
        building,
        helper_state: cell.helper_state,
    }
}

/// 用户点"重新授权/重试"：复位 helper 失败标记，再走一次提权路线。
///
/// 与 `ensure_index(false)` 的区别只有"允许再次弹 UAC"——自动路径失败后
/// 本进程不再主动重试（免骚扰），重试权交给用户显式点击。
#[command]
pub fn local_search_reauthorize() {
    reset_helper_failure();
    // 直接 force 重建：不走 start_background_index（那里的 AUTO_SCANNED
    // 守卫会让第二次调用变成空操作）。
    ensure_index(false, true);
}

/// 快速搜索：`query` 为空时返回最近修改的前 N 个文件；否则按模糊/
/// 子串匹配打分取前 N。
#[command]
pub async fn local_search(
    query: String,
    ext: Option<String>,
    dirs_only: Option<bool>,
) -> Result<LocalSearchResponse, String> {
    // 用户显式发起的查询：helper 不行也允许 jwalk 全盘兜底。
    ensure_index(true, false);
    tokio::task::spawn_blocking(move || search_blocking(query, ext, dirs_only.unwrap_or(false)))
        .await
        .map_err(|e| format!("搜索线程失败: {e}"))
}

/// 打开一条搜索结果：文件交给系统默认程序，目录交给资源管理器。
#[command]
pub fn local_search_open(path: String) -> Result<(), String> {
    open::that(&path).map_err(|e| format!("打开失败: {e}"))
}

// ── 右侧预览 ────────────────────────────────────────────────────────────
// 三态：图片（base64 data URL 直接渲染）/ 文本（截断读取）/ 不支持。
// 复用 src-tauri 已有的 base64 依赖，不引新库。

/// 文本预览读取上限：256 KB。超出截断（预览不是编辑器）。
const TEXT_PREVIEW_LIMIT: u64 = 256 * 1024;
/// 图片预览读取上限：20 MB。
const IMAGE_PREVIEW_LIMIT: u64 = 20 * 1024 * 1024;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico"];
/// 白名单文本扩展。二进制格式（docx/pdf 等）不硬读，交给「打开」。
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "rs", "py", "js", "ts", "tsx", "jsx", "json", "toml", "yaml",
    "yml", "xml", "html", "css", "csv", "tsv", "log", "ini", "cfg", "conf", "bat", "ps1",
    "sh", "c", "h", "cpp", "hpp", "java", "kt", "go", "sql", "gitignore", "env",
];

fn image_mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// 预览载荷。tag = "kind" 与前端 discriminated union 对齐。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PreviewPayload {
    Image { data_url: String },
    Text { text: String, truncated: bool },
    None { reason: String },
}

/// 预览一条搜索结果。只在用户点选某行时被调用，读取上限 + 白名单
/// 保证不把大文件/二进制塞进 webview。
#[command]
pub fn local_search_preview(path: String) -> Result<PreviewPayload, String> {
    let p = PathBuf::from(&path);
    let meta = std::fs::metadata(&p).map_err(|e| format!("读取失败: {e}"))?;
    if meta.is_dir() {
        return Ok(PreviewPayload::None {
            reason: "文件夹没有内容预览".into(),
        });
    }
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if IMAGE_EXTS.contains(&ext.as_str()) {
        if meta.len() > IMAGE_PREVIEW_LIMIT {
            return Ok(PreviewPayload::None {
                reason: "图片过大（超过 20 MB），请打开查看".into(),
            });
        }
        let bytes = std::fs::read(&p).map_err(|e| format!("读取失败: {e}"))?;
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(PreviewPayload::Image {
            data_url: format!("data:{};base64,{b64}", image_mime(&ext)),
        });
    }

    if TEXT_EXTS.contains(&ext.as_str()) {
        // 只读前 256 KB：用 File::take 避免整读再截断。
        use std::io::Read;
        let file = std::fs::File::open(&p).map_err(|e| format!("读取失败: {e}"))?;
        let mut buf = Vec::with_capacity(TEXT_PREVIEW_LIMIT as usize);
        file.take(TEXT_PREVIEW_LIMIT)
            .read_to_end(&mut buf)
            .map_err(|e| format!("读取失败: {e}"))?;
        let truncated = meta.len() > TEXT_PREVIEW_LIMIT;
        return Ok(PreviewPayload::Text {
            text: String::from_utf8_lossy(&buf).into_owned(),
            truncated,
        });
    }

    Ok(PreviewPayload::None {
        reason: "此文件类型不支持预览，请打开查看".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端功能验证：临时目录建索引 → 三条查询路径（模糊 / 子串 /
    /// ext 筛选）都能命中。arena 重排后 dir/name 偏移错了这里立刻炸。
    #[test]
    fn walk_and_search_finds_files() {
        let tmp = std::env::temp_dir().join(format!("hermix_lsearch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("AlphaReport.txt"), b"x").unwrap();
        std::fs::write(tmp.join("sub").join("beta_photo.png"), b"yy").unwrap();

        let idx = walk_root(&tmp);
        // 4 条 = 遍历根本身（jwalk 会 yield depth 0）+ sub 目录 + 2 个文件。
        // 根进索引是有意的：盘根（`C:\`）也是用户能打开的真实目录，滤掉它
        // 只会让 helper 路线与 jwalk 路线的可见集合进一步不一致。
        assert_eq!(idx.len(), 4);
        // 根条目的完整路径必须落在 tmp 下（interning 未串位）。
        assert!(idx.entries.iter().any(|e| idx.name(e) == "sub"));

        // 注入全局索引，直接测 search_blocking。
        *index_cell().lock().unwrap() = FileIndex {
            index: idx,
            built_at: Some(Instant::now()),
            building: false,
            helper_state: HelperState::Ok,
        };

        // 模糊路径：少打一个字母也能命中。
        let r = search_blocking("alpa".into(), None, false);
        assert!(r.hits.iter().any(|h| h.name == "AlphaReport.txt"));

        // 子串路径（含下划线，无点无反斜杠仍走模糊，但应命中）。
        let r = search_blocking("beta_photo".into(), None, false);
        assert!(r.hits.iter().any(|h| h.name == "beta_photo.png"));

        // 路径子串路径：带反斜杠走 substring 分支。
        let r = search_blocking("sub\\beta".into(), None, false);
        assert!(r.hits.iter().any(|h| h.name == "beta_photo.png"));

        // ext 筛选 + 空查询：只回 png。
        let r = search_blocking(String::new(), Some("png".into()), false);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].name, "beta_photo.png");
        assert_eq!(r.hits[0].size, Some(2));

        // dirs_only：sub 目录 + 遍历根本身，共 2 条。
        let r = search_blocking(String::new(), None, true);
        assert_eq!(r.hits.len(), 2);
        assert!(r.hits.iter().all(|h| h.is_dir));
        assert!(r.hits.iter().any(|h| h.name == "sub"));

        // 完整路径可用（目录 interning 未串位）。
        let r = search_blocking("alphareport".into(), None, false);
        let hit = &r.hits[0];
        assert!(hit.path.ends_with("AlphaReport.txt"));
        assert!(hit.path.contains(&tmp.to_string_lossy().to_string()));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
