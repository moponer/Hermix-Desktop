//! hermix-filesearch 独立守护进程。
//!
//! 独立可跑（双击 / 计划任务），也被 hermix-server 以提权方式拉起后经 stdio JSONL 驱动。
//! 协议（每行一个 JSON）：
//!   请求：{"id":1,"method":"search","params":{"query":"...","limit":100}}
//!         {"id":2,"method":"status"}
//!         {"id":3,"method":"setFilter","params":{...FilterRules...}}
//!   响应：{"id":N,"result":...} 或 {"id":N,"error":"..."}
//! stdin EOF = 退出信号（刷盘后结束）。
//!
//! **GUI 子系统（`windows_subsystem = "windows"`）**：控制台子系统会在每次
//! 被提权拉起时弹一个黑色命令行窗口——这是用户可见的缺陷。GUI 子系统从根上
//! 杜绝这个窗口；代价是没有 stderr，所有诊断改写 `filesearch.log`
//! （见 `hermix_filesearch::logger`），CLI 输出（`--status` / `--stop`）在
//! 有控制台时挂到父进程控制台，没有就只落日志文件。

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    process::exit,
    sync::Arc,
};

use hermix_filesearch::{core::storage, logger, SearchService};

fn main() {
    let mut volumes: Option<Vec<String>> = None;
    let mut data_dir = default_data_dir();
    let mut status_only = false;
    let mut serve = false;
    let mut stop_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--volumes" => {
                volumes = Some(
                    args.next()
                        .unwrap_or_default()
                        .split(',')
                        .map(|v| v.trim().trim_end_matches('\\').to_string())
                        .filter(|v| !v.is_empty())
                        .collect(),
                )
            }
            "--data-dir" => {
                data_dir = PathBuf::from(args.next().unwrap_or_default());
            }
            "--status" => status_only = true,
            "--serve" => serve = true,
            "--stop" => stop_only = true,
            "--help" | "-h" => {
                print_line(
                    "用法: hermix-filesearch [--volumes C:,D:] [--data-dir <path>] [--status] [--serve] [--stop]\n\
                     --stop  停掉正在运行的另一个自己（提权进程只能让它自己退出）",
                );
                return;
            }
            other => {
                logger::write_line(&format!("未知参数：{other}"));
                exit(2);
            }
        }
    }

    logger::init(&data_dir);

    if status_only {
        print_snapshot_status(&data_dir);
        return;
    }

    // 停掉正在跑的另一个自己（不需要管理员权限）。
    if stop_only {
        stop_running_helper(&data_dir);
        return;
    }

    // 建索引是纯后台活：整机优先级压到 BelowNormal，避免和前台应用抢 CPU
    // （全盘 MFT 扫描期间实测能吃满一个核，用户直接感到卡顿）。
    lower_process_priority();

    logger::write_line(&format!("[filesearch] 数据目录：{}", data_dir.display()));
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            logger::write_line(&format!("[filesearch] tokio 运行时启动失败：{error}"));
            exit(1);
        }
    };

    let service: Arc<SearchService> = SearchService::new(data_dir.clone());
    {
        let service = service.clone();
        runtime.block_on(async move {
            service.ensure_started(volumes);
        });
    }

    if serve {
        // 被提权拉起时的模式：跨提权边界无法继承 stdio 管道，
        // 改用 127.0.0.1 回环 + token 握手（endpoint.json 供宿主发现）。
        runtime.block_on(serve_loop(service, data_dir));
        return;
    }

    logger::write_line("[filesearch] 服务已启动，等待 stdio 指令。");
    // 单线程读+写：请求全部由同步方法处理（搜索/状态/改规则），无并发写 stdout。
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = handle(&service, &line, None);
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }

    logger::write_line("[filesearch] stdin 关闭，刷盘退出。");
    service.flush_all();
}

