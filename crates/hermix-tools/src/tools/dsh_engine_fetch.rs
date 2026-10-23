//! dsh 文档转换引擎的运行时获取（按需下载，不进安装包）。
//!
//! 引擎 = `@deepseek-ai/libreoffice-kit-win32-x64`（MPL-2.0，预裁剪 LibreOffice，
//! 71MB tgz / 解压后 190MB）。安装包**不再携带**它：首次 Office→PDF 或
//! PDF→图片时下载到 `<hermix home>/dsh-engine/package`，之后完全离线。
//!
//! pin 升级 = 改 [`VERSION`] + [`INTEGRITY`]（`npm view <pkg>@<ver> dist.integrity`）。
//! 与构建期脚本 `apps/desktop-tauri/scripts/fetch-dsh-engine.mjs` 共用同一 pin
//! 与同一 tar 包布局（tarball 顶层即 `package/`）。
//!
//! 解压用 Windows 10+ 自带的 bsdtar（`%SystemRoot%\System32\tar.exe`）：
//! 零新依赖，且 tgz 是 gzip，纯 Rust 解压需要 flate2+tar 两个 crate。

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha512};
use tokio::io::AsyncWriteExt;

/// npm 包版本（与 fetch-dsh-engine.mjs 的 VERSION 同步）。
pub(crate) const VERSION: &str = "0.2.0";
/// npm `dist.integrity`（base64 的 sha512）。
const INTEGRITY: &str =
    "sha512-5Fscl181XAcgKDTanf1poOgJOi5hdKuuLV9KmCuTOvKuirty1+kgMNXIuVQm4Rnq0Yh1mz02x/ThnqrPFta4vQ==";
const TARBALL: &str = "libreoffice-kit-win32-x64-0.2.0.tgz";
const MIRRORS: [&str; 2] = [
    "https://registry.npmmirror.com/@deepseek-ai/libreoffice-kit-win32-x64/-/libreoffice-kit-win32-x64-0.2.0.tgz",
    "https://registry.npmjs.org/@deepseek-ai/libreoffice-kit-win32-x64/-/libreoffice-kit-win32-x64-0.2.0.tgz",
];
/// 置为非空且非 `0` 即关闭自动下载（离线/受控环境；错误信息会给出该变量名）。
const DISABLE_ENV: &str = "HERMIX_DISABLE_ENGINE_FETCH";

/// 进程内串行化：多会话同时首次调用时只下载一次，其余等锁后复查。
static FETCH_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

/// `<hermix home>/dsh-engine` —— 运行期引擎的家（便携模式下在数据目录内）。
pub(crate) fn engine_root() -> PathBuf {
    hermix_config::paths::hermix_home().join("dsh-engine")
}

/// 引擎包根（与安装包时代 `<exe>/resources/dsh-engine/package` 同名同级）。
pub(crate) fn package_dir() -> PathBuf {
    engine_root().join("package")
}

/// 结构完整判据：`bin/dsoffice.exe` + 内层 `program/program/uno.ini`。
/// 只有这两样都在才算可用 —— 缺 uno.ini 的引擎会在 bootstrap 阶段崩
/// （exit -1073741819），比"没引擎"更难诊断。
pub(crate) fn is_complete(pkg: &Path) -> bool {
    pkg.join("bin").join("dsoffice.exe").is_file()
        && pkg.join("program").join("program").join("uno.ini").is_file()
}

