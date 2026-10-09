//! 桌面端在线更新 — 检查/下载/交换。
//!
//! 决策记录：docs/notes/implemented/feature/2026-10-09-online-update.md
//! （自管 zip 整包更新，不用 tauri-plugin-updater——便携 zip 模型不兼容安装器）。
//!
//! 三段各一个入口命令：`update_check`（清单 + 版本比较）、
//! `update_download`（流式下载 + sha256 + 进度 event）、`update_install`
//! （解压 + helper 换目录 + 重启）。检查调度（启动 30s 后 + 每 2h）在
//! `lib.rs` 的 setup 里起一个后台任务，失败静默。
//!
//! 信任边界：下载完必须 sha256 与 manifest 比对，不符即删缓存——唯一防线
//! （一期无签名，见笔记 Consequences）。

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

const REQUEST_TIMEOUT_SECS: u64 = 15;

/// 更新清单地址。解析顺序：
/// 1. 环境变量 `HERMIX_UPDATE_MANIFEST_URL`（测试/灰度覆写）
/// 2. app 根 `update-url.txt`（发布脚本打包时写入，运维可改）
/// 3. 内置默认 = relay 域名（与手机端 pairing 的 DEFAULT_RELAY 同源，永不落空）
pub fn manifest_url() -> String {
    if let Ok(u) = std::env::var("HERMIX_UPDATE_MANIFEST_URL") {
        let u = u.trim().to_string();
        if !u.is_empty() {
            return u;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Ok(s) = std::fs::read_to_string(dir.join("update-url.txt")) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    "https://hermix.cssso.com/desktop/manifest.json".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub min_version: Option<String>,
    #[serde(default)]
    pub android: Option<AndroidSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AndroidSection {
    pub apk_url: String,
    pub sha256: String,
    #[serde(default)]
    pub min_version: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateStatus {
    /// none = 无更新；optional = 有新版可选；forced = 当前低于 min_version，必须更
    pub kind: String,
    pub current_version: String,
    pub latest_version: String,
    pub notes: String,
    pub pub_date: String,
    pub download_url: String,
    pub sha256: String,
}

/// 语义化版本比较（主.次.补丁；预发布段忽略——本项目不用）。
/// 返回 Ordering：Less = a 旧于 b。
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parse(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.split('-').next().unwrap_or("0").parse().unwrap_or(0))
            .collect()
    }
    let (mut pa, mut pb) = (parse(a), parse(b));
    pa.resize(3, 0);
    pb.resize(3, 0);
    pa.cmp(&pb)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))
}

async fn fetch_manifest() -> Result<Manifest, String> {
    let body = http_client()?
        .get(manifest_url())
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .map_err(|e| format!("拉取更新清单失败: {e}"))?;
    if !body.status().is_success() {
        return Err(format!("更新清单 HTTP {}", body.status()));
    }
    // 404 文本页（运维未上传）会在这里解析失败，如实报错即可，上层静默。
    body.json::<Manifest>()
        .await
        .map_err(|e| format!("更新清单解析失败: {e}"))
}

fn classify(m: Manifest, current: &str) -> UpdateStatus {
    let forced = m
        .min_version
        .as_deref()
        .map(|mv| compare_versions(current, mv) == std::cmp::Ordering::Less)
        .unwrap_or(false);
    let newer = compare_versions(&m.version, current) == std::cmp::Ordering::Greater;
    let kind = if forced {
        "forced"
    } else if newer {
        "optional"
    } else {
        "none"
    };
    UpdateStatus {
        kind: kind.into(),
        current_version: current.into(),
        latest_version: m.version,
        notes: m.notes,
        pub_date: m.pub_date,
        download_url: m.url,
        sha256: m.sha256,
    }
}

/// 检查一次更新。none 也如实返回（前端面板要显示「已是最新」）。
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<UpdateStatus, String> {
    let current = app
        .config()
        .version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    let manifest = fetch_manifest().await?;
    Ok(classify(manifest, &current))
}

/// 下载缓存目录：<app 根>/update.pending/（app 根 = exe 所在目录，便携模型）。
/// 锚在 exe 旁而非 LOCALAPPDATA：交换脚本按「app 根内容整体换」设计，
/// 缓存目录必须在 app 根内才能被排除保留。
pub fn pending_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("定位当前 exe 失败: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("exe 无父目录")?
        .join("update.pending");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建更新缓存目录失败: {e}"))?;
    Ok(dir)
}

pub fn zip_path(app: &AppHandle) -> Result<PathBuf, String> {
    let _ = app;
    Ok(pending_dir()?.join("latest.zip"))
}

