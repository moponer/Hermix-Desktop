use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// `hermix` sidecar — workspace 内部 crate。合并单 workspace 后产物一律
/// 在仓库根 target/<profile>/，build.rs 在壳编译时把最新二进制刷进
/// binaries/（Tauri externalBin 硬要求 `binaries/{name}-{triple}.exe`
/// 存在，否则 tauri_build::build() 直接 fail）。
///
/// 源缺失的容错分级：release 缺源说明打包链断了（ensure-sidecar 没跑或
/// 编译失败），继续打包只会复刻「安装包内嵌陈旧 sidecar」的交付事故，
/// 必须立刻 panic；debug 缺源只是开发期后端还没编（fresh clone 的常态），
/// 警告即可，dev 由 sidecar.rs 的候选解析兜底。
fn copy_workspace_sidecar(manifest_dir: &Path, profile: &str) {
    let src = manifest_dir
        .join("../../../target")
        .join(profile)
        .join("hermix.exe");
    let dst = manifest_dir.join("binaries").join("hermix-x86_64-pc-windows-msvc.exe");

    println!("cargo:rerun-if-changed={}", src.display());

    if !src.exists() {
        let msg = format!(
            "hermix sidecar source not found: {}\nRun from repo root first:\n    cargo build --release -p hermix-server --bin hermix",
            src.display()
        );
        if profile == "release" {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        return;
    }

    if let Err(e) = fs::create_dir_all(dst.parent().unwrap()) {
        let msg = format!("binaries dir create failed: {e}");
        if profile == "release" {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        return;
    }
    if let Err(e) = fs::copy(&src, &dst) {
        let msg = format!("sidecar copy failed: {} -> {}: {e}", src.display(), dst.display());
        if profile == "release" {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
    }
}


fn main() {
    // sidecar 处理必须先于 tauri_build::build(),
    // 因为 tauri_build 会校验 binaries/{name}-{triple}.exe 已存在.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = env::var("PROFILE").expect("CARGO sets PROFILE for build scripts");
    copy_workspace_sidecar(&manifest_dir, &profile);

    tauri_build::build();
}