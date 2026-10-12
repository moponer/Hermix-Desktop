//! usn-indexer — 提权索引辅助进程（常驻，Everything 原理的 Rust 实现）。
//!
//! Phase 1（全量枚举）：`FSCTL_ENUM_USN_DATA` 直读各 NTFS 卷 MFT，
//! 原始记录流写入 `snapshot-<unixts>.jsonl`（每行 frn/parent/name/
//! is_dir/ts），完成后写 `done.json` 信号。helper 不做路径解析——
//! FRN→路径由主进程解决（主进程本就要维护 frn 映射供增量用）。
//!
//! Phase 2（驻留监听）：轮询各卷 USN Journal（`FSCTL_READ_USN_JOURNAL`，
//! ReturnOnlyOnClose=1 只取已关闭记录），把增删事件追加到
//! `updates.jsonl`。主进程 tail 该文件实时更新内存索引。
//!
//! 生命周期：主进程心跳（main.alive）**陈旧** >60s → 自杀；主进程正常退出前
//! 写 shutdown.request → 立即自杀（不等 60s，这样「关掉就重装」不会被自己
//! 锁住的镜像文件挡住）。主进程重启通过 helper.pid + helper.alive 复用本进程
//! （UAC 只弹一次）。主进程写 resnap.request → 重新全量枚举（新 snapshot 文件、
//! 清空 updates），用于目录改名后的全量校正。
//!
//! 协议（JSONL）：
//!   snapshot-*.jsonl: {"frn":u64,"parent":u64,"name":"...","is_dir":bool,"ts":i64}
//!   updates.jsonl:    {"op":"add","frn":..,"parent":..,"name":..,"is_dir":..,"ts":..}
//!                     {"op":"del","frn":..}
//!   done.json:        {"ok":bool,"indexed":N,"snapshot":"snapshot-...jsonl","error":...}

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0, USN_JOURNAL_DATA_V0,
    USN_RECORD_V2, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME,
    USN_REASON_RENAME_OLD_NAME,
};

/// NTFS 前 16 个 FRN 是元文件（$MFT/$Volume/根目录等），不进索引。
const META_FRN_MAX: u64 = 16;

/// 命令行：`usn-indexer.exe <out_dir> <C:> [D:] ...`
fn parse_args() -> (PathBuf, Vec<String>) {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: usn-indexer <out_dir> <volume>...");
        exit(2);
    }
    (PathBuf::from(&args[1]), args[2..].to_vec())
}

/// 规范卷名为 `\\.\C:` 形式并以只读打开（读 MFT 需要管理员权限）。
fn open_volume(volume: &str) -> windows::core::Result<HANDLE> {
    let vol = volume.trim_end_matches(['\\', ':']);
    let path = format!(r"\\.\{vol}:");
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 心跳点：必须**真写内容**。仅"以写权限打开后关闭、不写字节"在 NTFS 上
/// 不会更新 LastWriteTime —— 主进程的驻留检测（helper_resident）与让位检查
/// 都按 mtime 判新鲜度，mtime 冻结 = 每次启动都重复弹 UAC + 双 helper 并发。
fn touch(path: &Path) {
    let _ = std::fs::write(path, now_unix().to_string());
}

// windows 0.58 缺 FSCTL_GET_USN_JOURNAL 常量（winioctl.h 的稳定 ABI 值）。
const FSCTL_GET_USN_JOURNAL: u32 = 0x0009_00F4;

/// 一个卷的监听状态。
struct WatchState {
    handle: HANDLE,
    journal_id: u64,
    next_usn: i64,
    /// 不支持 USN（FAT32 等）：跳过监听。
    supported: bool,
}

fn open_watch(volume: &str) -> Option<WatchState> {
    let h = open_volume(volume).ok()?;
    // 查询 Journal ID 与当前 NextUsn；不活跃则尝试创建（32MB/8MB）。
    let mut jd = USN_JOURNAL_DATA_V0 {
        UsnJournalID: 0,
        FirstUsn: 0,
        NextUsn: 0,
        LowestValidUsn: 0,
        MaxUsn: 0,
        MaximumSize: 0,
        AllocationDelta: 0,
    };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            h,
            FSCTL_GET_USN_JOURNAL,
            None,
            0,
            Some((&mut jd as *mut USN_JOURNAL_DATA_V0) as *mut _),
            std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut returned),
            None,
        )
    };
    if ok.is_err() {
        let err = windows::core::Error::from_win32();
        // 1179 = ERROR_JOURNAL_NOT_ACTIVE：尝试激活（FAT32 直接失败跳过）。
        if err.code().0 & 0xFFFF == 1179 {
            #[repr(C)]
            struct CreateUsn {
                max: u64,
                delta: u64,
            }
            let cu = CreateUsn {
                max: 32 << 20,
                delta: 8 << 20,
            };
            let _ = unsafe {
                DeviceIoControl(
                    h,
                    windows::Win32::System::Ioctl::FSCTL_CREATE_USN_JOURNAL,
                    Some((&cu as *const CreateUsn) as *const _),
                    std::mem::size_of::<CreateUsn>() as u32,
                    None,
                    0,
                    None,
                    None,
                )
            };
            let mut returned: u32 = 0;
            let ok2 = unsafe {
                DeviceIoControl(
                    h,
                    FSCTL_GET_USN_JOURNAL,
                    None,
                    0,
                    Some((&mut jd as *mut USN_JOURNAL_DATA_V0) as *mut _),
                    std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                    Some(&mut returned),
                    None,
                )
            };
            if ok2.is_err() {
                unsafe {
                    let _ = CloseHandle(h);
                }
                return None;
            }
        } else {
            unsafe {
                let _ = CloseHandle(h);
            }
            return None;
        }
    }
    Some(WatchState {
        handle: h,
        journal_id: jd.UsnJournalID,
        next_usn: jd.NextUsn,
        supported: true,
    })
}

