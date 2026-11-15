//! apps/hx-shell/build.rs
//!
//! 历史：曾在 release profile 把 hx-shell.exe 同步到
//!   - apps/desktop-tauri/src-tauri/resources/loader/hx-shell.exe（Tauri 打包资源）
//!   - ~/.hermix/loader/hx-shell.exe（运行时执行位置）
//! 但 build script 运行在 crate 编译阶段，此时 hx-shell.exe 尚未链接生成，
//! src.exists() 恒为 false → 同步永远静默跳过，不可靠。
//!
//! 同步责任已转移到 apps/desktop-tauri/package.json：
//!   - _:sync-shell         → resources/loader（装包资源，build 链）
//!   - _:sync-shell-runtime → ~/.hermix/loader（运行时，build + dev 链）
//! 本文件仅保留 rerun-if-changed 触发，不再做文件同步。

fn main() {
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
