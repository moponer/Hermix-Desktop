//! Per-session tool execution context.
//!
//! ## Why this module exists
//!
//! Before (session isolation), per-session state (workspace,
//! approval context, permission profile, session id, stdout callback)
//! lived on process-global `Arc<Mutex<…>>` fields on
//! [`crate::registry::ToolRegistryInner`]. `LocalAgentService` wrote
//! to those globals at the start of each turn; tool handlers read
//! from them at dispatch time. When two sessions ran concurrently,
//! session B's writes clobbered session A's values before A's tool
//! dispatch completed — a cross-session race.
//!
//! `ToolCallContext` fixes this by **carrying** per-session state
//! through the dispatch chain instead of storing it globally. The
//! bridge closure in `agent_builder::bridge_tool_registry` captures
//! an `Arc<ToolCallContext>` and forwards it to
//! `ToolRegistry::dispatch_with_context`, which passes it to the
//! `ContextAware::execute_with_context` method of tools that opted
//! in. Tools that don't need session context keep using
//! `ToolHandler::execute` unchanged.
//!
//! ## Pattern
//!
//! This follows the same opt-in pattern as [`crate::SessionStamper`]:
//! `ToolEntry` has an optional `context_aware:
//! Option<Arc<dyn ContextAware>>`. Only the 3 tools that need
//! per-session state (terminal, todo, code_execution) implement
//! `ContextAware`; the other 27+ pay zero cost.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermix_core::{Capability, Origin, PermissionProfile, ToolError};
use serde_json::Value;

use crate::approval::TerminalApprovalContext;

// ---------------------------------------------------------------------------
// ToolCallContext
// ---------------------------------------------------------------------------

/// Per-session execution context, threaded through the tool dispatch
/// chain. Eliminates the need for process-global mutable state on
/// `ToolRegistryInner`.
///
/// All fields are `Clone` (closures are behind `Arc`), so the entire
/// struct is `Clone` — it can be cloned into each `JoinSet::spawn`
/// task in `execute_tool_calls` safely.
///
/// Constructed by `LocalAgentService` at the start of each
/// `send_message` / `send_message_stream` call from the session's
/// overrides and config. Passed to `bridge_tool_registry` which
/// captures it in the bridge closures.
#[derive(Clone, Default)]
pub struct ToolCallContext {
    /// The session this tool call belongs to. Used by `todo` to
    /// stamp events and by `terminal` to route approval requests.
    pub session_id: String,

    /// Per-turn workspace root. `terminal` uses this as the default
    /// `current_dir` when the LLM doesn't pass an explicit `workdir`.
    pub workspace: Option<PathBuf>,

    /// Per-request approval hook for the terminal tool. When
    /// populated, `RequiresConfirmation` is surfaced as a
    /// `request_approval` event on the active WS channel.
    pub approval_ctx: TerminalApprovalContext,

    /// Per-session permission profile. `terminal` reads this to
    /// skip its internal `ApprovalManager` for pre-granted commands.
    pub permission_profile: PermissionProfile,

    /// Wire-format assistant message id (`${session_id}-${rowIdx}`).
    /// Used by `todo` to anchor inline cards to the correct message.
    pub message_id: Option<String>,

