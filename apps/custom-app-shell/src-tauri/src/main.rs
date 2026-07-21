// Custom App Shell — 预编译 Tauri 壳
//
// 此文件由开发者一次性编译 (cargo tauri build --release)，产出
// app-shell.exe (~5MB)。用户打包自定义应用时直接复制此壳，不需要
// Rust / Tauri 环境。
//
// 壳的职责:
//   1. 读取 resources/manifest.json 获取窗口配置和应用元数据
//   2. 通过 app-loader (index.html) 动态加载用户的 ui.bundle.js
//   3. 提供一整套 shell_* IPC 命令供 backend.ts / ui.tsx 调用
//   4. 不包含任何业务逻辑 — 完全通用

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use serde::Serialize;
use std::path::PathBuf;
use tauri::Manager;

fn get_resource_path(name: &str) -> PathBuf {
    std::env::current_exe()
        .expect("无法获取 exe 路径")
        .parent()
        .expect("无法获取 exe 目录")
        .join("resources")
        .join(name)
}

fn load_manifest() -> serde_json::Value {
    let path = get_resource_path("manifest.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            eprintln!("[custom-app-shell] 读取 manifest 失败: {e} (path: {})", path.display());
            String::from("{}")
        });
    serde_json::from_str(&content).unwrap_or_else(|e| {
        eprintln!("[custom-app-shell] 解析 manifest 失败: {e}");
        serde_json::Value::Object(serde_json::Map::new())
    })
}

// ── IPC 命令: 文件系统 ──

#[tauri::command]
async fn shell_fs_read(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("读取文件失败: {e}"))
}

#[tauri::command]
async fn shell_fs_read_bytes(path: String) -> Result<Vec<u8>, String> {
    std::fs::read(&path).map_err(|e| format!("读取文件失败: {e}"))
}

#[tauri::command]
async fn shell_fs_write(path: String, content: String) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("写入文件失败: {e}"))
}

#[tauri::command]
async fn shell_fs_write_bytes(path: String, content: Vec<u8>) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(&path, &content).map_err(|e| format!("写入文件失败: {e}"))
}

#[derive(Serialize)]
struct DirEntry {
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<u64>,
}

#[tauri::command]
async fn shell_fs_list(dir: String) -> Result<Vec<DirEntry>, String> {
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败: {e}"))?;
    for entry in read_dir {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified,
        });
    }
    Ok(entries)
}

#[tauri::command]
async fn shell_fs_exists(path: String) -> Result<bool, String> {
    Ok(std::path::Path::new(&path).exists())
}

#[tauri::command]
async fn shell_fs_delete(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| format!("删除目录失败: {e}"))
    } else {
        std::fs::remove_file(p).map_err(|e| format!("删除文件失败: {e}"))
    }
}

#[tauri::command]
async fn shell_fs_mkdir(path: String) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(|e| format!("创建目录失败: {e}"))
}

#[tauri::command]
async fn shell_fs_move(from: String, to: String) -> Result<(), String> {
    std::fs::rename(&from, &to).map_err(|e| format!("移动/重命名失败: {e}"))
}

#[tauri::command]
async fn shell_fs_copy(from: String, to: String) -> Result<(), String> {
    if std::path::Path::new(&from).is_dir() {
        copy_dir_recursive(std::path::Path::new(&from), std::path::Path::new(&to))
            .map_err(|e| format!("复制目录失败: {e}"))
    } else {
        std::fs::copy(&from, &to)
            .map(|_| ())
            .map_err(|e| format!("复制文件失败: {e}"))
    }
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ── IPC 命令: 进程执行 ──

#[derive(Serialize)]
struct ExecResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[tauri::command]
async fn shell_exec(
    cmd: String,
    args: Vec<String>,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
) -> Result<ExecResult, String> {
    let mut command = tokio::process::Command::new(&cmd);
    command.args(&args);
    if let Some(dir) = &cwd {
        command.current_dir(dir);
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);

    let child = command
        .spawn()
        .map_err(|e| format!("启动进程失败: {e}"))?;

    let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(120));
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("进程执行超时 ({timeout:?})"))?
        .map_err(|e| format!("进程执行失败: {e}"))?;

    Ok(ExecResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

// ── IPC 命令: 网络请求 ──

#[derive(Serialize)]
struct FetchResult {
    status: u16,
    body: String,
    headers: std::collections::HashMap<String, String>,
}

#[tauri::command]
async fn shell_fetch(
    url: String,
    method: Option<String>,
    headers: Option<std::collections::HashMap<String, String>>,
    body: Option<String>,
) -> Result<FetchResult, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let http_method = method.unwrap_or_else(|| "GET".to_string());
    let mut req = match http_method.to_uppercase().as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        "PATCH" => client.patch(&url),
        _ => client.request(reqwest::Method::from_bytes(http_method.as_bytes()).map_err(|e| format!("无效的 HTTP 方法: {e}"))?, &url),
    };

    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.body(b);
    }

    let resp = req.send().await.map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let mut header_map = std::collections::HashMap::new();
    for (k, v) in resp.headers() {
        if let Ok(val) = v.to_str() {
            header_map.insert(k.as_str().to_string(), val.to_string());
        }
    }
    let resp_body = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;

    Ok(FetchResult {
        status,
        body: resp_body,
        headers: header_map,
    })
}