/// 确保引擎就位于 [`package_dir`]。已就位直接返回；否则下载、校验、解压。
///
/// 第二个返回值 = **本次调用是否真的下载了**（供调用方在结果里如实告知
/// "首次使用下载了一次性引擎"，而不是每次都提）。
///
/// `progress` 是给用户的实时进度（"正在下载 48%"），由工具的
/// `ToolCallContext::progress_cb` 透传进来；无 UI 通道时传 `None`。
pub(crate) async fn ensure_engine(
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<(PathBuf, bool), String> {
    let pkg = package_dir();
    if is_complete(&pkg) {
        return Ok((pkg, false));
    }
    if std::env::var(DISABLE_ENV)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
    {
        return Err(format!("自动下载已被环境变量 {DISABLE_ENV} 关闭"));
    }

    let _guard = FETCH_LOCK.lock().await;
    if is_complete(&pkg) {
        return Ok((pkg, false)); // 双检：并发调用者可能刚装好
    }
    tracing::info!(version = VERSION, "首次使用文档转换：下载内置引擎（约 71MB，一次性）");
    download_and_install(&pkg, progress).await?;
    Ok((pkg, true))
}

/// 推一行进度 + 记一条 debug 日志（日志用于没接 UI 的调用方排查）。
fn report(progress: Option<&(dyn Fn(&str) + Send + Sync)>, msg: &str) {
    if let Some(cb) = progress {
        cb(msg);
    }
    tracing::debug!(msg, "engine progress");
}

async fn download_and_install(
    pkg: &Path,
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<(), String> {
    let root = engine_root();
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("创建 {} 失败: {e}", root.display()))?;
    let tgz = root.join(TARBALL);

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("初始化 HTTP 客户端失败: {e}"))?;

    report(progress, "正在下载内置转换引擎（约 71MB，仅首次）…");
    let mut errors = Vec::new();
    for url in MIRRORS {
        match download_to(&client, url, &tgz, progress).await {
            Ok(()) => {
                errors.clear();
                break;
            }
            Err(e) => {
                tracing::warn!(url, error = %e, "引擎镜像下载失败，尝试下一个");
                errors.push(format!("{url}: {e}"));
            }
        }
    }
    if !errors.is_empty() {
        return Err(format!("所有镜像均下载失败（{}）", errors.join("；")));
    }

    report(progress, "正在校验引擎包完整性…");
    if let Err(e) = verify_integrity(&tgz) {
        let _ = tokio::fs::remove_file(&tgz).await;
        return Err(e);
    }
    report(progress, "正在解压内置转换引擎（约 190MB / 800 个文件）…");
    let installed = install_tarball(&tgz, pkg).await;
    let _ = tokio::fs::remove_file(&tgz).await;
    installed?;
    tracing::info!(path = %pkg.display(), "内置转换引擎就绪");
    Ok(())
}

/// 流式下载到 `<tarball>.part`，完成后原子改名（半截文件不会冒充成品）。
async fn download_to(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<(), String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    // 镜像给了 Content-Length 就算百分比，否则只报已下载量。
    let total = resp.content_length();

    let part = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    let mut next_log: u64 = 16 * 1024 * 1024;
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|e| format!("下载中断: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入文件失败: {e}"))?;
        written += chunk.len() as u64;
        if written >= next_log {
            report(progress, &download_progress_msg(written, total));
            tracing::info!(mb = written / (1024 * 1024), "引擎下载中");
            next_log += 16 * 1024 * 1024;
        }
    }
    file.flush().await.map_err(|e| format!("写入文件失败: {e}"))?;
    drop(file);
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| format!("重命名下载文件失败: {e}"))?;
    Ok(())
}

/// 下载进度文案。镜像给 `Content-Length` 就算百分比，否则只报已下载量
/// （npmmirror/npmjs 都会给，但没有它时不能算个假的百分比出来）。
fn download_progress_msg(written: u64, total: Option<u64>) -> String {
    let done_mb = written / (1024 * 1024);
    match total {
        Some(t) if t > 0 => format!(
            "正在下载内置转换引擎… {}%（{done_mb}/{} MB）",
            (written as f64 / t as f64 * 100.0).round() as u32,
            t / (1024 * 1024)
        ),
        _ => format!("正在下载内置转换引擎… {done_mb} MB"),
    }
}

/// sha512 与 npm `dist.integrity` 逐字节比对 —— 镜像不可信，校验在这里拦。
fn verify_integrity(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取下载文件失败: {e}"))?;
    let mut hasher = Sha512::new();
    hasher.update(&bytes);
    let actual = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
    );
    if actual != INTEGRITY {
        return Err(format!(
            "引擎包 sha512 校验失败（期望 {INTEGRITY}，实际 {actual}）"
        ));
    }
    Ok(())
}

/// 解压到 `.staging/package`，校验结构后同卷 rename 到最终位置（原子替换）。
async fn install_tarball(tgz: &Path, pkg: &Path) -> Result<(), String> {
    let root = engine_root();
    let staging = root.join(".staging");
    if staging.exists() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| format!("创建解压目录失败: {e}"))?;

    let tar = tar_exe();
    let out = tokio::process::Command::new(&tar)
        .arg("-xzf")
        .arg(tgz)
        .arg("-C")
        .arg(&staging)
        .output()
        .await
        .map_err(|e| format!("无法执行 {}: {e}", tar.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("解压引擎包失败: {}", stderr.trim()));
    }

    let staged = staging.join("package");
    if !is_complete(&staged) {
        return Err("引擎包解压后结构不完整（缺 bin/dsoffice.exe 或 program/program/uno.ini）".into());
    }
    if pkg.exists() {
        tokio::fs::remove_dir_all(pkg)
            .await
            .map_err(|e| format!("清理旧引擎失败: {e}"))?;
    }
    tokio::fs::rename(&staged, pkg)
        .await
        .map_err(|e| format!("安装引擎失败（rename）: {e}"))?;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    Ok(())
}

