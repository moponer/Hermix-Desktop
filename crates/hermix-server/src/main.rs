#![windows_subsystem = "windows"]

use std::net::SocketAddr;
use hermix_config::env_compat;

use hermix_config::load_config;
use hermix_core::AgentError;
use hermix_telemetry::init_telemetry_from_env;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
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

    let config = load_config(None).map_err(|e| AgentError::Config(e.to_string()))?;

    // 安全中心：启动即按 config.yaml 的 security 刷新自动备份策略。
    hermix_tools::backup::set_policy_from_config(&config.security, &hermix_config::paths::hermix_home());

    // --stdio：接管 stdin/stdout 跑 JSON Lines 多路复用协议（桌面端 IPC），
    // 不绑定任何 TCP 端口；默认（无 flag）保持 HTTP/WS TCP 监听。
    if stdio {
        let state = hermix_server::HttpServerState::build(config)
            .await?
            .with_automation_scheduler()
            // relay 出站连接与 HTTP 面无关，桌面端（唯一 stdio 消费者）必须有它——
            // 此前只挂在 run_server()（HTTP 模式）上，桌面端从未连过中转，
            // 配对与手机同步整条链路因此失效（设置页显示「尚未接通中转」）。
            .with_relay_client();
        return hermix_server::stdio_session::run_stdio_server(state).await;
    }

    let addr: SocketAddr = env_compat("HERMIX_SERVER_ADDR", "HERMES_SERVER_ADDR")
        .or_else(|| env_compat("HERMIX_DASHBOARD_ADDR", "HERMES_DASHBOARD_ADDR"))
        .or_else(|| env_compat("HERMIX_HTTP_ADDR", "HERMES_HTTP_ADDR"))
        .unwrap_or_else(|| "127.0.0.1:3000".to_string())
        .parse()
        .map_err(|e| AgentError::Config(format!("invalid server addr: {}", e)))?;

    hermix_server::run_server(addr, config).await
}
