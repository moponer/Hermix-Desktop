use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// `hermix` sidecar — workspace 内部 crate, 必须先 `cargo build` 才有产物,
/// 然后从 release/target 拷到 Tauri externalBin 要求的 binaries/ 路径.
///
/// Tauri externalBin 校验逻辑: `binaries/{name}-{target_triple}.exe` 必须存在
/// 否则 `tauri_build::build()` 会 fail. 所以 `cargo build -p hermix-server --release`
/// 必须先跑. 复制顺序: tauri.conf.json `beforeBuildCommand` 之前完成.
fn copy_workspace_sidecar(manifest_dir: &Path, profile: &str) {
    let src = manifest_dir
        .join("../../../release/target")
        .join(profile)
        .join("hermix.exe");
    let dst = manifest_dir.join("binaries").join("hermix-x86_64-pc-windows-msvc.exe");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=HERMIX_SKIP_SIDECAR_COPY");

    if env::var_os("HERMIX_SKIP_SIDECAR_COPY").is_some() {
        return;
    }
    if !src.exists() {
        println!(
            "cargo:warning=sidecar source not found: {} — run `cargo build --profile {} -p hermix-server` from repo root first",
            src.display(),
            profile
        );
        return;
    }

    if let Err(e) = fs::create_dir_all(dst.parent().unwrap()) {
        println!("cargo:warning=binaries dir create failed: {}", e);
        return;
    }
    if let Err(e) = fs::copy(&src, &dst) {
        println!(
            "cargo:warning=sidecar copy failed: {} -> {}: {}",
            src.display(),
            dst.display(),
            e
        );
    }
}

/// `officecli` sidecar — 项目级扩展资源二进制 (externalBin).
///
/// 2026-07-06 决策: 之前 build.rs 跨盘符找源 (D:\HermixDesktop\, repo\release\ 等),
/// 违反"项目内自包含"原则. 现在 officecli.exe 是项目资产, 由维护者手动
/// `cp` 到 `src-tauri/binaries/officecli-x86_64-pc-windows-msvc.exe`,
/// Tauri externalBin 直接读取该路径打包. build.rs 只做存在性 + 大小合理性校验,
/// 缺失立刻 panic 让 CI/本地立刻看到, 不再写 1KB stub 蒙混.
///
/// 更新流程:
///   1. 拿到新的 officecli.exe
///   2. `cp new_officecli.exe apps/desktop-tauri/src-tauri/binaries/officecli-x86_64-pc-windows-msvc.exe`
///   3. `cargo tauri build` 重新打包 → NSIS 安装包内自动包含新版本
///
/// runtime fallback: 如果用户硬塞了 stub 启动, `officecli::ensure_officecli()` 在
/// 真实 invoke 时会拿到 OS 错误 "不是有效的 Win32 应用", 给用户明确信号.
const OFFICECLI_MIN_BYTES: u64 = 100 * 1024; // 100 KB — 任何真二进制都超过; stub 都不过
fn assert_officecli_present(manifest_dir: &Path) {
    let dst = manifest_dir.join("binaries").join("officecli-x86_64-pc-windows-msvc.exe");
    println!("cargo:rerun-if-changed={}", dst.display());

    let meta = match fs::metadata(&dst) {
        Ok(m) => m,
        Err(_) => panic!(
            "officecli sidecar missing at {}\n\
             \n\
             This is a project-level resource binary. Place it manually:\n\
             \n\
                 cp <path-to>/officecli.exe {}\n\
             \n\
             Then rerun `cargo tauri build`. See build.rs for the update workflow.",
            dst.display(),
            dst.display()
        ),
    };

    if meta.len() < OFFICECLI_MIN_BYTES {
        // 不 panic —— 只警告, 让 stub placeholder 启动也能 build (主人刻意留这个口子)
        println!(
            "cargo:warning=officecli sidecar at {} is suspiciously small ({} bytes, expected > {}). \
             Likely a stub placeholder — replace with the real binary before shipping.",
            dst.display(),
            meta.len(),
            OFFICECLI_MIN_BYTES
        );
    }
}

fn main() {
    // 2026-07-06: sidecar 处理必须先于 tauri_build::build(),
    // 因为 tauri_build 会校验 binaries/{name}-{triple}.exe 已存在.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = env::var("PROFILE").expect("CARGO sets PROFILE for build scripts");
    copy_workspace_sidecar(&manifest_dir, &profile);
    assert_officecli_present(&manifest_dir);

    tauri_build::build();
}