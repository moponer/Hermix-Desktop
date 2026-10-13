//! Screencast 帧流原型 — 虚拟窗口的第一块基石。
//!
//! 通过 `Page.startScreencast` 订阅当前会话页面的 JPEG 帧流。浏览器实例
//! 本身完全隐藏（headless），本机鼠标键盘不受任何影响；消费者拿到的是
//! 纯内存字节流，后续接 Tauri 前端 canvas 即可形成"画中画"。
//!
//! 帧必须逐帧 ack（`ScreencastFrameAck`），否则 Chrome 只推第一帧。

use tokio::sync::broadcast;

use super::ToolError;

/// 启动当前页面的 screencast，返回帧广播接收端。
///
/// 每帧是完整的 JPEG 字节。`broadcast` 容量 4：消费慢时丢旧帧保新帧，
/// 画面类数据不需要积压。
pub async fn start(page: &chromiumoxide::Page) -> Result<broadcast::Receiver<std::sync::Arc<Vec<u8>>>, ToolError> {
    use chromiumoxide::cdp::browser_protocol::page::{
        StartScreencastFormat, StartScreencastParams,
    };

    page.execute(StartScreencastParams {
        format: Some(StartScreencastFormat::Jpeg),
        quality: Some(60),
        max_width: Some(1280),
        max_height: Some(800),
        every_nth_frame: None,
    })
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("启动 screencast 失败: {e}")))?;

    let (tx, rx) = broadcast::channel(4);
    let mut events = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::page::EventScreencastFrame>()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("订阅 screencast 事件失败: {e}")))?;

    // 独立任务：收帧 → ack → 广播。Page 是 Clone 的 Arc 句柄，生命周期安全。
    let ack_page = page.clone();
    tokio::spawn(async move {
        use futures::StreamExt;
        while let Some(frame) = events.next().await {
            // 逐帧 ack，Chrome 才会继续推后续帧。
            let _ = ack_page
                .execute(chromiumoxide::cdp::browser_protocol::page::ScreencastFrameAckParams::new(
                    frame.session_id,
                ))
                .await;
            // 事件里的 data 是 base64 文本（chromiumoxide 不自动解码），转成原始 JPEG。
            use base64::Engine as _;
            let raw: &[u8] = frame.data.as_ref();
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(raw) {
                let _ = tx.send(std::sync::Arc::new(bytes));
            }
        }
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工验证原型：启动真实 Edge → 开 screencast → 导航 → 存 3 帧 JPEG。
    /// 需要本机有 Edge，跑法：`cargo test -p hermix-tools
    /// --lib os_auto::browser::screencast -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要真实 Edge 进程，手工运行"]
    async fn captures_frames_from_real_edge() {
        let launched = super::super::launch::launch(true)
            .await
            .expect("launch edge");
        let (browser, mut handler) =
            chromiumoxide::Browser::connect(launched.ws_url.clone())
                .await
                .expect("connect");
        tokio::spawn(async move {
            use futures::StreamExt;
            while handler.next().await.is_some() {}
        });
        let page = browser.new_page("about:blank").await.expect("new page");

        let mut rx = start(&page).await.expect("screencast start");
        page.goto("https://example.com").await.expect("goto");

        // 静置 1s 等首帧，然后收 3 帧。
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        for i in 0..3 {
            let frame = rx.recv().await.expect("frame");
            let path = std::env::temp_dir().join(format!("hermix-screencast-{i}.jpg"));
            std::fs::write(&path, &*frame).expect("write frame");
            println!("帧 {i}: {} 字节 → {}", frame.len(), path.display());
        }

        let _ = super::super::session().lock().await.take();
    }
}
