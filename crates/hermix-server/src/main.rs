#![windows_subsystem = "windows"]

use std::net::SocketAddr;
use hermix_config::env_compat;

use hermix_config::load_config;
use hermix_core::AgentError;
use hermix_telemetry::init_telemetry_from_env;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    // 沙箱账户 setup 的提权入口（UAC runas 到自身）：处理 payload 后直接退出，
    // 不进任何服务器逻辑（必须在其他初始化之前）。
    #[cfg(windows)]
    if let Some(code) = hermix_environments::account_sandbox::elevated_entry_from_args() {
        std::process::exit(code);
    }

    // --stdio 时 stdout 是 JSON Lines 协议通道，日志必须走 stderr，否则日志行
    // 会被桌面端当帧解析（unparseable frame 死循环 + 启动卡死）。
    let stdio = std::env::args().any(|a| a == "--stdio");
    init_telemetry_from_env("hermes-server", "info", stdio);

    // 进程级 panic 兜底：任何 panic 在 unwind 前把 位置+消息+backtrace 同步写到
    // <hermix_home>/logs/panic-*.log。stdio agent loop 没有 catch_unwind 包裹
    // （那份只在已退役的 ws 路径），panic=unwind 会一路 unwind 进 main 退出进程；
    // 默认 hook 的消息只到被 windows_subsystem 吞掉的 stderr —— 正是"后端中途
    // 退出却查不到原因"的根因。装在任何可能 panic 的代码（含下面的 load_config）
    // 之前。
    hermix_server::crash_dump::install_panic_hook(hermix_config::paths::hermix_home());

    // ── 模型目录（上下文长度的权威来源）──
    //
    // 先同步装本地缓存（立即生效，不碰网络），缓存过期再后台刷一次。
    // 没有它，上下文长度只能靠静态家族表猜：`minimax → 204.8K` 在 M3.1
    // （实际 1M）上照样生效（2026-10-23 实测）。
    let cached_models = hermix_intelligence::model_directory::load_from_disk();
    let cached_endpoints = hermix_intelligence::model_directory::load_endpoints_from_disk();
    tracing::debug!(
        models = cached_models,
        providers = cached_endpoints,
        "模型目录：本地缓存"
    );

    let config = load_config(None).map_err(|e| AgentError::Config(e.to_string()))?;

    // 安全中心：启动即按 config.yaml 的 security 刷新自动备份策略。
    hermix_tools::backup::set_policy_from_config(&config.security, &hermix_config::paths::hermix_home());

    // ── 模型目录 + provider 级 endpoints 的后台刷新 ──
    //
    // 两件事都必须在拿到 config.model 之后做：
    // 1. 目录（/models）——聚合上下文，缓存过期才刷；
    // 2. 当前模型的 endpoints（/models/{id}/endpoints）——**provider 级真实
    //    窗口与输出上限**，压缩阈值按它算才不会估错（M3.1：1M 上下文 /
    //    512K 输出，聚合值给的是 1M，但若某模型的聚合值来自别家 provider，
    //    按它算阈值会超限 → 400）。
    //
    // 全程后台、失败静默：拿不到就继续用磁盘缓存 / 静态表兜底。
    {
        let model_name = config.model.clone();
        tokio::spawn(async move {
            let http = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(25))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("模型目录：HTTP client 构建失败: {e}");
                    return;
                }
            };
            if hermix_intelligence::model_directory::cache_is_stale() {
                let n = hermix_intelligence::model_directory::refresh(&http).await;
                tracing::info!(models = n, "模型目录已刷新（OpenRouter 公开目录）");
            }
            if let Some(model_name) = model_name {
                if let Some(id) =
                    hermix_intelligence::model_directory::openrouter_id_for(&model_name)
                {
                    let n =
                        hermix_intelligence::model_directory::refresh_endpoints(&http, &id).await;
                    if n > 0 {
                        tracing::info!(model = %id, providers = n, "provider 级上下文限额已刷新");
                    }
                }
            }
        });
    }

    // --stdio：接管 stdin/stdout 跑 JSON Lines 多路复用协议（桌面端 IPC），
    // 不绑定任何 TCP 端口；默认（无 flag）保持 HTTP/WS TCP 监听。
    if stdio {
        let state = hermix_server::HttpServerState::build(config)
            .await?
            .with_automation_scheduler()
            // relay 出站连接与 HTTP 面无关，桌面端（唯一 stdio 消费者）必须有它——
            // 此前只挂在 run_server()（HTTP 模式）上，桌面端从未连过中转，
            // 配对与手机同步整条链路因此失效（设置页显示「尚未接通中转」）。
            .with_relay_client()
            // 资源巡检同理（2026-10-25 实报）：漏挂它 → `GET /api/resource/patrol`
            // 在桌面端恒 503 → 任务台「资源」页签永远加载中，还把当时与它
            // Promise.all 绑在一起的「团队」页签一起拖死。run_server 挂了的
            // builder，stdio 路径必须逐个对齐——这是同一类坑的第二次。
            .with_resource_patrol();
        let result = hermix_server::stdio_session::run_stdio_server(state).await;

        // ── 显式退出（2026-10-29：修「每次重启都被强杀 + 每次都留 panic」）──
        //
        // 不能让 `#[tokio::main]` 在 main 返回后 drop runtime 来收尾：
        //  1) drop 会等阻塞池与分离任务，而 MCP 60s 重试 / relay / 资源巡检 /
        //     模型目录刷新这些循环从不结束 → 进程卡住不退。桌面外壳等 3s 便
        //     TerminateProcess，每次重启都跳过 SQLite/WAL 收尾。
        //  2) drop 期间仍有任务被 poll 并注册定时器 → 退出时必留一份
        //     `A Tokio 1.x context was found, but it is being shutdown`
        //     （tokio runtime/time/entry.rs:539），把真问题淹在噪音里。
        // 收尾（IM 通道停止、日志刷盘）已在 run_stdio_server 内做过，这里直接
        // 以退出码结束进程，不进 runtime 析构。
        let code = match &result {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!("stdio: 主循环异常退出: {e}");
                1
            }
        };
        hermix_telemetry::flush_logs();
        std::process::exit(code);
    }

    let addr: SocketAddr = env_compat("HERMIX_SERVER_ADDR", "HERMES_SERVER_ADDR")
        .or_else(|| env_compat("HERMIX_DASHBOARD_ADDR", "HERMES_DASHBOARD_ADDR"))
        .or_else(|| env_compat("HERMIX_HTTP_ADDR", "HERMES_HTTP_ADDR"))
        .unwrap_or_else(|| "127.0.0.1:3000".to_string())
        .parse()
        .map_err(|e| AgentError::Config(format!("invalid server addr: {}", e)))?;

    hermix_server::run_server(addr, config).await
}
