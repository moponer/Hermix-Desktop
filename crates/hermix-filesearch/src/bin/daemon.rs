//! hermix-filesearch 独立守护进程。
//!
//! 独立可跑（双击 / 计划任务），也被 hermix-server 以提权方式拉起后经 stdio JSONL 驱动。
//! 协议（每行一个 JSON）：
//!   请求：{"id":1,"method":"search","params":{"query":"...","limit":100}}
//!         {"id":2,"method":"status"}
//!         {"id":3,"method":"setFilter","params":{...FilterRules...}}
//!         {"id":4,"method":"rebuild","params":{"volumes":["C:"]}}   // 可选 params
//!   响应：{"id":N,"result":...} 或 {"id":N,"error":"..."}
//! stdin EOF = 退出信号（刷盘后结束）。

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    process::exit,
    sync::Arc,
    time::Duration,
};

use hermix_filesearch::{core::storage, SearchService};

fn main() {
    let mut volumes: Option<Vec<String>> = None;
    let mut data_dir = default_data_dir();
    let mut status_only = false;
    let mut serve = false;
    let mut stop_only = false;
    let mut install_task = false;
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
            "--install-task" => install_task = true,
            "--help" | "-h" => {
                println!(
                    "用法: hermix-filesearch [--volumes C:,D:] [--data-dir <path>] [--status] [--serve] [--stop]\n\
                     --stop  停掉正在运行的另一个自己（提权进程只能让它自己退出）
                     --install-task  已废弃：清理旧的开机自启计划任务（helper 改为按需拉起）"
                );
                return;
            }
            other => {
                eprintln!("未知参数：{other}");
                exit(2);
            }
        }
    }

    if status_only {
        print_snapshot_status(&data_dir);
        return;
    }

    // 停掉正在跑的另一个自己（不需要管理员权限）。
    if stop_only {
        stop_running_helper(&data_dir);
        return;
    }

    // 旧版自启协议：`--install-task` / `installTask` 曾经注册 AtLogOn + Highest
    // 计划任务，让索引开机即就绪。**已废弃**（2026-10-30）：那条常驻提权进程
    // 宿主杀不掉，关闭主程序后仍活着锁住 exe（编译 EBUSY / 安装失败），且
    // `-Force` 会让 debug 构建把任务抢注到 target/debug。现在反过来——收到
    // 旧协议也一律**清理**任务，见 remove_autostart_task。helper 恢复按需
    // 拉起（首次搜索弹一次 UAC，有 60s 冷却防连环弹）。
    if install_task {
        remove_autostart_task();
        return;
    }

    // 建索引是纯后台活：整机优先级压到 BelowNormal，避免和前台应用抢 CPU
    // （全盘 MFT 扫描期间实测能吃满一个核，用户直接感到卡顿）。
    lower_process_priority();

    // serve 模式从第一毫秒起就不能亮黑框：宿主 runas 拉起 / 开机计划任务 /
    // 用户双击都会给控制台子系统的 exe 一个控制台，而下面的快照装载要几十
    // 秒——藏晚了 = 黑框挂半分钟，用户关掉它 = daemon 被杀、索引建不完
    // （2026-10-25 用户实测的"灾难现场"）。stdio 模式（开发联调）不藏。
    if serve {
        hide_console_window();
    }

    eprintln!("[filesearch] 数据目录：{}", data_dir.display());
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[filesearch] tokio 运行时启动失败：{error}");
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
        // 控制台已在 ensure_started 之前藏掉（见上），装载期也不亮框。
        //
        // **单实例互斥**（2026-10-25）：server 与 viewer 两个宿主各自
        // ensure_helper，竞态下会拉起第二个实例——旧实例变孤儿（白吃
        // 几百 MB 内存），若快照还没落盘则两个实例同时全盘扫描，用户
        // 看到"每次都在重建"。起自身之前先请走旧的：shutdown 会让它
        // 刷盘退出，快照+增量安全交接给本实例装载。
        stop_running_helper(&data_dir);
        runtime.block_on(serve_loop(service, &data_dir));
        return;
    }

    eprintln!("[filesearch] 服务已启动，等待 stdio 指令。");
    // 读指令的循环**必须在运行时上下文里**：`rebuild` 会 `tokio::spawn`
    // 后台建索引任务，在运行时外调用会直接 panic。
    // （多线程运行时的驱动跑在 worker 上，这里的阻塞读 stdin 不会卡住定时器。）
    runtime.block_on(async {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let response = handle(&service, &line, None, &data_dir);
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
        }
    });

    eprintln!("[filesearch] stdin 关闭，刷盘退出。");
    service.flush_all();
}

