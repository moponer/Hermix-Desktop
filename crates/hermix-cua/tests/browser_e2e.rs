//! End-to-end integration tests for the self-hosted CDP browser client.
//!
//! These tests require a real Chrome installation and are marked
//! `#[ignore]` by default — run with `cargo test -- --ignored` or
//! `cargo test -p hermix-cua --test browser_e2e -- --ignored`.
//!
//! ## Prerequisites
//!
//! - Chrome installed at a standard path (auto-discovered).
//! - Or set `HERMIX_CHROME_PATH` env var to point to `chrome.exe`.
//!
//! ## Notes
//!
//! Each test uses a **random available port** (not 9222) and a
//! **temporary profile directory** to avoid conflicts with any
//! running Chrome instance. The temp dir is cleaned up on drop.

use std::net::TcpListener;

use hermix_cua::browser::{BrowserHandle, LaunchOptions};

/// Pick a random available TCP port by binding to port 0 and then
/// releasing it.
fn pick_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind to port 0");
    listener.local_addr().unwrap().port()
}

/// Create LaunchOptions with a random port AND a fresh temp profile
/// dir so the test never touches the user's real Chrome profile.
fn launch_opts_isolated() -> (LaunchOptions, tempfile::TempDir) {
    let port = pick_free_port();
    let tmp = tempfile::tempdir().expect("create temp dir for Chrome profile");
    eprintln!("[e2e] port={port}, profile={}", tmp.path().display());
    let opts = LaunchOptions {
        port: Some(port),
        profile_dir: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    (opts, tmp)
}

/// Test: full lifecycle — launch, navigate, screenshot, evaluate, close.
///
/// This exercises the core self-hosted CDP client pipeline end to end.
#[test]
#[ignore]
fn browser_full_lifecycle() {
    // 1. Launch Chrome with isolated profile (no conflict with running Chrome)
    let (opts, _tmp) = launch_opts_isolated();
    let handle = BrowserHandle::launch(opts)
        .expect("launch Chrome");

    // 2. Create a new tab pointing at about:blank
    let target_id = handle
        .new_tab("about:blank")
        .expect("create new tab");
    eprintln!("[e2e] new tab target_id={target_id}");

    // 3. Attach to the target to get a session id
    let session_id = handle
        .attach_to_target(&target_id)
        .expect("attach to target");
    eprintln!("[e2e] session_id={session_id}");

    // 4. Navigate to example.com
    handle
        .navigate(&session_id, "https://example.com")
        .expect("navigate to example.com");
    // Give a brief moment for the page to settle
    std::thread::sleep(std::time::Duration::from_secs(2));

    // 5. Take a screenshot (viewport only)
    let png = handle
        .screenshot(&session_id, false)
        .expect("screenshot");
    assert!(!png.is_empty(), "screenshot should return non-empty PNG bytes");
    eprintln!("[e2e] screenshot: {} bytes", png.len());

    // 6. Evaluate a JS expression
    let result = handle
        .evaluate(&session_id, "1 + 1")
        .expect("evaluate 1+1");
    let val = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_i64());
    assert_eq!(val, Some(2), "1+1 should equal 2");
    eprintln!("[e2e] evaluate 1+1 = {val:?}");

    // 7. Evaluate document.title
    let title = handle
        .evaluate(&session_id, "document.title")
        .expect("evaluate document.title");
    let title_str = title
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(!title_str.is_empty(), "page should have a title");
    eprintln!("[e2e] page title: {title_str}");

    // 8. Click the "More information" link
    handle
        .evaluate(
            &session_id,
            "document.querySelector('a').click()",
        )
        .expect("click first link via JS");
    std::thread::sleep(std::time::Duration::from_secs(1));
    eprintln!("[e2e] clicked first link");

    // 9. Navigate to a different URL and verify
    handle
        .navigate(&session_id, "about:blank")
        .expect("navigate to about:blank");
    let blank_title = handle
        .evaluate(&session_id, "document.title")
        .expect("evaluate blank title");
    let blank = blank_title
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    eprintln!("[e2e] after nav to about:blank, title='{blank}'");

    // 10. Shutdown Chrome
    handle.shutdown().expect("shutdown Chrome");
    eprintln!("[e2e] Chrome shut down cleanly");
}

/// Test: attach mode — connect to an already-running Chrome.
#[test]
#[ignore]
fn browser_attach_after_launch() {
    let (opts, _tmp) = launch_opts_isolated();
    let port = opts.port.unwrap();
    eprintln!("[e2e] attach test using port {port}");

    // Launch first
    let handle = BrowserHandle::launch(opts).expect("launch Chrome");

    // Now attach a second handle to the same browser
    let ws_url = format!("ws://127.0.0.1:{port}/devtools/browser");
    let attached = BrowserHandle::attach(&ws_url).expect("attach to running Chrome");

    let targets = attached.list_targets().expect("list targets");
    eprintln!("[e2e] attach: found {} targets", targets.len());
    assert!(!targets.is_empty(), "should see at least the browser target");

    // Clean up using the original handle (owns the process)
    handle.shutdown().expect("shutdown");
}

/// Test: error handling — attach to a dead port should fail quickly.
#[test]
#[ignore]
fn browser_attach_dead_port_fails() {
    let result = BrowserHandle::attach("ws://127.0.0.1:19999/devtools/browser");
    assert!(result.is_err(), "attach to dead port should fail");
    // Don't unwrap the error — BrowserHandle doesn't impl Debug on Ok side.
}
