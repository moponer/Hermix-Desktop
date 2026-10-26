//! `Arc<ToolRegistry>` → prefetch `SafeToolRegistry` 适配器。
//!
//! 从 `local_agent_service` 搬出的第三批（P1-13）。`hermix_intelligence` 的
//! prefetch 执行器只认自己的 `SafeToolRegistry` trait，而工具注册表住在
//! `hermix_tools` 里 —— 这个纯桥接没必要占组合根的位置。

use std::sync::Arc;

use hermix_core::traits::RiskLevel;
use hermix_tools::ToolRegistry;
use serde_json::Value;

/// 让 `Arc<ToolRegistry>` 满足 prefetch 的 `SafeToolRegistry`。
///
/// 执行器据此在 LLM 调用**之前**并行预取 Safe 类工具的结果（省一次往返），
/// 所以 `risk_level` 必须如实转发注册表的判定 —— 这里若返回过宽松的值，
/// 有副作用的工具会被提前执行。
pub(super) struct SafeRegistryWrapper(pub(super) Arc<ToolRegistry>);

#[async_trait::async_trait]
impl hermix_intelligence::prefetch::SafeToolRegistry for SafeRegistryWrapper {
    fn risk_level(&self, name: &str) -> Option<RiskLevel> {
        self.0.risk_level(name)
    }

    fn exists(&self, name: &str) -> bool {
        self.0.get_tool(name).is_some()
    }

    async fn dispatch(&self, name: &str, args: Value) -> String {
        self.0.dispatch_async(name, args).await
    }
}
