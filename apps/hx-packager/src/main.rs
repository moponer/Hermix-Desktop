//! hx-packager — Hermix 自定义应用打包器 CLI
//!
//! 子命令:
//!   pack <app_dir> <id> [out_dir]        把 app_dir 打成单 exe (PE 资源嵌入)
//!   install <app_dir> <id>               打包并安装到 ~/.hermix/custom-apps/<id>/
//!   list                                  列出已安装 custom app
//!   uninstall <id>                        卸载已安装 app（含 sqlite 数据）
//!   dev <app_dir> <id>                    dev runner — spawn hx-shell 加载磁盘 app_dir
//!   info [app_dir]                        打印 app_dir manifest / 校验状态

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "hx-packager", version, about = "Hermix 自定义应用打包器")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 把 app_dir 打成单 exe — 产物放 app_dir/dist/<id>.exe
    Pack {
        app_dir: PathBuf,
        id: String,
        /// 自定义输出目录（默认 app_dir/dist）
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    /// 打包并安装到 ~/.hermix/custom-apps/<id>/ (dist/<id>.exe + manifest.json + installed.json)
    Install {
        app_dir: PathBuf,
        id: String,
        /// 跳过 pack 步骤，直接用 app_dir/dist/<id>.exe（要求已 pack）
        #[arg(long)]
        skip_pack: bool,
    },
    /// 列出已安装 custom app（扫 ~/.hermix/custom-apps/*/manifest.json）
    List,
    /// 卸载已安装 app（含 sqlite 数据，请先用 --keep-data 保留）
    Uninstall {
        id: String,
        /// 保留 __sqlite__/ 目录
        #[arg(long)]
        keep_data: bool,
    },
    /// 启动 dev runner — 把 hx-shell 指向磁盘 app_dir（不走打包）
    Dev {
        app_dir: PathBuf,
        id: String,
    },
    /// 打印 app_dir manifest / 校验状态
    Info {
        app_dir: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,hermix_plugin=info".into()))
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Pack { app_dir, id, out_dir } => cmd_pack(&app_dir, &id, out_dir.as_deref()),
        Cmd::Install { app_dir, id, skip_pack } => cmd_install(&app_dir, &id, skip_pack),
        Cmd::List => cmd_list(),
        Cmd::Uninstall { id, keep_data } => cmd_uninstall(&id, keep_data),
        Cmd::Dev { app_dir, id } => cmd_dev(&app_dir, &id),
        Cmd::Info { app_dir } => cmd_info(&app_dir),
    }
}

fn cmd_pack(app_dir: &Path, id: &str, out_dir: Option<&Path>) -> Result<()> {
    if !app_dir.is_dir() {
        anyhow::bail!("app_dir 不存在或不是目录: {}", app_dir.display());
    }
    let shell_path = hermix_plugin::resolve_hx_shell_path();
    let loader_dll_path = hermix_plugin::resolve_webview2_loader_path();
    if !shell_path.exists() {
        anyhow::bail!(
            "hx-shell.exe 找不到: {}\n请先 cargo build -p hx-shell 并 install 到 {}",
            shell_path.display(),
            shell_path.display()
        );
    }
    let packager = hermix_plugin::HxShellPackager::new(shell_path.clone(), loader_dll_path);
    // Phase 0/2: packager 包内读 PE 资源 + 写新 exe 至 app_dir/dist/<id>.exe
    let default_exe = packager
        .package(app_dir, id, hermix_plugin::PackageMode::Sidecar)
        .with_context(|| format!("打包失败: app_dir={}, id={}", app_dir.display(), id))?;

    if let Some(out) = out_dir {
        std::fs::create_dir_all(out)?;
        let target = out.join(format!("{id}.exe"));
        std::fs::copy(&default_exe, &target).with_context(|| {
            format!("复制 {} → {}", default_exe.display(), target.display())
        })?;
        println!("✓ 已打包: {}", target.display());
    } else {
        println!("✓ 已打包: {}", default_exe.display());
    }
    Ok(())
}

