// capability.rs — 真·沙箱：manifest.permissions → 允许调用的桥接方法 (默认 deny)
//
// 这是当前 Tauri 壳"假沙箱"的对立面: 壳无条件注册全部 23 个 shell_* 命令,
// manifest.permissions 写了从不校验。这里每个方法调用前都必须过 allows()。

use std::collections::HashSet;

use crate::payload::Manifest;

#[derive(Clone)]
pub struct CapabilitySet {
    allowed: HashSet<String>,
}

impl CapabilitySet {
    /// 单点权限校验。path_* 是纯计算, 永远放行; 其余方法必须显式在白名单内。
    pub fn allows(&self, method: &str) -> bool {
        if method.starts_with("path_") {
            return true;
        }
        self.allowed.contains(method)
    }

    pub fn debug_list(&self) -> Vec<String> {
        let mut v: Vec<String> = self.allowed.iter().cloned().collect();
        v.sort();
        v
    }
}

pub fn from_manifest(m: &Manifest) -> CapabilitySet {
    let mut set = HashSet::new();
    for p in &m.permissions {
        match p.as_str() {
            "fs-read" => {
                for m in ["fs_read", "fs_read_bytes", "fs_list", "fs_exists"] {
                    set.insert(m.to_string());
                }
            }
            "fs-write" => {
                for m in [
                    "fs_write",
                    "fs_write_bytes",
                    "fs_mkdir",
                    "fs_move",
                    "fs_copy",
                    "fs_delete",
                ] {
                    set.insert(m.to_string());
                }
            }
            "exec" => {
                set.insert("exec".to_string());
            }
            "network" => {
                set.insert("fetch".to_string());
            }
            "dialog" => {
                for m in ["dialog_open", "dialog_save"] {
                    set.insert(m.to_string());
                }
            }
            "env" => {
                for m in ["env_get", "env_list"] {
                    set.insert(m.to_string());
                }
            }
            "temp" => {
                for m in ["temp_dir", "temp_file"] {
                    set.insert(m.to_string());
                }
            }
            _ => { /* 未知权限声明忽略 */ }
        }
    }
    CapabilitySet { allowed: set }
}
