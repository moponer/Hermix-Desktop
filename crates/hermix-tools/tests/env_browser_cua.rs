//! 真实环境验收测试：CUA（窗口/UIA 枚举）+ 浏览器（填表 / ref 句柄 / 登录态）。
//!
//! 常规 `cargo test` 下全部跳过，只有显式 `HERMIX_REAL_ENV=1` 才真跑 ——
//! 这些用例需要本机 Windows 桌面会话和一个真实的 CDP 浏览器实例。
//!
//! 本地跑法：
//! ```text
//! HERMIX_REAL_ENV=1 cargo test -p hermix-tools --test env_browser_cua \
//!   -- --nocapture --test-threads=1
//! ```
//! `--test-threads=1` 必需：用例共享 127.0.0.1:9333 上的浏览器实例，
//! 并且会写 `CHROME_CDP_URL` 环境变量。

#![cfg(windows)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use hermix_core::ToolHandler;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use hermix_tools::backends::browser::registry::BrowserRegistry;
use hermix_tools::backends::browser::session_wrapper::SessionedBrowserBackend;
use hermix_tools::tools::browser::{
    BrowserClickHandler, BrowserNavigateHandler, BrowserSnapshotHandler, BrowserTypeHandler,
};
use hermix_tools::tools::cua::{CuaListWindowsHandler, CuaUiFindHandler, CuaUiTreeHandler};

/// 验收专用调试端口（避开默认 9222，防止与用户手动开的实例打架）。
const CDP_PORT: u16 = 9333;

fn real_env() -> bool {
    std::env::var("HERMIX_REAL_ENV")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn field(raw: &str, key: &str) -> Value {
    serde_json::from_str::<Value>(raw)
        .unwrap_or_else(|e| panic!("tool output not JSON ({e}): {raw}"))
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("missing `{key}` in tool output: {raw}"))
}

// ---------------------------------------------------------------------------
// 本地 HTTP 夹具：带完整表单控件的页面 + 服务端可见的原始请求轨迹
// ---------------------------------------------------------------------------

const FORM_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Hermix Form Fixture</title></head>
<body>
<h1>Account Setup</h1>
<form id="f">
  <label for="name">Full name</label>
  <input id="name" name="name" type="text" placeholder="Your name">
  <label for="email">Email</label>
  <input id="email" name="email" type="email">
  <label for="bio">Bio</label>
  <textarea id="bio" name="bio" rows="3"></textarea>
  <label for="plan">Plan</label>
  <select id="plan" name="plan">
    <option value="free">Free tier</option>
    <option value="pro">Pro tier</option>
  </select>
  <input id="terms" name="terms" type="checkbox" value="yes">
  <label for="terms">I agree to the terms</label>
  <button id="go" type="submit">Create account</button>
</form>
<div id="result" role="status"></div>
<script>
document.getElementById('f').addEventListener('submit', function (e) {
  e.preventDefault();
  var el = document.getElementById('result');
  var fd = new FormData(e.target);
  var parts = [];
  fd.forEach(function (v, k) { parts.push(k + '=' + v); });
  el.textContent = 'RESULT_OK ' + parts.join(' ');
  // 把 fetch 的结果写回页面：验收时靠这个区分“没调用”和“调用了但失败”。
  try {
    fetch('/api?' + parts.join('&')).then(
      function (r) { el.textContent += ' FETCH_OK:' + r.status; },
      function (err) { el.textContent += ' FETCH_ERR:' + err; }
    );
  } catch (err) {
    el.textContent += ' THROW:' + err;
  }
});
</script>
</body></html>
"#;

/// BUG-13 双会话隔离夹具：两页文本互斥，快照串台立刻现形。
const ALPHA_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>PAGE_ALPHA</title></head>
<body><h1>PAGE_ALPHA heading</h1><p>Alpha page is the only page session A should ever see.</p></body></html>
"#;

const BETA_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>PAGE_BETA</title></head>
<body><h1>PAGE_BETA heading</h1><p>Beta page is the only page session B should ever see.</p></body></html>
"#;

/// 抓到的原始 HTTP 请求（含请求头，才能验证 Cookie 是否回传）。
#[derive(Debug, Clone)]
struct Captured {
    request_line: String,
    raw: String,
}

