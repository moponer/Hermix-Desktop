//! Screencast — per-session CDP frame capture (JPEG q60, broadcast, no disk).
//!
//! 多会话并行（2026-10-17）：每个浏览器会话（键 = `_session_id`）有独立的
//! 通道与 Page 句柄——两路帧流互不干扰，小窗按会话切换显示。键控 API：
//! `start(key, &page)` / `receiver(&key)` / `pause(&key)` / `resume(&key)` /
//! `shutdown(&key)` / `active_keys()`。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tokio::sync::{broadcast, Mutex};

use super::ToolError;

/// 帧广播发送端。
type FrameTx = broadcast::Sender<Arc<Vec<u8>>>;

/// 键 → 帧通道。`key` 是浏览器会话键（`_session_id`，无上下文时 "default"）。
static CHANNELS: OnceLock<Mutex<HashMap<String, FrameTx>>> = OnceLock::new();

/// 键 → Page 句柄（pause/resume 要对同一页面发 CDP 命令）。
static PAGES: OnceLock<Mutex<HashMap<String, chromiumoxide::Page>>> = OnceLock::new();

fn channels() -> &'static Mutex<HashMap<String, FrameTx>> {
    CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pages() -> &'static Mutex<HashMap<String, chromiumoxide::Page>> {
    PAGES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 当前 key 的 screencast 是否在跑。
pub async fn is_running(key: &str) -> bool {
    channels().lock().await.contains_key(key)
}

/// 所有活动会话的键（转发器与多路分发用）。
pub async fn active_keys() -> Vec<String> {
    channels().lock().await.keys().cloned().collect()
}

/// 订阅某会话的帧流。`None` = 该会话没有活动 screencast——调用方应稍后
/// 重试，而不是当错误。
pub async fn receiver(key: &str) -> Option<broadcast::Receiver<Arc<Vec<u8>>>> {
    channels().lock().await.get(key).map(|tx| tx.subscribe())
}

/// screencast 启动参数。start 与 resume 共用同一份——resume 就是对同一
/// 页面重发这份参数，Chrome 继续用原会话推帧，监听与通道无缝续上。
fn screencast_params() -> chromiumoxide::cdp::browser_protocol::page::StartScreencastParams {
    use chromiumoxide::cdp::browser_protocol::page::{StartScreencastFormat, StartScreencastParams};
    StartScreencastParams {
        format: Some(StartScreencastFormat::Jpeg),
        quality: Some(60),
        max_width: Some(1280),
        max_height: Some(800),
        every_nth_frame: None,
    }
}

/// 启动（或确认已启动）某会话页面的 screencast。幂等。
pub async fn start(key: &str, page: &chromiumoxide::Page) -> Result<(), ToolError> {
    if is_running(key).await {
        return Ok(());
    }
    page.execute(screencast_params())
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("启动 screencast 失败: {e}")))?;

    let (tx, _rx) = broadcast::channel(4);
    let mut events = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::page::EventScreencastFrame>()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("订阅 screencast 事件失败: {e}")))?;

    // 独立任务：收帧 → ack → 广播。Page 是 Clone 的 Arc 句柄，生命周期安全。
    let ack_page = page.clone();
    let tx_task = tx.clone();
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
                let _ = tx_task.send(Arc::new(bytes));
            }
        }
        // 事件流结束 = 页面/浏览器没了。CHANNELS 留着死发送端，靠 Session
        // Drop 的 shutdown() 清场（主路径）；此处不清是因为无法分辨
        // "自己的页面死了"与"已被新 session 顶替"。
    });

    let mut guard = channels().lock().await;
    if guard.contains_key(key) {
        // 竞态输家：重复启动。新起的事件监听随本页 drop 自行结束，
        // 保留先到者的通道。
        return Ok(());
    }
    guard.insert(key.to_string(), tx);
    // 记住挂在哪个 Page 上：pause/resume 要对同一页面发 stop/start。
    pages().lock().await.insert(key.to_string(), page.clone());
    Ok(())
}

