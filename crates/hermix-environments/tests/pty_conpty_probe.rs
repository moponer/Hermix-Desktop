//! 真机探针：portable-pty 0.9 + Windows ConPTY + PowerShell 的实际行为。
//!
//! 钉死四件事，为 pty_session.rs 的实现提供事实依据：
//! 1. ConPTY 输出流的文本编码（UTF-8 直出还是 OEM/GBK）
//! 2. PowerShell 在 ConPTY 下会合成哪些 OSC/CSI 噪音（标题、光标、SGR）
//! 3. `-NoExit -Command "function prompt {...}"` 能否作为 prompt-marker 就绪信号
//! 4. 输入回显（echo）与 `\r\n` 行尾的实际形态
//!
//! 维持为常规测试（Windows-only 仓库）：断言最基本的事实，
//! eprintln 保留原始字节供人读。

use std::io::{Read, Write};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn spawn_session() -> (
    portable_pty::PtyPair,
    Box<dyn portable_pty::Child + Send + Sync>,
    Box<dyn Read + Send>,
    Box<dyn Write + Send>,
) {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 30,
            cols: 250,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty failed — ConPTY unavailable?");
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args(["-NoLogo", "-NoProfile", "-NoExit", "-Command"]);
    cmd.arg("function prompt { 'HERMIXPTY-PROBE> ' }");
    let child = pair.slave.spawn_command(cmd).expect("spawn powershell failed");
    let reader = pair.master.try_clone_reader().expect("clone reader failed");
    let mut writer = pair.master.take_writer().expect("take writer failed");
    (pair, child, reader, writer)
}

#[test]
fn probe_conpty_powershell_behavior() {
    let (_pair, mut child, mut reader, mut writer) = spawn_session();

    // Reader thread: collect everything until killed.
    let handle = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => all.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        all
    });

    // Give PowerShell a moment to boot, then run two commands:
    // one ASCII, one with Chinese to pin down the encoding.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    writer
        .write_all(b"$x = 21 + 21; Write-Output \"answer=$x\"\r\n")
        .expect("write ascii cmd failed");
    std::thread::sleep(std::time::Duration::from_millis(800));
    writer
        .write_all("Write-Output \"中文输出测试\"\r\n".as_bytes())
        .expect("write cjk cmd failed");
    std::thread::sleep(std::time::Duration::from_millis(2000));

    let mut killer = child.clone_killer();
    let _ = killer.kill();

    let raw = handle.join().expect("reader thread panicked");
    let lossy = String::from_utf8_lossy(&raw);
    let valid_utf8 = std::str::from_utf8(&raw).is_ok();

    eprintln!("=== total bytes: {} (valid utf8: {}) ===", raw.len(), valid_utf8);
    eprintln!("=== lossy text ===\n{}", lossy);
    eprintln!("=== escape bytes (non-printable, hex) ===");
    let escapes: Vec<String> = raw
        .iter()
        .filter(|b| **b < 0x20 || **b == 0x1b)
        .map(|b| format!("{:02x}", b))
        .collect();
    eprintln!("{}", escapes.join(" "));

    // Assertions on facts we rely on in pty_session.rs:
    assert!(
        lossy.contains("answer=42"),
        "ASCII command output missing from ConPTY stream"
    );
    if valid_utf8 {
        assert!(
            lossy.contains("中文输出测试"),
            "UTF-8 CJK output missing — ConPTY re-encoded it?"
        );
    } else {
        // ConPTY emitted non-UTF-8 (OEM/GBK) — the session layer must
        // route ground-run bytes through decode_console_output.
        eprintln!("NOTE: ConPTY output is NOT valid UTF-8 — GBK fallback required");
    }
    assert!(
        lossy.contains("HERMIXPTY-PROBE"),
        "prompt marker never rendered — -NoExit -Command prompt override failed"
    );
}
