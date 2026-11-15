//! 真机探针：portable-pty 0.9 + Windows ConPTY + PowerShell 的行为契约。
//!
//! 钉死 `pty_session.rs` / `pty_filter.rs` 实现所依赖的四条事实，改动
//! 这些模块前先重跑本测试。探索期的二分案例（cmd vs PowerShell、
//! PSReadLine 噪音量化）已删除，只保留有断言的终版：
//!
//! 1. 会话形态 `-NoLogo -NoProfile -NonInteractive -NoExit -Command
//!    "function prompt { '<marker>> ' }"` 下，marker 提示符在无输入时
//!    **自渲染**（= `create_session` 的就绪信号），且每条命令结束后
//!    **重现**（= 逐命令就绪信号）。
//! 2. ConPTY 输出流恒为 UTF-8（CJK 完好）——过滤层据此只处理 UTF-8
//!    跨块续接。
//! 3. DSR 光标位置请求（`ESC[6n`）必须由输入管道回复 `ESC[1;1R`，
//!    否则 conhost 永不渲染（cmd 与 PowerShell 均静默卡死）。生产实现
//!    在 `pty_session::spawn_reader` 里自动应答，本探针同样内建。
//! 4. 关闭顺序 kill → 等退出 → 排干 → drop master；直接 drop 活着的
//!    pseudoconsole 会挂死（ClosePseudoConsole 阻塞）。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

#[test]
fn probe_session_shell_contract() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 30,
            cols: 250,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty failed — ConPTY unavailable (needs Windows 10 1809+)");
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-NoExit",
        "-Command",
        "function prompt { 'HERMIXPTY-PROBE> ' }",
    ]);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn powershell");
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("take writer")));

    // 读线程：收集字节 + 自动应答 DSR（生产实现的同款握手）。
    let collected: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collected.clone();
    let reply_sink = writer.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut w) = reply_sink.lock() {
                        if buf[..n].windows(4).any(|w4| w4 == b"\x1b[6n") {
                            let _ = w.write_all(b"\x1b[1;1R");
                            let _ = w.flush();
                        }
                    }
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        }
    });

    // 事实 1：marker 自渲染（冷启在 AV 重载机器上可能数秒）。
    let mut marker_at = None;
    for i in 0..10 {
        std::thread::sleep(Duration::from_millis(2000));
        let text = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        if text.contains("HERMIXPTY-PROBE") {
            marker_at = Some(i * 2);
            break;
        }
    }
    assert!(
        marker_at.is_some(),
        "marker prompt never self-rendered within 20s — the session \
         readiness signal this design depends on is gone"
    );

    // 事实 2 + 1（重现）：发一条含 CJK 的命令。
    writer
        .lock()
        .unwrap()
        .write_all("Write-Output \"中文输出测试\"\r\n".as_bytes())
        .expect("write cjk command");
    std::thread::sleep(Duration::from_millis(4000));

    let raw = collected.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&raw).to_string();
    let valid_utf8 = std::str::from_utf8(&raw).is_ok();
    eprintln!("=== valid utf8: {valid_utf8} ===\n=== raw ===\n{text}");

    assert!(
        valid_utf8 && text.contains("中文输出测试"),
        "ConPTY output must be UTF-8 with CJK intact"
    );
    assert!(
        text.matches("HERMIXPTY-PROBE").count() >= 2,
        "marker must re-render after each command (per-command readiness)"
    );

    // 事实 4：kill → 等退出 → 排干 → drop master。
    let mut killer = child.clone_killer();
    let _ = killer.kill();
    for _ in 0..40 {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(300));
    drop(pair);
}