/// 回环 TCP 模式：bind 127.0.0.1:0 → 写 endpoint.json（port+token+pid）→ 逐连接处理 JSONL。
async fn serve_loop(service: Arc<SearchService>, data_dir: PathBuf) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let token = generate_token();
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => {
            logger::write_line(&format!("[filesearch] 回环端口绑定失败：{error}"));
            exit(1);
        }
    };
    let port = listener
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or_default();

    let endpoint_path = data_dir.join("endpoint.json");
    if let Some(parent) = endpoint_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let endpoint = serde_json::json!({
        "port": port,
        "token": token,
        "pid": std::process::id(),
        "startedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    if let Err(error) = std::fs::write(&endpoint_path, endpoint.to_string()) {
        logger::write_line(&format!("[filesearch] 写 endpoint.json 失败：{error}"));
        exit(1);
    }
    logger::write_line(&format!("[filesearch] 服务已启动（回环 :{port}），等待指令。"));

    loop {
        let Ok((stream, _addr)) = listener.accept().await else {
            continue;
        };
        let service = service.clone();
        let token = token.clone();
        let endpoint_path = endpoint_path.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let response = handle(&service, &line, Some(&token));
                let _ = writer.write_all(response.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                // shutdown 指令：响应已回，刷盘退出（endpoint.json 一并清掉）。
                //
                // **刷盘必须离开 async 上下文**：`operation_gate` 是 tokio Mutex，
                // 在 worker 线程里 `blocking_lock()` 会 panic —— 任务当场死掉，
                // `exit(0)` 永远执行不到，进程就变成"活着但关不掉"（正是
                // 用户遇到的现象）。这里丢进 spawn_blocking，另加一个看门狗，
                // 刷盘卡住也一定退得掉。
                if response.contains("\"shuttingDown\":true") {
                    let _ = std::fs::remove_file(&endpoint_path);
                    let service = service.clone();
                    // 看门狗：刷盘最多等 60s，超时直接退（快照丢了只是下次
                    // 重建索引，不能变成"永远关不掉"）。
                    std::thread::spawn(|| {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                        logger::write_line("[filesearch] 刷盘超时，强制退出。");
                        exit(0);
                    });
                    tokio::task::spawn_blocking(move || {
                        service.flush_all();
                        logger::write_line("[filesearch] 收到 shutdown，刷盘退出。");
                        exit(0);
                    });
                    return;
                }
            }
        });
    }
}

/// 停掉正在跑的另一个自己。
///
/// **为什么需要它**：helper 是**提权进程**，普通 shell 的 `taskkill` 会被
/// Windows 拒绝（Access is denied），于是"重建它"会因 exe 被占用而失败
/// （`os error 5`）。但它**能自己退出**：回环协议里有 `shutdown` 方法，
/// 收到后刷盘、删 endpoint.json、退出。所以停它的正确姿势是"请它走"，
/// 而不是"杀它"。
fn stop_running_helper(data_dir: &std::path::Path) {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let endpoint_path = data_dir.join("endpoint.json");
    let Ok(raw) = std::fs::read_to_string(&endpoint_path) else {
        print_line("索引服务未在运行（没有 endpoint.json）");
        return;
    };
    let Ok(endpoint) = serde_json::from_str::<serde_json::Value>(&raw) else {
        print_line("endpoint.json 解析失败");
        return;
    };
    let port = endpoint.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let token = endpoint
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if port == 0 {
        print_line("endpoint.json 缺少端口");
        return;
    }

    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(error) => {
            print_line(&format!("连接索引服务失败（可能已退出）：{error}"));
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let payload = serde_json::json!({"id": 1, "method": "shutdown", "token": token}).to_string();
    if let Err(error) = stream.write_all(format!("{payload}\n").as_bytes()) {
        print_line(&format!("发送 shutdown 失败：{error}"));
        return;
    }
    let _ = stream.flush();

    let mut line = String::new();
    let _ = BufReader::new(stream).read_line(&mut line);
    print_line(&format!("索引服务已停止：{}", line.trim()));
}

/// token：std RandomState 的种子来自 OS 熵，拼上时间纳秒足够回环握手用（无新增依赖）。
fn generate_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );
    hasher.write_u64(std::process::id() as u64);
    format!("{:016x}", hasher.finish())
}

fn handle(service: &SearchService, line: &str, expected_token: Option<&str>) -> String {
    let Ok(request) = serde_json::from_str::<serde_json::Value>(line) else {
        return error_response(None, "请求不是合法 JSON");
    };
    let id = request.get("id").cloned();
    if let Some(expected) = expected_token {
        let got = request.get("token").and_then(|v| v.as_str()).unwrap_or("");
        if got != expected {
            return error_response(id, "token 校验失败");
        }
    }
    let Some(method) = request.get("method").and_then(|v| v.as_str()) else {
        return error_response(id, "缺少 method 字段");
    };

    match method {
        "search" => {
            let params = request.get("params").cloned().unwrap_or_default();
            let query = params
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);
            json_response(id, &service.search(query, limit))
        }
        "status" => json_response(id, &service.status()),
        "diskUsage" => {
            let params = request.get("params").cloned().unwrap_or_default();
            let volume = params
                .get("volume")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let parent_ref = params.get("parentRef").and_then(|v| v.as_u64());
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);
            match service.disk_usage(&volume, parent_ref, limit) {
                Ok(value) => json_response(id, &value),
                Err(error) => error_response(id, &error),
            }
        }
        "setFilter" => {
            let Some(params) = request.get("params") else {
                return error_response(id, "setFilter 需要 params");
            };
            match serde_json::from_value::<hermix_filesearch::core::model::FilterRules>(params.clone()) {
                Ok(rules) => {
                    service.set_filter(rules);
                    json_response(id, &serde_json::json!({"ok": true}))
                }
                Err(error) => error_response(id, &format!("FilterRules 解析失败：{error}")),
            }
        }
        "rebuild" => {
            let params = request.get("params").cloned().unwrap_or_default();
            let volume = params
                .get("volume")
                .and_then(|v| v.as_str())
                .map(|v| v.to_string());
            let volumes = service.request_rebuild(volume.as_deref());
            json_response(id, &serde_json::json!({ "volumes": volumes }))
        }
        "shutdown" => serde_json::json!({ "id": id, "result": {"shuttingDown": true} }).to_string(),
        other => error_response(id, &format!("未知 method：{other}")),
    }
}

