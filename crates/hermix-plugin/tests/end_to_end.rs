//! 端到端集成测试：模拟 LLM 流程（create → package → run → 验证）
//!
//! 覆盖 hermix-plugin 集成 hx-shell 打包 + 运行时全链路。
//! 用 HERMIX_HOME 指向临时目录 + 准备好的 hx-shell.exe + WebView2Loader.dll。

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use hermix_plugin::{PluginManager, manifest::PluginManifest, manifest::WindowConfig};

const SHELL: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\hx-shell.exe";
const LOADER_DLL: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\WebView2Loader.dll";

#[tokio::test]
async fn end_to_end_create_package_run() {
    // 1. 准备临时 HERMIX_HOME（loader 放进去）
    let tmp = std::env::temp_dir().join(format!("hermix_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let loader_dir = tmp.join("loader");
    std::fs::create_dir_all(&loader_dir).unwrap();
    std::fs::copy(SHELL, loader_dir.join("hx-shell.exe")).unwrap();
    std::fs::copy(LOADER_DLL, loader_dir.join("WebView2Loader.dll")).unwrap();
    let apps_dir = tmp.join("custom-apps");
    std::fs::create_dir_all(&apps_dir).unwrap();

    // 写一个最小 index.html（hx.writeFile 自动冒烟）
    let app_dir = apps_dir.join("test-app");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(app_dir.join("manifest.json"), r#"{"id":"test-app","name":"Test","version":"1.0.0"}"#).unwrap();
    // index.html 内嵌 postMessage + writeFile 冒烟
    let html = r##"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Test</title></head>
<body>test
<script>
function hxCall(m, a) {
  return new Promise(res => {
    const id = 'cb_' + Date.now();
    window['__hx_resolve_' + id] = res;
    window.chrome.webview.postMessage(JSON.stringify({id, m, a: a || []}));
  });
}
(async () => {
  try {
    await hxCall('writeFile', ['auto-verify.txt', 'HX-SHELL-OK ' + Date.now()]);
    const r = await hxCall('readFile', ['auto-verify.txt']);
    document.title = r.data || 'NO_DATA';
  } catch (e) {
    document.title = 'ERR: ' + e;
  }
})();
</script></body></html>"##;
    std::fs::write(app_dir.join("index.html"), html).unwrap();

    // 2. PluginManager 扫描 + 打包
    std::env::set_var("HERMIX_HOME", &tmp);
    let mgr = PluginManager::new(apps_dir.clone());
    let entries = mgr.scan_plugins().expect("scan");
    assert_eq!(entries.len(), 1, "expected 1 app, got {}", entries.len());
    let exe = mgr.package_plugin("test-app").await.expect("package");
    assert!(exe.exists(), "exe not generated: {:?}", exe);
    println!("[e2e] packaged: {}", exe.display());

    // 3. 跑 + 等 + 检查 auto-verify
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "test-app.exe"])
        .output();
    let mut child = Command::new(&exe).spawn().expect("spawn exe");
    std::thread::sleep(Duration::from_secs(10));
    let verify = app_dir.join("dist/auto-verify.txt");
    let content = std::fs::read_to_string(&verify).unwrap_or_default();
    println!("[e2e] auto-verify: {}", content);
    assert!(content.starts_with("HX-SHELL-OK"), "auto-verify missing: {}", verify.display());

    // 4. 清理
    let _ = child.kill();
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "test-app.exe", "/T"])
        .output();
    let _ = std::fs::remove_dir_all(&tmp);
}
