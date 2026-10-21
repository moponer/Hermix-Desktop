//! 浏览器自动化实测：冷启动、导航、快照的耗时，以及会话复用是否生效。
//!
//! ## 为什么要有这个文件
//!
//! 浏览器线的性能特征此前只有定性描述（"profile 每次 launch 新建 → Edge
//! 冷启动几百 ms 到数秒"）。没有数字就无法判断要不要投入做连接池/跨会话复用。
//!
//! 本测试在**有 Edge 的机器上真跑**，缺席时跳过（与 dsh 引擎 smoke 同款模式），
//! 并用 `--nocapture` 打出每个阶段的墙钟耗时。
//!
//! 页面用 `data:` URL —— 完全离线、确定，不依赖任何外部站点。

use std::time::Instant;

use hermix_tools::ToolRegistry;

/// 一个带按钮与输入框的小页面，够 snapshot / click / type 用。
const PAGE: &str = "data:text/html,<h1>probe</h1><button id='b'>go</button><input id='i'>";

fn registry() -> ToolRegistry {
    let r = ToolRegistry::new();
    hermix_tools::tools::os_auto::browser::register(&r);
    r
}

/// Edge 不在（或浏览器工具集整体不可用）时跳过，而不是失败。
fn browser_available(reg: &ToolRegistry) -> bool {
    hermix_tools::tools::os_auto::browser::launch::resolve_edge_exe().is_some()
        && reg
            .get_definitions()
            .iter()
            .any(|d| d.name == "browser_navigate")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_cold_start_navigate_snapshot_timings() {
    let reg = registry();
    if !browser_available(&reg) {
        eprintln!("SKIP: Edge 不可用或 browser 工具集未注册");
        return;
    }
    let session = "browser-bench";

    // 1. 冷启动 + 导航（第一次调用要拉起 Edge + 建 profile）
    let t = Instant::now();
    let nav = reg
        .dispatch_async("browser_navigate", serde_json::json!({"url": PAGE, "_session_id": session}))
        .await;
    let cold = t.elapsed();
    assert!(nav.contains("probe") || nav.contains("ok") || !nav.contains("\"error\""), "{nav}");

    // 2. 快照（复用同一 session，理论上不再冷启动）
    let t = Instant::now();
    let snap = reg
        .dispatch_async("browser_snapshot", serde_json::json!({"_session_id": session}))
        .await;
    let snap_ms = t.elapsed();
    assert!(snap.contains("ref="), "快照必须给出 ref 句柄: {snap}");

    // 3. 再一次导航：验证会话复用（应与第 2 步同量级，而非重新冷启动）
    let t = Instant::now();
    let nav2 = reg
        .dispatch_async(
            "browser_navigate",
            serde_json::json!({"url": PAGE, "_session_id": session}),
        )
        .await;
    let warm = t.elapsed();
    let _ = nav2;

    // 4. 收尾
    let _ = reg
        .dispatch_async("browser_close", serde_json::json!({"_session_id": session}))
        .await;

    println!("\n=== 浏览器自动化实测（同一 session）===");
    println!("冷启动+首次导航 : {cold:?}");
    println!("snapshot        : {snap_ms:?}");
    println!("第二次导航      : {warm:?}");
    println!(
        "复用收益        : {:.1}×",
        cold.as_secs_f64() / warm.as_secs_f64().max(0.001)
    );

    // 硬约束：会话复用必须让第二次导航显著快于冷启动。若这条红，说明
    // SESSIONS 单例没生效（每次都新建 Edge），那才是要投入优化的信号。
    assert!(
        warm < cold,
        "第二次导航 ({warm:?}) 不应慢于冷启动 ({cold:?}) —— 会话复用可能失效"
    );
}