fn cmd_install(app_dir: &Path, id: &str, skip_pack: bool) -> Result<()> {
    if !app_dir.is_dir() {
        anyhow::bail!("app_dir 不存在或不是目录: {}", app_dir.display());
    }
    let dist_exe = app_dir.join("dist").join(format!("{id}.exe"));
    if !skip_pack {
        // 复用 cmd_pack 逻辑：确保 dist/<id>.exe 已生成
        cmd_pack(app_dir, id, None)?;
    } else if !dist_exe.exists() {
        anyhow::bail!("--skip-pack 但 dist/{} 不存在，请先 pack 或去掉 --skip-pack", dist_exe.display());
    }

    // 目标: ~/.hermix/custom-apps/<id>/
    let target_dir = hermix_plugin::resolve_custom_app_dir(id);
    let target_dist = target_dir.join("dist");
    let target_exe = target_dist.join(format!("{id}.exe"));
    let target_manifest = target_dir.join("manifest.json");
    let target_installed = target_dir.join("installed.json");

    std::fs::create_dir_all(&target_dist).with_context(|| {
        format!("创建 custom-app dist 目录失败: {}", target_dist.display())
    })?;

    // 拷 manifest.json
    let src_manifest = app_dir.join("manifest.json");
    if !src_manifest.exists() {
        anyhow::bail!("app_dir/manifest.json 不存在: {}", src_manifest.display());
    }
    std::fs::copy(&src_manifest, &target_manifest).with_context(|| {
        format!("拷 manifest.json 失败: {} → {}", src_manifest.display(), target_manifest.display())
    })?;

    // 拷 dist/<id>.exe
    std::fs::copy(&dist_exe, &target_exe).with_context(|| {
        format!("拷 exe 失败: {} → {}", dist_exe.display(), target_exe.display())
    })?;

    // 写 installed.json
    let installed_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let installed = serde_json::json!({
        "installed_at": installed_at_unix,
        "app_dir": app_dir.display().to_string(),
        "hx_packager_version": env!("CARGO_PKG_VERSION"),
        "exe": format!("dist/{id}.exe"),
        "manifest": "manifest.json",
    });
    std::fs::write(
        &target_installed,
        serde_json::to_string_pretty(&installed)?,
    )?;

    println!("✓ 已安装: {}", target_dir.display());
    println!("    manifest: {}", target_manifest.display());
    println!("    exe:      {}", target_exe.display());
    println!("    metadata: {}", target_installed.display());
    Ok(())
}

fn cmd_list() -> Result<()> {
    let root = hermix_plugin::resolve_custom_apps_dir();
    if !root.exists() {
        println!("(custom-apps 目录不存在: {})", root.display());
        return Ok(());
    }
    let mut found = 0;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();

    for app_dir in entries {
        let manifest = app_dir.join("manifest.json");
        if !manifest.exists() {
            continue;
        }
        found += 1;
        let id = app_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let txt = std::fs::read_to_string(&manifest).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&txt).unwrap_or_default();
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("(no name)");
        let entry = v.get("entry").and_then(|x| x.as_str()).unwrap_or("?");
        let icon = v.get("icon").and_then(|x| x.as_str()).unwrap_or("?");

        let installed_at = std::fs::read_to_string(app_dir.join("installed.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("installed_at").and_then(|x| x.as_u64()));

        println!("• {id}  [{name}]");
        println!("    entry: {entry}  icon: {icon}");
        if let Some(ts) = installed_at {
            // 把 unix timestamp 粗略格式化成本地时间（前缀只到秒）
            let secs = ts;
            println!("    installed_at: {secs} (unix)");
        }
        println!("    path: {}", app_dir.display());
    }
    if found == 0 {
        println!("(无已安装 app)");
    } else {
        println!("\n共 {found} 个已安装 app");
    }
    Ok(())
}

fn cmd_uninstall(id: &str, keep_data: bool) -> Result<()> {
    let target = hermix_plugin::resolve_custom_app_dir(id);
    if !target.exists() {
        anyhow::bail!("custom app 不存在: {}", target.display());
    }
    let data_dir = target.join("__sqlite__");
    let data_exists = data_dir.exists();

    // 用 move-out 而非 remove_dir_all：sandbox/WebView2 LOCK 常驻 rm 会被拦，
    // 但 mv 出 custom-apps/ 目录到 trash 通常放行（与 .cargo-lock 移出仓库一致）。
    let trash_root = std::env::var_os("TEMP")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMP").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hermix-uninstall-trash");
    std::fs::create_dir_all(&trash_root)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let trash_target = trash_root.join(format!("{id}-{ts}"));

    if keep_data && data_exists {
        let data_backup = trash_root.join(format!("{id}-{ts}-data"));
        std::fs::rename(&data_dir, &data_backup).with_context(|| {
            format!("搬 data 失败: {} → {}", data_dir.display(), data_backup.display())
        })?;
        std::fs::rename(&target, &trash_target).with_context(|| {
            format!("移 app dir 失败: {} → {}", target.display(), trash_target.display())
        })?;
        // 重建空 dir 把 data 移回去（下次 install 时会覆盖，数据保留）
        std::fs::create_dir_all(&target)?;
        std::fs::rename(&data_backup, &data_dir)?;
        println!("✓ 已卸载: {} → {} (数据保留在 {})", target.display(), trash_target.display(), data_dir.display());
    } else {
        std::fs::rename(&target, &trash_target).with_context(|| {
            format!("移 app dir 失败: {} → {}", target.display(), trash_target.display())
        })?;
        if data_exists {
            println!("⚠️  已卸载: {} → {} (含 __sqlite__/ 数据)", target.display(), trash_target.display());
        } else {
            println!("✓ 已卸载: {} → {}", target.display(), trash_target.display());
        }
    }
    Ok(())
}

