//! 老版 .doc 抽取的功能验证：用 cfb 造一个合成 OLE 容器（WordDocument 流
//! 内含最小 FIB + UTF-16LE 正文），再走 extract_doc_text 解析回读。
//! 用法：`cargo run -p hermix-viewer --example doc_extract`//! 用法：`cargo run -p hermix-viewer --example doc_extract`//! 用法：`cargo run -p hermix-viewer --example doc_extract`

use std::io::Write;

fn main() {
    // --- 1. 合成 .doc：512 字节 FIB 头 + 正文（UTF-16LE）---
    let body = "查看器老版 .doc 预览验证：第一段。\r第二段：稳定性测试 2026。\r";
    let mut word = vec![0u8; 1536];
    // FIB 标志位（0x0A）：fExtChar(0x1000) 置位，fComplex(0x0004) 清零。
    word[0x0A..0x0C].copy_from_slice(&0x1000u16.to_le_bytes());
    // fcMin / fcMac（0x18 / 0x1C）：正文紧跟 FIB 占位区之后。
    let fc_min = 1536usize;
    let mut text_bytes = Vec::new();
    for u in body.encode_utf16() {
        text_bytes.extend_from_slice(&u.to_le_bytes());
    }
    let fc_mac = fc_min + text_bytes.len();
    word[0x18..0x1C].copy_from_slice(&(fc_min as u32).to_le_bytes());
    word[0x1C..0x20].copy_from_slice(&(fc_mac as u32).to_le_bytes());
    word.extend_from_slice(&text_bytes);

    let out_path = std::env::temp_dir().join("hermix-legacy-doc-test.doc");
    let mut cfb = cfb::create(&out_path).expect("create OLE container");
    // OLE 流内容必须是扇区对齐的完整写入；WordDocument 流直接塞整个缓冲。
    cfb.create_new_stream("WordDocument")
        .expect("new stream")
        .write_all(&word)
        .expect("write stream");
    drop(cfb);
    println!("synthetic .doc at {}", out_path.display());

    // --- 2. 走正式抽取路径解析回读 ---
    match hermix_viewer::legacy_doc::extract_doc_text(&out_path) {
        Ok(text) => {
            println!("--- extracted ---");
            println!("{}", text);
            let ok = text.contains("查看器老版") && text.contains("第一段") && text.contains("稳定性测试 2026");
            println!("--- verify: {} (段落={} 字符={}) ---", if ok { "PASS" } else { "MISMATCH" }, text.split(|c| c == '\n' || c == '\r').filter(|s| !s.trim().is_empty()).count(), text.chars().count());
            std::process::exit(if ok { 0 } else { 1 });
        }
        Err(e) => {
            println!("extract failed: {e}");
            std::process::exit(1);
        }
    }
}
