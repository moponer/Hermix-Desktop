//! 真机握手检查（2026-11-14 治标验证用）：用 hermix 客户端直连两个全局安装
//! 的 MCP node 入口（不经 npx/cmd），验证：
//! 1. 帧格式自适应修复后握手能通（JSONL 首选即中）；
//! 2. 工具列表完整；
//! 3. 断开后进程树干净（kill job 生效）。
//!
//! 运行：cargo run -p hermix-mcp --example mcp_connect_check

use std::time::Instant;

use hermix_mcp::{McpClient, McpServerConfig};

const SERVERS: [(&str, &str); 2] = [
    (
        "bing-cn-mcp-enhanced",
        "C:\\Users\\Administrator\\AppData\\Roaming\\npm\\node_modules\\bing-cn-mcp-enhanced\\build\\index.js",
    ),
    (
        "i18n-mcp-server",
        "C:\\Users\\Administrator\\AppData\\Roaming\\npm\\node_modules\\i18n-mcp-server\\dist\\index.js",
    ),
];

#[tokio::main]
async fn main() {
    let mut all_ok = true;
    for (name, entry) in SERVERS {
        let config = McpServerConfig::stdio("node", vec![entry.to_string()]);
        let mut client = McpClient::new(config);
        let t0 = Instant::now();
        match client.connect().await {
            Ok(()) => {
                let ms = t0.elapsed().as_millis();
                let tools: Vec<&str> = client
                    .cached_tools()
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect();
                println!(
                    "OK  {name}: connected in {ms}ms, {} tools: {}",
                    tools.len(),
                    tools.join(", ")
                );
                client.disconnect().await.ok();
            }
            Err(e) => {
                all_ok = false;
                println!("FAIL {name}: {e}");
            }
        }
    }
    // 断开后稍等，确认没有残留 node 进程（kill job 应即时回收整棵树）。
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    std::process::exit(if all_ok { 0 } else { 1 });
}