/// Windows 自带 bsdtar 优先用绝对路径 —— PATH 被裁剪/劫持时仍可用。
fn tar_exe() -> PathBuf {
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = Path::new(&root).join("System32").join("tar.exe");
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("tar")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_complete_requires_both_markers() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_complete(dir.path()));

        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin").join("dsoffice.exe"), b"x").unwrap();
        assert!(!is_complete(dir.path()), "缺 uno.ini 时不算完整");

        std::fs::create_dir_all(dir.path().join("program").join("program")).unwrap();
        std::fs::write(dir.path().join("program").join("program").join("uno.ini"), b"").unwrap();
        assert!(is_complete(dir.path()));
    }

    #[test]
    fn download_progress_msg_reports_percent_or_plain_mb() {
        const MB: u64 = 1024 * 1024;
        // 有 Content-Length：半程 → 50%
        assert_eq!(
            download_progress_msg(35 * MB + MB / 2, Some(71 * MB)),
            "正在下载内置转换引擎… 50%（35/71 MB）"
        );
        // 没有 Content-Length：只报已下载量，不编百分比
        assert_eq!(
            download_progress_msg(16 * MB, None),
            "正在下载内置转换引擎… 16 MB"
        );
        // Content-Length=0（部分代理会这样报）按未知处理
        assert_eq!(
            download_progress_msg(0, Some(0)),
            "正在下载内置转换引擎… 0 MB"
        );
    }

    #[test]
    fn verify_integrity_rejects_wrong_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("fake.tgz");
        std::fs::write(&f, b"not the tarball").unwrap();
        let err = verify_integrity(&f).unwrap_err();
        assert!(err.contains("sha512"), "错误应指明是校验失败: {err}");
    }

    #[test]
    fn tar_exe_resolves_to_real_binary() {
        // Windows 10+ 必有 System32\tar.exe；其它平台回落 PATH，不在此断言。
        if std::env::var("SystemRoot").is_ok() {
            assert!(tar_exe().is_file(), "应解析到 System32\\tar.exe");
        }
    }

    /// 端到端验证"用户能看到进度"：下载 → 校验 → 解压各阶段是否真的调了
    /// 回调，文案是否是前端要显示的那几句。默认忽略（71MB 网络）。
    /// 跑法（把 HERMIX_HOME 指到空目录，模拟新用户首次）：
    /// `$env:HERMIX_HOME='<TMPDIR>/dsh-verify'; cargo test -p hermix-tools --lib engine_progress_end_to_end -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "网络：下载 71MB 引擎包"]
    async fn engine_progress_end_to_end() {
        if is_complete(&package_dir()) {
            eprintln!("skip: 引擎已就位（不会触发下载）—— 换一个空的 HERMIX_HOME 再跑");
            return;
        }
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorder = seen.clone();
        let cb = move |msg: &str| {
            recorder.lock().unwrap().push(msg.to_string());
        };
        let (pkg, downloaded) = ensure_engine(Some(&cb)).await.expect("按需下载失败");
        assert!(is_complete(&pkg));
        assert!(downloaded, "首次安装应报告 downloaded=true");
        let msgs = seen.lock().unwrap().clone();
        println!("收到 {} 条进度:\n{}", msgs.len(), msgs.join("\n"));
        assert!(
            msgs.iter().any(|m| m.contains("正在下载内置转换引擎")),
            "缺下载进度: {msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("正在解压内置转换引擎")),
            "缺解压进度: {msgs:?}"
        );
    }

    /// 端到端：真实下载 → sha512 校验 → tar 解压 → 结构断言。
    /// 默认忽略（71MB 网络）；验一条发布链路时手动跑：
    /// `cargo test -p hermix-tools --lib engine_download_end_to_end -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "网络：下载 71MB 引擎包"]
    async fn engine_download_end_to_end() {
        let (pkg, _downloaded) = ensure_engine(None).await.expect("引擎按需下载失败");
        assert!(is_complete(&pkg), "解压后结构不完整: {}", pkg.display());
        assert!(!engine_root().join(".staging").exists(), "staging 应已清理");
    }
}
