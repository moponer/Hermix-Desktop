use std::env;
use std::fs;
use std::path::PathBuf;

/// 2026-06-26: 取消 track binaries/*.exe (体积翻倍 + 频繁重建), 改为
/// build.rs 自动从 workspace 的 release/target/{profile}/ 复制.
/// officecli 仍是手工放置的外部预编译二进制, 不在 workspace 内.
fn copy_sidecar(manifest_dir: &PathBuf, profile: &str, src_name: &str) -> Option<()> {
    let src = manifest_dir
        .join("../../../release/target")
        .join(profile)
        .join(src_name);
    let dst = manifest_dir.join("binaries").join(format!(
        "{}-x86_64-pc-windows-msvc.exe",
        src_name
    ));

    // 触发条件: 源文件变了或外部环境变了, 重新执行 build.rs.
    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=HERMIX_SKIP_SIDECAR_COPY");

    if env::var_os("HERMIX_SKIP_SIDECAR_COPY").is_some() {
        return Some(());
    }
    if !src.exists() {
        println!(
            "cargo:warning=sidecar source not found: {} — run `cargo build --profile {} -p hermix-server` from repo root first",
            src.display(),
            profile
        );
        return Some(());
    }

    if let Err(e) = fs::create_dir_all(dst.parent().unwrap()) {
        println!("cargo:warning=binaries dir create failed: {}", e);
        return Some(());
    }
    if let Err(e) = fs::copy(&src, &dst) {
        println!(
            "cargo:warning=sidecar copy failed: {} -> {}: {}",
            src.display(),
            dst.display(),
            e
        );
    }
    Some(())
}

fn main() {
    tauri_build::build();

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = env::var("PROFILE").expect("CARGO sets PROFILE for build scripts");
    copy_sidecar(&manifest_dir, &profile, "hermix");
}