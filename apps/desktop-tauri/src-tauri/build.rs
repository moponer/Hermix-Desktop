use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// 前端产物目录的内容哈希。只用于「变了没有」的判定，不用于安全。
///
/// 为什么需要：desktop 的前端在 `generate_context!` 宏展开期被读进二进制，
/// 而 **sccache 的缓存键不含宏展开期读取的文件**。前端变了、crate 源码没变时，
/// 缓存命中会把「嵌着旧 UI 的 rlib」原样吐出来——2026-10-19 记过的
/// 「10-14 的 exe 配 10-16 的 UI」正是这个根因，prepare-build 钩子只能保证
/// vite 跑过，管不住 cargo 端把旧产物捡回来。
///
/// 做法与 viewer 侧一致：哈希同时写进 OUT_DIR 的 .rs（lib.rs include! 它，
/// 源码变了必然重编）与 `cargo:rustc-env`（环境变了 sccache 必然 miss）。
fn frontend_fingerprint(dir: &Path) -> u64 {
    let mut files = Vec::new();
    collect_files(dir, &mut files);
    files.sort();
    let mut h = DefaultHasher::new();
    for f in &files {
        let rel = f.strip_prefix(dir).unwrap_or(f);
        rel.to_string_lossy().hash(&mut h);
        match fs::read(f) {
            Ok(bytes) => bytes.hash(&mut h),
            // 读不到也要算「变了」：宁可多编一次，不要拿旧 UI 冒充新的。
            Err(_) => 0u8.hash(&mut h),
        }
    }
    h.finish()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// 把前端内容指纹注入编译：dist 变了 → build.rs 重跑 → crate 重编。
fn pin_frontend_fingerprint(manifest_dir: &Path) {
    let frontend = manifest_dir.join("../../../release/frontend");
    println!("cargo:rerun-if-changed={}", frontend.display());

    let fp = frontend_fingerprint(&frontend);
    println!("cargo:rustc-env=HERMIX_DESKTOP_FRONTEND_FINGERPRINT={fp:016x}");

    let Ok(out_dir) = env::var("OUT_DIR") else { return };
    let _ = fs::write(
        PathBuf::from(out_dir).join("frontend_fingerprint.rs"),
        format!("pub const FRONTEND_FINGERPRINT: u64 = 0x{fp:016x};\n"),
    );
}

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
    pin_frontend_fingerprint(&manifest_dir);

    tauri_build::build();
}