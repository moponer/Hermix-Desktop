// inject_app.rs — 打包工具: 把 loader.exe + 应用 html + manifest 合成单文件 app.exe
//
// 用法: inject_app <loader.exe> <index.html> <manifest.json> <output.exe>
// 产物 = loader.exe 字节 + [manifest_len|manifest|html_len|html] + MAGIC + payload_len

use std::env;

use hermix_app_loader::payload::{parse_trailer, MAGIC};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 5 {
        eprintln!("用法: inject_app <loader.exe> <index.html> <manifest.json> <output.exe>");
        std::process::exit(2);
    }
    let loader_path = &args[1];
    let html_path = &args[2];
    let manifest_path = &args[3];
    let out_path = &args[4];

    let mut loader = std::fs::read(loader_path).expect("读 loader.exe 失败");
    let html = std::fs::read(html_path).expect("读 index.html 失败");
    let manifest = std::fs::read(manifest_path).expect("读 manifest.json 失败");

    // 先校验 manifest 能被解析 (早失败, 别等运行时)
    let _m: hermix_app_loader::payload::Manifest =
        serde_json::from_slice(&manifest).expect("manifest.json 解析失败");

    let mut payload = Vec::new();
    payload.extend_from_slice(&(manifest.len() as u64).to_le_bytes());
    payload.extend_from_slice(&manifest);
    payload.extend_from_slice(&(html.len() as u64).to_le_bytes());
    payload.extend_from_slice(&html);

    loader.extend_from_slice(&payload);
    loader.extend_from_slice(MAGIC);
    loader.extend_from_slice(&(payload.len() as u64).to_le_bytes());

    std::fs::write(out_path, &loader).expect("写 output.exe 失败");
    println!(
        "✅ 已注入 {} -> {} (payload {} bytes)",
        loader_path, out_path, payload.len()
    );

    // 自检: 回读解析, 确保 trailer 布局正确
    let check = std::fs::read(out_path).expect("自检读回失败");
    let p = parse_trailer(&check).expect("自检解析失败");
    println!(
        "✅ 自检通过: app=`{}` permissions={:?}",
        p.manifest.name, p.manifest.permissions
    );
}
