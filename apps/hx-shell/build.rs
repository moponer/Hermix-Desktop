//! apps/hx-shell/build.rs — cargo build 完成后自动同步编译产物到：
//!   1. apps/desktop-tauri/src-tauri/resources/loader/hx-shell.exe（Tauri 打包资源）
//!   2. ~/.hermix/loader/hx-shell.exe（运行时执行位置，dev 期可直接覆盖）
//!
//! 仅在 release profile 同步：debug 期位置 2 不重要，避免每次 cargo test 触发。
//! 用 mtime 比较：目标缺失或源比目标新才覆盖；失败不 panic（best-effort）。
//! Windows 上覆盖运行中/只读文件会失败，先 remove 再 copy。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let profile = env::var("PROFILE").unwrap_or_default();
    if profile != "release" {
        return;
    }

    let out_dir = match env::var("OUT_DIR") {
        Ok(v) => v,
        Err(_) => return,
    };
    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(v) => v,
        Err(_) => return,
    };

    // 位置 1：apps/hx-shell/target/release/hx-shell.exe
    // OUT_DIR = target/release/build/hx-shell-<HASH>/out，向上 3 级得到 target/release
    let src = match PathBuf::from(&out_dir).ancestors().nth(3) {
        Some(td) => td.join("hx-shell.exe"),
        None => return,
    };
    if !src.exists() {
        return;
    }

    // 位置 2：apps/desktop-tauri/src-tauri/resources/loader/hx-shell.exe
    // CARGO_MANIFEST_DIR = apps/hx-shell，向上 2 级得到仓库根
    let repo_root = PathBuf::from(&manifest_dir)
        .ancestors()
        .nth(2)
        .map(|p| p.to_path_buf());
    let dst2 = match &repo_root {
        Some(root) => root
            .join("apps")
            .join("desktop-tauri")
            .join("src-tauri")
            .join("resources")
            .join("loader")
            .join("hx-shell.exe"),
        None => return,
    };
    sync_to(&src, &dst2, "src-tauri/resources/loader/");

    // 位置 3：~/.hermix/loader/hx-shell.exe（dev 期 cargo build 跑在用户机器上可直接覆盖）
    let home = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .ok();
    if let Some(h) = home {
        let dst3 = PathBuf::from(h)
            .join(".hermix")
            .join("loader")
            .join("hx-shell.exe");
        sync_to(&src, &dst3, "~/.hermix/loader/");
    }
}

fn sync_to(src: &PathBuf, dst: &PathBuf, label: &str) {
    let need_copy = needs_copy(src, dst);
    if !need_copy {
        return;
    }
    if let Some(parent) = dst.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::remove_file(dst);
    match fs::copy(src, dst) {
        Ok(_) => println!("cargo:warning=build.rs: synced hx-shell.exe -> {}", label),
        Err(e) => println!("cargo:warning=build.rs: sync to {} failed: {}", label, e),
    }
}

/// 目标缺失 → true；目标存在但 mtime 早于源 → true；否则 false。
fn needs_copy(src: &PathBuf, dst: &PathBuf) -> bool {
    if !dst.exists() {
        return true;
    }
    let (Ok(s), Ok(d)) = (fs::metadata(src), fs::metadata(dst)) else {
        return false;
    };
    let (Some(s), Some(d)) = (s.modified().ok(), d.modified().ok()) else {
        return false;
    };
    s > d
}