fn cmd_dev(app_dir: &Path, id: &str) -> Result<()> {
    if !app_dir.is_dir() {
        anyhow::bail!("app_dir 不存在或不是目录: {}", app_dir.display());
    }
    let shell_path = hermix_plugin::resolve_hx_shell_path();
    if !shell_path.exists() {
        anyhow::bail!("hx-shell.exe 找不到: {}", shell_path.display());
    }
    let packager = hermix_plugin::HxShellPackager::new(shell_path.clone(), hermix_plugin::resolve_webview2_loader_path());
    packager
        .run_dev(app_dir, id)
        .with_context(|| format!("dev runner 启动失败: app_dir={}, id={}", app_dir.display(), id))?;
    Ok(())
}

fn cmd_info(app_dir: &Path) -> Result<()> {
    let manifest_path = app_dir.join("manifest.json");
    if manifest_path.exists() {
        let txt = std::fs::read_to_string(&manifest_path)?;
        println!("=== manifest.json ({} bytes) ===", txt.len());
        // 解析打印关键字段（不强 schema，LLM 自己读全文）
        let v: serde_json::Value = serde_json::from_str(&txt)?;
        if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
            println!("id: {id}");
        }
        if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
            println!("name: {name}");
        }
        if let Some(entry) = v.get("entry").and_then(|x| x.as_str()) {
            println!("entry: {entry}");
        }
        if let Some(icon) = v.get("icon").and_then(|x| x.as_str()) {
            println!("icon: {icon}");
        }
        println!("\n(raw 内容已读取，调用方按需 serde_json::from_str)");
    } else {
        println!("(无 manifest.json — 默认 app)");
    }
    println!("\n=== app_dir 结构 ===");
    for entry in walkdir(app_dir)? {
        println!("  {}", entry.strip_prefix(app_dir).unwrap_or(&entry).display());
    }
    Ok(())
}

/// 简易 walkdir（避免引入 walkdir crate）— 单层递归
fn walkdir(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    fn rec(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if p.is_file() {
            out.push(p.to_path_buf());
            return Ok(());
        }
        if p.is_dir() {
            for entry in std::fs::read_dir(p)? {
                let entry = entry?;
                rec(&entry.path(), out)?;
            }
        }
        Ok(())
    }
    rec(root, &mut out)?;
    out.sort();
    Ok(out)
}

#[allow(dead_code)]
fn _unused_command_marker() -> Command {
    Command::new("") // 占位避免 unused 警告
}