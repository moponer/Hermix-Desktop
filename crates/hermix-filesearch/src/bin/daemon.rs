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
            "--help" | "-h" => {
                println!("用法: hermix-filesearch [--volumes C:,D:] [--data-dir <path>] [--status]");
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

    eprintln!("[filesearch] 数据目录：{}", data_dir.display());
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[filesearch] tokio 运行时启动失败：{error}");
            exit(1);
        }
    };

    let service = Arc::new(SearchService::new(data_dir.clone()));
    {
        let service = service.clone();
        runtime.block_on(async move {
            service.ensure_started(volumes);
        });
    }
    eprintln!("[filesearch] 服务已启动，等待 stdio 指令。");

    // 单线程读+写：请求全部由同步方法处理（搜索/状态/改规则），无并发写 stdout。
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = handle(&service, &line);
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }

    eprintln!("[filesearch] stdin 关闭，刷盘退出。");
    service.flush_all();
}

fn handle(service: &SearchService, line: &str) -> String {
    let Ok(request) = serde_json::from_str::<serde_json::Value>(line) else {
        return error_response(None, "请求不是合法 JSON");
    };
    let id = request.get("id").cloned();
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

fn default_data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local).join("hermix").join("filesearch");
        }
    }
    PathBuf::from(".hermix-filesearch-data")
}
