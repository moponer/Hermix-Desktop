//! QQ 网关链路 Rust 侧探针：与桌面端走完全相同的代码路径。
//! 用法：cargo run -p hermix-server --example probe_qq_gateway
//! 需要环境变量 HERMIX_QQ_APP_ID / HERMIX_QQ_SECRET（或默认读 ~/.hermix/im_channels.json）。

#[tokio::main]
async fn main() {
    let cred = std::fs::read_to_string(format!(
        "{}/.hermix/im_channels.json",
        std::env::var("USERPROFILE").unwrap_or_default()
    ))
    .expect("读 im_channels.json 失败");
    let v: serde_json::Value = serde_json::from_str(&cred).expect("解析 im_channels.json 失败");
    let qq = &v["qq"];
    let app_id = qq["primary"].as_str().expect("缺 primary");
    let secret = qq["secondary"].as_str().expect("缺 secondary");

    println!("[1] appId={app_id}, secret 长度={}", secret.len());

    let api = std::sync::Arc::new(hermix_server::channels::qq::QqApi::new(app_id, secret));

    println!("[2] access_token()…");
    match api.access_token().await {
        Ok(t) => println!("    ✅ token 长度={}", t.len()),
        Err(e) => {
            println!("    ❌ {e}");
            return;
        }
    }

    println!("[3] gateway_url()…");
    let url = match api.gateway_url().await {
        Ok(u) => {
            println!("    ✅ {u}");
            u
        }
        Err(e) => {
            println!("    ❌ {e}");
            return;
        }
    };

    println!("[4] tokio-tungstenite connect_async…");
    let (mut ws, resp) = match tokio_tungstenite::connect_async(&url).await {
        Ok(x) => {
            println!("    ✅ WS 已建连, status={:?}", x.1.status());
            x
        }
        Err(e) => {
            println!("    ❌ 连接网关 {url} 失败: {e}");
            return;
        }
    };
    let _ = resp;

    println!("[5] 等 HELLO…");
    use futures::{SinkExt, StreamExt};
    let first = match tokio::time::timeout(std::time::Duration::from_secs(15), ws.next()).await {
        Ok(Some(Ok(m))) => m,
        Ok(Some(Err(e))) => {
            println!("    ❌ 读帧失败: {e}");
            return;
        }
        Ok(None) => {
            println!("    ❌ 连接被立刻关闭");
            return;
        }
        Err(_) => {
            println!("    ❌ 等 HELLO 超时");
            return;
        }
    };
    println!("    收到: {}", first.to_text().unwrap_or("<非文本>").chars().take(200).collect::<String>());

    let access = api.access_token().await.unwrap_or_default();
    let identify = serde_json::json!({
        "op": 2u8,
        "d": { "token": format!("QQBot {access}"), "intents": (1u64 << 12) | (1u64 << 25), "shard": [0u8, 1u8] }
    });
    println!("[6] 发 IDENTIFY…");
    if let Err(e) = ws.send(tokio_tungstenite::tungstenite::Message::Text(
        identify.to_string().into(),
    ))
    .await
    {
        println!("    ❌ {e}");
        return;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(15), ws.next()).await {
        Ok(Some(Ok(m))) => {
            let t = m.to_text().unwrap_or("<非文本>");
            println!("    收到: {}", t.chars().take(300).collect::<String>());
            if t.contains("READY") {
                println!("\n✅ Rust 侧全链路通：网关层没问题，问题在应用运行环境。");
            } else if t.contains("\"op\":9") {
                println!("\n❌ INVALID_SESSION。");
            } else {
                println!("\n⚠️ 未知响应，见上。");
            }
        }
        Ok(Some(Err(e))) => println!("    ❌ {e}"),
        Ok(None) => println!("    ❌ IDENTIFY 后连接被关闭（服务端拒了？）"),
        Err(_) => println!("    ❌ 等 READY 超时"),
    }
}