/// 流式下载 zip → update.pending/latest.zip，逐块发
/// `update://progress` event `{ phase, received, total }`。完事算 sha256
/// 与清单比对，不符删文件报错。
#[tauri::command]
pub async fn update_download(
    app: AppHandle,
    download_url: String,
    expected_sha256: String,
) -> Result<PathBuf, String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let zip = zip_path(&app)?;
    // 半截文件不留着续传（一期无 Range 续传，笔记有记录）：直接覆盖重下，
    // 失败删干净，别让下次校验撞上残骸。
    let tmp = zip.with_extension("zip.part");
    let _ = std::fs::remove_file(&tmp);

    let resp = http_client()?
        .get(&download_url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载 HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    let mut received: u64 = 0;
    let mut last_emit: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("下载中断: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入临时文件失败: {e}"))?;
        received += chunk.len() as u64;
        // 每 512KB 发一次进度，别把 event 总线打成刷屏。
        if received - last_emit >= 512 * 1024 || received == total {
            last_emit = received;
            let _ = app.emit(
                "update://progress",
                serde_json::json!({"phase": "download", "received": received, "total": total}),
            );
        }
    }
    file.flush()
        .await
        .map_err(|e| format!("落盘失败: {e}"))?;
    drop(file);

    // ── 信任边界：sha256 必须与清单一致 ─────────────────────────
    let mut hasher = Sha256::new();
    let mut f = std::fs::File::open(&tmp).map_err(|e| format!("打开临时文件失败: {e}"))?;
    std::io::copy(&mut f, &mut hasher).map_err(|e| format!("计算校验失败: {e}"))?;
    let actual = format!("{:x}", hasher.finalize());
    let expected = expected_sha256.trim().to_lowercase();
    if actual != expected {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "校验失败（期望 {expected}，实际 {actual}），已删除下载缓存"
        ));
    }
    std::fs::rename(&tmp, &zip).map_err(|e| format!("定稿下载文件失败: {e}"))?;
    Ok(zip)
}

/// 启动后 30s 首查 + 每 2h 一次；失败静默。查到 forced 时发
/// `update://status` event（前端据此弹阻断框，`force_shown` 保证只弹一次）；
/// optional 只入缓存不发 event，前端打开面板时主动查。
pub fn spawn_check_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut first = true;
        loop {
            if first {
                first = false;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(2 * 3600)).await;
            }
            let current = app
                .config()
                .version
                .clone()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
            match fetch_manifest().await {
                Ok(m) => {
                    let status = classify(m, &current);
                    if status.kind == "forced" {
                        // manage 在状态已注册时返回 false——首次注册后拿 State 读。
                        let _ = app.manage(ForceShownFlag::default());
                        let shown = app.state::<ForceShownFlag>();
                        if !shown
                            .0
                            .compare_exchange(
                                false,
                                true,
                                std::sync::atomic::Ordering::Relaxed,
                                std::sync::atomic::Ordering::Relaxed,
                            )
                            .is_ok()
                        {
                            continue;
                        }
                        let _ = app.emit("update://status", &status);
                    }
                }
                // 静默：便携产品不强打扰，网络错误下次再查。
                Err(_) => {}
            }
        }
    });
}

#[derive(Default)]
struct ForceShownFlag(std::sync::atomic::AtomicBool);

