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

/// `usn-indexer` sidecar — 与 hermix 同款：**编译期从 target/ 复制**，不靠脚本兜底。
///
/// 此前这里只做存在性校验，理由是「它是壳包自己的 [[bin]]，build.rs 与它同包同时
/// 编译，此刻产物尚不存在」。**该前提在 2026-10-09 usn-indexer 拆成独立 crate
/// （`crates/hermix-indexer`）之后就不成立了**：它有自己的 package 与 `[[bin]]`，
/// 壳包并不依赖它；`ensure-sidecar` 先跑 `cargo build --release -p hermix-indexer`，
/// 壳编译开始时 `target/release/usn-indexer.exe` 已经存在。
///
/// 只查存在留下的是一个没人守的复制环节：`binaries/` 那份仅由脚本刷新，忘了跑就
/// 永远是旧的，而校验还会因为「文件在」而放行。2026-10-10 排查的本地索引反复扫描，
/// 交付出去的就是一份不含修复的 usn-indexer。
///
/// 与 hermix 一样，复制交给编译器，产物不可能比这次编译旧。**边界**：这里保证的是
/// `binaries/` 与 `target/release/` 一致，不保证 `target/release/` 那份本身编自
/// 当前源码——后者由出包前的新鲜度断言负责。
fn copy_usn_indexer_sidecar(manifest_dir: &Path, profile: &str) {
    let src = manifest_dir
        .join("../../../target")
        .join(profile)
        .join("usn-indexer.exe");
    let dst = manifest_dir
        .join("binaries")
        .join("usn-indexer-x86_64-pc-windows-msvc.exe");

    println!("cargo:rerun-if-changed={}", src.display());

    if !src.exists() {
        let msg = format!(
            "usn-indexer sidecar source not found: {}\nRun from repo root first:\n    cargo build --release -p hermix-indexer --bin usn-indexer",
            src.display()
        );
        if profile == "release" {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        return;
    }

    if let Err(e) = fs::create_dir_all(dst.parent().unwrap()) {
        let msg = format!("usn-indexer binaries dir create failed: {e}");
        if profile == "release" {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        return;
    }
    if let Err(e) = fs::copy(&src, &dst) {
        let msg = format!("usn-indexer sidecar copy failed: {src:?} -> {dst:?}: {e}");
        if profile == "release" {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
    }
}

/// `officecli` sidecar — 项目级扩展资源二进制 (externalBin).
///
/// 约束: 之前 build.rs 跨盘符找源 (D:\HermixDesktop\, repo\release\ 等),
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
        // 不 panic —— 只警告, 让 stub placeholder 启动也能 build (刻意留的口子)
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
    // sidecar 处理必须先于 tauri_build::build(),
    // 因为 tauri_build 会校验 binaries/{name}-{triple}.exe 已存在.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = env::var("PROFILE").expect("CARGO sets PROFILE for build scripts");
    copy_workspace_sidecar(&manifest_dir, &profile);
    copy_usn_indexer_sidecar(&manifest_dir, &profile);
    assert_officecli_present(&manifest_dir);

    tauri_build::build();
}