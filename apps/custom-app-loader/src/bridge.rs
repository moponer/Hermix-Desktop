// bridge.rs — 单通道 JSON-RPC 桥接 (替代原 23 个 Tauri shell_* 命令)
//
// 所有方法都过 capability 门控 (在 dispatch 入口). 同步方法直接算,
// 异步方法 (exec/fetch/dialog) 在 dispatch 里 block_on.

use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::capability::CapabilitySet;
use crate::rt;

#[derive(Deserialize)]
struct RpcRequest {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct RpcResponse {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn dispatch(body: &[u8], caps: &CapabilitySet) -> String {
    let req: RpcRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return err_json(0, &format!("无效请求: {e}")),
    };
    if !caps.allows(&req.method) {
        return err_json(req.id, "permission denied");
    }
    let res = rt().block_on(do_method(&req.method, &req.params));
    match res {
        Ok(v) => ok_json(req.id, v),
        Err(e) => err_json(req.id, &e),
    }
}

async fn do_method(method: &str, params: &Value) -> Result<Value, String> {
    match method {
        // ── 文件系统 ──
        "fs_read" => {
            let p = sp(params, "path").ok_or("fs_read 缺少 path")?;
            let s = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            Ok(json!(s))
        }
        "fs_read_bytes" => {
            let p = sp(params, "path").ok_or("fs_read_bytes 缺少 path")?;
            let b = std::fs::read(&p).map_err(|e| e.to_string())?;
            Ok(json!({ "base64": B64.encode(&b) }))
        }
        "fs_write" => {
            let p = sp(params, "path").ok_or("fs_write 缺少 path")?;
            let c = sp(params, "content").ok_or("fs_write 缺少 content")?;
            if let Some(parent) = PathBuf::from(&p).parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, c).map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "fs_write_bytes" => {
            let p = sp(params, "path").ok_or("fs_write_bytes 缺少 path")?;
            let b64 = sp(params, "base64").ok_or("fs_write_bytes 缺少 base64")?;
            let b = B64.decode(&b64).map_err(|e| format!("base64 解码失败: {e}"))?;
            if let Some(parent) = PathBuf::from(&p).parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, &b).map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "fs_list" => {
            let dir = sp(params, "dir").ok_or("fs_list 缺少 dir")?;
            let mut entries = Vec::new();
            for e in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                let m = e.metadata().map_err(|e| e.to_string())?;
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                entries.push(json!({
                    "name": e.file_name().to_string_lossy().to_string(),
                    "is_dir": m.is_dir(),
                    "size": m.len(),
                    "modified": modified,
                }));
            }
            Ok(json!(entries))
        }
        "fs_exists" => {
            let p = sp(params, "path").ok_or("fs_exists 缺少 path")?;
            Ok(json!(PathBuf::from(&p).exists()))
        }
        "fs_delete" => {
            let p = sp(params, "path").ok_or("fs_delete 缺少 path")?;
            let pb = PathBuf::from(&p);
            if pb.is_dir() {
                std::fs::remove_dir_all(&pb)
            } else {
                std::fs::remove_file(&pb)
            }
            .map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "fs_mkdir" => {
            let p = sp(params, "path").ok_or("fs_mkdir 缺少 path")?;
            std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "fs_move" => {
            let from = sp(params, "from").ok_or("fs_move 缺少 from")?;
            let to = sp(params, "to").ok_or("fs_move 缺少 to")?;
            std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "fs_copy" => {
            let from = sp(params, "from").ok_or("fs_copy 缺少 from")?;
            let to = sp(params, "to").ok_or("fs_copy 缺少 to")?;
            if PathBuf::from(&from).is_dir() {
                copy_dir_recursive(&from, &to)?;
            } else {
                if let Some(parent) = PathBuf::from(&to).parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
            }
            Ok(json!(null))
        }

        // ── 进程执行 ──
        "exec" => {
            let cmd = sp(params, "cmd").ok_or("exec 缺少 cmd")?;
            let args = sa(params, "args");
            let cwd = sp(params, "cwd");
            let timeout = params.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(120);
            let mut command = tokio::process::Command::new(&cmd);
            command.args(&args);
            if let Some(c) = &cwd {
                command.current_dir(c);
            }
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::piped());
            command.kill_on_drop(true);
            let child = command.spawn().map_err(|e| e.to_string())?;
            let to = std::time::Duration::from_secs(timeout);
            let out = tokio::time::timeout(to, child.wait_with_output())
                .await
                .map_err(|_| "exec 超时".to_string())?
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "stdout": String::from_utf8_lossy(&out.stdout).to_string(),
                "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
                "exit_code": out.status.code().unwrap_or(-1),
            }))
        }