/// 从 fd（原始字节）解析一条 UTF-16 文件名。
fn name_from_bytes(fd: &[u8]) -> String {
    let units: Vec<u16> = fd
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
        .collect();
    String::from_utf16_lossy(&units)
}

/// 一轮 USN 读取：处理已关闭记录，产出增量行。返回 (行, 新 next_usn)。
fn poll_usn(st: &mut WatchState, out: &mut Vec<String>) -> bool {
    #[repr(C)]
    struct ReadUsn {
        start: i64,
        reason_mask: u32,
        return_on_close: u32,
        timeout: u64,
        bytes_to_wait: u64,
        journal_id: u64,
    }
    let ru = ReadUsn {
        start: st.next_usn,
        reason_mask: 0xFFFF_FFFF,
        return_on_close: 1,
        timeout: 0,
        bytes_to_wait: 0,
        journal_id: st.journal_id,
    };
    let mut buffer = vec![0u8; 1 << 20];
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            st.handle,
            FSCTL_READ_USN_JOURNAL,
            Some((&ru as *const ReadUsn) as *const _),
            std::mem::size_of::<ReadUsn>() as u32,
            Some(buffer.as_mut_ptr() as _),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    if ok.is_err() {
        let code = windows::core::Error::from_win32().code().0 & 0xFFFF;
        // 1181 = JOURNAL_ENTRY_DELETED / 1178 = JOURNAL_DELETE_IN_PROGRESS：
        // 日志失效，请求全量重建。
        if code == 1181 || code == 1178 {
            out.push(r#"{"op":"journal_broken"}"#.into());
            return false;
        }
        return false;
    }
    if returned < 8 {
        return true;
    }
    // 输出前 8 字节 = 续读位置（官方文档语义）。
    st.next_usn = i64::from_le_bytes(buffer[..8].try_into().unwrap());
    let mut off = 8usize;
    while off + 64 <= returned as usize {
        let rec = unsafe { &*(buffer.as_ptr().add(off) as *const USN_RECORD_V2) };
        let rec_len = rec.RecordLength as usize;
        if rec_len == 0 || off + rec_len > returned as usize {
            break;
        }
        let reason = rec.Reason;
        let interesting = reason
            & (USN_REASON_FILE_CREATE
                | USN_REASON_FILE_DELETE
                | USN_REASON_RENAME_OLD_NAME
                | USN_REASON_RENAME_NEW_NAME);
        if interesting != 0 {
            let name_off = off + rec.FileNameOffset as usize;
            let name_end = name_off + rec.FileNameLength as usize;
            let name = name_from_bytes(&buffer[name_off..name_end]);
            let is_dir = rec.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
            let ts = rec.TimeStamp / 10_000_000 - 11_644_473_600;
            let line = if interesting
                & (USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME)
                != 0
            {
                format!(r#"{{"op":"del","frn":{}}}"#, rec.FileReferenceNumber)
            } else {
                format!(
                    r#"{{"op":"add","frn":{},"parent":{},"name":{},"is_dir":{},"ts":{}}}"#,
                    rec.FileReferenceNumber,
                    rec.ParentFileReferenceNumber,
                    serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".into()),
                    is_dir,
                    ts
                )
            };
            out.push(line);
        }
        off += rec_len;
    }
    true
}

// ── Phase 1：全量枚举（原始记录流，不做路径重建） ─────────────────────

/// 枚举一个 NTFS 卷的 MFT，把原始记录写入 out。返回记录数。
///
/// `limit` = 该卷的截止时刻（None = 不限）。**逐卷限额是必须的**：本机实测
/// F: 卷的 `DeviceIoControl(FSCTL_ENUM_USN_DATA)` 会**不返回**（外置/休眠/
/// 链路异常的卷），一个坏卷就能把整次全量枚举永久卡死 —— helper 再也不会写
/// done.json，主进程只能等到超时，而其余 5 个能正常扫完的卷（合计 131 万条）
/// 的成果全部作废。超限时把已写出的部分 flush 后返回 Err，由调用方记为该卷
/// 失败并继续下一个卷。
fn index_volume(
    volume: &str,
    out: &mut BufWriter<File>,
    limit: Option<Instant>,
    written: &std::sync::atomic::AtomicUsize,
) -> Result<usize, String> {
    let start = Instant::now();
    let h = open_volume(volume).map_err(|e| format!("open {volume}: {e}"))?;
    let med = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: 0,
    };
    let mut buffer = vec![0u8; 1 << 20];
    let mut returned: u32 = 0;
    let mut count: usize = 0;
    unsafe {
        DeviceIoControl(
            h,
            FSCTL_ENUM_USN_DATA,
            Some((&med as *const MFT_ENUM_DATA_V0) as *const _),
            std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
            Some(buffer.as_mut_ptr() as _),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|e| format!("enum start {volume}: {e}"))?;

    loop {
        // 逐卷预算在**每次 DeviceIoControl 之后**检查。卡死的成因就是某一次
        // DeviceIoControl 不返回（外置/网络/休眠中的卷），而卡死前的批次已经
        // 正常落盘（本机 F: 60 秒只写出 1MB ≈ 1.1 万条即停），所以把检查点
        // 放在批次边界上足以中止：不会丢已写出的部分，只丢卡住那一卷的后半截。
        if let Some(limit) = limit {
            if Instant::now() >= limit {
                let _ = out.flush();
                unsafe {
                    let _ = CloseHandle(h);
                }
                return Err(format!(
                    "{volume}: 枚举超过 {}s 未完成（MFT 读取卡死），已跳过该卷",
                    limit
                        .saturating_duration_since(start)
                        .as_secs()
                ));
            }
        }
        if returned < 8 {
            break;
        }
        // 前 8 字节 = 续枚举起点（官方文档语义）。
        let start_frn = u64::from_le_bytes(buffer[..8].try_into().unwrap());
        let mut off = 8usize;
        while off + 64 <= returned as usize {
            let rec = unsafe { &*(buffer.as_ptr().add(off) as *const USN_RECORD_V2) };
            let rec_len = rec.RecordLength as usize;
            if rec_len == 0 || off + rec_len > returned as usize {
                break;
            }
            let frn = rec.FileReferenceNumber;
            if frn > META_FRN_MAX {
                let name_off = off + rec.FileNameOffset as usize;
                let name_end = name_off + rec.FileNameLength as usize;
                let name = name_from_bytes(&buffer[name_off..name_end]);
                if !name.is_empty() {
                    let is_dir = rec.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                    let ts = rec.TimeStamp / 10_000_000 - 11_644_473_600;
                    let _ = write!(
                        out,
                        "{{\"frn\":{},\"parent\":{},\"name\":{},\"is_dir\":{},\"ts\":{}}}\n",
                        frn,
                        rec.ParentFileReferenceNumber,
                        serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".into()),
                        is_dir,
                        ts
                    );
                    count += 1;
                }
            }
            // 已落盘的条数实时共享给看门狗：卡死时它据此上报"已收录多少"，
            // 而不是把成果丢掉。
            written.store(count, std::sync::atomic::Ordering::Release);
            off += rec_len;
        }
        let mut med2 = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: start_frn,
            LowUsn: 0,
            HighUsn: 0,
        };
        let ok = unsafe {
            DeviceIoControl(
                h,
                FSCTL_ENUM_USN_DATA,
                Some((&mut med2 as *mut MFT_ENUM_DATA_V0) as *mut _),
                std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buffer.as_mut_ptr() as _),
                buffer.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        if ok.is_err() {
            // 38 = ERROR_HANDLE_EOF：枚举完成。
            break;
        }
    }
    unsafe {
        let _ = CloseHandle(h);
    }
    Ok(count)
}

// ── Phase 2 + 生命周期：常驻监听 ─────────────────────────────────────

/// 心跳新鲜度窗口。主进程每 5s touch 一次 main.alive，15s 窗口容得下两次丢失
/// （磁盘忙 / 调度抖动），又远小于 60s 的自杀阈值。
const HEARTBEAT_WINDOW: Duration = Duration::from_secs(15);

/// 全量枚举的整体预算。超时即判定某个卷的 MFT 读取卡死，写 ok:false 退出。
///
/// 取值依据：本机一次成功枚举（192 万条 / 约 170MB snapshot）在分钟量级；
/// 240s 留足余量，又不会让卡死的 helper 无限期占着一个提权进程。
const ENUM_BUDGET: Duration = Duration::from_secs(240);

/// **逐卷**枚举预算。整体预算只防"整体挂住"，这个防"一个坏卷拖死全部"。
///
/// 本机实测（2026-10-08）：C/D/E/G/H 五个卷合计 131 万条、约 90 秒跑完；
/// 而 F: 卷的 DeviceIoControl 不返回 —— 60 秒只写出约 1 MB 就再也不动。
/// 逐卷限额让 F: 被跳过、C..H 的成果照常入库，索引从"完全没有"变成"少一个卷"。
const PER_VOLUME_BUDGET: Duration = Duration::from_secs(60);

/// 枚举期心跳间隔。必须远小于主进程的 60s 驻留窗口，否则枚举还在跑、
/// helper.alive 却已陈旧，主进程会再拉起一个提权 helper 并发枚举（见
/// [`spawn_watchdog`]）。5s 与常驻循环的 1s tick 同量级，留 12 倍余量。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// 写交接信号 done.json。`error` 为 None 时写 JSON null —— 成功就是成功，
/// 不能带一句编出来的失败文案（排查时极具误导性）。
fn write_done(dir: &Path, snapshots: &[String], ok: bool, indexed: usize, error: Option<String>) {
    let err_json = match &error {
        Some(e) => serde_json::to_string(e).unwrap(),
        None => "null".to_string(),
    };
    // 兼容两种形态:单文件(旧主进程读 string)、多文件(本版主进程读 array)。
    let snap_json = match snapshots {
        [one] => serde_json::to_string(one).unwrap(),
        many => serde_json::to_string(many).unwrap(),
    };
    let _ = std::fs::write(
        dir.join("done.json"),
        format!(
            r#"{{"ok":{},"indexed":{},"snapshot":{},"error":{}}}"#,
            ok,
            indexed,
            snap_json,
            err_json
        ),
    );
}

/// main.alive 是否算「主进程还活着」。
///
/// 判的是**最后修改时间**而非「文件是否存在」。主进程只写不删，正常退出后
/// main.alive 作为残留文件一直躺在交换目录里；早先这里按 `exists()` 判，结果
/// 是 IDLE 永远归零 —— helper 从此不自杀，一个提权进程长期锁着安装目录下的
/// 镜像文件，用户「关掉应用就重装」时 NSIS 报「无法打开要写入的文件」。
///
/// 纯函数（时间由调用方传入）是为了能单测「文件还在、但内容很旧」这个关键
/// 状态 —— 那正是旧实现唯一漏掉的情形。
fn alive_fresh(
    modified: Option<std::time::SystemTime>,
    now: std::time::SystemTime,
    window: Duration,
) -> bool {
    match modified {
        // 时钟回拨（modified 在 now 之后）时 duration_since 报错，这里按
        // 「不新鲜」处理：宁可 60s 后自杀一次，也不要在主进程活着时永不退出。
        Some(t) => now.duration_since(t).map(|d| d < window).unwrap_or(false),
        None => false,
    }
}

/// 全量枚举（含看门狗）。初扫与 resnap 共用。
///
/// 为什么要看门狗，以及为什么它必须能**取走已写出的成果**：
///
/// `index_volume` 走 DeviceIoControl 读 MFT。本机实测 F: 卷的那一次调用
/// **永不返回**（外置 / 休眠 / 链路异常的卷），整个 helper 就此卡死：进程
/// 1 线程 / 6MB / 8 秒 0.03s CPU，helper.alive 从此停更，再也不会写
/// done.json —— 主进程只能等到超时，界面报「索引构建失败」。
///
/// 卡死**发生在单次调用内部**，所以循环里的期限检查（每批次一次）永远跑不到。
/// 早先按「批次边界检查」实现过一版，实测证明无效：snapshot 明明已写到
/// 165.7MB（约 184 万条），到点后却只能报 ok:false / indexed:0，把成果全丢。
///
/// 所以：写入器与计数都用 Arc 共享给看门狗。到点时看门狗自己 lock 住写入器
/// flush（卡死线程正阻塞在 DeviceIoControl 上、不持写锁，所以必然拿得到），
/// 然后**按成功上报这部分索引**再退出。索引从「完全没有」变成「少一个卷」——
/// 这正是要的降级：能搜到 C/D/E/G/H 的 131 万条，远好过一条都搜不到。
///
/// 看门狗同时是**枚举期的心跳**（见 [`spawn_watchdog`]）——不是顺手加的，
/// 少了它主进程 60s 后就会误判"没有驻留 helper"而重复拉起，见该函数注释。
/// 卡死卷跳过清单。条目 {"<卷字母>": unixts}:7 天内不再尝试该卷 —— F: 这类
/// DeviceIoControl 卡死的卷,每次重试都要白烧 60s 预算并留一个孤儿线程。
/// 删除交换目录里的本文件即恢复重试。
const SKIP_FILE: &str = "volumes.skip.json";
const SKIP_TTL: Duration = Duration::from_secs(7 * 86400);

fn read_skip_volumes(dir: &Path) -> std::collections::HashMap<String, u64> {
    let Ok(text) = std::fs::read_to_string(dir.join(SKIP_FILE)) else {
        return Default::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn mark_skip_volume(dir: &Path, volume: &str) {
    let mut map = read_skip_volumes(dir);
    map.insert(volume.to_string(), now_unix());
    let _ = std::fs::write(dir.join(SKIP_FILE), serde_json::to_string(&map).unwrap());
}

fn skip_fresh(ts: u64) -> bool {
    let now = now_unix();
    ts <= now && now - ts < SKIP_TTL.as_secs()
}

/// 全量枚举(逐卷隔离线程 + 全局看门狗)。初扫与 resnap 共用。
///
/// F: 卷的 DeviceIoControl 会卡死在**单次调用内部**(2026-10-08/10-12 两次
/// 现场:60s 写出 ~1MB 后再也不动),批次边界的预算检查永远跑不到;而旧实现
/// 的逐卷 guard 包住整个 index_volume,看门狗到点去 lock 写入器就被卡死线程
/// 永久阻塞 —— 看门狗自己死锁,done.json 永远写不出来,helper 变幽灵
/// (2026-10-11 现场:helper.alive 14 小时陈旧、进程仍活、无 done.json)。
///
/// 修复,三层:
/// 1. **每卷独立线程 + 独立 snapshot 文件**:一个卷卡死只损失它自己,
///    C/D/E/G/H 的成果照常落盘;编排线程 recv_timeout 到点即放弃该卷
///    (卡死线程泄漏在 syscall 里,占用的只是它自己那卷的句柄与文件)。
/// 2. **卡卷写入跳过清单**(SKIP_FILE,7 天 TTL):不再每次启动白烧 60s。
///    删除该文件即恢复重试。
/// 3. **看门狗零锁化**:不碰任何写入器,只读原子计数与已完成文件清单
///    (编排线程永不挂死,锁必然拿得到),到点把已完成卷的 snapshot 清单
///    写进 done.json 后退出 —— 多文件协议,主进程 Done.snapshots 兼容
///    string(旧 helper)与 array(本版)两种形态。
fn enumerate_volumes(
    dir: &Path,
    volumes: &[String],
    alive: &Path,
) -> (bool, usize, Option<String>, Vec<String>) {
    let completed: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let written: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let finished: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    spawn_watchdog(
        dir.to_path_buf(),
        alive.to_path_buf(),
        completed.clone(),
        written.clone(),
        finished.clone(),
        ENUM_BUDGET,
        HEARTBEAT_INTERVAL,
    );

    let skips = read_skip_volumes(dir);
    let mut errs: Vec<String> = Vec::new();
    for v in volumes {
        if skip_fresh(*skips.get(v).unwrap_or(&0)) {
            errs.push(format!(
                "{v}: 跳过(此前枚举卡死,删除交换目录 {SKIP_FILE} 可重试)"
            ));
            continue;
        }
        let vol_file = format!("snapshot-{}-{}.jsonl", now_unix(), v);
        let vol_path = dir.join(&vol_file);
        let (tx, rx) = std::sync::mpsc::channel();
        let vol = v.clone();
        let vol_path_worker = vol_path.clone();
        let written_w = written.clone();
        std::thread::spawn(move || {
            let open = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&vol_path_worker);
            let n = match open {
                Ok(f) => {
                    let mut w = BufWriter::with_capacity(1 << 20, f);
                    let limit = Instant::now() + PER_VOLUME_BUDGET;
                    match index_volume(&vol, &mut w, Some(limit), &written_w) {
                        Ok(c) => {
                            let _ = w.flush();
                            c
                        }
                        Err(e) => {
                            let _ = w.flush();
                            eprintln!("[usn-indexer] {vol}: {e}");
                            0
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[usn-indexer] create {vol_path_worker:?}: {e}");
                    0
                }
            };
            let _ = tx.send(n);
        });
        match rx.recv_timeout(PER_VOLUME_BUDGET + Duration::from_secs(10)) {
            Ok(count) => {
                if count > 0 {
                    completed.lock().unwrap().push(vol_file);
                } else {
                    let _ = std::fs::remove_file(&vol_path); // 空文件无价值
                    errs.push(format!("{v}: 枚举完成但 0 条记录"));
                }
            }
            Err(_) => {
                // 卡死:worker 泄漏在 DeviceIoControl 里,但只占用它自己那卷。
                // 部分写出的 vol_file 保留 —— 主进程读它,能收多少收多少。
                completed.lock().unwrap().push(vol_file);
                mark_skip_volume(dir, v);
                errs.push(format!("{v}: MFT 读取卡死,已跳过(7 天内不再尝试)"));
            }
        }
    }
    finished.store(true, Ordering::Release);
    let n = written.load(Ordering::Acquire);
    let files = completed.lock().unwrap().clone();
    let ok = !files.is_empty();
    (ok, n, errs.first().cloned(), files)
}

/// 看门狗线程:枚举期间持续刷新 `helper.alive`,到点仍未结束则把已完成卷的
/// snapshot 清单写进 done.json 后退出。**零锁**:不再碰任何写入器(旧实现
/// 在这里 lock 写入器 flush,被卡死线程持有的锁永久阻塞 —— 2026-10-11 现场
/// helper.alive 14 小时陈旧而进程仍活的根因),只读原子计数与已完成清单。
fn spawn_watchdog(
    dir: PathBuf,
    alive: PathBuf,
    completed: Arc<std::sync::Mutex<Vec<String>>>,
    written: Arc<AtomicUsize>,
    finished: Arc<std::sync::atomic::AtomicBool>,
    budget: Duration,
    interval: Duration,
) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + budget;
        loop {
            std::thread::sleep(interval);
            // 心跳无条件先刷新:即便预算已到点,这一次 touch 也落在退出之前。
            touch(&alive);
            if finished.load(Ordering::Acquire) {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        let files = completed.lock().map(|c| c.clone()).unwrap_or_default();
        let n = written.load(Ordering::Acquire);
        let _ = std::fs::remove_file(dir.join("helper.pid"));
        // 同心跳自杀路径:清掉 alive,别让 60s 新鲜窗把死人当活人。
        let _ = std::fs::remove_file(&alive);
        if !files.is_empty() {
            write_done(
                &dir,
                &files,
                true,
                n,
                Some(format!(
                    "超过 {}s 未完成:存在 MFT 读取卡死的卷,已按已完成部分上报(收录 {n} 项)",
                    budget.as_secs()
                )),
            );
        } else {
            write_done(
                &dir,
                &files,
                false,
                0,
                Some(format!(
                    "全量枚举超过 {}s 未完成且无可用成果(卷的 MFT 读取卡死)",
                    budget.as_secs()
                )),
            );
        }
        std::process::exit(4);
    });
}

fn main() {
    let (dir, volumes) = parse_args();
    let _ = std::fs::create_dir_all(&dir); // 自愈:手动调试时主进程可能没建目录
    let pid_path = dir.join("helper.pid");
    let alive_path = dir.join("helper.alive");
    let main_alive = dir.join("main.alive");
    let updates_path = dir.join("updates.jsonl");
    let resnap_path = dir.join("resnap.request");
    let shutdown_path = dir.join("shutdown.request");


    // 已有存活 helper（主进程重启复用，免二次 UAC）：退出让位。
    if pid_path.exists() && alive_path.exists()
        && alive_path.metadata().ok()
            .and_then(|m| m.modified().ok())
            .map(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(60))
            .unwrap_or(false)
    {
        exit(0);
    }
    let _ = std::fs::write(&pid_path, std::process::id().to_string());
    touch(&alive_path);

    // 初扫与 resnap 走同一个带看门狗的枚举器 —— 两条路径面对同一批卷、
    // 同一个卡死风险（本机 F:）。
    let (ok, n, err, files) = enumerate_volumes(&dir, &volumes, &alive_path);
    write_done(&dir, &files, ok, n, err);
    if !ok {
        let _ = std::fs::remove_file(&pid_path);
        let _ = std::fs::remove_file(&alive_path);
        exit(3);
    }

    // 清空旧增量流，打开追加句柄。
    let _ = std::fs::remove_file(&updates_path);
    let mut updates = BufWriter::with_capacity(
        1 << 16,
        OpenOptions::new().create(true).append(true).open(&updates_path).expect("open updates"),
    );

    // 各卷 watch 状态；不支持的卷（FAT32）静默跳过。
    let mut watches: Vec<WatchState> = volumes.iter().filter_map(|v| open_watch(v)).collect();

    // 常驻循环：心跳检查（main.alive 陈旧 60s → 自杀）、正常退出信号、resnap
    // 重枚举、USN 轮询。
    thread_local! {
        static IDLE: std::cell::Cell<u32> = std::cell::Cell::new(0);
    }
    let mut last_flush = Instant::now();
    let mut resnap_pending = false;
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        touch(&alive_path);
        // 身份自愈：pid 与心跳同批重写。主进程在"非驻留"分支会删掉这两个
        // 文件（清理幽灵 helper），而本进程此刻活得很好 —— 不补写就退化成
        // 注释里说的"幽灵"：有提权进程、锁着镜像文件、却没有 pid 可查，
        // 下一个主进程必然再拉一个。2026-10-10 现场正是这条链。
        let _ = std::fs::write(&pid_path, std::process::id().to_string());

        // 主进程正常退出前会写 shutdown.request：**不等心跳超时，立刻退出**。
        // 这一条是为「关掉应用就重装 / 升级」准备的——helper 是提权进程，一直
        // 锁着自己的镜像文件，NSIS 覆写 usn-indexer.exe 会报
        // 「无法打开要写入的文件」。
        if shutdown_path.exists() {
            let _ = std::fs::remove_file(&shutdown_path);
            let _ = std::fs::remove_file(&pid_path);
            let _ = std::fs::remove_file(&alive_path);
            exit(0);
        }

        // 心跳：main.alive 陈旧超 60s → 主进程已死，自杀。
        let fresh = alive_fresh(
            main_alive.metadata().ok().and_then(|m| m.modified().ok()),
            std::time::SystemTime::now(),
            HEARTBEAT_WINDOW,
        );
        if fresh {
            IDLE.with(|c| c.set(0));
        } else {
            let idle = IDLE.with(|c| c.get()) + 1;
            IDLE.with(|c| c.set(idle));
            if idle > 60 {
                // 自杀必须清掉 alive 标记：它此后 60s 内仍"新鲜"，只看 mtime
                // 的驻留判定会把死人当活人（2026-10-10 现场：helper 随旧主
                // 进程自杀后 15s，新主进程误判驻留、resnap 无人消费、240s
                // 超时）。主进程侧已加 pid 判活兜底，这里清标记是除根。
                let _ = std::fs::remove_file(&alive_path);
                let _ = std::fs::remove_file(&pid_path);
                exit(0);
            }
        }

        // 主进程请求重枚举（目录改名等全量校正）。
        if !resnap_pending && resnap_path.exists() {
            let _ = std::fs::remove_file(&resnap_path);
            resnap_pending = true;
        }
        if resnap_pending {
            // 重新全量：新 snapshot + done.json（主进程重建后清 updates）。
            // 与初扫共用 enumerate_with_watchdog —— resnap 面对的是同一批卷、
            // 同一个卡死风险，单独走一条没保护的路径等于把同一个 bug 留着。
            resnap_pending = false;
            let (ok2, n2, err2, files2) = enumerate_volumes(&dir, &volumes, &alive_path);
            write_done(&dir, &files2, ok2, n2, err2);
            // 日志可能已滚过失效点，重建 watch（NextUsn 取当前）。
            watches = volumes.iter().filter_map(|v| open_watch(v)).collect();
        }

        // USN 轮询：每卷取一批，追加增量行。
        let mut any = false;
        for st in watches.iter_mut() {
            if !st.supported {
                continue;
            }
            let mut lines = Vec::new();
            let ok = poll_usn(st, &mut lines);
            if !lines.is_empty() {
                for l in &lines {
                    let _ = writeln!(updates, "{l}");
                }
                any = true;
            }
            if !ok {
                st.supported = false; // 日志失效等：本轮起跳过，等 resnap
            }
        }
        if any && last_flush.elapsed() > Duration::from_millis(500) {
            let _ = updates.flush();
            last_flush = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// 心跳判定必须看**新鲜度**。这一组用例钉住 2026-10-04 修的那个 bug：
    /// 旧实现只判文件是否存在，主进程退出后 main.alive 残留 → helper 永不自杀。
    #[test]
    fn alive_fresh_checks_staleness_not_existence() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let w = HEARTBEAT_WINDOW;

        // 刚 touch 过 → 活着
        assert!(alive_fresh(Some(now), now, w));
        // 3s 前 touch（正常心跳间隔 5s 内）→ 活着
        assert!(alive_fresh(Some(now - Duration::from_secs(3)), now, w));
        // 20s 前 touch：**文件还在**，但主进程早没了 → 必须判死。
        // 这条在旧实现下会返回「活着」，是 bug 的核心。
        assert!(!alive_fresh(Some(now - Duration::from_secs(20)), now, w));
        // 文件根本不存在 → 死
        assert!(!alive_fresh(None, now, w));
        // 时钟回拨（modified > now）→ 保守判死，不因一次时间跳变永久存活
        assert!(!alive_fresh(Some(now + Duration::from_secs(5)), now, w));
    }

    /// 60s 自杀阈值本身没被改动过：这里锁住「陈旧满 60s 一定判死」，
    /// 避免有人调小 HEARTBEAT_WINDOW 时把自杀线也一起放松。
    #[test]
    fn heartbeat_window_stays_well_below_the_60s_kill_threshold() {
        assert!(HEARTBEAT_WINDOW < Duration::from_secs(60));
        // 且必须容得下主进程的 5s 心跳间隔。
        assert!(HEARTBEAT_WINDOW > Duration::from_secs(5) * 2);
    }

    /// 枚举期心跳间隔必须远小于**主进程的驻留窗口**（60s）。这是 2026-10-10
    /// 那个 bug 的最小不变式：心跳停更超过 60s，主进程就判定"没有驻留
    /// helper"，再拉起一个提权 helper 并发枚举同一批卷。
    #[test]
    fn enum_heartbeat_interval_stays_well_below_the_resident_window() {
        assert!(HEARTBEAT_INTERVAL * 3 < Duration::from_secs(60));
    }

    /// 回归：看门狗线程**在枚举期间**就会刷新 helper.alive。
    ///
    /// 旧实现是"睡满 ENUM_BUDGET 再检查"的一次性 sleep，整个枚举期（最长
    /// 240s）不碰 helper.alive —— 主进程 60s 后必然重复拉起 helper。
    /// 这里用 50ms 间隔跑真实线程，断言心跳文件在预算到点前就已出现。
    #[test]
    fn watchdog_refreshes_alive_during_enumeration() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join(format!("hermix-usn-wd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let alive = dir.join("helper.alive");
        let _ = std::fs::remove_file(&alive);

        let completed: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        spawn_watchdog(
            dir.clone(),
            alive.clone(),
            completed,
            // 置 true = 枚举已完成，线程会在第一次心跳后自行返回，
            // 不会走到写 done.json + exit(4) 那条会杀掉测试进程的分支。
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(true)),
            Duration::from_secs(3600),
            Duration::from_millis(50),
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !alive.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(alive.exists(), "看门狗未在预算到点前刷新 helper.alive");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