fn json_response(id: Option<serde_json::Value>, result: &impl serde::Serialize) -> String {
    serde_json::json!({ "id": id, "result": result }).to_string()
}

fn error_response(id: Option<serde_json::Value>, error: &str) -> String {
    serde_json::json!({ "id": id, "error": error }).to_string()
}

/// --status 模式：只读快照文件汇报状态，不启动构建与监听（供脚本/桌面端探活）。
fn print_snapshot_status(data_dir: &std::path::Path) {
    let mut volumes = Vec::new();
    let indexes = data_dir.join("indexes");
    if let Ok(entries) = std::fs::read_dir(&indexes) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string();
            match storage::load_snapshot(data_dir, &stem) {
                Ok(Some(loaded)) => volumes.push(serde_json::json!({
                    "volume": loaded.snapshot.volume_state.volume,
                    "totalEntries": loaded.snapshot.entries.len(),
                    "indexedBytes": loaded.snapshot.entries.iter().map(|e| e.size).sum::<u64>(),
                    "indexedAt": loaded.snapshot.volume_state.indexed_at,
                    "lastSyncedAt": loaded.snapshot.volume_state.last_synced_at,
                    "source": loaded.snapshot.volume_state.source,
                })),
                Ok(None) | Err(_) => volumes.push(serde_json::json!({
                    "volume": stem, "error": "快照损坏或版本不匹配",
                })),
            }
        }
    }
    print_line(
        &serde_json::json!({ "dataDir": data_dir.display().to_string(), "volumes": volumes })
            .to_string(),
    );
}

// ── 控制台输出（GUI 子系统进程默认没有控制台） ────────────────────────

/// 往控制台写一行。
///
/// 顺序：本进程 stdout → 父进程控制台（`CONOUT$`）→ 日志文件。
/// 三条路都走不通才算失败——**不 panic**：CLI 输出失败不该让索引服务退出。
fn print_line(text: &str) {
    use std::io::Write;

    let mut stdout = std::io::stdout();
    if writeln!(stdout, "{text}").is_ok() && stdout.flush().is_ok() {
        return;
    }
    drop(stdout);

    if let Some(mut console) = open_parent_console() {
        if writeln!(console, "{text}").is_ok() {
            let _ = console.flush();
            return;
        }
    }
    logger::write_line(text);
}

/// 挂到父进程的控制台并打开 `CONOUT$`。
///
/// GUI 子系统的进程启动时没有控制台，`AttachConsole(ATTACH_PARENT_PROCESS)`
/// 之后需要**自己打开** `CONOUT$` 才能写——附着并不会重建标准句柄。
#[cfg(windows)]
fn open_parent_console() -> Option<std::fs::File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::CreateFileW;

    /// `AttachConsole` 的父进程标记。
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;

    unsafe {
        // 已经附着过控制台时这行会失败，忽略即可（下面的 CONOUT$ 仍能开）。
        let _ = windows_sys::Win32::System::Console::AttachConsole(ATTACH_PARENT_PROCESS);

        let mut wide: Vec<u16> = std::ffi::OsStr::new("CONOUT$").encode_wide().collect();
        wide.push(0);
        let handle = CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        Some(std::fs::File::from_raw_handle(handle))
    }
}

#[cfg(not(windows))]
fn open_parent_console() -> Option<std::fs::File> {
    None
}

/// 把本进程降到 BelowNormal（Windows）。失败无所谓——只是少了层礼让。
#[cfg(windows)]
fn lower_process_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    };
    unsafe {
        SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

#[cfg(not(windows))]
fn lower_process_priority() {}

fn default_data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local).join("hermix").join("filesearch");
        }
    }
    PathBuf::from(".hermix-filesearch-data")
}