/// 起最小 HTTP 服务：返回表单页 / 持久化 cookie 端点，并记录所有原始请求。
async fn spawn_fixture_server() -> (u16, Arc<Mutex<Vec<Captured>>>) {
    let seen = Arc::new(Mutex::new(Vec::<Captured>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fixture server");
    let port = listener.local_addr().expect("local addr").port();
    let sink = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let mut buf = [0u8; 16_384];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let request_line = raw.lines().next().unwrap_or("").to_string();
                sink.lock().unwrap().push(Captured {
                    request_line: request_line.clone(),
                    raw: raw.clone(),
                });

                let target = request_line.split_whitespace().nth(1).unwrap_or("/");
                let path = target.split('?').next().unwrap_or("/");
                let body = match path {
                    "/form.html" => FORM_HTML.to_string(),
                    "/alpha" => ALPHA_HTML.to_string(),
                    "/beta" => BETA_HTML.to_string(),
                    _ => "OK".to_string(),
                };
                let ctype = if path == "/form.html" || path == "/alpha" || path == "/beta" {
                    "text/html; charset=utf-8"
                } else {
                    "text/plain; charset=utf-8"
                };
                let mut head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close",
                    body.len()
                );
                if path == "/cookie-set" {
                    head.push_str("\r\nSet-Cookie: hermix_session=AbC123; Max-Age=86400; Path=/");
                }
                let resp = format!("{head}\r\n\r\n{body}");
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    (port, seen)
}

// ---------------------------------------------------------------------------
// 浏览器端套件（对齐生产注册路径）
// ---------------------------------------------------------------------------

struct BrowserHarness {
    navigate: BrowserNavigateHandler,
    snapshot: BrowserSnapshotHandler,
    ty: BrowserTypeHandler,
    click: BrowserClickHandler,
}

impl BrowserHarness {
    fn new() -> Self {
        // 走本机直连：本机 HTTP 代理若拦截 127.0.0.1，会伪装成
        // “CDP 解析失败” 的假象（排查中已经踩过一次）。
        std::env::set_var("CHROME_CDP_URL", format!("http://127.0.0.1:{CDP_PORT}"));
        std::env::set_var("no_proxy", "localhost,127.0.0.1");
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("HTTPS_PROXY");

        let registry = Arc::new(BrowserRegistry::new().with_cdp_port(Some(CDP_PORT)));
        let sessioned = Arc::new(SessionedBrowserBackend::new(registry));
        Self {
            // 逃生口只用于验收：fixture 服务必然跑在 loopback 上，而生产
            // handler 默认会按 SSRF 守卫拒掉 127.0.0.0/8（安全性另有
            // `t15_loopback_blocked_by_default_real` 钉死）。这里跳过的是
            // URL 前检，CDP 交互链路与生产完全一致。
            navigate: BrowserNavigateHandler::disabled(sessioned.clone())
                .with_cdp_bootstrap(CDP_PORT),
            snapshot: BrowserSnapshotHandler::new(sessioned.clone()),
            ty: BrowserTypeHandler::new(sessioned.clone()),
            click: BrowserClickHandler::new(sessioned),
        }
    }

    async fn navigate(&self, url: &str) -> String {
        self.navigate
            .execute(json!({ "url": url, "session": "acceptance" }))
            .await
            .unwrap_or_else(|e| panic!("browser_navigate failed: {e}"))
    }

    async fn snapshot(&self) -> String {
        self.snapshot
            .execute(json!({ "session": "acceptance" }))
            .await
            .unwrap_or_else(|e| panic!("browser_snapshot failed: {e}"))
    }

    async fn goto_wait(&self, url: &str) {
        let out = self.navigate(url).await;
        if out.contains("BROWSER UNAVAILABLE") {
            panic!("浏览器不可用: {out}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }

    async fn snap_tree(&self) -> String {
        let raw = self.snapshot().await;
        field(&raw, "tree").as_str().expect("tree").to_string()
    }
}

/// 语义失败的工具调用（比如 click_failed）返回 Ok(status=…_failed)，不是 Err。
fn assert_dispatched(raw: &str, what: &str) {
    let v: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
    if let Some(status) = v.get("status").and_then(|s| s.as_str()) {
        if status.ends_with("_failed") {
            panic!("{what} failed: {raw}");
        }
    }
}

fn sleep_ms(ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(ms)
}

/// 核心路径：`[ref=eN]` 点击。此前从未被端到端验证过，ref 直连改造后
/// 在这里补上；t11 覆盖的是 type 路径。
#[tokio::test(flavor = "multi_thread")]
async fn t16_click_via_ref_real() {
    if !real_env() {
        eprintln!("SKIP t16: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/form.html"))
        .await;

    let snap = h.snap_tree().await;
    let submit_ref = ref_for_label(&snap, "Create account")
        .unwrap_or_else(|| panic!("快照里找不到 Create account 按钮的句柄:\n{snap}"));
    println!("t16: 提交按钮句柄 = {submit_ref}");

    let out = h
        .click
        .execute(json!({ "selector": submit_ref, "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_click([ref]) failed: {e}"));
    assert_dispatched(&out, "browser_click via [ref]");

    let api = wait_for_request(&seen, "/api?", 5000)
        .await
        .unwrap_or_else(|| {
            let lines: Vec<String> = seen.lock().unwrap().iter().map(|c| c.request_line.clone()).collect();
            panic!("按 [ref] 点击没有触发提交；服务端收到: {lines:?}")
        });
    println!("t16 OK: [ref] 点击生效 -> {}", api.request_line);
}

/// 轮询等待某个请求出现（最多 `timeout_ms`）。表单提交里的 `fetch` 是异步的，
/// 固定 sleep 会随机漏掉 —— 轮询比赌时序可靠。
async fn wait_for_request(
    seen: &Arc<Mutex<Vec<Captured>>>,
    needle: &str,
    timeout_ms: u64,
) -> Option<Captured> {
    let deadline = tokio::time::Instant::now() + sleep_ms(timeout_ms);
    loop {
        let found = {
            let guard = seen.lock().unwrap();
            guard
                .iter()
                .find(|c| c.request_line.contains(needle))
                .cloned()
        };
        if found.is_some() {
            return found;
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(sleep_ms(100)).await;
    }
}

/// 从快照 YAML 里取某个可见文案对应的 `[ref=eN]` 句柄。
fn ref_for_label(snap: &str, label: &str) -> Option<String> {
    snap.lines()
        .find(|l| l.contains(label))
        .and_then(|l| {
            l.split_whitespace()
                .find(|t| t.starts_with("[ref=") && t.ends_with(']'))
                .map(|t| t.to_string())
        })
}

/// 诊断用：区分 cua_ui_find 失败是 FindAll 整体失效，还是条件构造失效。
#[tokio::test(flavor = "multi_thread")]
async fn t90_diag_uia_find_root_cause() {
    if !real_env() {
        eprintln!("SKIP t90: set HERMIX_REAL_ENV=1");
        return;
    }
    let report = tokio::task::spawn_blocking(|| {
        use hermix_cua::uia::{ControlType, UiaAutomation, UiaCondition};
        // SAFETY: COM 在本线程初始化；所有 UIA 调用都在这一个阻塞线程上完成。
        unsafe {
            let uia = UiaAutomation::new().expect("UiaAutomation::new");
            let true_cond = uia.find_all(&UiaCondition::Or(Vec::new())).unwrap_or_default();
            let probe = "TF_FloatingLangBar_WndTitle";
            let by_name = uia
                .find_all(&UiaCondition::Name(probe.to_string()))
                .unwrap_or_default();
            let tree = uia.get_control_view_tree(2).unwrap_or_default();
            let sample: Vec<String> = tree
                .iter()
                .take(4)
                .map(|e| format!("{:?}:{:?}", e.control_type, e.name))
                .collect();

            // 分层问诊：整数条件 vs 字符串条件，谁坏？
            let by_control_type = uia
                .find_all(&UiaCondition::ControlType(ControlType::Button))
                .unwrap_or_default();
            let by_class = uia
                .find_all(&UiaCondition::ClassName("Shell_TrayWnd".to_string()))
                .unwrap_or_default();

            // raw 树里到底有没有这个元素？有名字的话精确值是什么？
            let raw_names: Vec<String> = true_cond
                .iter()
                .filter(|e| e.name.contains("TF_") || e.class_name.contains("TF_"))
                .map(|e| format!("{:?}/{:?}", e.name, e.class_name))
                .collect();

            format!(
                "FindAll(True)={}  FindAll(Name)={}  FindAll(ControlType=Button)={}  \
                 FindAll(ClassName)={}  TreeWalker2={}  raw里TF_*={:?}  sample={:?}",
                true_cond.len(),
                by_name.len(),
                by_control_type.len(),
                by_class.len(),
                tree.len(),
                raw_names,
                sample
            )
        }
    })
    .await
    .expect("spawn_blocking");
    println!("t90 DIAG: {report}");
}

// ---------------------------------------------------------------------------
// Q2 — 窗口 / UI 枚举
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn t01_list_windows_real() {
    if !real_env() {
        eprintln!("SKIP t01: set HERMIX_REAL_ENV=1");
        return;
    }
    let raw = CuaListWindowsHandler::new()
        .execute(json!({ "limit": 50 }))
        .await
        .expect("cua_list_windows failed");
    let windows = field(&raw, "windows").as_array().cloned().expect("windows array");
    assert!(!windows.is_empty(), "没有枚举到任何顶层窗口: {raw}");

    let visible = windows
        .iter()
        .filter(|w| w["is_visible"] == json!(true))
        .count();
    assert!(visible > 0, "没有可见窗口: {raw}");

    for w in &windows {
        for key in ["hwnd", "title", "class_name", "process_id", "is_visible", "rect"] {
            assert!(w.get(key).is_some(), "窗口缺少 `{key}` 字段: {w}");
        }
        assert!(w["hwnd"].as_i64().unwrap_or(0) != 0, "hwnd 为 0: {w}");
        let rect = &w["rect"];
        assert!(
            rect.get("width").is_some() && rect.get("height").is_some(),
            "rect 不完整: {w}"
        );
    }
    println!("t01 OK: 枚举到 {} 个窗口（其中 {} 个可见）", windows.len(), visible);
}

#[tokio::test(flavor = "multi_thread")]
async fn t02_ui_tree_scoped_by_hwnd_real() {
    if !real_env() {
        eprintln!("SKIP t02: set HERMIX_REAL_ENV=1");
        return;
    }
    let raw = CuaListWindowsHandler::new()
        .execute(json!({ "limit": 50 }))
        .await
        .expect("cua_list_windows failed");
    let windows = field(&raw, "windows").as_array().cloned().expect("windows array");

    let target = windows
        .iter()
        .find(|w| {
            w["is_visible"] == json!(true)
                && w["title"]
                    .as_str()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
        })
        .cloned()
        .expect("没有带标题的可见窗口可用来做探针");
    let hwnd = target["hwnd"].as_i64().expect("hwnd");

    let scoped_raw = CuaUiTreeHandler::new()
        .execute(json!({ "hwnd": hwnd, "max_depth": 3 }))
        .await
        .unwrap_or_else(|e| panic!("cua_ui_tree(hwnd) failed: {e}"));
    let scoped = field(&scoped_raw, "elements")
        .as_array()
        .cloned()
        .expect("elements");
    assert!(!scoped.is_empty(), "按 hwnd 限定后 UIA 树为空: {scoped_raw}");
    for el in &scoped {
        for key in ["name", "control_type", "process_id", "rect", "is_enabled"] {
            assert!(el.get(key).is_some(), "UIA 元素缺少 `{key}`: {el}");
        }
    }

    let desktop_raw = CuaUiTreeHandler::new()
        .execute(json!({ "max_depth": 3 }))
        .await
        .unwrap_or_else(|e| panic!("cua_ui_tree(desktop) failed: {e}"));
    let desktop = field(&desktop_raw, "elements")
        .as_array()
        .cloned()
        .expect("elements");
    assert!(!desktop.is_empty(), "桌面根 UIA 树为空");

    let pids = |list: &[Value]| -> HashSet<i64> {
        list.iter().filter_map(|e| e["process_id"].as_i64()).collect()
    };
    let (scoped_pids, desktop_pids) = (pids(&scoped), pids(&desktop));
    assert!(
        scoped_pids.len() < desktop_pids.len(),
        "hwnd 没有生效：限定遍历={} 个进程，桌面遍历={} 个进程。\
         cua_ui_tree 的 hwnd 若未透传，两次都会从桌面根开始。",
        scoped_pids.len(),
        desktop_pids.len()
    );
    println!(
        "t02 OK: hwnd={} title={:?}；限定 {} 元素/{} 进程，桌面 {} 元素/{} 进程",
        hwnd,
        target["title"].as_str(),
        scoped.len(),
        scoped_pids.len(),
        desktop.len(),
        desktop_pids.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn t03_ui_find_real() {
    if !real_env() {
        eprintln!("SKIP t03: set HERMIX_REAL_ENV=1");
        return;
    }
    // 探针必须从「桌面根可达」的 UIA 树里取：某些 owned floating window
    // （输入法候选条等）Win32 枚举得到、ElementFromHandle 也到得了，却不在
    // 桌面根的 UIA 子树里，拿它们做探针会假报 find 失效。
    let tree_raw = CuaUiTreeHandler::new()
        .execute(json!({ "max_depth": 2 }))
        .await
        .expect("cua_ui_tree failed");
    let tree = field(&tree_raw, "elements")
        .as_array()
        .cloned()
        .expect("elements");
    let probe = tree
        .iter()
        .skip(1) // index 0 是桌面根自身，find 的 Descendants 不包含自己
        .filter_map(|e| e["name"].as_str())
        .find(|n| !n.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| panic!("桌面 UIA 树里没有可用来反查的名字: {tree_raw}"));

    let raw = CuaUiFindHandler::new()
        .execute(json!({ "name": probe, "max_results": 10 }))
        .await
        .unwrap_or_else(|e| panic!("cua_ui_find failed: {e}"));
    let count = field(&raw, "count").as_u64().expect("count") as usize;
    let elements = field(&raw, "elements").as_array().cloned().expect("elements");
    assert!(
        count > 0,
        "UIA 树里明明存在 `{probe}`，cua_ui_find 却查不到（count=0）—— find_all 有问题: {raw}"
    );
    assert_eq!(count, elements.len(), "count 与 elements 长度不一致: {raw}");
    for el in &elements {
        assert!(
            el.get("name").is_some() && el.get("control_type").is_some(),
            "UIA find 返回元素字段不齐: {el}"
        );
    }
    println!("t03 OK: 按 UIA 名字 `{probe}` 命中 {count} 个元素");
}

// ---------------------------------------------------------------------------
// Q3 — 浏览器填表
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn t10_snapshot_refs_real() {
    if !real_env() {
        eprintln!("SKIP t10: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, _seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/form.html"))
        .await;

    let snap = h.snap_tree().await;
    assert!(snap.contains("[ref="), "快照里没有任何 [ref=] 句柄:\n{snap}");
    for label in ["Full name", "Email", "Bio", "Plan", "Create account"] {
        assert!(snap.contains(label), "快照缺少 `{label}`:\n{snap}");
    }
    println!("t10 OK: 快照句柄数 = {}", snap.matches("[ref=").count());
}

#[tokio::test(flavor = "multi_thread")]
async fn t11_type_via_ref_and_css_real() {
    if !real_env() {
        eprintln!("SKIP t11: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/form.html"))
        .await;

    let snap = h.snap_tree().await;
    let name_ref = snap
        .split_whitespace()
        .find(|t| t.starts_with("[ref=") && t.ends_with(']'))
        .expect("快照里没有 ref 句柄")
        .to_string();

    // 1) [ref=eN] 路径（本轮修复的核心）
    let typed = h
        .ty
        .execute(json!({ "selector": name_ref, "text": "王小明", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_type([ref]) failed: {e}"));
    assert_dispatched(&typed, "browser_type via [ref]");

    // 2) CSS 路径（回归：ref 直连改造不能破掉原有选择器路径）
    let typed_css = h
        .ty
        .execute(json!({ "selector": "#email", "text": "xm@example.com", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_type(css) failed: {e}"));
    assert_dispatched(&typed_css, "browser_type via CSS");

    // 3) textarea
    let typed_bio = h
        .ty
        .execute(json!({ "selector": "#bio", "text": "hello world", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_type(textarea) failed: {e}"));
    assert_dispatched(&typed_bio, "browser_type into textarea");

    let clicked = h
        .click
        .execute(json!({ "selector": "#go", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_click(#go) failed: {e}"));
    assert_dispatched(&clicked, "browser_click #go");
    tokio::time::sleep(sleep_ms(900)).await;

    let api = wait_for_request(&seen, "/api?", 5000)
        .await
        .unwrap_or_else(|| {
            let lines: Vec<String> = seen.lock().unwrap().iter().map(|c| c.request_line.clone()).collect();
            panic!("表单没有提交到服务端；服务端只收到: {lines:?}")
        });
    for expect in ["name=%E7%8E%8B%E5%B0%8F%E6%98%8E", "email=xm@example.com", "bio=hello%20world"] {
        assert!(
            api.request_line.contains(expect),
            "提交缺少 `{expect}`:\n{}",
            api.request_line
        );
    }
    println!("t11 OK: ref + CSS 双路径写入都进了表单 -> {}", api.request_line);
}

#[tokio::test(flavor = "multi_thread")]
async fn t12_select_and_checkbox_real() {
    if !real_env() {
        eprintln!("SKIP t12: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/form.html"))
        .await;

    let select_raw = h
        .ty
        .execute(json!({ "selector": "#plan", "text": "pro", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_type(select) failed: {e}"));
    println!("t12: select -> {select_raw}");

    let checked = h
        .click
        .execute(json!({ "selector": "#terms", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_click(checkbox) failed: {e}"));
    assert_dispatched(&checked, "browser_click #terms");
    println!("t12: checkbox -> {checked}");

    let clicked = h
        .click
        .execute(json!({ "selector": "#go", "session": "acceptance" }))
        .await
        .unwrap_or_else(|e| panic!("browser_click(#go) failed: {e}"));
    assert_dispatched(&clicked, "browser_click #go");
    println!("t12: submit -> {clicked}");

    // 以页面侧 FormData 证据为准：页面的 `fetch` 在这个玩具夹具里偶发挂住，
    // 拿它当唯一判据会把验收变成抛硬币。服务端记录照旧打印，便于日后排查。
    let after = h.snap_tree().await;
    let lines: Vec<String> = seen.lock().unwrap().iter().map(|c| c.request_line.clone()).collect();
    println!("t12: 服务端收到的全部请求 = {lines:?}");
    if let Some(api) = wait_for_request(&seen, "/api?", 3000).await {
        println!("t12: 服务端也收到了提交 -> {}", api.request_line);
        for expect in ["plan=pro", "terms=yes"] {
            assert!(api.request_line.contains(expect), "提交缺 `{expect}`: {}", api.request_line);
        }
    } else {
        println!("t12: 服务端未收到异步 fetch（见已知说明），改校验页面侧 FormData");
    }
    assert!(
        after.contains("plan=pro") && after.contains("terms=yes"),
        "下拉/勾选没进 FormData：\n{after}"
    );
    println!("t12 OK: select + checkbox 都进了表单数据");
}

#[tokio::test(flavor = "multi_thread")]
async fn t13_result_visible_in_snapshot_real() {
    if !real_env() {
        eprintln!("SKIP t13: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, _seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/form.html"))
        .await;
    h.ty
        .execute(json!({ "selector": "#name", "text": "验收value", "session": "acceptance" }))
        .await
        .expect("type");
    h.click
        .execute(json!({ "selector": "#go", "session": "acceptance" }))
        .await
        .expect("click");
    tokio::time::sleep(sleep_ms(900)).await;

    let snap = h.snap_tree().await;
    assert!(
        snap.contains("RESULT_OK") && snap.contains("name=验收value"),
        "提交结果在快照里不可见 —— 模型看不到自己刚填的结果：\n{snap}"
    );
    println!("t13 OK: 提交结果可由快照读回");
}

#[tokio::test(flavor = "multi_thread")]
async fn t14_stale_ref_is_rejected_real() {
    if !real_env() {
        eprintln!("SKIP t14: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, _seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/form.html"))
        .await;
    let snap = h.snap_tree().await;
    let first = snap
        .split_whitespace()
        .find(|t| t.starts_with("[ref=") && t.ends_with(']'))
        .expect("ref token")
        .to_string();

    // 导航走之后旧句柄必须被拒绝，而不是撞到同名元素上。
    h.goto_wait(&format!("http://127.0.0.1:{port}/plain")).await;
    // 过期 ref 现在返回 Err（而非静默通过），两者都属于“拒绝”，这里一视同仁。
    let out = match h
        .click
        .execute(json!({ "selector": first, "session": "acceptance" }))
        .await
    {
        Ok(s) => s,
        Err(e) => e.to_string(),
    };
    assert!(
        out.contains("不存在") || out.contains("重新") || out.contains("引用句柄"),
        "过期 ref 没有被拒绝: {out}"
    );
    let preview: String = out.chars().take(120).collect();
    println!("t14 OK: 过期句柄被拒绝 -> {preview}");
}

/// 安全钉子：生产 handler 必须继续拒绝 loopback / 云 metadata 地址。
/// 这条挂了就说明有人为了跑测试把安全边界给削掉了。
#[tokio::test(flavor = "multi_thread")]
async fn t15_loopback_blocked_by_default_real() {
    if !real_env() {
        eprintln!("SKIP t15: set HERMIX_REAL_ENV=1");
        return;
    }
    let registry = Arc::new(BrowserRegistry::new().with_cdp_port(Some(CDP_PORT)));
    let sessioned = Arc::new(SessionedBrowserBackend::new(registry));
    let prod = BrowserNavigateHandler::new(sessioned).with_cdp_bootstrap(CDP_PORT);

    for blocked in [
        "http://127.0.0.1:9334/x",
        "http://localhost:9334/x",
        "http://169.254.169.254/latest/meta-data/",
        "http://192.168.0.1/admin",
    ] {
        let err = prod
            .execute(json!({ "url": blocked, "session": "acceptance" }))
            .await;
        assert!(
            err.is_err(),
            "生产 handler 竟然放行了 SSRF 目标 `{blocked}` —— 安全边界被削弱"
        );
        println!("t15: 已拦截 {blocked} -> {}", err.unwrap_err());
    }
    println!("t15 OK: loopback / link-local / 私网段默认全部被拦");
}

// ---------------------------------------------------------------------------
// BUG-13 — 双会话 tab 隔离（CDP target 层）
// ---------------------------------------------------------------------------

/// 两个会话先后导航到不同页面，各自快照必须只看到自己的内容。
/// 串台根因有两层，本用例两层都会抓：
/// 1. handler 路由层共享槽（已修：backend_for_call per-session 绑定）；
/// 2. CDP target 层——每个实例都连"最近导航的 tab"，B 后导航，
///    A 的快照就会落到 B 的页面上（本用例主断言）。
#[tokio::test(flavor = "multi_thread")]
async fn t17_dual_session_tab_isolation_real() {
    if !real_env() {
        eprintln!("SKIP t17: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, _seen) = spawn_fixture_server().await;
    // 与 BrowserHarness::new 同款环境固定：后端端点必须指向本验收专用
    // 的调试浏览器（9333），否则 from_env 落到默认 9222，撞上无关实例。
    std::env::set_var("CHROME_CDP_URL", format!("http://127.0.0.1:{CDP_PORT}"));
    std::env::set_var("no_proxy", "localhost,127.0.0.1");
    std::env::remove_var("HTTP_PROXY");
    std::env::remove_var("HTTPS_PROXY");
    let registry = Arc::new(BrowserRegistry::new().with_cdp_port(Some(CDP_PORT)));
    registry.ensure_browser_bootstrap_async().await;
    let sessioned = Arc::new(SessionedBrowserBackend::new(registry));
    let nav = BrowserNavigateHandler::disabled(sessioned.clone()).with_cdp_bootstrap(CDP_PORT);
    let snap = BrowserSnapshotHandler::new(sessioned.clone());

    let url_a = format!("http://127.0.0.1:{port}/alpha");
    let url_b = format!("http://127.0.0.1:{port}/beta");
    // B 后导航：若两层隔离缺任何一层，A 的后续快照都会落在 B 的页面。
    let ra = nav
        .execute(json!({ "url": url_a, "session": "bug13-a" }))
        .await
        .expect("会话 A navigate 失败");
    assert_dispatched(&ra, "A navigate");
    let rb = nav
        .execute(json!({ "url": url_b, "session": "bug13-b" }))
        .await
        .expect("会话 B navigate 失败");
    assert_dispatched(&rb, "B navigate");

    let sa = snap
        .execute(json!({ "session": "bug13-a" }))
        .await
        .expect("会话 A snapshot 失败");
    let sb = snap
        .execute(json!({ "session": "bug13-b" }))
        .await
        .expect("会话 B snapshot 失败");

    assert!(
        sa.contains("PAGE_ALPHA"),
        "会话 A 快照看不到自己的 alpha 页（隔离失效）:\n{sa}"
    );
    assert!(
        !sa.contains("PAGE_BETA"),
        "会话 A 快照串台到了 B 的 beta 页:\n{sa}"
    );
    assert!(
        sb.contains("PAGE_BETA"),
        "会话 B 快照看不到自己的 beta 页（隔离失效）:\n{sb}"
    );
    assert!(
        !sb.contains("PAGE_ALPHA"),
        "会话 B 快照串台到了 A 的 alpha 页:\n{sb}"
    );
    println!("t17 OK: 两个会话各看到各的页面，CDP target 层隔离成立");
}

// ---------------------------------------------------------------------------
// Q6 — profile 归属 / 登录态能否延续
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn t20_cookie_persists_across_browser_restart_real() {
    if !real_env() {
        eprintln!("SKIP t20: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, seen) = spawn_fixture_server().await;
    {
        let h = BrowserHarness::new();
        h.goto_wait(&format!("http://127.0.0.1:{port}/cookie-set"))
            .await;
    }
    let set_count = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.request_line.contains("/cookie-set"))
        .count();
    assert!(set_count > 0, "cookie-set 没有被请求");

    // 先在同一个浏览器会话里回读一次：区分「Set-Cookie 没被收下」和
    // 「收下了但没落盘」。
    {
        let h = BrowserHarness::new();
        h.goto_wait(&format!("http://127.0.0.1:{port}/cookie-read"))
            .await;
    }
    let same_session = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.request_line.contains("/cookie-read"))
        .any(|c| c.raw.to_ascii_lowercase().contains("cookie: hermix_session=abc123"));
    println!("t20: 同会话回读是否带上 cookie = {same_session}");

    // 给 Edge 一点时间把 cookie 落到 profile 里，再优雅关掉进程。
    tokio::time::sleep(sleep_ms(2500)).await;
    kill_debug_browser().await;

    {
        let h = BrowserHarness::new();
        h.goto_wait(&format!("http://127.0.0.1:{port}/cookie-read"))
            .await;
    }

    let captured = seen.lock().unwrap().clone();
    let read = captured
        .iter()
        .filter(|c| c.request_line.contains("/cookie-read"))
        .collect::<Vec<_>>();
    assert!(!read.is_empty(), "cookie-read 没有被请求");
    let carried = read
        .iter()
        .any(|c| c.raw.to_ascii_lowercase().contains("cookie: hermix_session=abc123"));
    assert!(
        carried,
        "浏览器重启后没有把 cookie 带回服务端 —— 登录态无法跨会话延续。原始请求:\n{}",
        read.last().map(|c| c.raw.clone()).unwrap_or_default()
    );
    println!("t20 OK: 浏览器进程被杀掉重启后，持久 cookie 仍然回传");
}

#[tokio::test(flavor = "multi_thread")]
async fn t21_headless_launch_command_line_real() {
    if !real_env() {
        eprintln!("SKIP t21: set HERMIX_REAL_ENV=1");
        return;
    }
    let (port, _seen) = spawn_fixture_server().await;
    let h = BrowserHarness::new();
    h.goto_wait(&format!("http://127.0.0.1:{port}/plain")).await;

    let lines = debug_browser_command_lines();
    assert!(
        !lines.is_empty(),
        "bootstrap 声称拉起了浏览器，但系统里找不到带 --remote-debugging-port={CDP_PORT} 的进程"
    );

    // Q6 的关键事实：浏览器必须跑在 Hermix 自己的隔离 profile 上。
    // 一旦 bootstrap 退化成用用户日常 Edge 的 profile，登录态是会“继承”，
    // 但那等于把用户所有账号的 cookie 暴露给自动化（以及反过来污染用户），
    // 所以这里钉死：必须显式指定 user-data-dir，且落在 .hermix 下面。
    let main_line = lines
        .iter()
        .find(|l| l.contains("--type=") == false && l.to_lowercase().contains("msedge.exe"))
        .cloned()
        .unwrap_or_else(|| panic!("没有找到浏览器主进程命令行: {lines:#?}"));
    assert!(
        main_line.contains("--user-data-dir="),
        "浏览器没有显式指定 profile —— 它会用用户日常浏览器的 profile: {main_line}"
    );
    let dir = main_line
        .split("--user-data-dir=")
        .nth(1)
        .unwrap_or("")
        .trim_matches('"')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(
        dir.to_lowercase().contains("\\.hermix\\"),
        "profile 目录不在 Hermix 自己的命名空间下: {dir}"
    );
    println!("t21 OK: 浏览器使用 Hermix 隔离 profile = {dir}");
}

/// 读取调试端口对应浏览器进程的命令行，用来判定它到底用的哪个 profile。
fn debug_browser_command_lines() -> Vec<String> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{port}*' }} \
                 | Select-Object -ExpandProperty CommandLine",
                port = format!("--remote-debugging-port={CDP_PORT}")
            ),
        ])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 收掉调试端口上的浏览器进程。**优雅退出**：`-Force` 强杀会让 Edge 来不及
/// 把 cookie 写盘，那不是产品的行为，是测试手法的副作用。
async fn kill_debug_browser() {
    let marker = format!("--remote-debugging-port={CDP_PORT}");
    let _ = tokio::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{marker}*' }} \
                 | ForEach-Object {{ Stop-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }}"
            ),
        ])
        .output()
        .await;
    // 等进程真正退干净，避免下一次 bootstrap 以为浏览器还在。
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let alive = debug_browser_command_lines();
        if alive.is_empty() {
            return;
        }
    }
}
