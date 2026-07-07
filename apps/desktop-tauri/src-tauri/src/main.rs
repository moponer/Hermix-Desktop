// Prevents additional console window on Windows in release, DO NOT REMOVE!!
// v33 (2026-07-06): unconditionally hide the console in *every*
// profile (debug + release). The previous `cfg_attr(not(debug_assertions), ...)`
// only kicked in for release builds, so `cargo tauri dev` still
// flashed a black cmd window on every startup. Debug builds stay
// debug-fast — the linker /SUBSYSTEM flag is purely cosmetic.
#![windows_subsystem = "windows"]

fn main() {
    hermix_desktop_lib::run()
}