/// 回环 TCP 模式：bind 127.0.0.1:0 → 写 endpoint.json（port+token+pid）→ 逐连接处理 JSONL。
async fn serve_loop(service: Arc<SearchService>, data_dir: &std::path::Path) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let token = generate_token();
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("[filesearch] 回环端口绑定失败：{error}");
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
    let started_at: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let endpoint = serde_json::json!({
        "port": port,
        "token": token,
        "pid": std::process::id(),
        "startedAt": started_at,
    });
    if let Err(error) = std::fs::write(&endpoint_path, endpoint.to_string()) {
        eprintln!("[filesearch] 写 endpoint.json 失败：{error}");
        exit(1);
    }
    eprintln!("[filesearch] 服务已启动（回环 :{port}），等待指令。");

    // 孤儿看门狗：endpoint.json 被新实例覆盖后，旧实例收不到协议 shutdown
    //（它不在新实例的 endpoint 里）。约定一个停止请求文件：新实例拉起前
    // 写入，所有实例周期检查——时间戳新于自己启动时间就刷盘退出。
    // 这是"提权进程只能让它自己退出"约束下唯一能罩住孤儿的通道。
    {
        let service = service.clone();
        let data_dir = data_dir.to_path_buf();
        let my_started_at = started_at;
        tokio::spawn(async move {
            loop {
                // 1s 一轮（2026-10-30 从 10s 收紧）：宿主退出时只写停止请求
                // 就立即 return，用户常常马上重开主程序或跑安装器 —— 10s 的
                // 窗口足够撞上"文件被占用"。代价只是每秒一次 stat。
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Ok(raw) = std::fs::read_to_string(data_dir.join("stop-request.json"))
                else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { continue };
                let ts = v.get("requestedAt").and_then(|x| x.as_u64()).unwrap_or(0);
                if ts > my_started_at {
                    eprintln!("[filesearch] 收到停止请求（新实例交接），刷盘退出。");
                    let svc = service.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(60));
                        exit(0);
                    });
                    tokio::task::spawn_blocking(move || {
                        svc.flush_all();
                        exit(0);
                    });
                    return;
                }
            }
        });
    }

    loop {
        let Ok((stream, _addr)) = listener.accept().await else {
            continue;
        };
        let service = service.clone();
        let token = token.clone();
        let endpoint_path = endpoint_path.clone();
        let data_dir = data_dir.to_path_buf();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                // handle 是同步重活（search 要在百万级条目上合并排序、
                // removeTask 要同步等 PowerShell）。直接在 async worker 上跑，
                // 一次慢请求就占死一个 worker——轮询连接一堆积，accept 循环、
                // 孤儿看门狗全部饿死，进程活着却对谁都不过话（2026-10-25
                // 实测：731 CPU 秒、端口在听、90s 无响应）。挪进阻塞线程池，
                // runtime 只管收发帧。
                let response = {
                    let service = service.clone();
                    let line = line.clone();
                    let token = token.clone();
                    let data_dir = data_dir.clone();
                    match tokio::task::spawn_blocking(move || {
                        handle(&service, &line, Some(&token), &data_dir)
                    })
                    .await
                    {
                        Ok(response) => response,
                        Err(error) => error_response(None, &format!("任务执行失败：{error}")),
                    }
                };
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
                    // 只删**自己的** endpoint.json：交接场景下新实例可能
                    // 已经写入了它的 port+pid（它的 stop_running_helper
                    // 正是本实例的 shutdown 调用方），删了会让宿主找不到
                    // 新实例，90s 轮询超时。
                    if let Ok(cur) = std::fs::read_to_string(&endpoint_path) {
                        let mine = serde_json::from_str::<serde_json::Value>(&cur)
                            .ok()
                            .and_then(|v| v.get("pid").and_then(|p| p.as_u64()))
                            .map(|p| p as u32 == std::process::id())
                            .unwrap_or(false);
                        if mine {
                            let _ = std::fs::remove_file(&endpoint_path);
                        }
                    }
                    let service = service.clone();
                    // 看门狗：刷盘最多等 60s，超时直接退（快照丢了只是下次
                    // 重建索引，不能变成"永远关不掉"）。
                    std::thread::spawn(|| {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                        eprintln!("[filesearch] 刷盘超时，强制退出。");
                        exit(0);
                    });
                    tokio::task::spawn_blocking(move || {
                        service.flush_all();
                        eprintln!("[filesearch] 收到 shutdown，刷盘退出。");
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
/// 写停止请求文件：罩住收不到协议 shutdown 的孤儿实例
/// （endpoint.json 已被新实例覆盖的旧 daemon）。
fn write_stop_request(data_dir: &std::path::Path) {
    let path = data_dir.join("stop-request.json");
    let body = serde_json::json!({
        "requestedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, body.to_string());
}

fn stop_running_helper(data_dir: &std::path::Path) {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let endpoint_path = data_dir.join("endpoint.json");
    let Ok(raw) = std::fs::read_to_string(&endpoint_path) else {
        println!("索引服务未在运行（没有 endpoint.json）");
        // endpoint 缺失 ≠ 没有实例：孤儿（endpoint 被覆盖/已损坏）可能
        // 还在跑。写停止请求，让看门狗罩住它。
        write_stop_request(data_dir);
        return;
    };
    let Ok(endpoint) = serde_json::from_str::<serde_json::Value>(&raw) else {
        println!("endpoint.json 解析失败");
        return;
    };
    let port = endpoint.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let token = endpoint
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if port == 0 {
        println!("endpoint.json 缺少端口");
        return;
    }

    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(error) => {
            println!("连接索引服务失败（可能已退出）：{error}");
            write_stop_request(data_dir);
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let payload = serde_json::json!({"id": 1, "method": "shutdown", "token": token}).to_string();
    if let Err(error) = stream.write_all(format!("{payload}\n").as_bytes()) {
        println!("发送 shutdown 失败：{error}");
        return;
    }
    let _ = stream.flush();

    let mut line = String::new();
    let _ = BufReader::new(stream).read_line(&mut line);
    println!("索引服务已停止：{}", line.trim());
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

fn handle(
    service: &Arc<SearchService>,
    line: &str,
    expected_token: Option<&str>,
    _data_dir: &std::path::Path,
) -> String {
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
        // 清理旧的「开机自启计划任务」。删任务要管理员权限，所以必须在这里
        // 做（daemon 是提权的；宿主侧执行会被「拒绝访问」挡回）。
        //
        // `installTask` 是**已废弃的旧名**，保留是因为旧宿主（已发布版本）
        // 握手成功后仍会调它 —— 让旧名执行清理，比让它继续注册任务更好。
        "installTask" | "removeTask" => {
            remove_autostart_task();
            json_response(id, &serde_json::json!({ "ok": true }))
        }
        // 重建索引：忽略现有快照重新扫 MFT。
        // **立即返回**（一次全盘扫描要几十秒到几分钟，协议读取超时只有 15s），
        // 进度走 status 轮询。
        "rebuild" => {
            let params = request.get("params").cloned().unwrap_or_default();
            let volumes = params
                .get("volumes")
                .and_then(|value| value.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|value| value.as_str().map(|text| text.to_string()))
                        .collect::<Vec<String>>()
                });
            match service.rebuild(volumes) {
                Ok(message) => json_response(id, &serde_json::json!({"ok": true, "message": message})),
                Err(error) => error_response(id, &error),
            }
        }
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
    println!(
        "{}",
        serde_json::json!({ "dataDir": data_dir.display().to_string(), "volumes": volumes })
    );
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

/// 藏掉自己的控制台窗口（serve 模式专用）。没有控制台（被 CREATE_NO_WINDOW
/// 拉起 / stdio 管道模式）时是空操作。与宿主侧 spawn_elevated 的 SW_HIDE
/// 构成双保险——计划任务与双击路径没有宿主，全靠这里。
#[cfg(windows)]
fn hide_console_window() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn hide_console_window() {}

/// 删除旧的「开机自启计划任务」——2026-10-30 起 helper 只按需拉起，
/// 任何一个 `installTask` / `--install-task` 调用都执行清理（旧协议名保留）。
///
/// 为什么自启要拆：那条任务是 `AtLogOn + RunLevel Highest`，拉起的是提权
/// 常驻进程，宿主 `TerminateProcess` 会被「拒绝访问」挡回 —— 于是"关掉主
/// 程序"永远有残留，残留实例还锁住 `hermix-filesearch.exe`（编译 EBUSY、
/// 安装器报文件被占用）。注册又用 `-Force`，任何一次 debug 构建的 daemon
/// 握手都会把任务抢注到 `target/debug/`，下次登录即锁住编译产物。
///
/// 删除同样要管理员权限，所以只能在 daemon（已提权）里做。任务不存在时
/// 静默：绝大多数用户从未注册过。
fn remove_autostart_task() {
    // 用 PowerShell 而非 schtasks：不存在的任务用
    // `-ErrorAction SilentlyContinue` 静默跳过，schtasks 会回非零退出码。
    // powershell 用绝对路径（提权 daemon 的 PATH 不可信）。
    let ps = format!(
        "{}/System32/WindowsPowerShell/v1.0/powershell.exe",
        std::env::var("SystemRoot")
            .unwrap_or_else(|_| "C:/Windows".to_string())
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    let script =
        "Unregister-ScheduledTask -TaskName HermixFileSearch -Confirm:$false -ErrorAction SilentlyContinue";
    let mut command = std::process::Command::new(&ps);
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：daemon 自己的控制台已藏，子进程也别再亮窗。
        command.creation_flags(0x0800_0000);
    }
    match command.output() {
        Ok(out) if out.status.success() => {
            eprintln!("[filesearch] 旧开机自启任务已清理（若存在）。");
        }
        Ok(out) => eprintln!(
            "[filesearch] 清理开机自启任务失败（{}）：stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(error) => eprintln!("[filesearch] 清理开机自启任务失败：{error}"),
    }
}

fn default_data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local).join("hermix").join("filesearch");
        }
    }
    PathBuf::from(".hermix-filesearch-data")
}
