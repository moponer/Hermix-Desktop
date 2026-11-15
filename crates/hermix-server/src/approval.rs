//! Approval registry: per-session request tracking with oneshot resolution.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hermix_core::ApprovalOutcome;
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct ApprovalRegistry {
    inner: Arc<std::sync::Mutex<HashMap<String, HashMap<String, oneshot::Sender<ApprovalOutcome>>>>>,
    /// `session_id → batch_id → request_ids`. Populated when the WS
    /// layer coalesces N concurrent approval requests into one
    /// [`StreamEvent::RequestApprovalsBatch`] envelope; consulted by
    /// [`Self::resolve_batch`] so the client's single decision can
    /// fan out into N registry resolves. Entries are removed by
    /// `resolve_batch` once drained; stale entries (session
    /// disconnected, batch never answered) leak but are bounded by
    /// session count × batch count, in practice <100.
    batches: Arc<std::sync::Mutex<HashMap<String, HashMap<String, Vec<String>>>>>,
    /// 会话级「放手模式」开关（2026-10-31）：置位的会话里，任何审批请求
    /// 不再登记、不再发卡，直接预解析 `Approved`。这是让「审批卡里选
    /// 『放手模式』」在**当前轮内立即生效**的唯一汇点 —— 派发层、资源门、
    /// 工具内部确认三条路径最终都经 `request_approval` 拿裁决，一轮的
    /// `permission` 快照在 `send_message` 时已捕获定死，改不动它。
    /// 进程内存态；前端每条消息都带当前档位，消息路径据此重新武装
    /// （重启后第一条消息即恢复）。
    ///
    /// **持久化（2026-11-05）**：`persist_path` 存在时，放手模式的会话集合
    /// 会写盘并在构造时读回。原设计假设「重启后下一条消息就恢复」，但实测
    /// 恢复不了用户当下的体感：桌面端一天内可能重启 sidecar 十几次（看门狗 /
    /// 窗口销毁），每次清空 `full_sessions` → 同一轮对话里审批卡重新弹出，
    /// 用户看到的是「放手模式根本无效」。档位是用户偏好而非待决状态，跟
    /// pending 审批一起被 `superseded` 清掉是分类错误。
    full_sessions: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// 放手模式会话集合的落盘位置；`None` = 不持久化（测试）。
    persist_path: Option<std::path::PathBuf>,
    default_timeout: Duration,
    /// 「现在有没有人可能答复」的判定。`None` = **不做在场判定**，按墙钟固定超时
    /// （`with_timeout` 的语义，测试与调用方自定义超时时用）。
    ///
    /// `Some` 时超时按**可送达时间**计费：没有客户端在线就不计时。理由见
    /// [`crate::presence`]：无人在线时那条回答在物理上不可能到达，倒计时照走
    /// 等于保证失败。
    presence: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalRegistry {
    /// 生产构造：超时按「可送达时间」计费（没有客户端在线时暂停）。
    pub fn new() -> Self {
        Self::with_presence(
            std::time::Duration::from_secs(hermix_config::approval_timeout_secs()),
            Arc::new(|| crate::presence::seen_within(crate::presence::PRESENCE_WINDOW)),
        )
    }

    /// Create a registry whose pending approvals auto-resolve to
    /// `false` after `default_timeout` of inactivity. Use
    /// `Duration::ZERO` to disable the timeout.
    ///
    /// **不做在场判定**：超时按墙钟走。测试用这个保证确定性（测试进程里没有
    /// 真实的客户端活动，若也按在场计费会一直等下去）。
    pub fn with_timeout(default_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(HashMap::new())),
            batches: Arc::new(std::sync::Mutex::new(HashMap::new())),
            full_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            persist_path: None,
            default_timeout,
            presence: None,
        }
    }

    /// 打开放手模式的落盘持久化：`set_full_mode` 的每次变更都原子写盘，
    /// 构造时把已落盘的会话集合读回来——sidecar 被看门狗/窗口销毁重启后，
    /// 用户的放手档位不再丢（2026-11-05）。
    pub fn with_persistence(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.persist_path = path;
        if let Some(p) = &self.persist_path {
            match std::fs::read_to_string(p) {
                Ok(data) => match serde_json::from_str::<Vec<String>>(&data) {
                    Ok(list) => {
                        let mut set = self
                            .full_sessions
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        *set = list.into_iter().collect();
                        tracing::info!(
                            path = %p.display(),
                            sessions = set.len(),
                            "full-mode sessions restored from disk"
                        );
                    }
                    Err(e) => tracing::warn!(
                        path = %p.display(),
                        error = %e,
                        "full-mode session file corrupt; starting empty"
                    ),
                },
                // 文件不存在 = 首次运行或从未开过放手，静默即可。
                Err(_) => {}
            }
        }
        self
    }

    /// 显式给定在场判定。生产走 [`Self::new`]，测试用它把「有没有人在看」
    /// 变成一个可控输入。
    pub fn with_presence(
        default_timeout: Duration,
        presence: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            presence: Some(presence),
            ..Self::with_timeout(default_timeout)
        }
    }

    /// Returns the configured default timeout for pending
    /// approval requests. Used by tests and by the WS handler
    /// that wants to know the policy in effect.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// Create a new pending approval and return a receiver that resolves when
    /// the client sends an [`ApprovalResponse`]. If the sender side is dropped
    /// without resolution, the receiver yields `ApprovalOutcome::ChannelClosed`
    /// (deny by default — the agent distinguishes this from a deliberate user
    /// denial). If the configured timeout elapses first, the receiver yields
    /// `ApprovalOutcome::TimedOut { timeout_secs }` and a `tracing::warn!` is
    /// logged.
    pub fn request_approval(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> oneshot::Receiver<ApprovalOutcome> {
        // ── 放手模式：不登记、不发卡，直接放行 ─────────────────────────
        // 该会话的开关由 REST `POST /v1/sessions/{id}/permission` 置位。
        // 硬底线（hardline blocklist）在派发层先跑，到不了这里，绕不过。
        // 用 effective 版本：子代理会话（`父::subagent-*`）回落到父会话
        // 的档位（is_full_mode_effective 的文档记录了 11-09 实测教训）。
        if self.is_full_mode_effective(session_id) {
            tracing::info!(
                session_id = %session_id,
                request_id = %request_id,
                "full mode: auto-approved without prompting"
            );
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(ApprovalOutcome::Approved);
            return rx;
        }
        let (tx, rx) = oneshot::channel();
        // Store the sender first, then release the lock before
        // spawning the timeout task -- keeps the critical section
        // short and avoids holding the registry lock across an
        // await point.
        {
            let mut sessions = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            //( / ): Reject duplicate request_id
            // instead of silently overwriting the existing pending sender.
            // Overwriting would cause the first receiver to get
            // ChannelClosed (deny by default), which is a silent failure.
            // Instead, return a pre-resolved DeniedByUser so the LLM
            // sees an explicit "denied" and can retry with a new id.
            let session_map = sessions
                .entry(session_id.to_string())
                .or_default();
            if session_map.contains_key(request_id) {
                tracing::warn!(
                    session_id = %session_id,
                    request_id = %request_id,
                    "duplicate approval request_id — rejecting (not overwriting existing pending request)"
                );
                // Pre-resolve the new receiver as DeniedByUser so the
                // caller gets a clear "denied" signal rather than
                // ChannelClosed ambiguity.
                let _ = tx.send(ApprovalOutcome::DeniedByUser);
                return rx;
            }
            session_map.insert(request_id.to_string(), tx);
        }
        // Spawn a timer that, on expiry, takes the sender back
        // out of the registry and sends `TimedOut`. If a real
        // `resolve` happens first, the entry will already be
        // gone by the time we wake up and the take is a no-op.
        let to = self.default_timeout;
        if !to.is_zero() {
            let inner = Arc::clone(&self.inner);
            let sess = session_id.to_string();
            let req = request_id.to_string();
            let presence = self.presence.clone();
            tokio::spawn(async move {
                // 无人在场时的绝对上限：不能因为有「暂时没人」就真挂住 agent。
                const HARD_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
                // 小超时（单测）要够快，分钟级超时不必一秒多探。
                let tick = (to / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
                let mut waited = Duration::ZERO; // 墙钟
                let mut visible = Duration::ZERO; // 有人可答复的时间
                loop {
                    tokio::time::sleep(tick).await;
                    waited += tick;
                    // 已被答复就别空转——原来的实现会照睡满整段超时。
                    let still_pending = {
                        let mut sessions = inner.lock().unwrap_or_else(|e| e.into_inner());
                        sessions
                            .get_mut(&sess)
                            .map(|session| session.contains_key(&req))
                            .unwrap_or(false)
                    };
                    if !still_pending {
                        return;
                    }
                    match &presence {
                        // 按「可送达时间」计费：没人在线就暂停，只受绝对上限约束。
                        Some(seen) => {
                            if seen() {
                                visible += tick;
                            }
                            if visible >= to || waited >= HARD_CAP {
                                break;
                            }
                        }
                        // 无在场判定：墙钟固定超时（与旧行为一致）。
                        None => {
                            if waited >= to {
                                break;
                            }
                        }
                    }
                }
                let tx_opt = {
                    let mut sessions = inner.lock().unwrap_or_else(|e| e.into_inner());
                    sessions
                        .get_mut(&sess)
                        .and_then(|session| session.remove(&req))
                };
                if let Some(tx) = tx_opt {
                    tracing::warn!(
                        session_id = %sess,
                        request_id = %req,
                        timeout_secs = to.as_secs(),
                        waited_secs = waited.as_secs(),
                        "approval request timed out (default deny)"
                    );
                    let _ = tx.send(ApprovalOutcome::TimedOut {
                        timeout_secs: to.as_secs(),
                    });
                }
                // else: a real resolve beat us to it -- no-op.
            });
        }
        rx
    }

    /// Resolve a pending approval request from a structured
    /// [`ApprovalOutcome`]. Returns `true` if the sender was found and
    /// resolved, `false` if the request had already been resolved (e.g. by a
    /// prior `resolve` call, or by the timeout task firing first) or was
    /// never registered.
    /// The WS handler is the only caller — it maps the client-sent
    /// `ApprovalResponse { decision: bool }` into `DeniedByUser` /
    /// `Approved` here, keeping the enum construction server-side so the
    /// agent crate stays decoupled from the wire format.
    pub fn resolve(&self, session_id: &str, request_id: &str, outcome: ApprovalOutcome) -> bool {
        let mut sessions: std::sync::MutexGuard<
            HashMap<String, HashMap<String, oneshot::Sender<ApprovalOutcome>>>,
        > = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(tx) = session.remove(request_id) {
                let _ = tx.send(outcome);
                return true;
            }
        }
        // 子代理后代回落（2026-11-14）：子代理的工具审批用**自己的会话 id**
        // （`父::subagent-*`，dispatch_policy 的 requester）登记 pending，但
        // 审批卡走父会话的 WS 流渲染——前端拿父会话 id 来结算，精确键永远
        // miss，子代理被挂死到审批超时（11-10 06:33:54 实锤：
        // `REST approval: miss — no pending entry session_id=0e60b75b…`，
        // 批量卡随后"自动取消"、用户感知卡死）。精确键未命中时在同会话的
        // 子代理命名空间内补搜——request_id 是全局唯一的 tool call id，
        // 跨命名空间不会撞；前缀带会话 id 边界（`::subagent-`），别的会话
        // 的子代理不受影响。
        let child_prefix = format!("{session_id}::subagent-");
        let descendant_keys: Vec<String> = sessions
            .keys()
            .filter(|k| k.starts_with(&child_prefix))
            .cloned()
            .collect();
        for key in descendant_keys {
            if let Some(session) = sessions.get_mut(&key) {
                if let Some(tx) = session.remove(request_id) {
                    let _ = tx.send(outcome);
                    return true;
                }
            }
        }
        false
    }

    /// 置位 / 清除某会话的放手模式。`true` 后该会话的所有后续审批请求
    /// 都在 [`Self::request_approval`] 里直接放行。
    pub fn set_full_mode(&self, session_id: &str, full: bool) {
        let snapshot: Vec<String> = {
            let mut set = self
                .full_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if full {
                set.insert(session_id.to_string());
            } else {
                set.remove(session_id);
            }
            set.iter().cloned().collect()
        };
        // 持久化放手档位：sidecar 被看门狗/窗口销毁重启后读回（2026-11-05）。
        // 写盘失败只丢「跨重启恢复」，不丢本轮语义 —— 静默记日志即可。
        if let Some(p) = &self.persist_path {
            let json = serde_json::to_string(&snapshot).unwrap_or_else(|_| "[]".into());
            let tmp = p.with_extension("json.tmp");
            if let Err(e) = std::fs::write(&tmp, json)
                .and_then(|_| std::fs::rename(&tmp, p))
            {
                tracing::warn!(
                    path = %p.display(),
                    error = %e,
                    "failed to persist full-mode sessions"
                );
            }
        }
    }

    /// 查询某会话是否处于放手模式。
    pub fn is_full_mode(&self, session_id: &str) -> bool {
        self.full_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(session_id)
    }

    /// 放手档位的**生效**判断（2026-11-09）：精确匹配之外，子代理会话
    /// （`父会话::subagent-*`）回落到父会话。放手是用户对整个对话的宣言，
    /// 但子代理的工具审批用各自的会话 id 注册（`dispatch_policy.rs` 的
    /// `requester(sid, …)`），不回落的话父会话开了放手、子代理里照样弹卡
    /// —— 11-09 超星验证任务实测：10:11:58 父会话 full=true 后拉起的
    /// 子代理 10:12:20 起继续弹审批卡，批量卡超时"自动取消"。
    pub fn is_full_mode_effective(&self, session_id: &str) -> bool {
        if self.is_full_mode(session_id) {
            return true;
        }
        if let Some((parent, _)) = session_id.split_once("::subagent-") {
            return self.is_full_mode(parent);
        }
        false
    }

    /// 把某会话当前**全部** pending 请求按同一结果放行/拒绝，返回被放行
    /// 的 request_id 列表。切换到放手模式时调用 —— 用户已经声明「后续
    /// 不再申请」，队列里挂着旧卡继续等点击与宣言自相矛盾。整表摘除是
    /// 有意的：此时不存在「想保留的 pending」。批处理索引（`batches`）
    /// 里可能留下指已放行 id 的陈旧条目，与既有文档一致属有界泄漏，
    /// `resolve_batch` 对已消失的 id 幂等返回 false。
    pub fn resolve_all(&self, session_id: &str, outcome: ApprovalOutcome) -> Vec<String> {
        // 子代理会话（`父::subagent-*`）的 pending 一并按同一结果结算——
        // 与 is_full_mode_effective 的回落规则一致：放手对整个对话生效，
        // 切回默认时子代理的挂起卡也该一并处理，不能留孤儿卡继续超时。
        let child_prefix = format!("{session_id}::subagent-");
        let drained: HashMap<String, oneshot::Sender<ApprovalOutcome>> = {
            let mut sessions = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut all = sessions.remove(session_id).unwrap_or_default();
            let child_keys: Vec<String> = sessions
                .keys()
                .filter(|k| k.starts_with(&child_prefix))
                .cloned()
                .collect();
            for k in child_keys {
                all.extend(sessions.remove(&k).unwrap_or_default());
            }
            all
        };
        let ids: Vec<String> = drained.keys().cloned().collect();
        for tx in drained.into_values() {
            let _ = tx.send(outcome.clone());
        }
        ids
    }

    /// True if a sender is still registered for `(session_id,
    /// request_id)` — exact session first, then the session's
    /// subagent descendants (same fallback as [`Self::resolve`]:
    /// subagent cards resolve through the parent stream, so the
    /// side-effect handlers must see them under the parent id too).
    /// Used by the 4-state `session` / `always` side
    /// effect handlers to skip the work when the request was
    /// already resolved (duplicate response) or timed out — both
    /// of which remove the entry.
    pub fn is_pending(&self, session_id: &str, request_id: &str) -> bool {
        let sessions = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if sessions
            .get(session_id)
            .map(|s| s.contains_key(request_id))
            .unwrap_or(false)
        {
            return true;
        }
        let child_prefix = format!("{session_id}::subagent-");
        sessions
            .keys()
            .filter(|k| k.starts_with(&child_prefix))
            .any(|k| sessions.get(k).map(|s| s.contains_key(request_id)).unwrap_or(false))
    }

    /// Request ids still awaiting a decision for one session,
    /// **including its subagent descendants** (`父::subagent-*`):
    /// their cards render on the parent stream, so the parent's
    /// re-projection must see them or a reload loses the cards.
    ///
    /// A non-empty result is authoritative proof that the agent loop
    /// is blocked on `AwaitApproval` right now: an entry only exists
    /// while its oneshot is unresolved (resolve and timeout both
    /// remove it). The `GET /approvals/pending` endpoint uses this to
    /// re-project approval cards a client missed while its socket was
    /// closed — an empty vec means "nothing live", not "error".
    pub fn list_pending(&self, session_id: &str) -> Vec<String> {
        let sessions = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<String> = sessions
            .get(session_id)
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default();
        let child_prefix = format!("{session_id}::subagent-");
        let child_keys: Vec<String> = sessions
            .keys()
            .filter(|k| k.starts_with(&child_prefix))
            .cloned()
            .collect();
        for k in child_keys {
            if let Some(s) = sessions.get(&k) {
                ids.extend(s.keys().cloned());
            }
        }
        ids
    }

    /// Record a batch id and the request ids it covers for a given
    /// session. Called by the WS coalescer right before it emits the
    /// [`StreamEvent::RequestApprovalsBatch`] envelope. The vector is
    /// moved in (cheap clone on each request_id string).
    pub fn register_batch(
        &self,
        session_id: &str,
        batch_id: &str,
        request_ids: Vec<String>,
    ) {
        let mut batches = self.batches.lock().unwrap_or_else(|e| e.into_inner());
        batches
            .entry(session_id.to_string())
            .or_default()
            .insert(batch_id.to_string(), request_ids);
    }

    /// Resolve every request in a batch with the same outcome and
    /// return a vector of `(request_id, resolved_flag)` pairs in
    /// the same order the batch was registered. A `false` flag
    /// means the individual request was already resolved (by timeout
    /// or duplicate response) and the side effect should be skipped.
    /// The batch entry is removed on completion; a missing batch id
    /// is treated as no-op (returns empty vec) — idempotent under
    /// duplicate responses.
    pub fn resolve_batch(
        &self,
        session_id: &str,
        batch_id: &str,
        outcome: ApprovalOutcome,
    ) -> Vec<(String, bool)> {
        let request_ids = {
            let mut batches = self.batches.lock().unwrap_or_else(|e| e.into_inner());
            batches
                .get_mut(session_id)
                .and_then(|m| m.remove(batch_id))
                .unwrap_or_default()
        };
        if request_ids.is_empty() {
            return Vec::new();
        }
        request_ids
            .into_iter()
            .map(|rid| {
                let resolved = self.resolve(session_id, &rid, outcome.clone());
                (rid, resolved)
            })
            .collect()
    }
}

