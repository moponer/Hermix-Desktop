//! Live integration test for StreamableHttpTransport against a real
//! streamable-HTTP MCP server. `#[ignore]`d — needs network + a live endpoint;
//! run explicitly:
//!
//! ```text
//! OPS_MCP_URL=https://host/ops/mcp OPS_MCP_TOKEN=homk_… \
//!   cargo test -p hermix-mcp --test streamable_live -- --ignored --nocapture
//! ```

use hermix_mcp::{BearerTokenAuth, McpClient, McpServerConfig};

#[tokio::test]
#[ignore = "live network test — set OPS_MCP_URL + OPS_MCP_TOKEN"]
async fn streamable_live_connects_and_discovers_tools() {
    let url = std::env::var("OPS_MCP_URL").expect("OPS_MCP_URL not set");
    let token = std::env::var("OPS_MCP_TOKEN").expect("OPS_MCP_TOKEN not set");

    let config = McpServerConfig::http(url)
        .with_auth(std::sync::Arc::new(BearerTokenAuth::new(token)));
    let mut client = McpClient::new(config);
    client.connect().await.expect("connect failed");

    let tools = client.list_tools().await.expect("tools/list failed");
    let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    println!("discovered {} tools: {:?}", names.len(), names);
    assert!(
        names.iter().any(|n| n.contains("sys_overview")),
        "expected ops tools in the discovered set, got: {names:?}"
    );

    client.disconnect().await.unwrap();
}
