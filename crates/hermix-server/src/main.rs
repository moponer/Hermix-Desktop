use std::net::SocketAddr;
use hermix_config::env_compat;

use hermix_config::load_config;
use hermix_core::AgentError;
use hermix_telemetry::init_telemetry_from_env;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    init_telemetry_from_env("hermes-server", "info");

    let config = load_config(None).map_err(|e| AgentError::Config(e.to_string()))?;
    let addr: SocketAddr = env_compat("HERMIX_SERVER_ADDR", "HERMES_SERVER_ADDR")
        .or_else(|| env_compat("HERMIX_DASHBOARD_ADDR", "HERMES_DASHBOARD_ADDR"))
        .or_else(|| env_compat("HERMIX_HTTP_ADDR", "HERMES_HTTP_ADDR"))
        .unwrap_or_else(|| "127.0.0.1:3000".to_string())
        .parse()
        .map_err(|e| AgentError::Config(format!("invalid server addr: {}", e)))?;

    hermix_server::run_server(addr, config).await
}
