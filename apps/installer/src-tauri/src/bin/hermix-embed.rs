//! hermix-embed — 安装器载荷嵌入 CLI（构建链工具，console 子系统）。
//!
//! 把 app 根 zip 以 RT_RCDATA 1003 写进安装器 exe 资源节，并内建读回验证
//! （embed 后立即从磁盘文件读回比对字节 + zip 中央目录校验）。
//!
//! 用法：
//!   hermix-embed embed  <installer.exe> <payload.zip>   嵌入 + 验证
//!   hermix-embed verify <installer.exe>                 只验证（zip 可完整解析）
//!
//! 退出码：0 成功；1 失败（stderr 带原因）。release.sh 的 B 路调用。

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = run(&args);
    std::process::exit(code);
}

fn run(args: &[String]) -> i32 {
    let Some(cmd) = args.get(1) else {
        eprintln!("用法: hermix-embed <embed|verify> <installer.exe> [payload.zip]");
        return 1;
    };
    match cmd.as_str() {
        "embed" => {
            let (Some(exe), Some(zip)) = (args.get(2), args.get(3)) else {
                eprintln!("embed 需要: <installer.exe> <payload.zip>");
                return 1;
            };
            cmd_embed(exe, zip)
        }
        "verify" => {
            let Some(exe) = args.get(2) else {
                eprintln!("verify 需要: <installer.exe>");
                return 1;
            };
            cmd_verify(exe)
        }
        other => {
            eprintln!("未知子命令: {other}");
            1
        }
    }
}

fn cmd_embed(exe: &str, zip: &str) -> i32 {
    let payload = match std::fs::read(zip) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("读 payload 失败（{zip}）: {e}");
            return 1;
        }
    };
    // 前置校验：输入必须是合法 zip（中央目录可解析），防把坏包嵌进 exe。
    if let Err(e) = zip::ZipArchive::new(std::io::Cursor::new(&payload)) {
        eprintln!("payload 不是合法 zip: {e}");
        return 1;
    }
    if let Err(e) = hermix_installer::payload::embed_payload_into(std::path::Path::new(exe), &payload) {
        eprintln!("嵌入失败: {e}");
        return 1;
    }
    // 内建读回验证：嵌入后从磁盘读回，必须与输入逐字节一致。
    match hermix_installer::payload::read_payload_from_file(std::path::Path::new(exe)) {
        Ok(back) if back == payload => {
            println!("embed OK: {} bytes -> {exe}（读回一致，zip 校验通过）", payload.len());
            0
        }
        Ok(back) => {
            eprintln!("读回不一致: 写入 {} bytes，读回 {} bytes", payload.len(), back.len());
            1
        }
        Err(e) => {
            eprintln!("读回验证失败: {e}");
            1
        }
    }
}

fn cmd_verify(exe: &str) -> i32 {
    match hermix_installer::payload::read_payload_from_file(std::path::Path::new(exe)) {
        Ok(bytes) => match zip::ZipArchive::new(std::io::Cursor::new(&bytes)) {
            Ok(archive) => {
                println!("verify OK: {exe} 载荷 {} bytes，{} 个条目，zip 结构完整", bytes.len(), archive.len());
                0
            }
            Err(e) => {
                eprintln!("载荷 zip 校验失败: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("读回失败: {e}");
            1
        }
    }
}
