// tauri generate_context! 要求 frontendDist 存在：未跑 vite 构建时（cargo check /
// CI gate）落一个占位页保证可编译。真实构建由 release 脚本先跑 vite（覆盖本占位），
// 再 cargo build —— 产物嵌入的是真实向导 UI。
//
// rerun-if-changed：dist 变化后强制重编，避免「vite 更新了但 exe 里还是旧 UI」
// （同类事故：2026-10-06/08 打包含旧产物，见 AGENTS.md Gotchas）。
use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../dist");
    if !dist.join("index.html").exists() {
        let _ = std::fs::create_dir_all(&dist);
        let _ = std::fs::write(
            dist.join("index.html"),
            "<!doctype html><meta charset=utf-8><title>Hermix Installer</title>\
             <p style=\"font-family:system-ui;color:#475569\">安装器前端未构建：请先运行 pnpm build:vite（apps/installer）。</p>",
        );
    }
    println!("cargo:rerun-if-changed=../dist");
    // 产品版本由 release 脚本经 HERMIX_PRODUCT_VERSION 注入（installer_meta 回给
    // 前端显示）；未设置（dev）回落安装器自身 Cargo 版本。
    let pv = std::env::var("HERMIX_PRODUCT_VERSION").unwrap_or_default();
    println!("cargo:rustc-env=HERMIX_PRODUCT_VERSION={pv}");
    tauri_build::build();
}