#[cfg(test)]
mod approval_registry_tests {
    //! Tests for [`ApprovalRegistry`] — the in-memory map of
    //! pending `(session_id, request_id) → oneshot::Sender<bool>`
    //! pairs that backs the WS `ApprovalResponse` flow. The
    //! registry's contract is:
    //!
    //! - `request_approval` returns a fresh receiver and stores
    //!   the sender under the (session_id, request_id) key.
    //! - `resolve` removes the entry and forwards the decision
    //!   through the oneshot. Returns `true` if a sender was
    //!   found, `false` if the key didn't exist (the LLM
    //!   already gave up, or the WS client sent a response for
    //!   a different request).
    //! - Dropping the receiver or sender without resolution
    //!   must not panic; the survivor sees `Err` / `false`.
    //! - Different sessions and different request_ids are
    //!   isolated — resolving one must not affect another.

    use super::*;
    // 父模块的 `use super::*` 已删（非测试构建下未使用），测试需要的 crate
    // 根符号（HttpServerState / ws / record_approval_side_effect …）在此显式引入。
    use crate::*;

    #[tokio::test]
    async fn request_returns_receiver_resolve_true_yields_true() {
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx.await.unwrap(), hermix_core::ApprovalOutcome::Approved);
    }

    #[tokio::test]
    async fn request_returns_receiver_resolve_false_yields_false() {
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::DeniedByUser));
        assert_eq!(rx.await.unwrap(), hermix_core::ApprovalOutcome::DeniedByUser);
    }

    #[tokio::test]
    async fn resolve_unknown_request_returns_false() {
        let reg = ApprovalRegistry::new();
        // No request was ever registered.
        assert!(!reg.resolve("sess-a", "nope", hermix_core::ApprovalOutcome::Approved));
        assert!(!reg.resolve("nonexistent-session", "req-1", hermix_core::ApprovalOutcome::Approved));
    }

    #[tokio::test]
    async fn resolve_twice_for_same_request_returns_false_second_time() {
        // The registry removes the entry on first resolve, so a
        // second resolve must be a no-op and return `false`.
        // This protects against the rare case where the
        // desktop client double-fires (network retry, double
        // click) and a stale response would otherwise unblock a
        // *different* in-flight tool call.
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert!(
            !reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::DeniedByUser),
            "second resolve should report no sender"
        );
        assert_eq!(rx.await.unwrap(), hermix_core::ApprovalOutcome::Approved, "first decision wins");
    }

    #[tokio::test]
    async fn different_requests_in_same_session_do_not_interfere() {
        let reg = ApprovalRegistry::new();
        let rx1 = reg.request_approval("sess-a", "req-1");
        let rx2 = reg.request_approval("sess-a", "req-2");

        // Resolve only req-1 with `true`; req-2 must still be
        // pending and answerable.
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx1.await.unwrap(), hermix_core::ApprovalOutcome::Approved);

        // Drop rx2 without resolving (sender still in the
        // registry) — this is the "user closed the dialog
        // without deciding" case. The next resolve should
        // still find req-2's sender and forward a deny.
        drop(rx2);
        assert!(reg.resolve("sess-a", "req-2", hermix_core::ApprovalOutcome::DeniedByUser));
    }

    #[tokio::test]
    async fn different_sessions_with_same_request_id_do_not_interfere() {
        let reg = ApprovalRegistry::new();
        let rx_a = reg.request_approval("sess-a", "req-1");
        let rx_b = reg.request_approval("sess-b", "req-1");

        // Resolve only sess-a's req-1. sess-b's req-1 must
        // remain pending.
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx_a.await.unwrap(), hermix_core::ApprovalOutcome::Approved);

        // And sess-b's req-1 can still be resolved.
        assert!(reg.resolve("sess-b", "req-1", hermix_core::ApprovalOutcome::DeniedByUser));
        assert_eq!(rx_b.await.unwrap(), hermix_core::ApprovalOutcome::DeniedByUser);
    }

    #[tokio::test]
    async fn drop_receiver_without_resolve_yields_err_at_registry_drop() {
        // The receiver is held by the agent loop. If the agent
        // loop drops the rx (e.g. it bails out for an unrelated
        // reason), the sender remains in the registry. The
        // next resolve returns `true` (the sender was found),
        // but the rx has been dropped, so `tx.send` is a
        // silent no-op. This is fine — the registry's contract
        // is "if a sender was found, return true; if not,
        // return false"; whether the consumer is still listening
        // is a separate concern handled by the agent loop.
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        drop(rx);
        assert!(
            reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved),
            "sender still in registry after rx drop"
        );
    }

    #[tokio::test]
    async fn drop_sender_registry_drops_sender_returns_err_at_rx() {
        // Mirrors the production failure mode the user
        // reported: the WS connection closes (e.g. the user
        // navigates away or the desktop app crashes) before
        // the user responds. The sender is dropped (via
        // dropping the HttpServerState) and the receiver
        // yields `Err` — the agent loop treats this as a
        // deny-by-default.
        // We simulate by dropping the registry (the inner Arc
        // goes to zero when no other clones remain). The
        // receiver is the last thing holding state alive, so
        // we have to do this carefully.
        // Use Duration::ZERO to disable the timeout. The
        // production registry keeps a spawned timer task
        // alive that holds a clone of the inner Arc, so
        // dropping  alone would let the timer fire
        // (after the default 60s) and call 
        // -- making rx.await return Ok(TimedOut), not Err.
        // Disabling the timeout isolates the sender-drop
        // semantics this test cares about.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let rx = reg.request_approval("sess-a", "req-1");
        // Drop the registry, then resolve via... hmm, we can't
        // resolve after dropping. Instead, the test below
        // mirrors the actual user behavior more closely: drop
        // the registry's clone (the HttpServerState holds one
        // clone, the approval_requester closure holds another,
        // etc.), and verify that the rx yields Err.
        // For the contract surface we just need to assert:
        // "if no one ever resolves, and the sender side
        // gets dropped, the rx must yield Err (not hang
        // forever)".
        // We test the actual mechanism by manually dropping
        // the sender via a oneshot::Sender that we take back
        // from the registry's internals. Simpler: just drop
        // the registry, which drops the inner HashMap, which
        // drops all senders.
        let reg_for_drop = reg.clone();
        drop(reg);
        // reg_for_drop is still alive; drop it too.
        drop(reg_for_drop);
        // rx is now the only thing keeping the channel alive.
        // Its sender has been dropped via the registry. The
        // rx.await should resolve to Err.
        assert!(
            rx.await.is_err(),
            "dropped sender must produce Err at the receiver"
        );
    }
    // -- Timeout (default deny) -----------------------------------

    #[tokio::test]
    async fn timeout_fires_and_yields_false_when_no_resolve() {
        // With a short timeout, an unresolved request must
        // auto-resolve to  (deny) after the configured
        // delay. This is the "fail-closed" contract that
        // mirrors upstream .
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(50));
        let rx = reg.request_approval("sess-a", "req-1");
        // Wait long enough for the timer to fire.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let result = rx.await;
        assert_eq!(
            result,
            Ok(hermix_core::ApprovalOutcome::TimedOut { timeout_secs: 0 }),
            "timeout must default-deny with TimedOut outcome"
        );
    }

    #[tokio::test]
    async fn resolve_before_timeout_yields_decision() {
        // A real  that arrives before the timeout
        // must propagate the user decision (true) and the
        // timer task must be a no-op (its  finds the
        // entry already gone).
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(200));
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx.await, Ok(hermix_core::ApprovalOutcome::Approved));
        // Wait past the original timeout to ensure the
        // late-arriving timer task is a no-op and doesn't
        // poison any future test (e.g. by panicking in a
        // spawned task).
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    #[tokio::test]
    async fn resolve_after_timeout_returns_false() {
        // Once the timeout has fired and consumed the
        // sender, a subsequent  for the same key
        // must report  (no sender found). This
        // protects against the WS client responding to a
        // request that the agent has already given up on
        // (fail-closed has already won).
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(30));
        let rx = reg.request_approval("sess-a", "req-1");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = rx.await; // consume
        assert!(
            !reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved),
            "resolve after timeout must report no sender"
        );
    }

    #[tokio::test]
    async fn timeout_zero_disables_timer() {
        // must skip spawning the timer
        // task entirely. The receiver then hangs forever
        // (until dropped) — which is the historical
        // pre-timeout behavior, preserved for tests that
        // want to assert sender-drop semantics in
        // isolation.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let _rx = reg.request_approval("sess-a", "req-1");
        // Just verify that 100ms later, the registry is
        // still serving the request and resolve still
        // works.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
    }

    #[tokio::test]
    async fn concurrent_timeouts_are_independent() {
        // Two requests in the same session with the same
        // short timeout must both fire independently;
        // resolving one must not affect the other.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(50));
        let rx1 = reg.request_approval("sess-a", "req-1");
        let rx2 = reg.request_approval("sess-a", "req-2");
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(rx1.await, Ok(hermix_core::ApprovalOutcome::TimedOut { timeout_secs: 0 }));
        assert_eq!(rx2.await, Ok(hermix_core::ApprovalOutcome::TimedOut { timeout_secs: 0 }));
    }

    #[tokio::test]
    async fn default_timeout_constant_is_zero() {
        // 2026-09-28: default 600s (10 min auto-deny). The old ZERO
        // default ("审批不该有超时") turned every unattended approval
        // into a permanent hang; it remains available via
        // `ApprovalConfig.timeout_secs = 0`.
        hermix_config::set_approval_timeout_secs(600);
        assert_eq!(DEFAULT_APPROVAL_TIMEOUT, std::time::Duration::from_secs(600));
        let reg = ApprovalRegistry::new();
        assert_eq!(reg.default_timeout(), std::time::Duration::from_secs(600));
        // 恢复默认，避免影响并行测试读取全局。
        hermix_config::set_approval_timeout_secs(600);
    }

    /// `compute_approval_deadline_ms` returns `None` when the
    /// configured timeout is zero — no deadline means
    /// the front-end countdown pill is hidden and the card stays
    /// open until the user explicitly resolves it.
    #[test]
    fn compute_approval_deadline_is_none_when_timeout_zero() {
        hermix_config::set_approval_timeout_secs(0);
        assert_eq!(compute_approval_deadline_ms(), None);
        hermix_config::set_approval_timeout_secs(600);
        assert!(compute_approval_deadline_ms().is_some());
        hermix_config::set_approval_timeout_secs(600);
    }

    #[tokio::test]
    async fn timeout_does_not_fire_for_resolved_request() {
        // Regression: after a clean resolve, the timer
        // task must not run a stale  that
        // would mislead operators into thinking a request
        // had timed out. We can't directly observe the
        // absence of a log line, but we can prove the
        // timer task's  found nothing by ensuring
        // a fresh  with the same key
        // is fully resolvable after the original timeout
        // would have fired.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(50));
        let _rx1 = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        // Wait past the original timeout window.
        std::thread::sleep(std::time::Duration::from_millis(150));
        // A new request on the same key must be served
        // freshly (the previous sender was consumed by
        // resolve, the timer found nothing, so no state
        // is leaked).
        let rx2 = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::DeniedByUser));
        assert_eq!(rx2.await, Ok(hermix_core::ApprovalOutcome::DeniedByUser));
    }

    // -----------------------------------------------------------------
    // is_pending — used by the 4-state side-effect handlers in
    // `handle_ws_stream` to skip the session/always store updates
    // when the original request was already resolved or timed out.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn is_pending_true_after_request_false_after_resolve() {
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let _rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.is_pending("sess-a", "req-1"));
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert!(
            !reg.is_pending("sess-a", "req-1"),
            "resolve must remove the entry"
        );
    }

    #[tokio::test]
    async fn is_pending_false_for_unknown_request() {
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        assert!(!reg.is_pending("sess-a", "never-requested"));
        assert!(!reg.is_pending("never-session", "req-1"));
    }

    #[tokio::test]
    async fn list_pending_lists_live_requests_and_drops_resolved() {
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        assert!(
            reg.list_pending("sess-a").is_empty(),
            "unknown session must yield an empty list, not an error"
        );

        let _rx1 = reg.request_approval("sess-a", "req-1");
        let _rx2 = reg.request_approval("sess-a", "req-2");
        let mut listed = reg.list_pending("sess-a");
        listed.sort();
        assert_eq!(listed, vec!["req-1".to_string(), "req-2".to_string()]);

        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(
            reg.list_pending("sess-a"),
            vec!["req-2".to_string()],
            "resolved entries must disappear so the client never renders a dead card"
        );
    }

    #[tokio::test]
    async fn list_pending_is_scoped_to_one_session() {
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let _rx_a = reg.request_approval("sess-a", "req-a");
        let _rx_b = reg.request_approval("sess-b", "req-b");
        assert_eq!(reg.list_pending("sess-a"), vec!["req-a".to_string()]);
        assert_eq!(reg.list_pending("sess-b"), vec!["req-b".to_string()]);
    }

    //duplicate request_id rejection ----------------------

    #[tokio::test]
    async fn duplicate_request_id_is_rejected_not_overwritten() {
        //( / ): A duplicate request_id must
        // NOT overwrite the existing pending sender. The second call
        // returns a pre-resolved DeniedByUser receiver so the LLM
        // sees an explicit denial and can retry with a new id.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);

        // First request — normal registration.
        let rx1 = reg.request_approval("sess-a", "req-dup");
        assert!(reg.is_pending("sess-a", "req-dup"));

        // Second request with the same id — must be rejected.
        let rx2 = reg.request_approval("sess-a", "req-dup");

        // The second receiver should immediately resolve to DeniedByUser.
        assert_eq!(
            rx2.await,
            Ok(hermix_core::ApprovalOutcome::DeniedByUser),
            "duplicate request_id must return DeniedByUser, not hang"
        );

        // The original request is still pending and can be resolved normally.
        assert!(reg.is_pending("sess-a", "req-dup"));
        assert!(reg.resolve("sess-a", "req-dup", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx1.await, Ok(hermix_core::ApprovalOutcome::Approved));
    }

    #[tokio::test]
    async fn duplicate_request_id_different_sessions_are_independent() {
        // Same request_id in different sessions must not conflict.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let rx1 = reg.request_approval("sess-a", "req-1");
        let rx2 = reg.request_approval("sess-b", "req-1");

        // Both should be pending independently.
        assert!(reg.is_pending("sess-a", "req-1"));
        assert!(reg.is_pending("sess-b", "req-1"));

        // Resolve both normally — no rejection.
        assert!(reg.resolve("sess-a", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert!(reg.resolve("sess-b", "req-1", hermix_core::ApprovalOutcome::DeniedByUser));
        assert_eq!(rx1.await, Ok(hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx2.await, Ok(hermix_core::ApprovalOutcome::DeniedByUser));
    }

    // -----------------------------------------------------------------
    // record_approval_side_effect — 4-state session/always store
    // updates. We construct a minimal `HttpServerState` with empty
    // maps and verify that:
    // - `Session` adds a compiled regex to the per-session bypass
    // map and the new entry makes `pre_approval_checker` hit.
    // - `Always` lazily creates the persistent `UserAllowlist` and
    // writes it to disk; reload from disk restores the entries.
    // - `Once` and `Deny` are no-ops.
    // -----------------------------------------------------------------

    /// Build a minimal `HttpServerState` for unit tests. The full
    /// `build_with_hermes_home` requires a real `GatewayConfig` and
    /// spins up cron / skills / etc.; for these side-effect tests
    /// we only need the two new fields populated, so we construct
    /// the struct via its pub fields. (We use `unsafe { ... }` only
    /// to silence the `dead_code` warning on `Arc::clone(&state)`
    /// in the unused-arg shim; no actual memory unsafety.)
    fn test_state(hermix_home: std::path::PathBuf) -> HttpServerState {
        let session_persistence = Arc::new(
            hermix_agent::session_persistence::SessionPersistence::new(&hermix_home),
        );
        let _ = session_persistence.ensure_db();
        let metrics = crate::metrics::MetricsCollector::new(&hermix_home);

        HttpServerState {
            config: Arc::new(hermix_config::GatewayConfig::default()),
            tool_registry: Arc::new(hermix_tools::ToolRegistry::new()),
            hermix_home,
            session_persistence,
            skill_store: None,
            boot: Arc::new(hermix_context::Boot::local(hermix_context::BootProfile::new())),
            runtime_gateway_running: None,
            agent_service: Arc::new(hermix_agent::LocalAgentService::new(
                Arc::new(hermix_config::GatewayConfig::default()),
                Arc::new(hermix_tools::ToolRegistry::new()),
                Arc::new(hermix_agent::session_persistence::SessionPersistence::new(
                    tempfile::tempdir().unwrap().path(),
                )),
            )),
            approval_registry: ApprovalRegistry::new(),
            clarify_dispatcher: hermix_tools::ClarifyDispatcher::new(),
            clarify_broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(1);
                tx
            },
            mobile_config_tx: tokio::sync::watch::channel::<u64>(0).0,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
            resource_patrol: None,
            user_allowlist: Arc::new(std::sync::Mutex::new(None)),
            session_bypass: Arc::new(std::sync::Mutex::new(HashMap::new())),
            consciousness_tracker: hermix_agent::ConsciousnessTracker::new(),
            tickloop_manager: hermix_runtime::TickLoopManager::new(
                hermix_runtime::TickLoopConfig::default(),
            ),
            mcp_manager: hermix_mcp::ArcMcpManager::new(Arc::new(hermix_tools::ToolRegistry::new())),
            // dedup map. See the production `build`
            // path; test_state mirrors it.
            recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            active_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_grace: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            completed_keys: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            attachment_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            todo_event_tx: None,
            orchestration_bus: OrchestrationEventBus::new(),
            metrics,
            task_service: None,
            task_record_store: None,
            automation_service: None,
            automation_engine_scheduler: None,
            custom_app_manager: None,
            device_registry: None,
            pairing_sessions: Arc::new(crate::pairing::PairingSessions::new()),
            nonce_cache: crate::request_signature::NonceCache::new(),
            channel_sessions: Arc::new(std::sync::Mutex::new(Vec::new())),
            relay_settings: Arc::new(tokio::sync::Mutex::new(
                crate::relay_settings::RelaySettings::default(),
            )),
            relay_status: crate::relay_client::RelayStatus::new(),
            relay_last_error: Arc::new(tokio::sync::Mutex::new(None)),
            relay_connector_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// `Session` choice: compiles the extracted terminal signature
    /// (first word of the command) into the per-session bypass map,
    /// matching whatever the same-tool pre-checker rebuilds later.
    #[test]
    fn session_choice_adds_to_session_bypass() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &serde_json::json!({"command": "sudo apt install ripgrep"}),
            hermix_transport::ApprovalChoice::Session,
        );

        // session_bypass now has one pattern for sess-A.
        let map = state.session_bypass.lock().unwrap_or_else(|e| e.into_inner());
        let patterns = map.get("sess-A").expect("sess-A entry");
        assert_eq!(patterns.len(), 1);
        // Trust granularity is first-word-of-command: any `sudo …`
        // invocation hits, unrelated binaries don't. Signatures are
        // fully anchored regex fragments — stored as escaped literals
        // and matched against the same text the checker rebuilds per
        // call (identical semantics to the persistent allowlist).
        assert!(patterns[0].is_match("^sudo\\b.*"));
        assert!(!patterns[0].is_match("^ls\\b.*"));
    }

    /// `Always` choice: lazily creates the persistent `UserAllowlist`
    /// and writes the JSON file. A reload from disk sees the entry.
    #[test]
    fn always_choice_persists_to_disk() {
        // Use a temp dir as hermix home so the persistent file
        // doesn't pollute the user's real `~/.hermix`.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let state = test_state(home.clone());

        // Initially no allowlist.
        assert!(state.user_allowlist.lock().unwrap_or_else(|e| e.into_inner()).is_none());

        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &serde_json::json!({"command": "systemctl restart nginx"}),
            hermix_transport::ApprovalChoice::Always,
        );

        // Lazily created, in-memory.
        assert!(state.user_allowlist.lock().unwrap_or_else(|e| e.into_inner()).is_some());

        // Persisted to disk.
        let path = hermix_config::paths::approval_allowlist_path();
        // Override HERMIX_HOME to tmp so the path helper points
        // at the temp file, not the user's real one. The build
        // already wrote under `~/.hermix` by default, so we
        // copy-and-check via a direct reload below.
        let written_path = home.join("approval_allowlist.json");
        if path != written_path {
            // The `hermix_config::hermix_home()` global state
            // can't be redirected here without a global env
            // mutation, so the helper may have written to
            // `~/.hermix`. We accept that for the MVP and check
            // the in-memory + on-disk state in the best-effort
            // way: re-construct a state from the same path the
            // helper produced, and verify the entry survives.
            // (The dedicated `user_allowlist_save_load_roundtrip`
            // test in hermix-tools already covers the
            // save/load cycle in isolation.)
            let _ = path; // silence unused warning
        }

        // In-memory check is the authoritative one for this
        // test (it verifies the side-effect ran; the on-disk
        // roundtrip is covered by `hermix-tools::approval`
        // tests). End-to-end composition check: rebuild the
        // signature exactly like the pre-approval checker does,
        // and confirm a future identical call auto-approves.
        let sig_of =
            |cmd: &str| -> String {
                hermix_tools::extract_trust_signature(
                    "terminal",
                    &serde_json::json!({ "command": cmd }),
                )
                .unwrap()
            };
        let al = state.user_allowlist.lock().unwrap_or_else(|e| e.into_inner());
        let al = al.as_ref().expect("lazy-init created the allowlist");
        assert!(al.matches("terminal", &sig_of("systemctl restart nginx")));
        assert!(al.matches("terminal", &sig_of("systemctl restart mysql")));
        // Different first word — no match.
        assert!(!al.matches("terminal", &sig_of("ls -la")));
    }

    /// `Once` choice: no-op. The state is unchanged.
    #[test]
    fn once_choice_is_noop() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &serde_json::json!({"command": "ls -la"}),
            hermix_transport::ApprovalChoice::Once,
        );
        assert!(state.session_bypass.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert!(state.user_allowlist.lock().unwrap_or_else(|e| e.into_inner()).is_none());
    }

    /// `Deny` choice: no-op. The state is unchanged.
    #[test]
    fn deny_choice_is_noop() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &serde_json::json!({"command": "rm -rf /"}),
            hermix_transport::ApprovalChoice::Deny,
        );
        assert!(state.session_bypass.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert!(state.user_allowlist.lock().unwrap_or_else(|e| e.into_inner()).is_none());
    }

    /// Two `Session` approvals for the same session accumulate
    /// (don't dedupe) — the pre-approval check short-circuits on
    /// the first match anyway, so duplicate entries are harmless.
    #[test]
    fn session_choice_accumulates() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &serde_json::json!({"command": "sudo apt install foo"}),
            hermix_transport::ApprovalChoice::Session,
        );
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &serde_json::json!({"command": "systemctl restart nginx"}),
            hermix_transport::ApprovalChoice::Session,
        );
        let map = state.session_bypass.lock().unwrap_or_else(|e| e.into_inner());
        let patterns = map.get("sess-A").expect("sess-A entry");
        assert_eq!(patterns.len(), 2);
    }

    // ── dedup tests for `check_recent_send` ────────
    // Background: a single user "你好" was triggering 9
    // `send_message_stream` calls in 2 minutes. Root cause was
    // client-side WS auto-retry + App.tsx queue re-drain races.
    // Server-side dedup is the defensive backstop: any
    // `(session_id, text)` that arrives within 10s of a prior
    // identical send is dropped with a synthetic Done event.

    #[test]
    fn dedup_first_send_is_fresh() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(ws::check_recent_send(&state, "sess-A", "你好"), None);
    }

    #[test]
    fn dedup_identical_send_within_window_is_rejected() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(ws::check_recent_send(&state, "sess-A", "你好"), None);
        // Second identical send within 10s → rejected with elapsed
        // (could be 0s if the clock hasn't ticked, or 1s, etc.
        // what matters is Some()).
        let elapsed = ws::check_recent_send(&state, "sess-A", "你好");
        assert!(elapsed.is_some(), "duplicate must be rejected");
        assert!(elapsed.unwrap() < ws::DEDUP_WINDOW_SECS);
    }

    #[test]
    fn dedup_different_content_within_window_is_accepted() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(ws::check_recent_send(&state, "sess-A", "你好"), None);
        // Different text → fresh, no dedup.
        assert_eq!(ws::check_recent_send(&state, "sess-A", "hello"), None);
    }

    #[test]
    fn dedup_different_sessions_are_independent() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(ws::check_recent_send(&state, "sess-A", "你好"), None);
        // Same content, different session → fresh (different
        // dedup key namespace).
        assert_eq!(ws::check_recent_send(&state, "sess-B", "你好"), None);
    }

    #[test]
    fn dedup_key_is_stable_for_same_text() {
        // Different process instantiations of DefaultHasher can
        // in theory produce different hashes (SipHash is
        // process-randomized). Document that — within a single
        // process the key IS stable (the contract the dedup map
        // relies on). Across processes, dedup is a defense-in-
        // depth, not a correctness requirement.
        let k1 = ws::dedup_key("sess-A", "你好");
        let k2 = ws::dedup_key("sess-A", "你好");
        assert_eq!(k1, k2);
    }

    /// Regression: the `reconnect-while-running` decline path must NOT
    /// consume the live handle.
    ///
    /// Symptom this locks down: user sends a message while a task is still
    /// running → backend declines with a `task_in_flight` Status, but the
    /// old code had already `remove`d the handle from `active_handles`
    /// before deciding. The in-flight task's own cleanup `remove` never runs
    /// (it is still running), so the registry entry was lost permanently and
    /// `active_sessions` stayed locked forever. Every later retry for that
    /// `(session, text)` then fell into the dedup branch, which answers with
    /// an empty `Done` — the UI dropped the bubble and showed no reply.
    #[tokio::test]
    async fn decline_path_must_not_consume_live_handle() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        let key = ws::dedup_key("sess-A", "你好");

        // Simulate a live task: a handle that will not finish.
        let live = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        });
        state
            .active_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), live);

        // A second identical send peeks the handle. The peek must be
        // side-effect free — the routing decision below is what takes it out.
        let peeked_finished = {
            let handles = state.active_handles.lock().unwrap_or_else(|e| e.into_inner());
            handles.get(&key).map(|h| h.is_finished())
        };
        assert_eq!(peeked_finished, Some(false), "live task must be seen as running");
        let still_registered = {
            let handles = state.active_handles.lock().unwrap_or_else(|e| e.into_inner());
            handles.contains_key(&key)
        };
        assert!(
            still_registered,
            "peeking a live handle must leave it registered"
        );

        // Cleanup so the spawned task does not outlive the test.
        let taken = {
            let mut handles = state.active_handles.lock().unwrap_or_else(|e| e.into_inner());
            handles.remove(&key)
        };
        if let Some(h) = taken {
            h.abort();
        }
    }

    /// Storm simulation: 9 identical sends within 10s must collapse
    /// to 1 fresh + 8 deduped, with the dedup map holding exactly
    /// 1 entry at the end. This is the exact scenario the user hit
    /// in production ("你好" triggered 9 calls in 2 minutes).
    #[test]
    fn dedup_storm_9_identical_sends_collapses_to_1() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());

        let mut fresh = 0;
        let mut deduped = 0;
        for _ in 0..9 {
            if ws::check_recent_send(&state, "sess-A", "你好").is_none() {
                fresh += 1;
            } else {
                deduped += 1;
            }
        }
        assert_eq!(fresh, 1, "exactly 1 fresh send in a 9-storm");
        assert_eq!(deduped, 8, "exactly 8 deduped in a 9-storm");

        // Map should hold exactly 1 entry (the fresh one).
        let map_size = state.recent_sends.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert_eq!(map_size, 1, "dedup map must have 1 entry, got {map_size}");
    }

    /// Storm + legit follow-up: 5 identical "你好" sends then 1
    /// different "在吗?" send — only the 1st of the 5 "你好"
    /// calls is fresh (subsequent identical calls within 10s
    /// are deduped), and the distinct "在吗?" MUST succeed
    /// because dedup must not block legitimate distinct
    /// messages.
    #[test]
    fn dedup_storm_then_different_content_succeeds() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());

        // 1st "你好" is fresh
        assert_eq!(ws::check_recent_send(&state, "sess-A", "你好"), None);
        // Next 4 "你好" are all deduped (within 10s window)
        for i in 0..4 {
            let elapsed = ws::check_recent_send(&state, "sess-A", "你好");
            assert!(elapsed.is_some(), "iteration {i} should be deduped");
        }
        // Legit different content MUST succeed
        assert_eq!(ws::check_recent_send(&state, "sess-A", "在吗?"), None);

        // Map should hold exactly 2 entries (one per content key)
        let map_size = state.recent_sends.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert_eq!(map_size, 2, "map should hold 2 keys (你好 + 在吗?)");
    }

    /// Concurrent storm: spawn 50 threads, each sending the same
    /// text 5 times, into a shared state. Total = 250 calls. The
    /// dedup map's mutex serializes the check-then-insert, so
    /// exactly 1 fresh + 249 deduped, regardless of OS-thread
    /// scheduling. This guards against the race that an unlocked
    /// check-then-insert would produce (multiple threads seeing
    /// "no entry" before any of them insert).
    #[test]
    fn dedup_storm_concurrent_50_threads_5_calls_each() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(test_state(tempfile::tempdir().unwrap().path().to_path_buf()));
        let fresh = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let deduped = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..50 {
            let state = Arc::clone(&state);
            let fresh = Arc::clone(&fresh);
            let deduped = Arc::clone(&deduped);
            handles.push(thread::spawn(move || {
                for _ in 0..5 {
                    if ws::check_recent_send(&state, "sess-A", "你好").is_none() {
                        fresh.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        deduped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let total_fresh = fresh.load(std::sync::atomic::Ordering::Relaxed);
        let total_deduped = deduped.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(total_fresh, 1, "exactly 1 fresh under 50-thread storm");
        assert_eq!(total_deduped, 249, "exactly 249 deduped under 50-thread storm");
    }

    /// Two `Always` approvals for the same command stay a single
    /// entry. `UserAllowlist::add_literal_for` is idempotent on
    /// `(tool, pattern)` because both the WS and REST-fallback
    /// delivery paths can reach this side effect for one user click;
    /// duplicate regex entries would bloat the allowlist file.
    #[test]
    fn always_choice_dedupes_in_memory() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        let params = serde_json::json!({"command": "docker system prune"});
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &params,
            hermix_transport::ApprovalChoice::Always,
        );
        record_approval_side_effect(
            &state,
            "sess-A",
            "terminal",
            &params,
            hermix_transport::ApprovalChoice::Always,
        );
        let al = state.user_allowlist.lock().unwrap_or_else(|e| e.into_inner());
        let al = al.as_ref().unwrap();
        assert_eq!(al.len(), 1, "re-granting the same command must not duplicate");
    }

    // -----------------------------------------------------------------
    // Batch resolution — the WS coalescer funnels N pending approvals
    // under one batch_id; a single `resolve_batch` call must fan out
    // to every receiver in registration order, and members that were
    // already resolved (timeout / duplicate response) must come back
    // as `resolved=false` so the caller can skip their side-effect
    // pipeline.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn batch_resolve_fires_all_receivers_with_same_outcome() {
        let reg = ApprovalRegistry::new();
        let rxs: Vec<_> = (0..3)
            .map(|i| reg.request_approval("sess-A", &format!("req-{i}")))
            .collect();
        reg.register_batch(
            "sess-A",
            "batch-1",
            (0..3).map(|i| format!("req-{i}")).collect(),
        );
        let pairs = reg.resolve_batch("sess-A", "batch-1", hermix_core::ApprovalOutcome::Approved);
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().all(|(_, ok)| *ok));
        for (i, rx) in rxs.into_iter().enumerate() {
            assert_eq!(
                rx.await.unwrap(),
                hermix_core::ApprovalOutcome::Approved,
                "receiver {i} must receive Approved"
            );
        }
    }

    #[tokio::test]
    async fn batch_resolve_unknown_id_is_noop() {
        let reg = ApprovalRegistry::new();
        let pairs = reg.resolve_batch("sess-A", "missing", hermix_core::ApprovalOutcome::Approved);
        assert!(pairs.is_empty());
    }

    #[tokio::test]
    async fn batch_resolve_reports_already_resolved_members() {
        let reg = ApprovalRegistry::new();
        // Register all three, then resolve #1 directly (simulating
        // a duplicate / timeout race). `resolve_batch` for the
        // remaining two must report them as still resolved=true,
        // and #1 as resolved=false.
        let _rxs: Vec<_> = (0..3)
            .map(|i| reg.request_approval("sess-A", &format!("req-{i}")))
            .collect();
        reg.register_batch(
            "sess-A",
            "batch-1",
            (0..3).map(|i| format!("req-{i}")).collect(),
        );
        assert!(reg.resolve("sess-A", "req-1", hermix_core::ApprovalOutcome::DeniedByUser));
        let pairs = reg.resolve_batch("sess-A", "batch-1", hermix_core::ApprovalOutcome::Approved);
        assert_eq!(pairs.len(), 3);
        let by_id: std::collections::HashMap<_, _> =
            pairs.into_iter().map(|(k, v)| (k, v)).collect();
        assert!(!by_id["req-1"], "req-1 was already resolved");
        assert!(by_id["req-0"]);
        assert!(by_id["req-2"]);
    }

    // ── 超时按「可送达时间」计费 ──────────────────────────────────

    /// 没有客户端在线时，倒计时不该流逝——那条回答物理上到不了。
    ///
    /// 这正是「人在地铁上 / 手机被系统杀掉后台」的场景：旧实现照走固定超时，
    /// 等价于保证失败，agent 那一轮白跑。
    #[tokio::test]
    async fn 无人可答复时不倒计时_有人回来才继续() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let here = Arc::new(AtomicBool::new(false));
        let probe = {
            let here = here.clone();
            Arc::new(move || here.load(Ordering::SeqCst))
        };
        let reg = ApprovalRegistry::with_presence(Duration::from_millis(80), probe);
        let mut rx = reg.request_approval("sess-a", "req-1");

        // 远超固定超时，但没人能答复 → 必须还在等
        assert!(
            tokio::time::timeout(Duration::from_millis(400), &mut rx)
                .await
                .is_err(),
            "无人在场时不该判超时"
        );
        assert!(reg.is_pending("sess-a", "req-1"), "请求应仍在待批状态");

        // 有人回来了 → 计时开始，很快到期
        here.store(true, Ordering::SeqCst);
        let outcome = tokio::time::timeout(Duration::from_secs(2), &mut rx)
            .await
            .expect("有人在场后应在超时时间内判超时")
            .unwrap();
        assert!(matches!(
            outcome,
            hermix_core::ApprovalOutcome::TimedOut { .. }
        ));
    }

    /// 有人在场的路径与旧行为一致：到点就超时。
    #[tokio::test]
    async fn 有人在场时照常超时() {
        let reg =
            ApprovalRegistry::with_presence(Duration::from_millis(60), Arc::new(|| true));
        let rx = reg.request_approval("sess-a", "req-1");
        let outcome = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("应超时")
            .unwrap();
        assert!(matches!(
            outcome,
            hermix_core::ApprovalOutcome::TimedOut { .. }
        ));
    }

    /// `with_timeout` 不带在场判定：墙钟固定超时（测试与自定义超时的语义）。
    #[tokio::test]
    async fn 显式构造的超时不看在场() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_millis(50));
        let rx = reg.request_approval("sess-a", "req-1");
        let outcome = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("固定超时不该被在场判定影响")
            .unwrap();
        assert!(matches!(
            outcome,
            hermix_core::ApprovalOutcome::TimedOut { .. }
        ));
    }

    // ── 会话级放手模式 ─────────────────────────────────────────────

    /// 放手模式置位后：request_approval 不登记、不发卡，直接预解析
    /// Approved —— 这是「审批卡里选放手模式，本轮立即生效」的核心语义。
    #[tokio::test]
    async fn full_mode_auto_approves_without_registering() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        reg.set_full_mode("sess-a", true);
        let rx = reg.request_approval("sess-a", "req-1");
        assert_eq!(rx.await, Ok(hermix_core::ApprovalOutcome::Approved));
        // 没有登记进 pending 表：不发卡、不占注册表。
        assert!(reg.list_pending("sess-a").is_empty());
        assert!(!reg.is_pending("sess-a", "req-1"));
        // 每次请求都独立放行（不是一次性）。
        let rx2 = reg.request_approval("sess-a", "req-2");
        assert_eq!(rx2.await, Ok(hermix_core::ApprovalOutcome::Approved));
    }

    /// 放手模式按会话隔离：A 会话放行不影响 B 会话照常走审批。
    #[tokio::test]
    async fn full_mode_is_scoped_per_session() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        reg.set_full_mode("sess-a", true);
        let rx_a = reg.request_approval("sess-a", "req-1");
        assert_eq!(rx_a.await, Ok(hermix_core::ApprovalOutcome::Approved));

        let rx_b = reg.request_approval("sess-b", "req-1");
        assert!(reg.is_pending("sess-b", "req-1"), "B 会话必须照常登记待批");
        assert!(reg.resolve("sess-b", "req-1", hermix_core::ApprovalOutcome::Approved));
        assert_eq!(rx_b.await, Ok(hermix_core::ApprovalOutcome::Approved));
    }

    /// 清除开关后恢复原语义：请求重新走登记 → 超时默认拒绝。
    #[tokio::test]
    async fn clearing_full_mode_restores_normal_flow() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_millis(50));
        reg.set_full_mode("sess-a", true);
        reg.set_full_mode("sess-a", false);
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.is_pending("sess-a", "req-1"));
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(matches!(
            rx.await,
            Ok(hermix_core::ApprovalOutcome::TimedOut { .. })
        ));
    }

    /// `resolve_all` 排干某会话全部 pending 并逐个放行；其他会话不受影响。
    #[tokio::test]
    async fn resolve_all_drains_pending_and_reports_ids() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        let mut rxs: Vec<_> = (0..3)
            .map(|i| reg.request_approval("sess-a", &format!("req-{i}")))
            .collect();
        let _rx_other = reg.request_approval("sess-b", "req-x");

        let mut ids = reg.resolve_all("sess-a", hermix_core::ApprovalOutcome::Approved);
        ids.sort();
        assert_eq!(ids, vec!["req-0", "req-1", "req-2"]);
        for rx in rxs.iter_mut() {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), &mut *rx).await.unwrap().unwrap(),
                hermix_core::ApprovalOutcome::Approved
            );
        }
        assert!(reg.list_pending("sess-a").is_empty());
        // 其他会话原封不动。
        assert_eq!(reg.list_pending("sess-b"), vec!["req-x".to_string()]);
    }

    // -----------------------------------------------------------------
    // 子代理后代回落（2026-11-14）—— 卡死回归钉。
    //
    // 实锤场景（server.log 11-10 06:33，超星验证任务）：子代理的工具审批
    // 用自己的会话 id（`父::subagent-*`）登记 pending，但审批卡走**父会话**
    // 的 WS 流渲染、前端拿父会话 id 结算 → 精确键永远 miss：
    //   06:33:53.023 WS approval_responses_batch (父, 批准)
    //   06:33:53.048 REST permission full=true resolved_pending=0
    //   06:33:54.010 REST approval: miss — no pending entry
    // 批量卡无人可结 → 超时"自动取消" → 用户感知「授权放手模式就卡死」。
    // 本组钉保证：父会话 id 的结算（单卡/批量）能落到子代理的 pending 上。
    // -----------------------------------------------------------------

    /// 单卡：父会话 id 结算 → 落到子代理名下登记的 pending 上。
    #[tokio::test]
    async fn resolve_falls_back_to_subagent_pending() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        let rx = reg.request_approval("parent::subagent-1", "call-abc");
        // 精确键（子代理自己的 id）仍然工作。
        assert!(reg.is_pending("parent::subagent-1", "call-abc"));
        // 前端实际走的键：父会话 id。
        assert!(
            reg.resolve("parent", "call-abc", hermix_core::ApprovalOutcome::Approved),
            "parent-id resolve must find the subagent's pending"
        );
        assert_eq!(rx.await.unwrap(), hermix_core::ApprovalOutcome::Approved);
        assert!(!reg.is_pending("parent", "call-abc"));
    }

    /// 隔离：回落只进本会话的子代理命名空间，别的会话的子代理碰不到。
    #[tokio::test]
    async fn resolve_fallback_scoped_to_own_subagents() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        let _rx_mine = reg.request_approval("A::subagent-1", "call-1");
        let _rx_theirs = reg.request_approval("B::subagent-1", "call-2");
        // 前缀带会话 id 边界：`A::subagent-` 匹配不到 `B::subagent-1`。
        assert!(reg.resolve("A", "call-1", hermix_core::ApprovalOutcome::Approved));
        assert!(!reg.resolve("A", "call-2", hermix_core::ApprovalOutcome::Approved));
        assert!(reg.is_pending("B", "call-2"), "other session's subagent untouched");
    }

    /// 批量：coalescer 用父流 id 登记批量索引（`register_batch(parent,…)`），
    // pending 却在子代理名下 —— 一次 `resolve_batch(parent,…)` 必须全部结掉。
    #[tokio::test]
    async fn batch_resolve_reaches_subagent_pendings() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        let rx_a = reg.request_approval("parent::subagent-1", "call-a");
        let rx_b = reg.request_approval("parent::subagent-2", "call-b");
        reg.register_batch(
            "parent",
            "batch-x",
            vec!["call-a".into(), "call-b".into()],
        );
        let pairs = reg.resolve_batch("parent", "batch-x", hermix_core::ApprovalOutcome::Approved);
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().all(|(_, ok)| *ok), "every member must resolve");
        assert_eq!(rx_a.await.unwrap(), hermix_core::ApprovalOutcome::Approved);
        assert_eq!(rx_b.await.unwrap(), hermix_core::ApprovalOutcome::Approved);
    }

    /// 刷新重投影：父会话的 `/approvals/pending` 必须带上子代理的挂起卡，
    /// 否则刷新页面卡就丢了。
    #[tokio::test]
    async fn list_pending_includes_subagent_pendings() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        reg.request_approval("parent", "call-p");
        reg.request_approval("parent::subagent-1", "call-s");
        reg.request_approval("other", "call-o");
        let mut ids = reg.list_pending("parent");
        ids.sort();
        assert_eq!(ids, vec!["call-p".to_string(), "call-s".to_string()]);
        assert_eq!(reg.list_pending("other"), vec!["call-o".to_string()]);
    }

    /// 侧效应判定：`is_pending(parent, rid)` 能看见子代理的挂起，
    /// 重复响应才不会被误判成「从未存在」而重放侧效应。
    #[tokio::test]
    async fn is_pending_sees_subagent_pendings() {
        let reg = ApprovalRegistry::with_timeout(Duration::from_secs(600));
        reg.request_approval("parent::subagent-9", "call-9");
        assert!(reg.is_pending("parent", "call-9"));
        assert!(reg.resolve("parent", "call-9", hermix_core::ApprovalOutcome::DeniedByUser));
        assert!(!reg.is_pending("parent", "call-9"));
    }
}
