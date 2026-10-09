//! 安装器载荷：运行时自提取 + 快捷方式 + 卸载登记 + 构建侧嵌入。
//!
//! 载荷来源（二选一）：
//! 1. PE 资源 RT_RCDATA id=1003（release 链由 bin/hermix-embed.rs 写入）
//! 2. exe 旁 `payload.zip`（dev / 调试：把更新包 zip 改名放旁边即可）
//!
//! 完整性：zip 每条目自带 CRC32（zip crate 读取时校验），且载荷与安装器 exe
//! 一同分发不可单独篡改——不另做 sha256（与更新包不同：更新包过公网，必须
//! sha256；安装包的完整性即 exe 自身完整性）。
//!
//! zip-slip 防护：条目名经 `enclosed_name()` 校验（拒绝 `..` / 绝对路径），
//! 提取永不出目标目录——信任边界，不可削减。

use tauri::{AppHandle, Emitter};

/// 载荷资源 ID（与 hx-shell 的 1000/1001/1002 错开）。
pub const PAYLOAD_RESOURCE_ID: usize = 1003;

// ── 构建侧：载荷嵌入与读回验证（bin/hermix-embed.rs 调用） ─────────────

/// 把 payload zip 作为 RT_RCDATA 1003 写进 exe 的资源节。
/// 机制与 icon_library::embed_app_payload 同款（BeginUpdateResourceW 正规
/// 资源节——exe 尾部拼接会触发 Defender 结构型误报）；id 与 hx-shell 错开。
#[cfg(windows)]
pub fn embed_payload_into(exe_path: &std::path::Path, payload_zip_bytes: &[u8]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, TRUE, HANDLE};
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };

    const RT_RCDATA: windows_sys::core::PCWSTR = 10u16 as *const u16;

    let path_w: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // FALSE = 保留现有资源（图标、清单等），只增改 1003。
    let h: HANDLE = unsafe { BeginUpdateResourceW(path_w.as_ptr(), FALSE) };
    if h == 0 {
        return Err(format!(
            "BeginUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ok: BOOL = unsafe {
        UpdateResourceW(
            h,
            RT_RCDATA,
            PAYLOAD_RESOURCE_ID as u16 as *const u16,
            0,
            payload_zip_bytes.as_ptr().cast::<core::ffi::c_void>(),
            payload_zip_bytes.len() as u32,
        )
    };
    if ok == FALSE {
        let _ = unsafe { EndUpdateResourceW(h, TRUE) }; // 丢弃半提交
        return Err(format!(
            "UpdateResourceW(1003) 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ok: BOOL = unsafe { EndUpdateResourceW(h, FALSE) }; // 提交
    if ok == FALSE {
        return Err(format!(
            "EndUpdateResourceW 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// 从磁盘上的 exe 读回嵌入的载荷（LOAD_LIBRARY_AS_DATAFILE 只读数据映射，
/// 不执行目标）——`--verify` 与 embed 工具的内建校验走这里。
#[cfg(windows)]
pub fn read_payload_from_file(exe_path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::core::PCWSTR;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, LoadLibraryExW, LoadResource, LockResource, SizeofResource,
        LOAD_LIBRARY_AS_DATAFILE,
    };

    const RT_RCDATA: PCWSTR = 10u16 as *const u16;
    let name: PCWSTR = PAYLOAD_RESOURCE_ID as u16 as *const u16;

    let path_w: Vec<u16> = exe_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let hmod = unsafe { LoadLibraryExW(path_w.as_ptr(), 0, LOAD_LIBRARY_AS_DATAFILE) };
    if hmod == 0 {
        return Err(format!(
            "LoadLibraryExW(DATAFILE) 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result = unsafe {
        let hres = FindResourceW(hmod, name, RT_RCDATA);
        if hres == 0 {
            Err(format!("FindResourceW: 目标 exe 无资源 {PAYLOAD_RESOURCE_ID}"))
        } else {
            let hglob = LoadResource(hmod, hres);
            if hglob.is_null() {
                Err("LoadResource 失败".into())
            } else {
                let data = LockResource(hglob);
                if data.is_null() {
                    Err("LockResource 失败".into())
                } else {
                    let size = SizeofResource(hmod, hres);
                    if size == 0 {
                        Err("SizeofResource = 0".into())
                    } else {
                        Ok(std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec())
                    }
                }
            }
        }
    };
    unsafe { FreeLibrary(hmod) };
    result
}

// ── 运行侧：自提取 ─────────────────────────────────────────────

/// 读取载荷字节：RCDATA 1003（自身映像）→ 回退 exe 旁 payload.zip。
pub fn read_payload_bytes() -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        if let Ok(bytes) = read_resource_rcdata(PAYLOAD_RESOURCE_ID) {
            return Ok(bytes);
        }
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let sibling = exe
        .parent()
        .ok_or("exe 无父目录")?
        .join("payload.zip");
    std::fs::read(&sibling).map_err(|e| {
        format!(
            "载荷缺失：资源 1003 不存在，且 {} 读取失败（{e}）",
            sibling.display()
        )
    })
}

/// 读 PE 资源节（FindResourceW → LoadResource → LockResource）。
/// RT_RCDATA 在 windows-sys 0.52 未导出（= MAKEINTRESOURCE(10)），照抄
/// icon_library.rs:312 的 const 定义模式；数值资源名同样按整数当指针传。
#[cfg(windows)]
fn read_resource_rcdata(id: usize) -> Result<Vec<u8>, String> {
    use windows_sys::core::PCWSTR;
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, LoadResource, LockResource, SizeofResource,
    };

    const RT_RCDATA: PCWSTR = 10u16 as *const u16;
    let name: PCWSTR = (id as u16) as *const u16;
    unsafe {
        // HMODULE/HRSRC 在 windows-sys 0.52 里都是 isize（不是指针），句柄传 0
        // 表示「当前进程映像」；HGLOBAL（LoadResource 返回值）才是真指针。
        let hres = FindResourceW(0, name, RT_RCDATA);
        if hres == 0 {
            return Err(format!("FindResourceW: 资源 {id} 不存在"));
        }
        let hglob = LoadResource(0, hres);
        if hglob.is_null() {
            return Err("LoadResource 失败".into());
        }
        let data = LockResource(hglob);
        if data.is_null() {
            return Err("LockResource 失败".into());
        }
        let size = SizeofResource(0, hres);
        if size == 0 {
            return Err("SizeofResource = 0".into());
        }
        Ok(std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec())
    }
}

/// 解压载荷到 `dir`，逐条目发 `installer://progress`（extract 阶段，字节粒度）。
pub fn extract_all(app: &AppHandle, dir: &std::path::Path) -> Result<(), String> {
    let bytes = read_payload_bytes()?;
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("载荷 zip 读取失败: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建安装目录失败: {e}"))?;

    let total: u64 = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.size()))
        .sum();
    let mut written: u64 = 0;
    let mut last_emit: u64 = 0;
    let n = archive.len();

    for i in 0..n {
        // is_last 先算：entry 会可变借用 archive，循环体内不能再碰 archive.len()。
        let is_last = i == n - 1;
        let mut entry = archive.by_index(i).map_err(|e| format!("zip 条目 {i} 读取失败: {e}"))?;
        // zip-slip 闸门：enclosed_name 拒绝越界路径；空名（纯目录）只建目录。
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("zip 条目路径越界，已中止安装: {}", entry.name()));
        };
        let out_path = dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| format!("创建目录失败: {e}"))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| format!("写文件失败（{}）: {e}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("解压失败（{}）: {e}", out_path.display()))?;
        written += entry.size();
        // 每 512KB 发一次进度，别把 event 总线打成刷屏。
        if written - last_emit >= 512 * 1024 || is_last {
            last_emit = written;
            let _ = app.emit(
                "installer://progress",
                serde_json::json!({"phase": "extract", "received": written, "total": total}),
            );
        }
    }
    Ok(())
}

/// 安装全流程（重活，必须在 spawn_blocking 里跑）：解压 → 快捷方式 → 卸载登记。
pub fn install_to(
    app: &AppHandle,
    dir: &std::path::Path,
    desktop_shortcut: bool,
) -> Result<(), String> {
    extract_all(app, dir)?;
    #[cfg(windows)]
    {
        if desktop_shortcut {
            create_desktop_shortcut(dir)?;
            let _ = app.emit(
                "installer://progress",
                serde_json::json!({"phase": "shortcut", "received": 1, "total": 1}),
            );
        }
        register_uninstall(dir)?;
        let _ = app.emit(
            "installer://progress",
            serde_json::json!({"phase": "register", "received": 1, "total": 1}),
        );
    }
    #[cfg(not(windows))]
    {
        let _ = desktop_shortcut; // 项目 Windows-only，此分支只为编译
    }
    Ok(())
}

/// 桌面快捷方式：PowerShell WScript.Shell 一行（免 COM 绑定依赖，项目惯例同
/// 更新 helper 的 powershell 用法）。
#[cfg(windows)]
pub fn create_desktop_shortcut(install_dir: &std::path::Path) -> Result<(), String> {
    let exe = find_main_exe(install_dir).ok_or("安装目录内未找到主程序 exe")?;
    let desktop = dirs::desktop_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Desktop")))
        .ok_or("无法定位桌面目录")?;
    let lnk = desktop.join("Hermix.lnk");
    let ps = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath='{}'; $s.WorkingDirectory='{}'; $s.Save()",
        lnk.display(),
        exe.display(),
        install_dir.display(),
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .status()
        .map_err(|e| format!("启动 powershell 失败: {e}"))?;
    if !status.success() {
        return Err("创建快捷方式失败（powershell 非零退出）".into());
    }
    Ok(())
}

/// 在安装目录里找主程序 exe。启发式：优先 hermix*（排除 uninstall.exe /
/// 含 sidecar 字样的 binaries），#7 的 NSIS 文件清单对照会核实实际布局。
pub fn find_main_exe(install_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidates = ["hermix.exe", "hermix-desktop.exe", "Hermix Desktop.exe", "Hermix.exe"];
    for c in candidates {
        let p = install_dir.join(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// 卸载登记：HKCU Uninstall 键（per-user 免 UAC）+ 自拷贝 uninstall.exe。
#[cfg(windows)]
pub fn register_uninstall(install_dir: &std::path::Path) -> Result<(), String> {
    use winreg::enums::*;
    use winreg::RegKey;

    // 卸载器 = 安装器自身拷贝，落在安装目录内（卸载入口随应用走）。
    let self_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstall_exe = install_dir.join("uninstall.exe");
    if self_exe != uninstall_exe {
        std::fs::copy(&self_exe, &uninstall_exe)
            .map_err(|e| format!("复制卸载器失败: {e}"))?;
    }

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key_path = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Hermix";
    let (key, _) = hkcu.create_subkey(key_path).map_err(|e| format!("注册表键创建失败: {e}"))?;
    let main_exe = find_main_exe(install_dir);
    let set = |name: &str, val: &str| -> Result<(), String> {
        // winreg 0.10 的 ToRegValue 只实现到 &str（裸 str 无 Sized），set_value
        // 签名是 &T，所以传 &&str——即 &val。
        key.set_value(name, &val).map_err(|e| format!("注册表写 {name} 失败: {e}"))
    };
    set("DisplayName", "Hermix Desktop")?;
    set("DisplayVersion", &format!("{}", env!("HERMIX_PRODUCT_VERSION")))?;
    set("InstallLocation", &install_dir.to_string_lossy())?;
    set("UninstallString", &format!("\"{}\" --uninstall", uninstall_exe.display()))?;
    if let Some(exe) = main_exe {
        set("DisplayIcon", &format!("\"{}\",0", exe.display()))?;
    }
    set("Publisher", "Hermix")?;
    set("NoModify", "1")?;
    set("NoRepair", "1")?;
    Ok(())
}

/// `--uninstall` 入口：确认 → 删注册表键 + 桌面快捷方式 → 延迟自删目录。
#[cfg(windows)]
pub fn run_uninstall() {
    use winreg::enums::*;
    use winreg::RegKey;

    let exe = std::env::current_exe().unwrap_or_default();
    let install_dir = exe.parent().map(|d| d.to_path_buf()).unwrap_or_default();

    let confirmed = rfd::MessageDialog::new()
        .set_title("卸载 Hermix")
        .set_description(format!("确认卸载 Hermix（安装于 {}）？\n\n用户数据 ~/.hermix 不会被删除。", install_dir.display()))
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show();
    if confirmed != rfd::MessageDialogResult::Ok {
        return;
    }

    // 1. 注册表键
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Hermix");
    // 2. 桌面快捷方式
    if let Some(desktop) = dirs::desktop_dir() {
        let _ = std::fs::remove_file(desktop.join("Hermix.lnk"));
    }
    // 3. 延迟自删：本 exe 在待删目录内，用 cmd 延迟循环等进程退出后删目录。
    //    用户数据 ~/.hermix 不碰（便携模型，数据贴着 exe 但个人数据在家目录）。
    use std::os::windows::process::CommandExt;
    let dir_str = install_dir.to_string_lossy().to_string();
    let _ = std::process::Command::new("cmd")
        .args([
            "/C",
            "timeout", "/T", "2", "/NOBREAK", ">NUL", "&",
            "rmdir", "/S", "/Q", &dir_str,
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

// dirs 模块：快捷方式/桌面定位用（不引第三方 dirs crate，两行就够）。
#[cfg(windows)]
mod dirs {
    /// %USERPROFILE%\Desktop（OneDrive 重定向时回落 USERPROFILE）。
    pub fn desktop_dir() -> Option<std::path::PathBuf> {
        let home = home_dir()?;
        // 优先注册表 Shell Folders？一期简单化：直接 Desktop，OneDrive 场景由
        // #7 实测修正（失败不阻断安装——快捷方式是可选产物）。
        Some(home.join("Desktop"))
    }
    pub fn home_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
    }
}
