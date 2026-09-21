//! Tauri IPC commands for workbench management.
//!
//! 工作台与 custom app 并列，但生命周期不同：custom app 是分发型（打包成
//! 独立 exe），工作台是演进型（宿主内运行、对话迭代、无需打包）。
//! 本模块只暴露存储与骨架读写；数据层与权限网关不经过这里。

use tauri::Emitter;

use hermix_plugin::{
    create_workbench, db_exec, db_query, format_module_index, format_violations, fs_list, fs_read,
    fs_write, lint_workbench, list_code_files, list_workbenches, module_index, read_code_file,
    read_manifest, set_deps, undo_history, undo_last, vendor_index, vendor_dir,
    workbench_dir, write_code_file, write_manifest,
};

/// 新建一个工作台：预生成骨架目录并返回 manifest。
#[tauri::command]
pub async fn workbench_create(name: String) -> Result<serde_json::Value, String> {
    let manifest = create_workbench(&name)?;
    serde_json::to_value(manifest).map_err(|e| format!("serialize manifest: {e}"))
}

/// 列出全部工作台，按最近更新排序。
#[tauri::command]
pub async fn workbench_list() -> Result<serde_json::Value, String> {
    let list = list_workbenches()?;
    serde_json::to_value(list).map_err(|e| format!("serialize list: {e}"))
}

/// 读取代码层某个文件。
#[tauri::command]
pub async fn workbench_read(id: String, path: String) -> Result<String, String> {
    read_code_file(&id, &path)
}

/// 写入代码层某个文件。锁定文件、路径逃逸、单文件行数上限与 lint 规则在此拦截。
/// 写成功后发一个 Tauri 事件让画布重载 —— 工作台没有构建步骤，
/// 「重新加载」就是它的热更新。
#[tauri::command]
pub async fn workbench_write(
    app: tauri::AppHandle,
    id: String,
    path: String,
    content: String,
) -> Result<(), String> {
    write_code_file(&id, &path, &content)?;
    let _ = app.emit("workbench://changed", id);
    Ok(())
}

/// 列出代码层文件，供模块索引注入使用。
#[tauri::command]
pub async fn workbench_files(id: String) -> Result<Vec<String>, String> {
    list_code_files(&id)
}

/// 画布 iframe 的入口文件绝对路径。前端转成 asset:// 后加载 ——
/// 自定义协议下 ESM importmap 与相对路径才能正常工作（file:// 会被 CORS 拦下）。
#[tauri::command]
pub async fn workbench_entry_path(id: String) -> Result<String, String> {
    let path = workbench_dir(&id).join("code").join("index.html");
    if !path.exists() {
        return Err(format!("workbench index missing: {}", path.display()));
    }
    Ok(path.to_string_lossy().to_string())
}

/// 绑定持久会话。右侧对话栏靠它跨会话保留上下文，也是 skill 常驻的依据。
#[tauri::command]
pub async fn workbench_bind_session(id: String, session_id: String) -> Result<(), String> {
    let mut manifest = read_manifest(&id)?;
    manifest.session_id = Some(session_id);
    write_manifest(&manifest)
}

/// 确保会话已绑定，返回真正生效的会话 id。
///
/// **已绑定的会话不再被覆盖**：调用方每次打开都会新建一个会话，若无条件覆盖，
/// 工作台的 skill 常驻会跟着漂到最新会话，而画布/对话栏用的是当前 active 会话 ——
/// 两侧一错位，agent 就在没有工作台 skill 的情况下改代码。
#[tauri::command]
pub async fn workbench_ensure_session(id: String, session_id: String) -> Result<String, String> {
    let mut manifest = read_manifest(&id)?;
    match manifest.session_id.clone() {
        Some(existing) => Ok(existing),
        None => {
            manifest.session_id = Some(session_id.clone());
            write_manifest(&manifest)?;
            Ok(session_id)
        }
    }
}

/// 跑 lint 规则表。返回带「文件:行号 + 规则 ID + 修法」的文本，空表示通过 ——
/// 直接回传给 agent 自修，没有可执行的定位信息它只会整篇重写。
/// `path`/`content` 成对传入时做落盘前预检，不先写脏文件。
#[tauri::command]
pub async fn workbench_lint(
    id: String,
    path: Option<String>,
    content: Option<String>,
) -> Result<String, String> {
    let pending = match (path.as_deref(), content.as_deref()) {
        (Some(p), Some(c)) => Some((p, c)),
        _ => None,
    };
    let violations = lint_workbench(&id, pending)?;
    Ok(format_violations(&violations))
}

/// 设定第三方依赖并把 importmap 写回 `index.html`。未安装的库一律拒绝，
/// 由 agent 走依赖申请，而不是自己从 CDN 引入。
#[tauri::command]
pub async fn workbench_set_deps(id: String, deps: Vec<String>) -> Result<Vec<String>, String> {
    set_deps(&id, &deps)
}

/// 已安装的第三方库清单 —— agent 选依赖前先看这个，避免申请已存在的库。
#[tauri::command]
pub async fn workbench_vendor_index() -> Result<serde_json::Value, String> {
    let index = vendor_index();
    serde_json::to_value(index).map_err(|e| format!("serialize vendor index: {e}"))
}

/// 共享 vendor 目录。依赖申请通过后库文件放这里，并在其中维护 index.json。
#[tauri::command]
pub async fn workbench_vendor_dir() -> Result<String, String> {
    Ok(vendor_dir().to_string_lossy().to_string())
}

/// 模块索引（文件 → 导出符号）。迭代前注入给 agent，让它在已有模块上改，
/// 而不是每次推倒重来。工作台上限是 wb.* 能力通道，不是文件数量，
/// 所以索引必须一直给全，不能截断。
#[tauri::command]
pub async fn workbench_module_index(id: String) -> Result<String, String> {
    let entries = module_index(&id)?;
    Ok(format_module_index(&entries))
}

// ── 宿主网关：数据与本机能力 ────────────────────────────
// 这些命令只被宿主外壳调用（iframe → postMessage → 宿主 → IPC），
// 代码层永远拿不到直接的 IPC 入口。

#[tauri::command]
pub async fn workbench_db_query(
    id: String,
    sql: String,
    params: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let rows = db_query(&id, &sql, &params)?;
    serde_json::to_value(rows).map_err(|e| format!("serialize rows: {e}"))
}

#[tauri::command]
pub async fn workbench_db_exec(
    id: String,
    sql: String,
    params: Vec<serde_json::Value>,
) -> Result<usize, String> {
    db_exec(&id, &sql, &params)
}

// 文件系统不按工作台区分：能力是同一份，没必要假装每个工作台有不同的边界。
#[tauri::command]
pub async fn workbench_fs_read(path: String) -> Result<String, String> {
    fs_read(&path)
}

#[tauri::command]
pub async fn workbench_fs_write(path: String, content: String) -> Result<(), String> {
    fs_write(&path, &content)
}

#[tauri::command]
pub async fn workbench_fs_list(path: String) -> Result<Vec<String>, String> {
    fs_list(&path)
}

/// 撤销最近一次代码改动。数据层不动。
#[tauri::command]
pub async fn workbench_undo(id: String) -> Result<Option<String>, String> {
    undo_last(&id)
}

/// 可撤销的改动列表（最近在前）。
#[tauri::command]
pub async fn workbench_history(id: String) -> Vec<String> {
    undo_history(&id)
}
