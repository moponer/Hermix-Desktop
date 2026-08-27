//! 端到端集成测试：模拟 LLM 流程（create → package → run → 验证）
//!
//! 覆盖 hermix-plugin 集成 hx-shell 打包 + 运行时全链路。
//! 用 HERMIX_HOME 指向临时目录 + 准备好的 hx-shell.exe + WebView2Loader.dll。

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use hermix_plugin::PluginManager;

const SHELL: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\hx-shell.exe";
const LOADER_DLL: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\WebView2Loader.dll";
const ESBUILD: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\esbuild.exe";
const REACT_UMD: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\react\react.production.min.js";
const REACT_DOM_UMD: &str = r"<repo>\apps\desktop-tauri\src-tauri\resources\loader\react\react-dom.production.min.js";

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

/// TSX/React 模式打包：esbuild 编译 src/main.tsx → React UMD inline → 运行渲染
#[tokio::test]
async fn tsx_react_package_builds_and_runs() {
    // 需要 esbuild + React UMD 就位（Hermix 内置）
    if !PathBuf::from(ESBUILD).exists() {
        eprintln!("[skip] esbuild not present at {ESBUILD}");
        return;
    }
    if !PathBuf::from(REACT_UMD).exists() || !PathBuf::from(REACT_DOM_UMD).exists() {
        eprintln!("[skip] React UMD not present");
        return;
    }

    let tmp = std::env::temp_dir().join(format!("hermix_e2e_tsx_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let loader_dir = tmp.join("loader");
    std::fs::create_dir_all(&loader_dir).unwrap();
    std::fs::copy(SHELL, loader_dir.join("hx-shell.exe")).unwrap();
    std::fs::copy(LOADER_DLL, loader_dir.join("WebView2Loader.dll")).unwrap();
    std::fs::copy(ESBUILD, loader_dir.join("esbuild.exe")).unwrap();
    let react_dir = loader_dir.join("react");
    std::fs::create_dir_all(&react_dir).unwrap();
    std::fs::copy(REACT_UMD, react_dir.join("react.production.min.js")).unwrap();
    std::fs::copy(REACT_DOM_UMD, react_dir.join("react-dom.production.min.js")).unwrap();
    let apps_dir = tmp.join("custom-apps");
    std::fs::create_dir_all(&apps_dir).unwrap();

    // app: index.html（含 #root）+ src/main.tsx（React 全局变量）
    let app_dir = apps_dir.join("tsx-app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    std::fs::write(
        app_dir.join("manifest.json"),
        r#"{"id":"tsx-app","name":"TSX","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        app_dir.join("index.html"),
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>TSX</title></head><body><div id="root"></div></body></html>"#,
    )
    .unwrap();
    std::fs::write(
        app_dir.join("src/main.tsx"),
        r##"const { useState } = React;
const { createRoot } = ReactDOM;
function App() {
  const [n, setN] = useState(0);
  return <div id="counter"><h1>{n}</h1><button onClick={() => setN(n+1)}>+</button></div>;
}
createRoot(document.getElementById("root")).render(<App />);
"##,
    )
    .unwrap();

    // 打包（HERMIX_HOME 指向临时目录）
    std::env::set_var("HERMIX_HOME", &tmp);
    let mgr = PluginManager::new(apps_dir.clone());
    let entries = mgr.scan_plugins().expect("scan");
    assert_eq!(entries.len(), 1);
    let exe = mgr.package_plugin("tsx-app").await.expect("package tsx app");
    assert!(exe.exists(), "exe not generated: {:?}", exe);

    // 产物应包含 React UMD + 编译后的 JS（> 100KB 说明 React 内嵌了）
    let size = std::fs::metadata(&exe).map(|m| m.len()).unwrap_or(0);
    println!("[e2e-tsx] packaged {} bytes", size);
    assert!(size > 100_000, "TSX 产物应含 React UMD（>100KB），实际 {size}");

    // 运行 + 检查 React 渲染（页面写 auto-verify 冒烟）
    let _ = Command::new("taskkill").args(["/F", "/IM", "tsx-app.exe"]).output();
    let mut child = Command::new(&exe).spawn().expect("spawn");
    std::thread::sleep(Duration::from_secs(12));
    let _ = child.kill();
    let _ = Command::new("taskkill").args(["/F", "/IM", "tsx-app.exe", "/T"]).output();
    let _ = std::fs::remove_dir_all(&tmp);
}
