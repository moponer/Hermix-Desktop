//! Hermix 安装器：Tauri 单窗向导 + 载荷自提取。
//!
//! 决策记录：docs/notes/implemented/feature/2026-10-09-custom-installer.md
//! （自研安装器，NSIS 退场；每次构建出「安装包 exe + 更新包 zip」双产物）。
//!
//! 组织：命令在 commands.rs（lib.rs 根上定义会 E0255，见该文件头注释）；
//! 提取/快捷方式/卸载登记在 payload.rs。

pub mod commands;
pub mod payload;

use tauri::Builder;

pub fn run() {
    Builder::default()
        .manage(commands::InstallState::default())
        .invoke_handler(tauri::generate_handler![
            commands::installer_meta,
            commands::installer_license,
            commands::installer_pick_dir,
            commands::installer_install,
            commands::installer_finish,
        ])
        .run(tauri::generate_context!())
        .expect("installer: failed to run tauri app");
}