// ── IPC 命令: 文件对话框 ──

#[tauri::command]
async fn shell_dialog_open(
    app: tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<(String, Vec<String>)>>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    if let Some(t) = &title {
        dialog = dialog.set_title(t);
    }
    if let Some(filters) = &filters {
        for (name, exts) in filters {
            dialog = dialog.add_filter(name, &exts.iter().map(|e| e.as_str()).collect::<Vec<_>>());
        }
    }
    let result = dialog.blocking_pick_file();
    Ok(result.map(|p| p.to_string()))
}

#[tauri::command]
async fn shell_dialog_save(
    app: tauri::AppHandle,
    title: Option<String>,
    default_name: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    if let Some(t) = &title {
        dialog = dialog.set_title(t);
    }
    if let Some(name) = &default_name {
        dialog = dialog.set_file_name(name);
    }
    let result = dialog.blocking_save_file();
    Ok(result.map(|p| p.to_string()))
}

// ── IPC 命令: 路径工具 ──

#[tauri::command]
async fn shell_path_join(parts: Vec<String>) -> Result<String, String> {
    let mut path = PathBuf::new();
    for part in parts {
        path.push(part);
    }
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn shell_path_basename(path: String) -> Result<String, String> {
    Ok(std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default())
}

#[tauri::command]
async fn shell_path_dirname(path: String) -> Result<String, String> {
    Ok(std::path::Path::new(&path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default())
}

#[tauri::command]
async fn shell_path_extname(path: String) -> Result<String, String> {
    Ok(std::path::Path::new(&path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default())
}

// ── IPC 命令: 环境变量 ──

#[tauri::command]
async fn shell_env_get(key: String) -> Result<Option<String>, String> {
    Ok(std::env::var(&key).ok())
}

#[tauri::command]
async fn shell_env_list() -> Result<std::collections::HashMap<String, String>, String> {
    let mut env_map = std::collections::HashMap::new();
    for (k, v) in std::env::vars() {
        env_map.insert(k, v);
    }
    Ok(env_map)
}

// ── IPC 命令: 临时文件 ──

#[tauri::command]
async fn shell_temp_dir() -> Result<String, String> {
    Ok(std::env::temp_dir().to_string_lossy().to_string())
}

#[tauri::command]
async fn shell_temp_file(prefix: Option<String>, suffix: Option<String>) -> Result<String, String> {
    let prefix = prefix.unwrap_or_else(|| "app".to_string());
    let suffix = suffix.unwrap_or_else(|| ".tmp".to_string());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{prefix}_{timestamp}{suffix}"));
    std::fs::write(&path, "").map_err(|e| format!("创建临时文件失败: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

// ── 入口 ──

fn main() {
    let manifest = load_manifest();
    let wc = &manifest["window_config"];
    let width = wc["width"].as_f64().unwrap_or(600.0);
    let height = wc["height"].as_f64().unwrap_or(480.0);
    let title: String = manifest["name"]
        .as_str()
        .unwrap_or("自定义应用")
        .to_string();
    let resizable = wc["resizable"].as_bool().unwrap_or(true);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            shell_fs_read,
            shell_fs_read_bytes,
            shell_fs_write,
            shell_fs_write_bytes,
            shell_fs_list,
            shell_fs_exists,
            shell_fs_delete,
            shell_fs_mkdir,
            shell_fs_move,
            shell_fs_copy,
            shell_exec,
            shell_fetch,
            shell_dialog_open,
            shell_dialog_save,
            shell_path_join,
            shell_path_basename,
            shell_path_dirname,
            shell_path_extname,
            shell_env_get,
            shell_env_list,
            shell_temp_dir,
            shell_temp_file,
        ])
        .setup(move |app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(&title);
                let _ = window.set_resizable(resizable);
                if width > 0.0 && height > 0.0 {
                    let _ = window.set_size(tauri::LogicalSize::new(width, height));
                }

                // Set custom window/taskbar icon if present
                if let Some(icon_file) = manifest.get("icon_file").and_then(|v| v.as_str()) {
                    let icon_path = get_resource_path(icon_file);
                    if icon_path.exists() {
                        match tauri::image::Image::from_path(&icon_path) {
                            Ok(icon) => {
                                let _ = window.set_icon(icon);
                                eprintln!("[custom-app-shell] 已设置自定义图标: {}", icon_path.display());
                            }
                            Err(e) => {
                                eprintln!("[custom-app-shell] 加载图标失败: {e} (path: {})", icon_path.display());
                            }
                        }
                    }
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("custom-app-shell 运行失败");
}