        // ── 网络 ──
        "fetch" => {
            let url = sp(params, "url").ok_or("fetch 缺少 url")?;
            let method = sp(params, "method").unwrap_or_else(|| "GET".to_string());
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| e.to_string())?;
            let mut reqb = client.request(
                reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
                    .map_err(|e| e.to_string())?,
                &url,
            );
            if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
                for (k, v) in headers {
                    if let Some(s) = v.as_str() {
                        reqb = reqb.header(k, s);
                    }
                }
            }
            if let Some(body) = sp(params, "body") {
                reqb = reqb.body(body);
            }
            let resp = reqb.send().await.map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let headers_map: serde_json::Map<String, Value> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), json!(v.to_str().unwrap_or(""))))
                .collect();
            let body_text = resp.text().await.map_err(|e| e.to_string())?;
            Ok(json!({ "status": status, "body": body_text, "headers": headers_map }))
        }

        // ── 文件对话框 ──
        "dialog_open" => {
            let mut dlg = rfd::AsyncFileDialog::new();
            if let Some(t) = sp(params, "title") {
                dlg = dlg.set_title(t);
            }
            if let Some(filters) = params.get("filters").and_then(|v| v.as_array()) {
                for f in filters {
                    let name = f.get(0).and_then(|x| x.as_str()).unwrap_or("");
                    let exts: Vec<&str> = f
                        .get(1)
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|e| e.as_str()).collect())
                        .unwrap_or_default();
                    dlg = dlg.add_filter(name, &exts);
                }
            }
            let handle = dlg.pick_file().await;
            Ok(json!(handle.map(|h| h.path().to_string_lossy().to_string())))
        }
        "dialog_save" => {
            let mut dlg = rfd::AsyncFileDialog::new();
            if let Some(t) = sp(params, "title") {
                dlg = dlg.set_title(t);
            }
            if let Some(name) = sp(params, "default_name") {
                dlg = dlg.set_file_name(name);
            }
            let handle = dlg.save_file().await;
            Ok(json!(handle.map(|h| h.path().to_string_lossy().to_string())))
        }

        // ── 路径工具 (纯计算, 永远放行) ──
        "path_join" => {
            let parts = sa(params, "parts");
            let mut p = PathBuf::new();
            for part in parts {
                p.push(part);
            }
            Ok(json!(p.to_string_lossy().to_string()))
        }
        "path_basename" => {
            let p = sp(params, "path").ok_or("path_basename 缺少 path")?;
            Ok(json!(PathBuf::from(&p)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()))
        }
        "path_dirname" => {
            let p = sp(params, "path").ok_or("path_dirname 缺少 path")?;
            Ok(json!(PathBuf::from(&p)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()))
        }
        "path_extname" => {
            let p = sp(params, "path").ok_or("path_extname 缺少 path")?;
            Ok(json!(PathBuf::from(&p)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default()))
        }

        // ── 环境变量 ──
        "env_get" => {
            let k = sp(params, "key").ok_or("env_get 缺少 key")?;
            Ok(json!(std::env::var(&k).ok()))
        }
        "env_list" => {
            let map: serde_json::Map<String, Value> = std::env::vars()
                .map(|(k, v)| (k, json!(v)))
                .collect();
            Ok(json!(map))
        }

        // ── 临时文件 ──
        "temp_dir" => Ok(json!(std::env::temp_dir().to_string_lossy().to_string())),
        "temp_file" => {
            let prefix = sp(params, "prefix").unwrap_or_else(|| "app".to_string());
            let suffix = sp(params, "suffix").unwrap_or_else(|| ".tmp".to_string());
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!("{prefix}_{ts}{suffix}"));
            std::fs::write(&p, "").map_err(|e| e.to_string())?;
            Ok(json!(p.to_string_lossy().to_string()))
        }

        _ => Err(format!("未知方法: {method}")),
    }
}

fn sp(params: &Value, k: &str) -> Option<String> {
    params.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn sa(params: &Value, k: &str) -> Vec<String> {
    params
        .get(k)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

fn copy_dir_recursive(from: &str, to: &str) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let src = e.path();
        let dst = PathBuf::from(to).join(e.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src.to_string_lossy(), &dst.to_string_lossy())?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn ok_json(id: u64, result: Value) -> String {
    serde_json::to_string(&RpcResponse {
        id,
        ok: true,
        result: Some(result),
        error: None,
    })
    .unwrap()
}

fn err_json(id: u64, error: &str) -> String {
    serde_json::to_string(&RpcResponse {
        id,
        ok: false,
        result: None,
        error: Some(error.to_string()),
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::from_manifest;
    use crate::payload::Manifest;

    fn caps_with(perms: &[&str]) -> CapabilitySet {
        let p: Vec<String> = perms.iter().map(|s| s.to_string()).collect();
        let m = Manifest {
            id: "t".into(),
            name: "t".into(),
            permissions: p,
            window_config: Default::default(),
        };
        from_manifest(&m)
    }

    #[test]
    fn fs_write_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hcap_test_{}", std::process::id()));
        let p = dir.join("a.txt");
        let caps = caps_with(&["fs-read", "fs-write"]);
        let w = dispatch(
            &serde_json::to_vec(&json!({"id":1,"method":"fs_write","params":{"path":p.to_string_lossy(),"content":"hello"}})).unwrap(),
            &caps,
        );
        assert!(w.contains("\"ok\":true"), "write failed: {w}");
        let r = dispatch(
            &serde_json::to_vec(&json!({"id":2,"method":"fs_read","params":{"path":p.to_string_lossy()}})).unwrap(),
            &caps,
        );
        assert!(r.contains("hello"), "read failed: {r}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn denies_unauthorized_method() {
        let caps = caps_with(&[]); // 空权限: 除 path_* 外全 deny
        let r = dispatch(
            &serde_json::to_vec(&json!({"id":3,"method":"fs_read","params":{"path":"x"}})).unwrap(),
            &caps,
        );
        assert!(r.contains("permission denied"), "应被拒绝: {r}");
    }

    #[test]
    fn path_join_always_allowed() {
        let caps = caps_with(&[]); // 无权限, 但 path_* 纯计算永远放行
        let r = dispatch(
            &serde_json::to_vec(&json!({"id":4,"method":"path_join","params":{"parts":["a","b","c.txt"]}})).unwrap(),
            &caps,
        );
        assert!(r.contains("a\\\\b\\\\c.txt") || r.contains("a/b/c.txt"), "path_join 失败: {r}");
    }
}
