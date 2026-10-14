//! Screencast 帧通道——浏览器小窗（`browser-view` 子窗）的画面来源。
//!
//! 浏览器恒定 headless（见 `launch`），本机不存在任何浏览器窗口；操作画面
//! 经 `Page.startScreencast` 的 JPEG 帧流送到 desktop-tauri 的
//! `browser_view_controller`，渲染成可拖动/可缩放的独立小窗。本机鼠标键盘
//! 全程不受影响——所有输入走 CDP 合成事件，不进系统输入队列。
//!
//! 本模块只管 Chrome → Rust 的帧通道，**不涉及 Tauri**（consumer 在另一个
//! crate）。职责切分：
//!
//! - `start` 幂等：同一时刻只有一条 screencast 通道。Session 建立即调用；
//!   已存在则直接返回，不会泄漏第二个事件监听。
//! - 帧必须逐帧 ack（`ScreencastFrameAck`），否则 Chrome 只推第一帧。
//! - `broadcast` 容量 4：消费慢时丢旧帧保新帧，画面类数据不需要积压；
//!   无人订阅时 send 直接 Err，零成本丢弃。
//! - 暂停语义（`pause`/`resume`）：发 `Page.stopScreencast` **彻底停
//!   Chrome 侧 JPEG 编码**——小窗最小化/遮挡时的 CPU 闸门（评审修正 1：
//!   此前"只停 Rust→前端推流"是没实测就抄的退化路径，动画页 Edge 照常
//!   拉满）。`resume` 重发 `start`（CDP restart 成本毫秒级；ego 的 WGC/
//!   FFmpeg 停流抖动教训不适用于 CDP 路线，真机若见抖动再退化为
//!   "只降 every_nth_frame"）。`start`/`pause` 幂等，resume 在未暂停时
//!   是 no-op。
//! - 单例可重置（`Mutex<Option<..>>` 而非 OnceLock）：Session 被替换时
//!   Drop 跑 `shutdown()` 清场，新 session 的 start 才能真起一条新流。

use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast;

use super::ToolError;

/// 帧广播发送端。`None` = 无活动 screencast（会话未建立 / 已关闭）。
type FrameTx = broadcast::Sender<Arc<Vec<u8>>>;

static CHANNEL: OnceLock<Mutex<Option<FrameTx>>> = OnceLock::new();

/// 当前 screencast 挂在哪个 Page 上（Clone 的 Arc 句柄）。pause/resume
/// 需要它发 CDP 命令；`None` = 无活动 screencast。
static PAGE: OnceLock<Mutex<Option<chromiumoxide::Page>>> = OnceLock::new();

fn channel() -> &'static Mutex<Option<FrameTx>> {
    CHANNEL.get_or_init(|| Mutex::new(None))
}

fn page_slot() -> &'static Mutex<Option<chromiumoxide::Page>> {
    PAGE.get_or_init(|| Mutex::new(None))
}

/// 当前 screencast 是否在跑（无会话或已关闭时为 false）。
pub fn is_running() -> bool {
    channel().lock().map(|g| g.is_some()).unwrap_or(false)
}

/// 订阅帧流。返回 `None` = 当前没有活动的 screencast（浏览器会话未建立
/// 或已关闭）——调用方应稍后重试，而不是当错误。
pub fn receiver() -> Option<broadcast::Receiver<Arc<Vec<u8>>>> {
    channel().lock().ok()?.as_ref().map(|tx| tx.subscribe())
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

/// 启动（或确认已启动）当前页面的 screencast。幂等。
pub async fn start(page: &chromiumoxide::Page) -> Result<(), ToolError> {
    if is_running() {
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
        // 事件流结束 = 页面/浏览器没了。CHANNEL 留着死发送端，靠 Session
        // Drop 的 shutdown() 清场（主路径）；此处不清是因为无法分辨
        // "自己的页面死了"与"已被新 session 顶替"。
    });

    let mut guard = channel().lock().map_err(|_| {
        ToolError::ExecutionFailed("screencast 通道锁中毒".into())
    })?;
    if guard.is_some() {
        // 竞态输家：重复启动。新起的事件监听随本页 drop 自行结束，
        // 保留先到者的通道。
        return Ok(());
    }
    *guard = Some(tx);
    // 记住挂在哪个 Page 上：pause/resume 要对同一页面发 stop/start。
    if let Ok(mut pg) = page_slot().lock() {
        *pg = Some(page.clone());
    }
    Ok(())
}