    /// Per-session stdout streaming callback for `code_execution`.
    /// Wrapped in `Arc<Mutex>` so `LocalAgentService` can set/clear it
    /// mid-stream (the context is captured at bridge time; the shared
    /// Arc handle allows dynamic updates without re-bridging).
    pub stdout_cb: Arc<Mutex<Option<std::sync::Arc<dyn for<'r> Fn(&'r str) + Send + Sync + 'static>>>>,

    /// Active `tool_call_id` for the currently running `code_execution`.
    /// Updated per tool-call by the stream loop so the stdout callback
    /// can emit `tool_stdout` events with the correct ID.
    pub active_call_id: Arc<Mutex<Option<String>>>,

    /// 通用执行进度回调：任何 opt-in [`ContextAware`] 的工具都可以在执行
    /// 过程中推一行文本，前端那张工具卡片会实时显示（`tool_stdout` 事件）。
    ///
    /// 与 [`Self::stdout_cb`] 的分工：stdout_cb 是 code_execution 的逐行
    /// 子进程输出，会持久化进 `execution_timeline`（重载会话要能回放）；
    /// 这里是**瞬时进度**（"正在下载引擎 48%"），只走 WS 不落库——
    /// 进度在回放里只是噪音。
    ///
    /// 事件里的 `tool` / `tool_call_id` 取 [`Self::active_tool`] 与
    /// [`Self::active_call_id`]，由调度方在 `tool_start` 时写入。
    pub progress_cb: Arc<Mutex<Option<Box<dyn Fn(&str) + Send + Sync>>>>,

    /// 当前正在执行的工具名（每次 `tool_start` 由调度方写入）。
    /// `progress_cb` 用它给事件打上正确的 `tool` 字段，前端才能把进度
    /// 落到对应的那张卡片上。
    pub active_tool: Arc<Mutex<Option<String>>>,

    /// 来源（§5.5 第 5 层）。由调度方根据连接属性填入：
    /// - `LocalAgentService`（桌面 WS）固定写 `Origin::Local`；
    /// - HTTP 路由在 `dispatch_with_context` 之前从 `Extension<Origin>`
    ///   读取后填入（`hermix-server/src/security.rs` 的 `request_guard`
    ///   已经把推导结果塞进 axum Extension，handler 桥接时取出来覆盖）；
    /// - 测试 / 头less 调用走 `Default::default()` ⇒ `Origin::Local`，
    ///   与"桌面直连"语义一致。
    /// 工具实现（terminal / 后续 code_execution 等）在
    /// `execute_with_context` 里读 `ctx.origin` 走来源感知审批，
    /// 而不是接收 `&Origin` 参数——这样新增 ContextAware 工具零样板。
    pub origin: Origin,

    /// 设备能力位集（§5.5 第 4 层细粒度授权，§3.3 落到工具关口消费点）。
    /// `request_guard` 从 `paired_devices.scopes` 解析出的
    /// `HashSet<Capability>`，HTTP 路径由 handler 从
    /// `Extension<Option<HashSet<Capability>>>` 取出后填入。
    /// 桌面 WS 路径（`LocalAgentService`）/ 未接入设备维度的调用方填
    /// `None`（无设备维度，纯 `Origin` 矩阵判定，行为等同旧版）。
    /// `terminal` 关口把它传给
    /// [`crate::approval::ApprovalManager::check_approval_for_origin_with_scopes`]，
    /// 让 `PairedDevice` 下未配 `DangerousTool` 的设备连 shell 都跑不了
    /// （§3.3 D5 拍板"高危工具需桌面确认"的细粒度落地）。
    pub device_scopes: Option<std::collections::HashSet<Capability>>,
}

impl ToolCallContext {
    /// Create a context with just a session id (all other fields
    /// default). Useful for tests and headless callers that don't
    /// need workspace/approval wiring.
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// ContextAware trait
// ---------------------------------------------------------------------------

/// Opt-in trait for tools that need per-session context at dispatch
/// time. Follows the same pattern as [`crate::SessionStamper`]:
/// `ToolEntry` stores an `Option<Arc<dyn ContextAware>>` alongside
/// the `Arc<dyn ToolHandler>`. Only tools that need session-scoped
/// state (terminal, todo, code_execution) implement this trait.
///
/// When `ToolRegistry::dispatch_with_context` is called:
/// - If the tool has a `ContextAware` impl, `execute_with_context`
///   is called with the `ToolCallContext`.
/// - Otherwise, the standard `ToolHandler::execute` is called
///   (context is ignored — backward compatible).
///
/// Both paths converge on the same truncation / error-wrapping logic
/// in `dispatch_with_context`.
#[async_trait]
pub trait ContextAware: Send + Sync {
    /// Execute the tool with the given parameters and per-session
    /// context. The context provides workspace, approval hooks,
    /// session id, and other per-session state that was previously
    /// read from process-global `Arc<Mutex<…>>` fields.
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError>;
}
