//! 工具派发前的审批决策矩阵。
//!
//! 从 `local_agent_service` 搬出的第七批（P1-13）。两块内容：
//! - `resource_guard`：按 `PermissionProfile` 判断"这一步是否越出授权目录"。
//!   只覆盖**破坏性**文件操作（删除/清理）——写文件与命令执行按 2026-09-26
//!   的裁决不走这条路（依据见函数内注释）。
//! - `decide_pre_tool_dispatch`：五层决策矩阵（hardline → Full 直通 →
//!   Tirith → Tool 自带 RiskLevel → 无审批设施时的 fallback）。
//!
//! 与 `hermix_tools::permission_guard` **不是重复实现**：那个管的是"工具
//! 不许改写权限治理文件"（防自我提权），这里管的是"用户给这一轮的授权范围
//! 到哪里"。两层都在，缺一层就有绕过面。

use std::sync::Arc;

use serde_json::Value;

use hermix_core::traits::{PermissionLevel, RiskLevel};
use hermix_core::{
    ApprovalOutcome, AutomationMeta, Message, MessageRole, PermissionProfile, ToolCall,
};
use hermix_tools::{TirithAction, TirithSecurity, ToolRegistry};

use crate::agent_helpers::{resolve_approval_fallback, ApprovalFallback};
use crate::agent_loop::ToolDispatchDecision;
use crate::hardline;

use super::LocalAgentService;

