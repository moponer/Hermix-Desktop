// build.rs —— 在 tauri_build::build() 之外，把「被嵌进二进制的���端内容」
// 纳入编译指纹。
//
// 为什么必须自己算：viewer's 前端在 `generate_context!` 宏展开期被读进二进制，
// 而 **sccache 的缓存键不含宏展开期读取的文件**。dist 变了、crate 源码没变时，
// 缓存命中会把「嵌着旧 UI 的 rlib」原样吐出来，链接出的 exe 里就是旧界面。
// 2026-10-19 记过的同款事故在 dev 链路上又复现了一次：一次 `cargo test`
// 就把当天编好的 exe 退回成 10-15 的旧产物（连 CSP 修复都没了）。
//
// 做法：把 dist 的内容哈希同时写进两处——
//   1) OUT_DIR 里的 .rs，由 lib.rs `include!`（源码变了 → 必然重编）；
//   2) `cargo:rustc-env`（环境变了 → sccache 键变了 → 必然 miss）。
// 任一条生效就够；两条一起上是为了不依赖任一方的具体实现细节。
//
// 哈希用标准库 DefaultHasher：只用于「变了没有」的判定，不用于安全。

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// dist 目录里所有文件的内容哈希。只用于「变了没有」的判定。
fn dist_fingerprint(dist: &Path) -> u64 {
    let mut files = Vec::new();
    collect_files(dist, &mut files);
    files.sort();
    let mut h = DefaultHasher::new();
    for f in &files {
        let rel = f.strip_prefix(dist).unwrap_or(f);
        rel.to_string_lossy().hash(&mut h);
        match std::fs::read(f) {
            Ok(bytes) => bytes.hash(&mut h),
            // 读不到也要算「变了」：宁可多编一次，不要拿旧 UI 冒充新的。
            Err(_) => 0u8.hash(&mut h),
        }
    }
    h.finish()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dist = manifest_dir.join("../dist");

    // dist 变了 → build.rs 重跑 → 指纹变 → crate 重编。
    println!("cargo:rerun-if-changed={}", dist.display());

    let fp = dist_fingerprint(&dist);
    println!("cargo:rustc-env=HERMIX_VIEWER_DIST_FINGERPRINT={fp:016x}");

    // 写进 OUT_DIR 的 .rs：lib.rs include! 它，源码一变必然重编。
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let _ = std::fs::write(
        out_dir.join("dist_fingerprint.rs"),
        format!("pub const DIST_FINGERPRINT: u64 = 0x{fp:016x};\n"),
    );

    tauri_build::build();
}