/// 暂停：发 `Page.stopScreencast` 彻底停 Chrome 侧 JPEG 编码（小窗最小化/
/// 遮挡时的 CPU 闸门）。幂等：未在跑时是 no-op。通道、事件监听任务与
/// 帧泵 receiver **原样保留**——pause 后 Chrome 不再发帧，事件流不结束
/// （页面活着），帧泵的 recv 挂起等待；resume 后同一条监听自然续上，
/// 这正是不能清通道的原因：清了会导致帧泵阻塞在旧 receiver 上永远
/// 收不到新通道的帧。
pub async fn pause() {
    let Some(page) = page_slot().lock().ok().and_then(|g| g.clone()) else {
        return;
    };
    use chromiumoxide::cdp::browser_protocol::page::StopScreencastParams;
    let _ = page.execute(StopScreencastParams {}).await;
    tracing::debug!("screencast paused（Chrome 侧编码已停）");
}

/// 恢复：对同一页面重发 `start` 参数（Chrome 继续用原会话推帧，监听与
/// 通道无缝续上）。幂等；页面已死时静默失败——下一次 Session ensure 的
/// start 会重建一切。
pub async fn resume() {
    let Some(page) = page_slot().lock().ok().and_then(|g| g.clone()) else {
        return;
    };
    if let Err(e) = page.execute(screencast_params()).await {
        tracing::debug!(error = %e, "screencast resume 失败（页面可能已死，等下次 ensure 重建）");
    }
}

/// 同步清空通道。**Session Drop 用**——Chrome 侧推流随页面关闭自动停止，
/// 无需（也不能在同步 Drop 里）发 CDP 命令。清场后 `start` 可重建。
pub fn shutdown() {
    if let Ok(mut guard) = channel().lock() {
        *guard = None;
    }
    if let Ok(mut pg) = page_slot().lock() {
        *pg = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单例核心不变量：无会话时 receiver 为 None；有会话时订阅拿得到。
    /// 与 ignored 实机测试共享进程级 CHANNEL，故用逻辑断言而非硬编码。
    #[test]
    fn receiver_tracks_channel_lifecycle() {
        if is_running() {
            assert!(receiver().is_some(), "通道在，订阅必须拿得到");
        } else {
            assert!(receiver().is_none(), "通道不在，订阅必须是 None");
        }
    }

    /// shutdown 后可重建：清场 → is_running false → receiver None。
    /// 放在实机流程后面跑时验证"旧会话清场不挡住新会话起流"。
    #[test]
    fn shutdown_clears_the_channel() {
        shutdown();
        assert!(!is_running());
        assert!(receiver().is_none());
        // 再次 start 的实机路径见 ignored 测试；这里只验证清场语义。
    }

    /// 手工验证原型：启动真实 Edge → screencast 常开（ensure 建立 session
    /// 即 start）→ 导航 → 从 receiver 收 3 帧 JPEG；再验证幂等与清场。
    /// 需要本机有 Edge，跑法：`cargo test -p hermix-tools
    /// --lib os_auto::browser::screencast -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要真实 Edge 进程，手工运行"]
    async fn captures_frames_from_real_edge() {
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
        start(&page).await.expect("start 1");
        start(&page).await.expect("start 2 (idempotent)");
        let mut rx = receiver().expect("receiver");

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
        shutdown();
        assert!(!is_running());
        start(&page).await.expect("restart after shutdown");
        assert!(receiver().is_some(), "重建后订阅必须拿得到");
    }
}