impl LocalAgentService {
    /// P6 (, direction 1): returns `Some(<resource description>)`
    /// when `tc` is a resource-bearing tool whose target is outside
    /// `profile`'s granted scope, else `None`.
    /// Only **mutating** `file`-family tools (by `path` argument) and
    /// `terminal` / `code_execution` (by `command` argument) are gated —
    /// read-only tools (`read_file`, `read_image`, `search_files`,
    /// `list_directory`) are **exempt** because they never modify state.
    /// Requiring approval for read/search/list operations is pure friction
    /// with zero safety benefit: the agent runs on the user's own machine
    /// and reading files is its core capability. Everything else falls
    /// through to the normal risk matrix unchanged. The description string
    /// is surfaced verbatim to the desktop UI's approval card so the user
    /// knows exactly what they're granting.
    pub(super) fn resource_guard(
        tc: &ToolCall,
        profile: &PermissionProfile,
        params: &Value,
    ) -> Option<String> {
        let name = tc.function.name.as_str();

        // Read-only file tools never modify state — skip the path guard
        // entirely.  These operations cannot cause damage, so gating them
        // behind an approval prompt adds friction without safety value.
        const READONLY_FILE_TOOLS: &[&str] = &[
            "read_file",
            "read_image",
            "search_files",
            "list_directory",
        ];
        if READONLY_FILE_TOOLS.contains(&name) {
            return None;
        }

        // 文件内容写入（write_file / patch / move_file）不做资源门拦截。
        // 2026-09-26 定策："写文件不用审批，只有删除等高危才审批"。
        // 本机自主机上的文件内容写/改/移动是低风险操作，弹「未授权路径」卡
        // 纯属摩擦；真正的高危删除（delete_file / disk_cleanup）仍走资源门
        // （见下方 DESTRUCTIVE_FILE_TOOLS）。
        //
        // 命令执行（terminal / code_execution）不做 command-grant 资源门拦截。
        // 依据同上：「命令执行（设置环境变量、命令编码等）不用审批」。
        // 破坏性命令（rm/del/format/diskpart/mkfs …）由下游 DENIED/CONFIRM
        // 审批层 + hardline blocklist 独立兜底，不依赖这里的 grant 前缀匹配。
        // 旧实现在这里用「未授权命令」卡拦截了所有首 token 不在 grant 列表的
        // 命令（$env: 环境变量、export、编码命令等），与"命令执行不审批"策略直接冲突。
        if name == "terminal" || name == "code_execution" {
            return None;
        }

        // 破坏性文件操作 — 删除类是明确需要审批的高危项。
        // delete_file / disk_cleanup 越出授权目录才弹资源卡。
        const DESTRUCTIVE_FILE_TOOLS: &[&str] = &["delete_file", "disk_cleanup"];
        if DESTRUCTIVE_FILE_TOOLS.contains(&name) {
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                if !profile.allows_path(path) {
                    return Some(format!("未授权删除路径: {path}"));
                }
            }
        }
        None
    }

    /// Internal: pre-dispatch decision for a tool call.
    /// Extracted from the `on_pre_tool_dispatch` closure in
    /// [`Self::send_message`] for testability — the closure was a
    /// 140-line inline lambda that couldn't be exercised by unit
    /// tests. The five-layer decision matrix is:
    /// 0. **Hardline blocklist** (L0, the floor) — catastrophic
    /// patterns (`rm -rf /`, fork bomb, `mkfs.*` on block
    /// device, `dd` to block device, `curl|sh`) are denied
    /// **regardless of `PermissionLevel`**. This is the agent's
    /// last-resort defense — there is no override path. See
    /// [`crate::hardline`] and the upstream
    /// `tools/approval.py::UNRECOVERABLE_BLOCKLIST` it mirrors.
    /// 1. **`PermissionLevel::Full`** — user has explicitly opted
    /// out of the safety net; allow everything (Tirith is also
    /// bypassed because the user has taken responsibility for
    /// whatever the model invents).
    /// 2. **Tirith pre-check** — structural policy
    /// (`\\?\`-prefix block, system-dir reads, etc.). A
    /// `Block` short-circuits to `Deny` with a
    /// machine-readable reason so the LLM can reformulate; a
    /// `RequireApproval` re-routes through the standard
    /// approval path with `risk: "caution"` regardless of
    /// the tool's registered risk level; `Warn` and `Allow`
    /// fall through.
    /// 3. **Tool `RiskLevel`** — `Safe` always allows;
    /// `Caution` always allows (the tool's own internal
    /// guards run); `Dangerous` requires explicit user
    /// approval in `Default` mode.
    /// 4. **No approval infrastructure wired** — the caller must
    ///    declare its [`ApprovalFallback`] for this transport.
    ///    Network-reachable callers pass [`ApprovalFallback::Deny`]
    ///    so a Dangerous tool can never execute unprompted; local
    #[allow(clippy::too_many_arguments)]
    pub(super) fn decide_pre_tool_dispatch(
        tc: &ToolCall,
        permission: PermissionLevel,
        tool_registry: &ToolRegistry,
        tirith: Option<&TirithSecurity>,
        approval_requester: Option<
            &Arc<
                dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<ApprovalOutcome>
                    + Send
                    + Sync,
            >,
        >,
        on_event: Option<&Arc<dyn Fn(serde_json::Value) + Send + Sync>>,
        pre_approval_checker: Option<
            &Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>,
        >,
        session_id: &str,
        approval_fallback: ApprovalFallback,
    ) -> ToolDispatchDecision {
        // L0: hardline blocklist — the floor beneath `Full`.
        // Run *before* the `Full` short-circuit: even a user who has
        // taken full responsibility cannot authorize a pattern on
        // this list. There is no override path.
        let raw_params: Value = serde_json::from_str(&tc.function.arguments)
            .unwrap_or(Value::Object(Default::default()));
        if let Some(hit) = hardline::check_hardline_blocklist(&tc.function.name, &raw_params) {
            tracing::error!(
                tool = %tc.function.name,
                rule = %hit.name,
                reason = %hit.reason,
                "hardline blocklist denied tool call (L0 floor)"
            );
            return ToolDispatchDecision::Deny {
                reason: format!("Hardline blocklist ({}): {}", hit.name, hit.reason),
            };
        }
        // Full permission → always allow
        if permission == PermissionLevel::Full {
            return ToolDispatchDecision::Allow;
        }
        // Tirith pre-check
        if let Some(engine) = tirith {
            let params = &raw_params;
            let tirith_result = engine.check_tool_call(&tc.function.name, params);
            match tirith_result.action {
                TirithAction::Block => {
                    tracing::warn!(
                        tool = %tc.function.name,
                        reason = %tirith_result.reason,
                        rule = ?tirith_result.matched_rule,
                        "Tirith blocked tool call before dispatch"
                    );
                    return ToolDispatchDecision::Deny {
                        reason: format!("Tirith policy: {}", tirith_result.reason),
                    };
                }
                TirithAction::Warn => {
                    tracing::warn!(
                        tool = %tc.function.name,
                        reason = %tirith_result.reason,
                        "Tirith warning on tool call"
                    );
                    // fall through to the risk-level gate
                }
                TirithAction::RequireApproval => {
                    tracing::info!(
                        tool = %tc.function.name,
                        reason = %tirith_result.reason,
                        "Tirith promoted tool call to require approval"
                    );
                    // Pre-approval shortcut: if the user previously
                    // picked "Always Allow" or "Allow for this session"
                    // for this exact tool/args shape, skip the WS
                    // round-trip and dispatch immediately. The
                    // checker is server-supplied (it knows about the
                    // persistent `UserAllowlist` + the in-memory
                    // per-session bypass set), so the agent crate
                    // stays decoupled from the allowlist module.
                    if let Some(checker) = pre_approval_checker {
                        if checker(&tc.function.name, params) {
                            tracing::info!(
                                tool = %tc.function.name,
                                "pre-approval checker hit: skipping WS prompt"
                            );
                            return ToolDispatchDecision::Allow;
                        }
                    }
// Schema-required gate: never raise an approval for a call that would
                    // die in the handler's param validation (see
                    // `tool_dispatch::schema_required_violation`).
                    if let Some(err) = crate::tool_dispatch::schema_required_violation(tool_registry, tc) {
                        return ToolDispatchDecision::Deny { reason: err };
                    }
                    let (Some(requester), Some(on_event_fn)) = (approval_requester, on_event)
                    else {
                        return resolve_approval_fallback(approval_fallback, &tc.function.name);
                    };
                    let request_id = tc.id.clone();
                    let tool_name = tc.function.name.clone();
                    // Before/after diff for the approval card (backend-owned;
                    // `None` for call shapes we can't diff — UI keeps its
                    // plain-description fallback).
                    let diff = crate::approval_diff::approval_diff(&tool_name, params);
                    let rx = requester(session_id.to_string(), request_id.clone());
                    on_event_fn(serde_json::json!({
                        "event": "request_approval",
                        "request_id": request_id,
                        "tool": tool_name,
                        "arguments": params,
                        "risk": "caution",
                        "diff": diff,
                    }));
                    return ToolDispatchDecision::AwaitApproval(rx);
                }
                TirithAction::Allow => {
                    // fall through to the risk-level gate
                }
            }
        }
        // Risk-level gate
        let risk = tool_registry
            .risk_level(&tc.function.name)
            .unwrap_or(RiskLevel::Caution);
        if risk == RiskLevel::Safe {
            return ToolDispatchDecision::Allow;
        }
        if risk != RiskLevel::Dangerous {
            return ToolDispatchDecision::Allow;
        }
        // Dangerous tool in Default mode → ask the user
        //
        // Scratch-scope exemption: cleaning hermix's OWN throwaway tree
        // (`%TEMP%\hermix-scratch`, defined in
        // `hermix_tools::tools::storage_clean::scratch_root`) is not a
        // user-facing destructive act — those files were created by the
        // agent in this same task and are disposable by construction.
        // Without this exemption the agent has to ask permission to tidy
        // up after itself, and the prompt's "must clean temp files" rule
        // becomes unusable: a user who once denies a delete teaches the
        // model to ask forever, and the workspace fills with debris.
        //
        // The exemption is deliberately narrow:
        //   - it applies only to `storage_clean`, not to `terminal`
        //     (a shell can delete anything regardless of this check);
        //   - `is_scratch_scoped` requires EVERY path to canonicalize
        //     inside the scratch root, so a mixed list (one scratch file
        //     + one Downloads item) still gets the full Dangerous
        //     treatment;
        //   - it downgrades the *prompt*, not the tool: `safety_check`
        //     inside `storage_clean` still runs in full and can still
        //     refuse any individual path.
        if tc.function.name == "storage_clean" {
            let paths: Vec<String> = raw_params
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if hermix_tools::tools::storage_clean::is_scratch_scoped(&paths) {
                tracing::info!(
                    tool = %tc.function.name,
                    count = paths.len(),
                    "storage_clean scoped to hermix scratch root; skipping approval prompt"
                );
                return ToolDispatchDecision::Allow;
            }
        }
        // Pre-approval shortcut: if the user previously picked
        // "Always Allow" or "Allow for this session" for the same
        // tool/args shape, skip the WS round-trip. See the
        // matching branch in Tirith's `RequireApproval` arm for the
        // full design rationale.
        if let Some(checker) = pre_approval_checker {
            if checker(&tc.function.name, &raw_params) {
                tracing::info!(
                    tool = %tc.function.name,
                    "pre-approval checker hit: skipping WS prompt for Dangerous tool"
                );
                return ToolDispatchDecision::Allow;
            }
        }
// Schema-required gate: never raise an approval for a call that would
        // die in the handler's param validation (see
        // `tool_dispatch::schema_required_violation`).
        if let Some(err) = crate::tool_dispatch::schema_required_violation(tool_registry, tc) {
                        return ToolDispatchDecision::Deny { reason: err };
                    }
        let (Some(requester), Some(on_event_fn)) = (approval_requester, on_event) else {
            return resolve_approval_fallback(approval_fallback, &tc.function.name);
        };
        let request_id = tc.id.clone();
        let tool_name = tc.function.name.clone();
        // Before/after diff for the approval card (backend-owned).
        let diff = crate::approval_diff::approval_diff(&tool_name, &raw_params);
        let rx = requester(session_id.to_string(), request_id.clone());
        on_event_fn(serde_json::json!({
            "event": "request_approval",
            "request_id": request_id,
            "tool": tool_name,
            "arguments": raw_params,
            "risk": risk.as_str(),
            "diff": diff,
        }));
        ToolDispatchDecision::AwaitApproval(rx)
    }
}

