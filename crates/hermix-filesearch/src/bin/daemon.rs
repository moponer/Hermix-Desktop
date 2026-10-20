//! hermix-filesearch 独立守护进程。
//!
//! 独立可跑（双击 / 计划任务），也被 hermix-server 以提权方式拉起后经 stdio JSONL 驱动。
//! 协议（每行一个 JSON）：
//!   请求：{"id":1,"method":"search","params":{"query":"...","limit":100}}
//!         {"id":2,"method":"status"}
//!         {"id":3,"method":"setFilter","params":{...FilterRules...}}
//!   响应：{"id":N,"result":...} 或 {"id":N,"error":"..."}
//! stdin EOF = 退出信号（刷盘后结束）。

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    process::exit,
    sync::Arc,
};

use hermix_filesearch::{core::storage, SearchService};

fn main() {
    let mut volumes: Option<Vec<String>> = None;
    let mut data_dir = default_data_dir();
    let mut status_only = false;
    let mut serve = false;
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
            "--help" | "-h" => {
                println!("用法: hermix-filesearch [--volumes C:,D:] [--data-dir <path>] [--status] [--serve]");
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

    // 建索引是纯后台活：整机优先级压到 BelowNormal，避免和前台应用抢 CPU
    // （全盘 MFT 扫描期间实测能吃满一个核，用户直接感到卡顿）。
    lower_process_priority();

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
        runtime.block_on(serve_loop(service, data_dir));
        return;
    }

    eprintln!("[filesearch] 服务已启动，等待 stdio 指令。");
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

    eprintln!("[filesearch] stdin 关闭，刷盘退出。");
    service.flush_all();
}

/// 回环 TCP 模式：bind 127.0.0.1:0 → 写 endpoint.json（port+token+pid）→ 逐连接处理 JSONL。
async fn serve_loop(service: Arc<SearchService>, data_dir: PathBuf) {
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
        eprintln!("[filesearch] 写 endpoint.json 失败：{error}");
        exit(1);
    }
    eprintln!("[filesearch] 服务已启动（回环 :{port}），等待指令。");

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
                if response.contains("\"shuttingDown\":true") {
                    let _ = std::fs::remove_file(&endpoint_path);
                    service.flush_all();
                    eprintln!("[filesearch] 收到 shutdown，刷盘退出。");
                    exit(0);
                }
            }
        });
    }
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

fn default_data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local).join("hermix").join("filesearch");
        }
    }
    PathBuf::from(".hermix-filesearch-data")
}
