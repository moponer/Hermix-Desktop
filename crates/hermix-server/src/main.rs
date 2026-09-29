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

    let config = load_config(None).map_err(|e| AgentError::Config(e.to_string()))?;

    // 安全中心：启动即按 config.yaml 的 security 刷新自动备份策略。
    hermix_tools::backup::set_policy_from_config(&config.security, &hermix_config::paths::hermix_home());

    // --stdio：接管 stdin/stdout 跑 JSON Lines 多路复用协议（桌面端 IPC），
    // 不绑定任何 TCP 端口；默认（无 flag）保持 HTTP/WS TCP 监听。
    if stdio {
        let state = hermix_server::HttpServerState::build(config)
            .await?
            .with_automation_scheduler();
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