/// 把一轮「外部触发」的消息重新标回 [`MessageRole::Automation`]。
///
/// 提示词按**内容特征**定位（带着本次自动化的元信息 + 角色是 User），
/// **不用固定下标**：agent 循环可能在本轮消息前面插入内容（系统消息、
/// 瞬态提示的增删都会让下标错位），错位的后果是**提示词以 user 身份落库**
/// → 下一轮 `load_session` 把它当成「用户说过的话」喂给模型（这正是
/// Automation 角色要避免的），前端也会多渲染出一个用户气泡。
///
/// 2026-10-21 实测：定时任务刚修好会触发之后，这条路径立刻暴露出
/// 「每轮落库多一条 user 角色的提示词」。
///
/// 本轮的回复（提示词之后的 assistant 行）一并改标；历史对话不动 ——
/// 它们来自 `load_session`，是真人参与的轮次。
pub(super) fn relabel_automation_turn(messages: &mut [Message], meta: &AutomationMeta) {
    let Some(idx) = messages.iter().position(|m| {
        m.role == MessageRole::User && m.automation_meta.as_ref() == Some(meta)
    }) else {
        tracing::warn!(
            "automation turn: prompt not found by automation marker — \
             it may persist as a user message and pollute the next turn's context"
        );
        return;
    };
    messages[idx].role = MessageRole::Automation;
    for msg in messages.iter_mut().skip(idx + 1) {
        if msg.role == MessageRole::Assistant {
            msg.role = MessageRole::Automation;
            msg.automation_meta = Some(meta.clone());
        }
    }
}
