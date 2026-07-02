//! Debug helper: launch Chrome and print diagnostics.
use std::process::Command;

fn main() {
    let chrome_path = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
    println!("Chrome exists: {}", std::path::Path::new(chrome_path).is_file());

    let port = 19222u16;
    let args = vec![
        format!("--user-data-dir={}\\AppData\\Local\\Temp\\hermix-e2e-debug", std::env::var("USERPROFILE").unwrap()),
        format!("--remote-debugging-port={port}"),
        "--remote-debugging-address=127.0.0.1".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-extensions".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-component-update".to_string(),
        "--disable-features=Translate,InfiniteSessionRestore".to_string(),
        "about:blank".to_string(),
    ];

    println!("Launching Chrome on port {port}...");
    let mut child = match Command::new(chrome_path).args(&args).spawn() {
        Ok(c) => {
            println!("Chrome spawned, pid={}", c.id());
            c
        }
        Err(e) => {
            println!("FAILED to spawn: {e}");
            return;
        }
    };

    // Poll the CDP endpoint
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            std::time::Duration::from_secs(2),
        ) {
            Ok(mut stream) => {
                use std::io::{Read, Write};
                let req = format!("GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(req.as_bytes());
                let mut resp = String::new();
                let _ = stream.read_to_string(&mut resp);
                println!("Attempt {attempt}: GOT RESPONSE ({len} bytes)", len=resp.len());
                println!("First 200 chars: {}", &resp[..resp.len().min(200)]);
                break;
            }
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    println!("Attempt {attempt}: TIMEOUT - {e}");
                    break;
                }
                println!("Attempt {attempt}: {e}");
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }

    // Check if Chrome is still alive
    match child.try_wait() {
        Ok(Some(status)) => println!("Chrome exited: {status}"),
        Ok(None) => println!("Chrome still running"),
        Err(e) => println!("Error checking Chrome: {e}"),
    }

    let _ = child.kill();
    let _ = child.wait();
    println!("Done");
}