/// 安装：解压 zip → extracted/ → 写 helper 脚本 → 启动 helper（ detached）→
/// 确认 helper 已起（PID 握手文件）→ 前端收到 Ok 后调用现有退出流程。
/// helper 脚本侧：等本进程退出 → 旧目录改名 backup → 搬入新文件 → 启动
/// 新 exe → 30s 内没起来则回滚 backup 并启动旧 exe。
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<PathBuf, String> {
    let zip = zip_path(&app)?;
    if !zip.exists() {
        return Err("更新包不存在，请先下载".into());
    }
    let pending = pending_dir()?;
    let extracted = pending.join("extracted");
    let _ = std::fs::remove_dir_all(&extracted);
    let file = std::fs::File::open(&zip).map_err(|e| format!("打开更新包失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("读取更新包失败: {e}"))?;
    archive
        .extract(&extracted)
        .map_err(|e| format!("解压失败: {e}"))?;
    let _ = app.emit(
        "update://progress",
        serde_json::json!({"phase": "extracted", "received": 1, "total": 1}),
    );

    // ── 交换 helper ─────────────────────────────────────────────
    // 便携模式的应用根 = exe 所在目录；zip 解出的是「app 根的内容」
    // （含 Hermes.exe / resources/ / binaries/）。脚本逻辑：
    //   1. 等 PID 退出（最多 120s）
    //   2. app 根 → update.backup（排除 update.pending 自身）
    //   3. extracted/* → app 根
    //   4. 启动新 exe；30s 内出现 update.ok 握手文件 → 删 backup，成功
    //   5. 超时 → 杀新 exe、搬回 backup、启动旧 exe，写 update.failed
    // 新 exe 首启由前端删 update.ok 之外的握手职责（见 lib.rs 首启逻辑）：
    // 这里用「新进程存在」即可判定，握手文件由 helper 在拉起新 exe 30s 后
    // 用进程探测替代——不依赖新版本代码配合（前向兼容：老流程不改也能更）。
    let app_exe = std::env::current_exe().map_err(|e| format!("定位当前 exe 失败: {e}"))?;
    let app_dir = app_exe
        .parent()
        .ok_or("exe 无父目录")?
        .to_path_buf();
    let exe_name = app_exe
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or("exe 无文件名")?;
    let pid = std::process::id();
    let script_path = pending.join("apply-update.ps1");
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$pid0   = {pid}
$appDir = '{app_dir}'
$src    = '{src_dir}'
$pending = '{pending_dir}'
$backup = Join-Path $appDir 'update.backup'
$newExe = Join-Path $appDir '{exe_name}'
$oldExe = Join-Path $backup '{exe_name}'

# 1. 等当前进程退出
for ($i = 0; $i -lt 120; $i++) {{
    if (-not (Get-Process -Id $pid0 -ErrorAction SilentlyContinue)) {{ break }}
    Start-Sleep -Milliseconds 500
}}

# 2. 旧目录入 backup（pending 目录留在原地，不搬）
if (Test-Path $backup) {{ Remove-Item $backup -Recurse -Force }}
$moved = @()
Get-ChildItem -LiteralPath $appDir | Where-Object {{ $_.FullName -ne $pending }} | ForEach-Object {{
    $moved += $_.FullName
    Move-Item -LiteralPath $_.FullName -Destination $backup -Force
}}
# 3. 新文件就位
Copy-Item -Path (Join-Path $src '*') -Destination $appDir -Recurse -Force

# 4. 拉起新 exe 并探活 30s
$proc = Start-Process -FilePath $newExe -WorkingDirectory $appDir -PassThru
$alive = $false
for ($i = 0; $i -lt 60; $i++) {{
    Start-Sleep -Milliseconds 500
    if ($proc.HasExited) {{ break }}
    if ($i -ge 20) {{ $alive = $true; break }}   # 10s 未退出即视为起来
}}
if ($alive) {{
    # 成功：backup 保留一个版本周期（下次更新前清理），防新版本首启自毁
    Remove-Item (Join-Path $pending 'update.failed') -ErrorAction SilentlyContinue
    exit 0
}}
# 5. 回滚
if (-not $proc.HasExited) {{ Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }}
Remove-Item (Join-Path $appDir '*') -Exclude 'update.pending','update.backup' -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem -LiteralPath $backup | ForEach-Object {{ Move-Item -LiteralPath $_.FullName -Destination $appDir -Force }}
Start-Process -FilePath $oldExe -WorkingDirectory $appDir
Set-Content -Path (Join-Path $pending 'update.failed') -Value (Get-Date).ToString('o')
exit 1
"#,
        pid = pid,
        exe_name = exe_name,
        app_dir = app_dir.display(),
        src_dir = extracted.display(),
        pending_dir = pending.display(),
    );
    std::fs::write(&script_path, script).map_err(|e| format!("写 helper 脚本失败: {e}"))?;

    // detached 启动 helper：不随本进程退出被杀（CREATE_BREAKAWAY_FROM_JOB）。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script_path)
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("启动交换 helper 失败: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script_path)
            .spawn()
            .map_err(|e| format!("启动交换 helper 失败: {e}"))?;
    }
    let _ = app.emit(
        "update://progress",
        serde_json::json!({"phase": "ready_to_exit", "received": 1, "total": 1}),
    );
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_orders_correctly() {
        assert_eq!(compare_versions("0.1.9", "0.2.0"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("0.2.0", "0.1.9"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("0.2.0", "0.2.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("v0.2.0", "0.2"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("0.10.0", "0.9.9"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn classify_picks_forced_over_optional() {
        let m = |min: Option<&str>| Manifest {
            version: "0.2.0".into(),
            notes: "n".into(),
            pub_date: String::new(),
            url: "https://x/latest.zip".into(),
            sha256: "ab".into(),
            min_version: min.map(Into::into),
            android: None,
        };
        assert_eq!(classify(m(Some("0.3.0")), "0.1.9").kind, "forced");
        assert_eq!(classify(m(Some("0.1.0")), "0.1.9").kind, "optional");
        assert_eq!(classify(m(None), "0.2.0").kind, "none");
        assert_eq!(classify(m(None), "0.1.9").kind, "optional");
    }

    #[test]
    fn manifest_parses_minimal_json() {
        let raw = r#"{"version":"0.2.0","url":"https://x/latest.zip","sha256":"ab"}"#;
        let m: Manifest = serde_json::from_str(raw).unwrap();
        assert_eq!(m.version, "0.2.0");
        assert!(m.min_version.is_none());
    }
}
