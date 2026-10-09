// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![windows_subsystem = "windows"]

fn main() {
    // --uninstall：卸载模式走原生对话框，不需要 WebView，直接短路。
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some("--uninstall") {
        hermix_installer::payload::run_uninstall();
        return;
    }
    hermix_installer::run()
}
