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
//! 生命周期：主进程心跳（main.alive）消失 >60s → 自杀；主进程重启
//! 通过 helper.pid + helper.alive 复用本进程（UAC 只弹一次）。
//! 主进程写 resnap.request → 重新全量枚举（新 snapshot 文件、清空
//! updates），用于目录改名后的全量校正。
//!
//! 协议（JSONL）：
//!   snapshot-*.jsonl: {"frn":u64,"parent":u64,"name":"...","is_dir":bool,"ts":i64}
//!   updates.jsonl:    {"op":"add","frn":..,"parent":..,"name":..,"is_dir":..,"ts":..}
//!                     {"op":"del","frn":..}
//!   done.json:        {"ok":bool,"indexed":N,"snapshot":"snapshot-...jsonl","error":...}

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0, READ_USN_JOURNAL_DATA_V0,
    USN_JOURNAL_DATA_V0, USN_RECORD_V2, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
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

fn touch(path: &Path) {
    let _ = OpenOptions::new().create(true).write(true).open(path);
}

// windows 0.58 缺 FSCTL_GET_USN_JOURNAL 常量（winioctl.h 的稳定 ABI 值）。
const FSCTL_GET_USN_JOURNAL: u32 = 0x0009_00F4;

/// 一个卷的监听状态。
struct WatchState {
    volume: String,
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
        volume: volume.to_string(),
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
fn index_volume(volume: &str, out: &mut BufWriter<File>) -> Result<usize, String> {
    use windows::Win32::System::Ioctl::FSCTL_ENUM_USN_DATA;

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

fn main() {
    let (dir, volumes) = parse_args();
    let pid_path = dir.join("helper.pid");
    let alive_path = dir.join("helper.alive");
    let main_alive = dir.join("main.alive");
    let updates_path = dir.join("updates.jsonl");
    let resnap_path = dir.join("resnap.request");

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

    // 全量枚举 → snapshot-<ts>.jsonl + done.json。
    let snap_name = format!("snapshot-{}.jsonl", now_unix());
    let snap_path = dir.join(&snap_name);
    let (ok, n, err) = match OpenOptions::new().write(true).create_new(true).open(&snap_path) {
        Ok(f) => {
            let mut out = BufWriter::with_capacity(1 << 20, f);
            let mut total = 0usize;
            let mut errs: Vec<String> = Vec::new();
            for v in &volumes {
                match index_volume(v, &mut out) {
                    Ok(c) => total += c,
                    Err(e) => errs.push(format!("{v}: {e}")),
                }
            }
            let flush_err = out.flush().err().map(|e| e.to_string());
            errs.extend(flush_err);
            let ok = total > 0 && errs.len() < volumes.len();
            (
                ok,
                total,
                errs.first().cloned().unwrap_or_else(|| "no volumes".into()),
            )
        }
        Err(e) => (false, 0, format!("create snapshot: {e}")),
    };
    let _ = std::fs::write(
        dir.join("done.json"),
        format!(
            r#"{{"ok":{},"indexed":{},"snapshot":{},"error":{}}}"#,
            ok,
            n,
            serde_json::to_string(&snap_name).unwrap(),
            serde_json::to_string(&err).unwrap()
        ),
    );
    if !ok {
        let _ = std::fs::remove_file(&pid_path);
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

    // 常驻循环：心跳检查（60s 无 main.alive → 自杀）、resnap 重枚举、USN 轮询。
    thread_local! {
        static IDLE: std::cell::Cell<u32> = std::cell::Cell::new(0);
    }
    let mut last_flush = Instant::now();
    let mut resnap_pending = false;
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        touch(&alive_path);

        // 心跳：main.alive 消失超 60s → 主进程已死，自杀。
        if main_alive.exists() {
            IDLE.with(|c| c.set(0));
        } else {
            let idle = IDLE.with(|c| c.get()) + 1;
            IDLE.with(|c| c.set(idle));
            if idle > 60 {
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
            resnap_pending = false;
            let snap2 = format!("snapshot-{}.jsonl", now_unix());
            if let Ok(f) = OpenOptions::new().write(true).create_new(true).open(dir.join(&snap2)) {
                let mut out = BufWriter::with_capacity(1 << 20, f);
                let mut total = 0usize;
                for v in &volumes {
                    if let Ok(c) = index_volume(v, &mut out) {
                        total += c;
                    }
                }
                let _ = out.flush();
                let _ = std::fs::write(
                    dir.join("done.json"),
                    format!(
                        r#"{{"ok":true,"indexed":{},"snapshot":{},"error":null}}"#,
                        total,
                        serde_json::to_string(&snap2).unwrap()
                    ),
                );
            }
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