/// 暂停：发 `Page.stopScreencast` 彻底停 Chrome 侧 JPEG 编码（小窗最小化/
/// 遮挡时的 CPU 闸门）。幂等：未在跑时是 no-op。通道、事件监听任务与
/// 帧泵 receiver **原样保留**——pause 后 Chrome 不再发帧，事件流不结束
/// （页面活着），帧泵的 recv 挂起等待；resume 后同一条监听自然续上，
/// 这正是不能清通道的原因：清了会导致帧泵阻塞在旧 receiver 上永远
/// 收不到新通道的帧。
pub async fn pause(key: &str) {
    let Some(page) = pages().lock().await.get(key).cloned() else {
        return;
    };
    use chromiumoxide::cdp::browser_protocol::page::StopScreencastParams;
    let _ = page.execute(StopScreencastParams {}).await;
    tracing::debug!(key = %key, "screencast paused（Chrome 侧编码已停）");
}

/// 恢复：对同一页面重发 `start` 参数（Chrome 继续用原会话推帧，监听与
/// 通道无缝续上）。幂等；页面已死时静默失败——下一次 ensure 的
/// start 会重建一切。
pub async fn resume(key: &str) {
    let Some(page) = pages().lock().await.get(key).cloned() else {
        return;
    };
    if let Err(e) = page.execute(screencast_params()).await {
        tracing::debug!(key = %key, error = %e, "screencast resume 失败（页面可能已死，等下次 ensure 重建）");
    }
}

/// 同步清空某会话的通道。**Session Drop 用**——Chrome 侧推流随页面关闭
/// 自动停止，无需（也不能在同步 Drop 里）发 CDP 命令。清场后 `start`
/// 可重建。注意：这是同步锁（Drop 里不能 .await），CHANNELS 用的是
/// tokio Mutex——同步 try_lock 失败时留下死发送端，由下次 start 的
/// 覆盖注册兜底（与多会话前同款妥协，事件流本身已随页面死亡终止）。
pub fn shutdown(key: &str) {
    if let Ok(mut guard) = channels().try_lock() {
        guard.remove(key);
    }
    if let Ok(mut pg) = pages().try_lock() {
        pg.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 键控核心不变量：无会话时 receiver 为 None；start（实机路径）后有。
    /// 与 ignored 实机测试共享进程级 CHANNELS，故用逻辑断言而非硬编码。
    #[tokio::test]
    async fn receiver_tracks_channel_lifecycle() {
        if is_running("selftest").await {
            assert!(receiver("selftest").await.is_some(), "通道在，订阅必须拿得到");
        } else {
            assert!(receiver("selftest").await.is_none(), "通道不在，订阅必须是 None");
        }
    }

    /// shutdown 后可重建：清场 → is_running false → receiver None。
    #[tokio::test]
    async fn shutdown_clears_the_channel() {
        shutdown("selftest-shutdown");
        assert!(!is_running("selftest-shutdown").await);
        assert!(receiver("selftest-shutdown").await.is_none());
    }

    /// 键隔离：shutdown 一个 key 不影响另一个 key 的通道。
    #[tokio::test]
    async fn shutdown_is_key_scoped() {
        // 不起真 Edge：只验证"清 A 不动 B"在空表上是无害 no-op。
        shutdown("selftest-a");
        shutdown("selftest-b");
        assert!(!is_running("selftest-a").await);
        assert!(!is_running("selftest-b").await);
    }

    /// 手工验证原型：启动真实 Edge → screencast 常开（ensure 建立 session
    /// 即 start）→ 导航 → 从 receiver 收 3 帧 JPEG；再验证幂等与清场。
    /// 需要本机有 Edge，跑法：`cargo test -p hermix-tools
    /// --lib os_auto::browser::screencast -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要真实 Edge 进程，手工运行"]
    async fn captures_frames_from_real_edge() {
        let key = "manual-edge-test";
        let launched = super::super::launch::launch()
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

        // 幂等：第二次启动不报错、不泄漏监听。
        start(key, &page).await.expect("start 1");
        start(key, &page).await.expect("start 2 (idempotent)");
        let mut rx = receiver(key).await.expect("receiver");
        assert!(active_keys().await.contains(&key.to_string()));

        page.goto("https://example.com").await.expect("goto");

        // 静置 1s 等首帧，然后收 3 帧。
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        for i in 0..3 {
            let frame = rx.recv().await.expect("frame");
            let path = std::env::temp_dir().join(format!("hermix-screencast-{i}.jpg"));
            std::fs::write(&path, &*frame).expect("write frame");
            println!("帧 {i}: {} 字节 → {}", frame.len(), path.display());
        }

        // 清场后可重建（下一次 ensure 的场景）。
        shutdown(key);
        assert!(!is_running(key).await);
        start(key, &page).await.expect("restart after shutdown");
        assert!(receiver(key).await.is_some(), "重建后订阅必须拿得到");
    }
}
