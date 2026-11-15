// Prevents additional console window on Windows in release, DO NOT REMOVE!!
// 无条件在所有 profile (debug + release) 隐藏 console。之前
// `cfg_attr(not(debug_assertions), ...)` 只在 release 生效，debug 启动
// 仍会闪黑 cmd 窗口。Debug 构建保持 fast — linker /SUBSYSTEM flag 纯装饰。
#![windows_subsystem = "windows"]

fn main() {
    hermix_desktop_lib::run()
}
